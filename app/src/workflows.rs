//! Workflow engine — drives a `WorkflowRun`'s DAG over the existing turn
//! machinery. All the pure logic (ready-set, gate outcomes, templates) lives
//! in `hive_core::workflow`; this module owns the IO: posting stage prompts,
//! running turns, creating gate proposals, suspending on votes, and
//! persisting every transition as a synced `WorkflowRunUpserted` event.
//!
//! Concurrency model: one driver task per run, and the driver is the run
//! record's *single writer* (votes mutate proposals, never the run). The
//! driver holds the session's `responding` slot for the whole run so
//! `maybe_respond` can't double-dispatch into the same chat mid-run.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hive_core::workflow::{self as wf, NodeRunStatus, WorkflowNodeKind, WorkflowRunStatus};
use hive_core::{
    ActionProposal, ChatMessage, ChatSession, MessageRole, ProposalKind, ProposalStatus, Timestamp,
    WorkspaceAgent,
};
use hive_proto::{
    WorkflowDefinitionDto, WorkflowNodeDto, WorkflowNodeRunDto, WorkflowRunDto, WorkflowRunEvent,
};
use hive_runtime::ChatTurn;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::{
    map_err, responder_for, rfc3339, run_prepared_turn, windowed_context,
    AppState, Responder, TurnOutcome,
};

/// How long a gate suspension sleeps between proposal re-checks. Local votes
/// wake the driver instantly via `Notify`; this poll is the safety net for
/// votes that arrive from other devices through the sync loop.
const GATE_POLL: Duration = Duration::from_secs(5);

/// How long a remote-stage suspension sleeps between transcript re-checks. Like
/// `GATE_POLL` this is only the safety net: the sync loop pokes this run's waker
/// whenever it ingests new events, so a synced reply is normally picked up
/// within sync latency rather than on this poll.
const REMOTE_POLL: Duration = Duration::from_secs(5);

/// Bounded wait for a remote-owned stage's reply to sync back before the node is
/// failed. Cross-device dispatch has no delivery guarantee (the owner's device /
/// worker may be offline), so the wait is capped instead of hanging the run
/// forever. Generous enough to cover an LLM turn plus a few sync round-trips.
const REMOTE_STAGE_TIMEOUT: Duration = Duration::from_secs(300);

/// Sentinel error a remote-stage wait returns when the run is being canceled, so
/// the driver reverts the node to `Pending` and lets the top-of-loop cancel
/// handler skip it cleanly instead of recording a genuine failure. The NUL
/// prefix keeps it distinct from any real provider/error string.
const CANCELED_WHILE_WAITING: &str = "\u{0}workflow-canceled";

/// Truncation for the run card's per-node output preview.
const EXCERPT_CHARS: usize = 400;

// ---------------------------------------------------------------------------
// Session-busy guard
// ---------------------------------------------------------------------------

/// Owns the session's slot in `AppState.responding` for the lifetime of a
/// run. Acquired in the start/resume command (so callers get a clear error
/// instead of a silently-queued run) and moved into the driver task.
pub(crate) struct SessionBusyGuard {
    app: AppHandle,
    session_id: Uuid,
}

impl SessionBusyGuard {
    pub(crate) fn acquire(app: &AppHandle, session_id: Uuid) -> Result<Self, String> {
        let state = app.state::<AppState>();
        let mut inflight = state.responding.lock().unwrap();
        if !inflight.insert(session_id) {
            return Err("another response or workflow is already running in this chat".into());
        }
        Ok(Self { app: app.clone(), session_id })
    }
}

impl Drop for SessionBusyGuard {
    fn drop(&mut self) {
        let state = self.app.state::<AppState>();
        state.responding.lock().unwrap().remove(&self.session_id);
    }
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// Deregisters a run's driver-state (wakers, gate map, cancel flag) on drop,
/// so a panic in the driver can't leave a run permanently "being driven"
/// (uncancelable/unresumable until app restart).
struct DriverRegistration {
    app: AppHandle,
    run_id: Uuid,
}

impl Drop for DriverRegistration {
    fn drop(&mut self) {
        let state = self.app.state::<AppState>();
        state.run_wakers.lock().unwrap().remove(&self.run_id);
        state.gate_runs.lock().unwrap().retain(|_, r| *r != self.run_id);
        state.canceled_runs.lock().unwrap().remove(&self.run_id);
    }
}

/// Pure crash-recovery selector: given every persisted run's `(id, status)`
/// and the set of run ids that already have a live in-process driver, return
/// the ids that need a driver re-spawned. A run qualifies iff it is
/// `Running`/`AwaitingGate` (interruptible states) *and* has no live driver.
/// Terminal runs (Completed/Failed/Halted/Canceled) and already-driven runs
/// are excluded, so re-running this after spawning is idempotent.
pub(crate) fn runs_needing_recovery(
    runs: impl IntoIterator<Item = (Uuid, WorkflowRunStatus)>,
    live: &std::collections::HashSet<Uuid>,
) -> Vec<Uuid> {
    runs.into_iter()
        .filter(|(id, status)| {
            matches!(
                status,
                WorkflowRunStatus::Running | WorkflowRunStatus::AwaitingGate
            ) && !live.contains(id)
        })
        .map(|(id, _)| id)
        .collect()
}

pub(crate) async fn drive_run(
    app: AppHandle,
    session_id: Uuid,
    workspace_id: Uuid,
    run_id: Uuid,
    guard: SessionBusyGuard,
) {
    let _guard = guard;
    let _registration = DriverRegistration { app: app.clone(), run_id };
    let waker = Arc::new(Notify::new());
    {
        let state = app.state::<AppState>();
        state.run_wakers.lock().unwrap().insert(run_id, waker.clone());
    }

    if let Err(e) = drive_run_inner(&app, session_id, workspace_id, run_id, &waker).await {
        eprintln!("workflow: run {run_id} errored: {e}");
        mark_run_failed(&app, session_id, workspace_id, run_id, &e);
    }
}

async fn drive_run_inner(
    app: &AppHandle,
    session_id: Uuid,
    workspace_id: Uuid,
    run_id: Uuid,
    waker: &Notify,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    loop {
        let (mut run, session) = load_run(&state, session_id, run_id)?;

        // Bail if the run was settled elsewhere (e.g. another device of this
        // account persisted Canceled/Completed via sync). Re-deriving from a
        // terminal snapshot would misreport it — all-Skipped nodes derive as
        // Completed — and re-persisting would revert the external decision.
        if matches!(
            run.status,
            WorkflowRunStatus::Canceled
                | WorkflowRunStatus::Completed
                | WorkflowRunStatus::Failed
                | WorkflowRunStatus::Halted
        ) {
            close_orphaned_gates(app, &state, session_id, workspace_id, &run)?;
            return Ok(());
        }

        // Cancellation — mark everything unfinished skipped and stop.
        if state.canceled_runs.lock().unwrap().remove(&run_id) {
            for n in &mut run.nodes {
                if !n.status.is_terminal() {
                    n.status = NodeRunStatus::Skipped;
                }
            }
            run.status = WorkflowRunStatus::Canceled;
            persist_run(app, &state, session_id, workspace_id, &mut run)?;
            close_orphaned_gates(app, &state, session_id, workspace_id, &run)?;
            return Ok(());
        }

        // Fold settled gate proposals into the run (and make sure every
        // still-open gate is registered for instant local-vote wakeups —
        // after resume the registry starts empty).
        for i in 0..run.nodes.len() {
            if run.nodes[i].status != NodeRunStatus::AwaitingApproval {
                continue;
            }
            let Some(pid) = run.nodes[i].proposal_id else { continue };
            let node_id = run.nodes[i].node_id.clone();
            match session.proposals.iter().find(|p| p.id == pid).map(|p| p.status) {
                Some(ProposalStatus::Approved) | Some(ProposalStatus::Applied) => {
                    wf::apply_gate_outcome(&mut run, &node_id, true);
                    state.gate_runs.lock().unwrap().remove(&pid);
                }
                Some(ProposalStatus::Rejected) => {
                    wf::apply_gate_outcome(&mut run, &node_id, false);
                    state.gate_runs.lock().unwrap().remove(&pid);
                }
                Some(ProposalStatus::Open) => {
                    state.gate_runs.lock().unwrap().insert(pid, run_id);
                }
                None => {
                    // Proposal vanished (shouldn't happen) — treat as rejected.
                    wf::apply_gate_outcome(&mut run, &node_id, false);
                }
            }
        }

        wf::propagate_skips(&mut run);
        run.status = wf::derive_run_status(&run);
        // A rejected RouteTo gate resets its subtree — which may include a
        // *sibling* gate that was awaiting approval — and skips cascade over
        // gate nodes too. Close any gate proposals those transitions abandoned
        // so they don't linger as votable cards.
        close_orphaned_gates(app, &state, session_id, workspace_id, &run)?;
        if matches!(
            run.status,
            WorkflowRunStatus::Completed | WorkflowRunStatus::Failed | WorkflowRunStatus::Halted
        ) {
            return persist_run(app, &state, session_id, workspace_id, &mut run);
        }

        let ready: Vec<_> = wf::ready_nodes(&run).into_iter().cloned().collect();
        if ready.is_empty() {
            let awaiting =
                run.nodes.iter().any(|n| n.status == NodeRunStatus::AwaitingApproval);
            if !awaiting {
                // No ready stages, no gates, not terminal: wedged (should be
                // unreachable — Running nodes never persist across driver
                // iterations).
                return Err("no runnable stages left".into());
            }
            persist_run(app, &state, session_id, workspace_id, &mut run)?;
            tokio::select! {
                _ = waker.notified() => {}
                _ = tokio::time::sleep(GATE_POLL) => {}
            }
            continue;
        }

        // Open gates first (cheap, no turns), then run all ready agent
        // stages in parallel.
        let outputs = collect_outputs(&run, &session);
        let mut agent_nodes = Vec::new();
        // Serialize stages targeting the SAME agent: two concurrent prompts to
        // one (remote) agent can't be correlated to its reply — both stages would
        // match the single synced answer, silently dropping one prompt's result.
        // Claim one stage per agent this iteration; the rest stay ready and run
        // next round (once the first completes).
        let mut claimed_agents: std::collections::HashSet<Option<Uuid>> = std::collections::HashSet::new();
        for node in &ready {
            match &node.kind {
                WorkflowNodeKind::Gate {
                    title_template,
                    body_template,
                    required_approvals,
                    ..
                } => {
                    let title: String = wf::render_template(title_template, &run.input, &outputs)
                        .chars()
                        .take(160)
                        .collect();
                    // A workflow gate has no human author; empty author means the
                    // self-approval guard excludes no one.
                    let mut proposal = ActionProposal::new(title, ProposalKind::Decision, "");
                    proposal.body = wf::render_template(body_template, &run.input, &outputs);
                    proposal.required_approvals = (*required_approvals).max(1);
                    let pid = proposal.id;
                    {
                        let mut svc = state.service.lock().unwrap();
                        svc.upsert_proposal(session_id, workspace_id, proposal).map_err(map_err)?;
                    }
                    let s = run.node_state_mut(&node.id).expect("node state exists");
                    s.status = NodeRunStatus::AwaitingApproval;
                    s.proposal_id = Some(pid);
                    state.gate_runs.lock().unwrap().insert(pid, run_id);
                }
                WorkflowNodeKind::Agent { agent_id, .. } => {
                    // Defer a same-agent sibling to a later iteration (see above).
                    if !claimed_agents.insert(*agent_id) {
                        continue;
                    }
                    run.node_state_mut(&node.id).expect("node state exists").status =
                        NodeRunStatus::Running;
                    agent_nodes.push(node.clone());
                }
            }
        }
        persist_run(app, &state, session_id, workspace_id, &mut run)?;

        if agent_nodes.is_empty() {
            continue;
        }

        // Prepare stages one at a time — each posts its prompt and snapshots
        // its context before the next sibling's prompt lands, so every turn
        // ends on *its own* prompt — then execute the turns concurrently.
        let mut prepared = Vec::new();
        for node in &agent_nodes {
            match prepare_stage(app, &state, session_id, workspace_id, &run, node).await {
                Ok(p) => prepared.push((node.id.clone(), p)),
                Err(e) => {
                    let s = run.node_state_mut(&node.id).expect("node state exists");
                    s.status = NodeRunStatus::Failed;
                    s.error = e;
                }
            }
        }

        // Local stages stream here; remote-owned stages were dispatched by
        // `prepare_stage` (their prompt @mentions the agent) and only need us to
        // wait for the owner's device / worker to sync the reply back. Both
        // resolve to a `TurnOutcome`, so they fold into node state identically.
        let turns = prepared.into_iter().map(|(node_id, p)| {
            let state = &state;
            async move {
                let result = match p {
                    PreparedStage::Local { responder, session, system, turns } => {
                        run_prepared_turn(
                            app,
                            state,
                            session_id,
                            workspace_id,
                            &responder,
                            &session,
                            system,
                            turns,
                        )
                        .await
                    }
                    PreparedStage::Remote { agent_author, prompt_message_id } => {
                        await_remote_stage(
                            state,
                            session_id,
                            run_id,
                            waker,
                            prompt_message_id,
                            &agent_author,
                        )
                        .await
                    }
                };
                (node_id, result)
            }
        });
        for (node_id, result) in futures::future::join_all(turns).await {
            let s = run.node_state_mut(&node_id).expect("node state exists");
            match result {
                Ok(outcome) => {
                    s.status = NodeRunStatus::Succeeded;
                    s.message_id = Some(outcome.message_id);
                    s.output_excerpt = outcome.body.chars().take(EXCERPT_CHARS).collect();
                }
                // A remote wait aborted by cancellation: revert to Pending so the
                // top-of-loop cancel handler skips it like every other unfinished
                // node, rather than leaving a spurious Failed in a Canceled run.
                Err(e) if e == CANCELED_WHILE_WAITING => {
                    s.status = NodeRunStatus::Pending;
                    s.message_id = None;
                    s.output_excerpt = String::new();
                    s.error = String::new();
                }
                Err(e) => {
                    s.status = NodeRunStatus::Failed;
                    s.error = e;
                }
            }
        }
        persist_run(app, &state, session_id, workspace_id, &mut run)?;
    }
}

/// A stage prepared for execution. A stage whose responder this device owns runs
/// its turn locally (`Local`, streamed by `run_prepared_turn`); one owned by
/// another member was already dispatched by posting a prompt that @mentions its
/// agent, and the driver only waits for the synced reply (`Remote`).
enum PreparedStage {
    Local {
        responder: Responder,
        session: ChatSession,
        system: String,
        turns: Vec<ChatTurn>,
    },
    Remote {
        /// Display name the owner's device / worker authors the reply under —
        /// the key the reply is matched by (mirrors `pending_mentions`).
        agent_author: String,
        /// The stage's posted prompt; the reply must appear *after* it.
        prompt_message_id: Uuid,
    },
}

async fn prepare_stage(
    app: &AppHandle,
    state: &State<'_, AppState>,
    session_id: Uuid,
    workspace_id: Uuid,
    run: &wf::WorkflowRun,
    node: &wf::WorkflowNode,
) -> Result<PreparedStage, String> {
    let _ = app;
    let WorkflowNodeKind::Agent { agent_id, prompt_template } = &node.kind else {
        return Err("not an agent stage".into());
    };

    // Render against the transcript as it stands (all dependency outputs are
    // in by now — deps gate readiness). Posting a user message never changes
    // the roster, so resolve the responder from this pre-post snapshot to
    // classify the stage before deciding how to post its prompt.
    let before = {
        let svc = state.service.lock().unwrap();
        svc.load(session_id).map_err(map_err)?.ok_or("unknown session")?
    };
    let outputs = collect_outputs(run, &before);
    let rendered = wf::render_template(prompt_template, &run.input, &outputs);
    let agent = resolve_stage_agent(&before, agent_id, &node.name)?;
    let responder = responder_for(state, &before, agent.as_ref());
    let local_actor_id = state.local_actor_id();
    // A relay-less (local) or solo workspace has no other device to defer a stage
    // to, so this device always runs it — even when the responder's recorded owner
    // differs (churned identity). Mirrors `send_message`/`maybe_respond`; all three
    // gates must use `turn_runs_here` or they drift. Without this, a stale
    // `creator_actor_id` classifies the stage Remote, the driver holds the
    // SessionBusyGuard for the whole run (blocking local `maybe_respond`), and the
    // stage hangs `REMOTE_STAGE_TIMEOUT` then fails with no local fallback.
    let local_only = state.is_local_only();
    let solo = crate::human_member_count(&before) <= 1;

    // Remote-owned stage: dispatch it over the existing cross-device path by
    // posting a prompt that @mentions its agent, then wait for the reply.
    if !crate::turn_runs_here(local_only, solo, &local_actor_id, &responder.owner_actor_id) {
        let prompt = format!(
            "{} **[Workflow · {} → {}]**\n\n{rendered}",
            stage_mention_token(agent.as_ref()),
            run.definition.name,
            node.name
        );
        let posted = {
            let mut svc = state.service.lock().unwrap();
            svc.post_user_message(session_id, workspace_id, &prompt).map_err(map_err)?
        };
        return Ok(PreparedStage::Remote {
            agent_author: responder.author.clone(),
            prompt_message_id: posted.id,
        });
    }

    // Local stage: unchanged from single-device runs — post the plain prompt,
    // reload so context includes it, and snapshot the windowed context.
    let prompt = format!(
        "**[Workflow · {} → {}]**\n\n{rendered}",
        run.definition.name, node.name
    );
    {
        let mut svc = state.service.lock().unwrap();
        svc.post_user_message(session_id, workspace_id, &prompt).map_err(map_err)?;
    }
    let session = {
        let svc = state.service.lock().unwrap();
        svc.load(session_id).map_err(map_err)?.ok_or("unknown session")?
    };
    let agent = resolve_stage_agent(&session, agent_id, &node.name)?;
    let responder = responder_for(state, &session, agent.as_ref());
    let (system, turns) = windowed_context(state, session_id, &session, &responder).await;
    Ok(PreparedStage::Local { responder, session, system, turns })
}

/// Resolve a stage's agent from the roster (`None` ⇒ the session's primary
/// runtime). Errors if a named agent has since left the roster.
fn resolve_stage_agent(
    session: &ChatSession,
    agent_id: &Option<Uuid>,
    node_name: &str,
) -> Result<Option<WorkspaceAgent>, String> {
    match agent_id {
        Some(id) => Some(
            session
                .workspace_agents
                .iter()
                .find(|a| a.id == *id)
                .cloned()
                .ok_or_else(|| {
                    format!("stage '{node_name}': its agent is no longer in the roster")
                }),
        )
        .transpose(),
        None => Ok(None),
    }
}

/// The `@mention` that routes a remote stage's prompt to its responder through
/// the existing dispatch: `@primary` for the session runtime, else the agent's
/// display name. Kept in step with `parse_mentions`, which the owner's device /
/// worker uses to decide the mention is theirs to answer.
fn stage_mention_token(agent: Option<&WorkspaceAgent>) -> String {
    match agent {
        Some(a) => format!("@{}", a.name),
        None => "@primary".to_string(),
    }
}

/// Whether a stage's responder runs on *this* device (unowned/legacy or owned by
/// us) or must be dispatched to its owner. Single source of truth for the
/// local-vs-remote split — `owns_responder` in `lib.rs` delegates here so the
/// two can't drift, and this pure form is unit-testable without a `Responder`.
pub(crate) fn stage_owner_runs_here(local_actor_id: &str, owner_actor_id: &str) -> bool {
    owner_actor_id.is_empty() || owner_actor_id == local_actor_id
}

/// Suspend the driver until a remote-owned stage's reply syncs back, then return
/// it as this stage's output. Matches the first assistant/agent message authored
/// by `agent_author` that appears after the stage's prompt (`prompt_message_id`)
/// — the same author-name correlation `pending_mentions` uses. Bounded by
/// `REMOTE_STAGE_TIMEOUT`; honors Cancel promptly via the run waker.
///
/// Deferred (documented limitations of correlating by author-name over the
/// existing dispatch, which carries no per-request id):
///   1. Multiple remote stages ready *simultaneously* are reliably serviced only
///      by a `hive worker` (it drains the whole mention backlog). A desktop peer
///      answering via `maybe_respond` only sees the trailing message, so the
///      earlier siblings wait until a worker picks them up (or time out).
///   2. Two concurrently-ready remote stages bound to the *same* agent aren't
///      disambiguated — the worker's per-agent latest-mention dedup collapses
///      them, so both would match the one reply. A sequential DAG (the common
///      shape, and every single-remote-stage ready-set) is unaffected.
async fn await_remote_stage(
    state: &State<'_, AppState>,
    session_id: Uuid,
    run_id: Uuid,
    waker: &Notify,
    prompt_message_id: Uuid,
    agent_author: &str,
) -> Result<TurnOutcome, String> {
    let deadline = Instant::now() + REMOTE_STAGE_TIMEOUT;
    loop {
        // Observe (don't consume) cancellation — the top-of-loop handler still
        // needs to see it to settle the run.
        if state.canceled_runs.lock().unwrap().contains(&run_id) {
            return Err(CANCELED_WHILE_WAITING.to_string());
        }
        let messages = {
            let svc = state.service.lock().unwrap();
            svc.load(session_id).map_err(map_err)?.ok_or("unknown session")?.messages
        };
        if let Some((message_id, body)) =
            match_stage_reply(&messages, prompt_message_id, agent_author)
        {
            return Ok(TurnOutcome { message_id, body });
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no reply from {} synced back within {}s — is its owner's device or a \
                 worker online for this agent?",
                agent_author,
                REMOTE_STAGE_TIMEOUT.as_secs()
            ));
        }
        tokio::select! {
            _ = waker.notified() => {}
            _ = tokio::time::sleep(REMOTE_POLL) => {}
        }
    }
}

/// A remote stage's output: the first finished assistant/agent message authored
/// by `agent_author` appearing *after* the stage's prompt (`after_id`). Mirrors
/// the author-name match `pending_mentions` uses to call a mention "answered",
/// so a reply the owner's device / worker synced back is picked up identically.
fn match_stage_reply(
    messages: &[ChatMessage],
    after_id: Uuid,
    agent_author: &str,
) -> Option<(Uuid, String)> {
    let start = messages.iter().position(|m| m.id == after_id)?;
    messages[start + 1..]
        .iter()
        .find(|m| {
            matches!(m.role, MessageRole::Assistant | MessageRole::Agent)
                && !m.is_streaming
                && !m.body.is_empty()
                // Precise per-request match: this reply answers exactly this
                // stage's prompt. Falls back to author-name only for legacy
                // replies without a request id — which is what previously let two
                // concurrently-ready stages on the *same* agent collapse onto one
                // reply (the documented deferred gap).
                && match m.reply_to_message_id {
                    Some(answers) => answers == after_id,
                    None => m.author == agent_author,
                }
        })
        .map(|m| (m.id, m.body.clone()))
}

fn load_run(
    state: &State<'_, AppState>,
    session_id: Uuid,
    run_id: Uuid,
) -> Result<(wf::WorkflowRun, ChatSession), String> {
    let svc = state.service.lock().unwrap();
    let session = svc.load(session_id).map_err(map_err)?.ok_or("unknown session")?;
    let run = session
        .workflow_runs
        .iter()
        .find(|r| r.id == run_id)
        .cloned()
        .ok_or("unknown workflow run")?;
    Ok((run, session))
}

/// Full outputs of every succeeded stage, re-read from the transcript by
/// message id (the run record only keeps excerpts).
fn collect_outputs(run: &wf::WorkflowRun, session: &ChatSession) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for n in &run.nodes {
        if n.status != NodeRunStatus::Succeeded {
            continue;
        }
        let Some(mid) = n.message_id else { continue };
        if let Some(m) = session.messages.iter().find(|m| m.id == mid) {
            map.insert(n.node_id.clone(), m.body.clone());
        }
    }
    map
}

fn persist_run(
    app: &AppHandle,
    state: &State<'_, AppState>,
    session_id: Uuid,
    workspace_id: Uuid,
    run: &mut wf::WorkflowRun,
) -> Result<(), String> {
    run.updated_at = Timestamp::now();
    {
        let mut svc = state.service.lock().unwrap();
        svc.upsert_workflow_run(session_id, workspace_id, run.clone()).map_err(map_err)?;
    }
    let _ = app.emit(
        WorkflowRunEvent::EVENT,
        WorkflowRunEvent {
            session_id: session_id.to_string(),
            run_id: run.id.to_string(),
            status: run_status_str(run.status).to_string(),
        },
    );
    Ok(())
}

/// Best-effort: surface a driver error on the run record so it doesn't show
/// as running forever.
fn mark_run_failed(
    app: &AppHandle,
    session_id: Uuid,
    workspace_id: Uuid,
    run_id: Uuid,
    error: &str,
) {
    let state = app.state::<AppState>();
    let Ok((mut run, _)) = load_run(&state, session_id, run_id) else { return };
    for n in &mut run.nodes {
        if n.status == NodeRunStatus::Running {
            n.status = NodeRunStatus::Failed;
            if n.error.is_empty() {
                n.error = error.to_string();
            }
        } else if !n.status.is_terminal() {
            n.status = NodeRunStatus::Skipped;
        }
    }
    run.status = WorkflowRunStatus::Failed;
    let _ = persist_run(app, &state, session_id, workspace_id, &mut run);
    let _ = close_orphaned_gates(app, &state, session_id, workspace_id, &run);
}

/// Reject and deregister any gate proposal this run created that no longer
/// backs an awaiting node — abandoned by a cancel, a failure, or a RouteTo
/// reset of a sibling gate. Without this, the proposal stays `Open` and keeps
/// showing as a votable approval card in the Review pane forever.
fn close_orphaned_gates(
    app: &AppHandle,
    state: &State<'_, AppState>,
    session_id: Uuid,
    workspace_id: Uuid,
    run: &wf::WorkflowRun,
) -> Result<(), String> {
    // Proposal ids that still legitimately back an awaiting gate.
    let active: std::collections::HashSet<Uuid> = run
        .nodes
        .iter()
        .filter(|n| n.status == NodeRunStatus::AwaitingApproval)
        .filter_map(|n| n.proposal_id)
        .collect();
    let orphans: Vec<Uuid> = {
        let g = state.gate_runs.lock().unwrap();
        g.iter()
            .filter(|(pid, r)| **r == run.id && !active.contains(*pid))
            .map(|(pid, _)| *pid)
            .collect()
    };
    if orphans.is_empty() {
        return Ok(());
    }
    {
        let mut svc = state.service.lock().unwrap();
        let session = svc.load(session_id).map_err(map_err)?.ok_or("unknown session")?;
        for pid in &orphans {
            if let Some(p) = session.proposals.iter().find(|p| p.id == *pid) {
                if p.status == ProposalStatus::Open {
                    let mut withdrawn = p.clone();
                    withdrawn.status = ProposalStatus::Rejected;
                    svc.upsert_proposal(session_id, workspace_id, withdrawn).map_err(map_err)?;
                }
            }
        }
    }
    let mut g = state.gate_runs.lock().unwrap();
    for pid in orphans {
        g.remove(&pid);
    }
    let _ = app.emit("workspace://synced", 1);
    Ok(())
}

// ---------------------------------------------------------------------------
// Agent-authored workflows ([[workflow: {…}]] reply directive)
// ---------------------------------------------------------------------------

/// The lenient JSON shape agents emit. Friendlier than the wire DTO: stages
/// address agents by roster *name*, ids default to slugified names, and
/// `onReject` is either "halt" or {"retryFrom": "<stage id>"}.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirectiveWorkflow {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    input_label: Option<String>,
    #[serde(default, alias = "nodes")]
    stages: Vec<DirectiveStage>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirectiveStage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// "agent" (default) | "gate"
    #[serde(default)]
    kind: Option<String>,
    #[serde(default, alias = "dependsOn", alias = "runsAfter")]
    after: Vec<String>,
    /// Roster name of the agent to run; absent ⇒ primary runtime.
    #[serde(default)]
    agent: Option<String>,
    #[serde(default, alias = "promptTemplate")]
    prompt: Option<String>,
    #[serde(default, alias = "gateTitle")]
    title: Option<String>,
    #[serde(default, alias = "gateBody")]
    body: Option<String>,
    #[serde(default, alias = "requiredApprovals")]
    approvals: Option<u32>,
    #[serde(default)]
    on_reject: Option<DirectiveReject>,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum DirectiveReject {
    /// "halt"
    Word(String),
    /// {"retryFrom": "<stage id>"}
    #[serde(rename_all = "camelCase")]
    Retry { retry_from: String },
}

/// Parse an agent's `[[workflow: …]]` payload into a validated definition.
/// Agent names resolve against the session roster; everything else goes
/// through the same `validate()` the builder uses.
pub(crate) fn definition_from_directive(
    json: &str,
    session: &ChatSession,
) -> Result<wf::WorkflowDefinition, String> {
    let dw: DirectiveWorkflow =
        serde_json::from_str(json).map_err(|e| format!("invalid workflow JSON: {e}"))?;
    let mut nodes = Vec::with_capacity(dw.stages.len());
    for (i, s) in dw.stages.iter().enumerate() {
        let name = s
            .name
            .clone()
            .or_else(|| s.id.clone())
            .unwrap_or_else(|| format!("Stage {}", i + 1));
        let id = s.id.clone().unwrap_or_else(|| crate::workflows::slug(&name));
        let kind = match s.kind.as_deref() {
            None | Some("agent") => wf::WorkflowNodeKind::Agent {
                agent_id: match &s.agent {
                    None => None,
                    Some(agent_name) => Some(
                        session
                            .workspace_agents
                            .iter()
                            .find(|a| a.name.eq_ignore_ascii_case(agent_name))
                            .map(|a| a.id)
                            .ok_or_else(|| {
                                let roster: Vec<&str> = session
                                    .workspace_agents
                                    .iter()
                                    .map(|a| a.name.as_str())
                                    .collect();
                                format!(
                                    "stage {id:?} names unknown agent {agent_name:?} (roster: {})",
                                    if roster.is_empty() { "empty".into() } else { roster.join(", ") }
                                )
                            })?,
                    ),
                },
                prompt_template: s
                    .prompt
                    .clone()
                    .ok_or_else(|| format!("agent stage {id:?} needs a \"prompt\""))?,
            },
            Some("gate") => wf::WorkflowNodeKind::Gate {
                title_template: s.title.clone().unwrap_or_else(|| name.clone()),
                body_template: s.body.clone().unwrap_or_default(),
                required_approvals: s.approvals.unwrap_or(1).max(1),
                on_reject: match &s.on_reject {
                    None => wf::GateRejectPolicy::Halt,
                    Some(DirectiveReject::Word(w)) if w == "halt" => wf::GateRejectPolicy::Halt,
                    Some(DirectiveReject::Word(w)) => {
                        return Err(format!("gate {id:?}: unknown onReject {w:?}"))
                    }
                    Some(DirectiveReject::Retry { retry_from }) => {
                        wf::GateRejectPolicy::RouteTo { node: retry_from.clone() }
                    }
                },
            },
            Some(other) => return Err(format!("stage {id:?}: unknown kind {other:?}")),
        };
        nodes.push(wf::WorkflowNode {
            id,
            name,
            depends_on: s.after.clone(),
            kind,
            x: None,
            y: None,
        });
    }
    let def = wf::WorkflowDefinition {
        id: Uuid::new_v4(),
        name: dw.name,
        description: dw.description,
        input_label: dw.input_label.filter(|l| !l.trim().is_empty()),
        nodes,
        created_at: Timestamp::now(),
    };
    wf::validate(&def)?;
    Ok(def)
}

/// Mirror of the frontend's slugify, for defaulted stage ids.
pub(crate) fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "stage".into() } else { out }
}

// ---------------------------------------------------------------------------
// DTO mapping
// ---------------------------------------------------------------------------

fn run_status_str(s: WorkflowRunStatus) -> &'static str {
    match s {
        WorkflowRunStatus::Running => "running",
        WorkflowRunStatus::AwaitingGate => "awaitingGate",
        WorkflowRunStatus::Completed => "completed",
        WorkflowRunStatus::Failed => "failed",
        WorkflowRunStatus::Halted => "halted",
        WorkflowRunStatus::Canceled => "canceled",
    }
}

fn node_status_str(s: NodeRunStatus) -> &'static str {
    match s {
        NodeRunStatus::Pending => "pending",
        NodeRunStatus::Running => "running",
        NodeRunStatus::AwaitingApproval => "awaitingApproval",
        NodeRunStatus::Succeeded => "succeeded",
        NodeRunStatus::Failed => "failed",
        NodeRunStatus::Rejected => "rejected",
        NodeRunStatus::Skipped => "skipped",
    }
}

fn node_dto(n: &wf::WorkflowNode) -> WorkflowNodeDto {
    let base = WorkflowNodeDto {
        id: n.id.clone(),
        name: n.name.clone(),
        depends_on: n.depends_on.clone(),
        kind: String::new(),
        agent_id: None,
        prompt_template: None,
        gate_title: None,
        gate_body: None,
        required_approvals: None,
        on_reject: None,
        reject_target: None,
        x: n.x,
        y: n.y,
    };
    match &n.kind {
        WorkflowNodeKind::Agent { agent_id, prompt_template } => WorkflowNodeDto {
            kind: "agent".into(),
            agent_id: agent_id.map(|u| u.to_string()),
            prompt_template: Some(prompt_template.clone()),
            ..base
        },
        WorkflowNodeKind::Gate { title_template, body_template, required_approvals, on_reject } => {
            let (on_reject_str, target) = match on_reject {
                wf::GateRejectPolicy::Halt => ("halt".to_string(), None),
                wf::GateRejectPolicy::RouteTo { node } => {
                    ("routeTo".to_string(), Some(node.clone()))
                }
            };
            WorkflowNodeDto {
                kind: "gate".into(),
                gate_title: Some(title_template.clone()),
                gate_body: Some(body_template.clone()),
                required_approvals: Some(*required_approvals),
                on_reject: Some(on_reject_str),
                reject_target: target,
                ..base
            }
        }
    }
}

pub(crate) fn definition_dto(def: &wf::WorkflowDefinition) -> WorkflowDefinitionDto {
    WorkflowDefinitionDto {
        id: def.id.to_string(),
        name: def.name.clone(),
        description: def.description.clone(),
        input_label: def.input_label.clone(),
        nodes: def.nodes.iter().map(node_dto).collect(),
    }
}

pub(crate) fn definition_from_dto(
    dto: &WorkflowDefinitionDto,
) -> Result<wf::WorkflowDefinition, String> {
    let id = if dto.id.trim().is_empty() {
        Uuid::new_v4()
    } else {
        Uuid::parse_str(&dto.id).map_err(map_err)?
    };
    let mut nodes = Vec::with_capacity(dto.nodes.len());
    for n in &dto.nodes {
        let kind = match n.kind.as_str() {
            "agent" => WorkflowNodeKind::Agent {
                agent_id: match n.agent_id.as_deref() {
                    None | Some("") => None,
                    Some(s) => Some(Uuid::parse_str(s).map_err(map_err)?),
                },
                prompt_template: n.prompt_template.clone().unwrap_or_default(),
            },
            "gate" => WorkflowNodeKind::Gate {
                title_template: n
                    .gate_title
                    .clone()
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| n.name.clone()),
                body_template: n.gate_body.clone().unwrap_or_default(),
                required_approvals: n.required_approvals.unwrap_or(1).max(1),
                on_reject: match n.on_reject.as_deref() {
                    Some("routeTo") => wf::GateRejectPolicy::RouteTo {
                        node: n
                            .reject_target
                            .clone()
                            .ok_or_else(|| format!("gate '{}': routeTo needs a target stage", n.name))?,
                    },
                    _ => wf::GateRejectPolicy::Halt,
                },
            },
            other => return Err(format!("unknown stage kind {other:?}")),
        };
        nodes.push(wf::WorkflowNode {
            id: n.id.clone(),
            name: n.name.clone(),
            depends_on: n.depends_on.clone(),
            kind,
            x: n.x,
            y: n.y,
        });
    }
    Ok(wf::WorkflowDefinition {
        id,
        name: dto.name.clone(),
        description: dto.description.clone(),
        input_label: dto.input_label.clone().filter(|l| !l.trim().is_empty()),
        nodes,
        created_at: Timestamp::now(),
    })
}

pub(crate) fn run_dto(run: &wf::WorkflowRun, driver_alive: bool) -> WorkflowRunDto {
    WorkflowRunDto {
        driver_alive,
        id: run.id.to_string(),
        definition_id: run.definition_id.to_string(),
        definition_name: run.definition.name.clone(),
        input: run.input.clone(),
        status: run_status_str(run.status).to_string(),
        nodes: run
            .nodes
            .iter()
            .map(|s| {
                let node = run.definition.nodes.iter().find(|n| n.id == s.node_id);
                WorkflowNodeRunDto {
                    node_id: s.node_id.clone(),
                    name: node.map(|n| n.name.clone()).unwrap_or_else(|| s.node_id.clone()),
                    kind: match node.map(|n| &n.kind) {
                        Some(WorkflowNodeKind::Gate { .. }) => "gate".into(),
                        _ => "agent".into(),
                    },
                    status: node_status_str(s.status).to_string(),
                    message_id: s.message_id.map(|u| u.to_string()),
                    proposal_id: s.proposal_id.map(|u| u.to_string()),
                    output_excerpt: s.output_excerpt.clone(),
                    attempts: s.attempts,
                    error: s.error.clone(),
                }
            })
            .collect(),
        started_at: rfc3339(run.started_at),
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub(crate) fn list_workflows(
    state: State<AppState>,
    session_id: String,
) -> Result<Vec<WorkflowDefinitionDto>, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let svc = state.service.lock().unwrap();
    Ok(svc
        .load(sid)
        .map_err(map_err)?
        .map(|s| s.workflow_definitions.iter().map(definition_dto).collect())
        .unwrap_or_default())
}

#[tauri::command]
pub(crate) fn save_workflow(
    state: State<AppState>,
    session_id: String,
    definition: WorkflowDefinitionDto,
) -> Result<WorkflowDefinitionDto, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let def = definition_from_dto(&definition)?;
    wf::validate(&def)?;
    let mut svc = state.service.lock().unwrap();
    svc.save_workflow_definition(sid, state.active_workspace_id(), def.clone())
        .map_err(map_err)?;
    Ok(definition_dto(&def))
}

#[tauri::command]
pub(crate) fn remove_workflow(
    state: State<AppState>,
    session_id: String,
    workflow_id: String,
) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let wfid = Uuid::parse_str(&workflow_id).map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    svc.remove_workflow_definition(sid, state.active_workspace_id(), wfid).map_err(map_err)
}

#[tauri::command]
pub(crate) fn add_workflow_preset(
    state: State<AppState>,
    session_id: String,
    preset: String,
) -> Result<WorkflowDefinitionDto, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let def = match preset.as_str() {
        "reviewGate" => wf::preset_review_gate(),
        "fanOutVote" => wf::preset_fan_out_vote(),
        other => return Err(format!("unknown preset {other:?}")),
    };
    let mut svc = state.service.lock().unwrap();
    svc.save_workflow_definition(sid, state.active_workspace_id(), def.clone())
        .map_err(map_err)?;
    Ok(definition_dto(&def))
}

#[tauri::command]
pub(crate) fn list_workflow_runs(
    state: State<AppState>,
    session_id: String,
) -> Result<Vec<WorkflowRunDto>, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let live = state.run_wakers.lock().unwrap();
    let svc = state.service.lock().unwrap();
    Ok(svc
        .load(sid)
        .map_err(map_err)?
        // Newest first for the runs list.
        .map(|s| {
            s.workflow_runs
                .iter()
                .rev()
                .map(|r| run_dto(r, live.contains_key(&r.id)))
                .collect()
        })
        .unwrap_or_default())
}

#[tauri::command]
pub(crate) fn start_workflow_run(
    app: AppHandle,
    state: State<AppState>,
    session_id: String,
    workflow_id: String,
    input: String,
) -> Result<String, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let wfid = Uuid::parse_str(&workflow_id).map_err(map_err)?;
    let workspace_id = state.active_workspace_id();

    let (def, session) = {
        let svc = state.service.lock().unwrap();
        let session = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?;
        let def = session
            .workflow_definitions
            .iter()
            .find(|d| d.id == wfid)
            .cloned()
            .ok_or("unknown workflow")?;
        (def, session)
    };
    wf::validate(&def)?;
    ensure_stages_runnable(&session, &def)?;

    let run = wf::new_run(&def, input, state.local_actor_id());
    let run_id = run.id;
    // Fail fast if the chat is busy, and hand the slot to the driver.
    let guard = SessionBusyGuard::acquire(&app, sid)?;
    {
        let mut svc = state.service.lock().unwrap();
        svc.upsert_workflow_run(sid, workspace_id, run).map_err(map_err)?;
    }
    let _ = app.emit(
        WorkflowRunEvent::EVENT,
        WorkflowRunEvent {
            session_id: sid.to_string(),
            run_id: run_id.to_string(),
            status: "running".into(),
        },
    );
    tauri::async_runtime::spawn(drive_run(app.clone(), sid, workspace_id, run_id, guard));
    Ok(run_id.to_string())
}

#[tauri::command]
pub(crate) fn cancel_workflow_run(
    app: AppHandle,
    state: State<AppState>,
    session_id: String,
    run_id: String,
) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let rid = Uuid::parse_str(&run_id).map_err(map_err)?;
    // Mark canceled + notify the driver's waker while holding the `run_wakers`
    // lock, so `DriverRegistration::drop` (which removes from `run_wakers` first,
    // then `canceled_runs`) can't deregister in a gap between the check and the
    // insert and leave a stale `canceled_runs` entry no live driver will consume.
    // Safe from deadlock: `drop` releases `run_wakers` before touching
    // `canceled_runs`, so it never nests the two in the opposite order.
    {
        let wakers = state.run_wakers.lock().unwrap();
        if let Some(w) = wakers.get(&rid) {
            state.canceled_runs.lock().unwrap().insert(rid);
            w.notify_waiters();
            return Ok(());
        }
    }
    // No live driver (e.g. the app restarted mid-run): settle the record directly.
    let workspace_id = state.active_workspace_id();
    let (mut run, _) = load_run(&state, sid, rid)?;
    if !matches!(run.status, WorkflowRunStatus::Running | WorkflowRunStatus::AwaitingGate) {
        return Err("this run already finished".into());
    }
    for n in &mut run.nodes {
        if !n.status.is_terminal() {
            n.status = NodeRunStatus::Skipped;
        }
    }
    run.status = WorkflowRunStatus::Canceled;
    persist_run(&app, &state, sid, workspace_id, &mut run)
}

#[tauri::command]
pub(crate) fn resume_workflow_run(
    app: AppHandle,
    state: State<AppState>,
    session_id: String,
    run_id: String,
) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let rid = Uuid::parse_str(&run_id).map_err(map_err)?;
    if state.run_wakers.lock().unwrap().contains_key(&rid) {
        return Err("this run is already being driven".into());
    }
    let workspace_id = state.active_workspace_id();
    let (mut run, session) = load_run(&state, sid, rid)?;
    if !matches!(run.status, WorkflowRunStatus::Running | WorkflowRunStatus::AwaitingGate) {
        return Err("only an interrupted run can be resumed".into());
    }
    ensure_stages_runnable(&session, &run.definition)?;

    // Stages that were mid-turn when the driver died restart from scratch;
    // succeeded stages keep their outputs (re-read from the transcript).
    for n in &mut run.nodes {
        if n.status == NodeRunStatus::Running {
            n.status = NodeRunStatus::Pending;
            n.message_id = None;
            n.output_excerpt = String::new();
            n.error = String::new();
        }
    }
    let guard = SessionBusyGuard::acquire(&app, sid)?;
    persist_run(&app, &state, sid, workspace_id, &mut run)?;
    tauri::async_runtime::spawn(drive_run(app.clone(), sid, workspace_id, rid, guard));
    Ok(())
}

/// Pre-flight check run at start/resume. A run may now span devices: a stage
/// owned by another member is dispatched over the cross-device path (its prompt
/// @mentions the agent, and the owner's device / worker answers) rather than
/// rejected. The only genuinely-unrunnable case guarded here is a *named* agent
/// that isn't in the roster at all — no device could service it, so fail fast
/// with a clear error instead of dispatching into the void and timing out.
///
/// Note: we can't prove from this device that a remote agent actually has a live
/// owner/worker; if none answers, the stage fails cleanly on
/// `REMOTE_STAGE_TIMEOUT` (per-stage), which is the intended fallback.
fn ensure_stages_runnable(
    session: &ChatSession,
    def: &wf::WorkflowDefinition,
) -> Result<(), String> {
    for node in &def.nodes {
        let WorkflowNodeKind::Agent { agent_id, .. } = &node.kind else { continue };
        if let Some(id) = agent_id {
            if !session.workspace_agents.iter().any(|a| a.id == *id) {
                return Err(format!(
                    "stage '{}': its agent is not in this chat's roster",
                    node.name
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod directive_tests {
    use super::*;
    use hive_core::WorkspaceAgent;

    fn session_with_scout() -> ChatSession {
        let mut s = ChatSession::new("Demo", Uuid::nil(), "anthropic");
        s.workspace_agents.push(WorkspaceAgent::new("Scout", "r1"));
        s
    }

    #[test]
    fn parses_a_full_pipeline_with_gate_and_agent_resolution() {
        let json = r#"{
            "name": "Nightly triage",
            "description": "Scan and fix",
            "inputLabel": "What to triage?",
            "stages": [
                {"id": "scan", "kind": "agent", "agent": "scout", "prompt": "Scan: {{input}}"},
                {"name": "Fix it", "prompt": "Fix based on {{nodes.scan.output}}", "after": ["scan"]},
                {"id": "ok", "kind": "gate", "title": "Approve fixes", "approvals": 2,
                 "onReject": {"retryFrom": "fix-it"}, "after": ["fix-it"]}
            ]
        }"#;
        let session = session_with_scout();
        let def = definition_from_directive(json, &session).unwrap();
        assert_eq!(def.name, "Nightly triage");
        assert_eq!(def.nodes.len(), 3);
        // Case-insensitive roster resolution by name → uuid.
        match &def.nodes[0].kind {
            wf::WorkflowNodeKind::Agent { agent_id, .. } => {
                assert_eq!(*agent_id, Some(session.workspace_agents[0].id));
            }
            _ => panic!("expected agent stage"),
        }
        // Missing id defaults to the slugified name.
        assert_eq!(def.nodes[1].id, "fix-it");
        match &def.nodes[2].kind {
            wf::WorkflowNodeKind::Gate { required_approvals, on_reject, .. } => {
                assert_eq!(*required_approvals, 2);
                assert_eq!(
                    *on_reject,
                    wf::GateRejectPolicy::RouteTo { node: "fix-it".into() }
                );
            }
            _ => panic!("expected gate stage"),
        }
    }

    #[test]
    fn unknown_agent_name_is_rejected_with_roster() {
        let json = r#"{"name": "w", "stages": [{"id": "a", "agent": "Ghost", "prompt": "x"}]}"#;
        let err = definition_from_directive(json, &session_with_scout()).unwrap_err();
        assert!(err.contains("Ghost"));
        assert!(err.contains("Scout"));
    }

    #[test]
    fn agent_stage_without_prompt_is_rejected() {
        let json = r#"{"name": "w", "stages": [{"id": "a"}]}"#;
        let err = definition_from_directive(json, &session_with_scout()).unwrap_err();
        assert!(err.contains("prompt"));
    }

    #[test]
    fn structural_validation_still_applies() {
        // Cycle via "after" → the same validate() the builder uses rejects it.
        let json = r#"{"name": "w", "stages": [
            {"id": "a", "prompt": "x", "after": ["b"]},
            {"id": "b", "prompt": "y", "after": ["a"]}
        ]}"#;
        let err = definition_from_directive(json, &session_with_scout()).unwrap_err();
        assert!(err.contains("cycle"));
    }

    #[test]
    fn slug_mirrors_frontend() {
        assert_eq!(slug("Judge results!"), "judge-results");
        assert_eq!(slug("  "), "stage");
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn selects_only_interrupted_runs_without_a_live_driver() {
        let running = Uuid::new_v4();
        let awaiting = Uuid::new_v4();
        let already_live = Uuid::new_v4();
        let completed = Uuid::new_v4();
        let failed = Uuid::new_v4();
        let halted = Uuid::new_v4();
        let canceled = Uuid::new_v4();

        let runs = vec![
            (running, WorkflowRunStatus::Running),
            (awaiting, WorkflowRunStatus::AwaitingGate),
            (already_live, WorkflowRunStatus::Running),
            (completed, WorkflowRunStatus::Completed),
            (failed, WorkflowRunStatus::Failed),
            (halted, WorkflowRunStatus::Halted),
            (canceled, WorkflowRunStatus::Canceled),
        ];
        // `already_live` has a driver, so it must be skipped even though Running.
        let live: HashSet<Uuid> = [already_live].into_iter().collect();

        let need: HashSet<Uuid> = runs_needing_recovery(runs, &live).into_iter().collect();
        assert_eq!(need, [running, awaiting].into_iter().collect());
    }

    #[test]
    fn recovery_is_idempotent_once_drivers_are_live() {
        let running = Uuid::new_v4();
        let runs = vec![(running, WorkflowRunStatus::Running)];
        // First pass: no live drivers → recover it.
        let live = HashSet::new();
        assert_eq!(runs_needing_recovery(runs.clone(), &live), vec![running]);
        // Second pass after the driver registered → nothing left to recover.
        let live: HashSet<Uuid> = [running].into_iter().collect();
        assert!(runs_needing_recovery(runs, &live).is_empty());
    }
}

#[cfg(test)]
mod cross_device_tests {
    use super::*;

    fn msg(role: MessageRole, author: &str, body: &str) -> ChatMessage {
        ChatMessage::new(role, author, body)
    }

    // --- local-vs-remote classification (mirrors owns_responder) -------------

    #[test]
    fn unowned_and_self_owned_stages_run_here() {
        let me = "device-A";
        // Unowned (legacy / local-only) always runs here.
        assert!(stage_owner_runs_here(me, ""));
        // Owned by this device runs here.
        assert!(stage_owner_runs_here(me, "device-A"));
    }

    #[test]
    fn other_owned_stage_is_remote() {
        assert!(!stage_owner_runs_here("device-A", "device-B"));
    }

    // --- the @mention that dispatches a remote stage -------------------------

    #[test]
    fn mention_token_targets_agent_or_primary() {
        let scout = WorkspaceAgent::new("Scout", "r1");
        assert_eq!(stage_mention_token(Some(&scout)), "@Scout");
        assert_eq!(stage_mention_token(None), "@primary");
        // A rendered remote prompt actually parses as a mention of that agent,
        // so the owner's device / worker will pick it up.
        let mut session = ChatSession::new("t", Uuid::nil(), "anthropic");
        session.workspace_agents.push(scout.clone());
        let prompt = format!("{} do the thing", stage_mention_token(Some(&scout)));
        assert_eq!(
            hive_runtime::parse_mentions(&prompt, &session).agents,
            vec![scout.id]
        );
    }

    // --- reply matching (mirrors pending_mentions' author-name rule) ---------

    #[test]
    fn matches_first_agent_reply_after_the_prompt() {
        let prompt = msg(MessageRole::User, "Mara", "@Scout go");
        let reply = msg(MessageRole::Assistant, "Scout", "done: result");
        let after_id = prompt.id;
        let reply_id = reply.id;
        let messages = vec![prompt, reply];
        let out = match_stage_reply(&messages, after_id, "Scout");
        assert_eq!(out, Some((reply_id, "done: result".to_string())));
    }

    #[test]
    fn ignores_replies_before_the_prompt_and_other_authors() {
        // A stale reply by Scout *before* the prompt must not count, nor a reply
        // by a different author after it.
        let stale = msg(MessageRole::Assistant, "Scout", "old output");
        let prompt = msg(MessageRole::User, "Mara", "@Scout go");
        let other = msg(MessageRole::Assistant, "Hive", "not the stage agent");
        let after_id = prompt.id;
        let messages = vec![stale, prompt, other];
        assert!(match_stage_reply(&messages, after_id, "Scout").is_none());
    }

    #[test]
    fn skips_streaming_and_empty_placeholders() {
        let prompt = msg(MessageRole::User, "Mara", "@Scout go");
        let mut streaming = msg(MessageRole::Assistant, "Scout", "");
        streaming.is_streaming = true;
        let empty = msg(MessageRole::Assistant, "Scout", "");
        let after_id = prompt.id;
        let messages = vec![prompt, streaming, empty];
        // Neither the in-flight stream nor an empty completion counts as output.
        assert!(match_stage_reply(&messages, after_id, "Scout").is_none());
    }

    #[test]
    fn missing_prompt_yields_no_match() {
        let reply = msg(MessageRole::Assistant, "Scout", "done");
        assert!(match_stage_reply(&[reply], Uuid::new_v4(), "Scout").is_none());
    }

    #[test]
    fn agent_role_replies_also_match() {
        // Some historical replies use MessageRole::Agent rather than Assistant.
        let prompt = msg(MessageRole::User, "Mara", "@Scout go");
        let reply = msg(MessageRole::Agent, "Scout", "agent-role output");
        let after_id = prompt.id;
        let reply_id = reply.id;
        let messages = vec![prompt, reply];
        assert_eq!(
            match_stage_reply(&messages, after_id, "Scout"),
            Some((reply_id, "agent-role output".to_string()))
        );
    }
}
