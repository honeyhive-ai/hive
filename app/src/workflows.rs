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
use std::time::Duration;

use hive_core::workflow::{self as wf, NodeRunStatus, WorkflowNodeKind, WorkflowRunStatus};
use hive_core::{ActionProposal, ChatSession, ProposalKind, ProposalStatus, Timestamp};
use hive_proto::{
    WorkflowDefinitionDto, WorkflowNodeDto, WorkflowNodeRunDto, WorkflowRunDto, WorkflowRunEvent,
};
use hive_runtime::ChatTurn;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::{
    map_err, owns_responder, responder_for, rfc3339, run_prepared_turn, windowed_context,
    AppState, Responder,
};

/// How long a gate suspension sleeps between proposal re-checks. Local votes
/// wake the driver instantly via `Notify`; this poll is the safety net for
/// votes that arrive from other devices through the sync loop.
const GATE_POLL: Duration = Duration::from_secs(5);

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
                    let mut proposal = ActionProposal::new(title, ProposalKind::Decision);
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
                WorkflowNodeKind::Agent { .. } => {
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

        let turns = prepared.into_iter().map(|(node_id, p)| {
            let state = &state;
            async move {
                let result = run_prepared_turn(
                    app,
                    state,
                    session_id,
                    workspace_id,
                    &p.responder,
                    &p.session,
                    p.system,
                    p.turns,
                )
                .await;
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
                Err(e) => {
                    s.status = NodeRunStatus::Failed;
                    s.error = e;
                }
            }
        }
        persist_run(app, &state, session_id, workspace_id, &mut run)?;
    }
}

struct PreparedStage {
    responder: Responder,
    session: ChatSession,
    system: String,
    turns: Vec<ChatTurn>,
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
    // in by now — deps gate readiness).
    let before = {
        let svc = state.service.lock().unwrap();
        svc.load(session_id).map_err(map_err)?.ok_or("unknown session")?
    };
    let outputs = collect_outputs(run, &before);
    let rendered = wf::render_template(prompt_template, &run.input, &outputs);
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
    let agent = match agent_id {
        Some(id) => Some(
            session
                .workspace_agents
                .iter()
                .find(|a| a.id == *id)
                .cloned()
                .ok_or_else(|| {
                    format!("stage '{}': its agent is no longer in the roster", node.name)
                })?,
        ),
        None => None,
    };
    let responder = responder_for(state, &session, agent.as_ref());
    let (system, turns) = windowed_context(state, session_id, &session, &responder).await;
    Ok(PreparedStage { responder, session, system, turns })
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

pub(crate) fn run_dto(run: &wf::WorkflowRun) -> WorkflowRunDto {
    WorkflowRunDto {
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
    let svc = state.service.lock().unwrap();
    Ok(svc
        .load(sid)
        .map_err(map_err)?
        // Newest first for the runs list.
        .map(|s| s.workflow_runs.iter().rev().map(run_dto).collect())
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
    ensure_stages_run_locally(&state, &session, &def)?;

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
    let has_driver = state.run_wakers.lock().unwrap().contains_key(&rid);
    if has_driver {
        state.canceled_runs.lock().unwrap().insert(rid);
        if let Some(w) = state.run_wakers.lock().unwrap().get(&rid) {
            w.notify_waiters();
        }
        return Ok(());
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
    ensure_stages_run_locally(&state, &session, &run.definition)?;

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

/// v1 constraint: the starting device must own every stage's responder —
/// runs execute wholly on the device that starts them.
fn ensure_stages_run_locally(
    state: &State<AppState>,
    session: &ChatSession,
    def: &wf::WorkflowDefinition,
) -> Result<(), String> {
    let local = state.local_actor_id();
    for node in &def.nodes {
        let WorkflowNodeKind::Agent { agent_id, .. } = &node.kind else { continue };
        let agent = match agent_id {
            Some(id) => Some(
                session
                    .workspace_agents
                    .iter()
                    .find(|a| a.id == *id)
                    .ok_or_else(|| {
                        format!("stage '{}': its agent is not in this chat's roster", node.name)
                    })?,
            ),
            None => None,
        };
        let responder = responder_for(state, session, agent);
        if !owns_responder(&local, &responder) {
            return Err(format!(
                "stage '{}' uses an agent owned by another member; workflows currently run \
                 entirely on the device that starts them",
                node.name
            ));
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
