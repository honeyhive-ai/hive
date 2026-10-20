//! The agent worker: drain queued `@mentions` for the agents hosted on this
//! box and answer them. Shared by `hive worker` (headless daemon) and
//! `hive serve` (the home host for the iOS app), which also listens for the
//! live events a turn produces through a [`TurnSink`].
//!
//! A turn runs on the agent's workspace runtime (spec §12.5 — never a personal
//! key). HTTP model runtimes that may front a local server get the offline
//! harness's tool loop (built-in file tools + MCP tools, see
//! [`reply::generate_reply_streaming`]); file-mutating subprocess agents run in
//! an eagerly cut worktree as before. Either way, a turn's file changes come
//! back as a review-gated `FileDiff` proposal on the chat, synced like the
//! desktop's per-turn proposals.
//!
//! The chat service is shared behind a mutex and only locked for short,
//! synchronous store operations — never across a model call — so an HTTP API
//! running beside the worker stays responsive while a turn streams.

use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use hive_core::{workspace_config_session_id, ModelProviderKind, WorkspaceAgent, WorkspaceRuntime};
use hive_runtime::{pending_mentions, resolve_workspace_credential, turns_for, ChatService, PendingMention, ResolvedRuntime};
use uuid::Uuid;

use crate::reply::{self, ToolSettings, TurnFiles};
use crate::repos::{repos_dir, RepoBindings};
use crate::Config;

/// Lifecycle of one agent turn, as reported to a [`TurnSink`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    Queued,
    Running,
    Done,
    Error,
}

impl TurnState {
    pub fn as_str(self) -> &'static str {
        match self {
            TurnState::Queued => "queued",
            TurnState::Running => "running",
            TurnState::Done => "done",
            TurnState::Error => "error",
        }
    }
}

/// Live observer of the worker's turns. Every method defaults to a no-op.
pub trait TurnSink: Send + Sync {
    /// A turn changed state.
    fn status(&self, _chat: Uuid, _agent: Uuid, _state: TurnState, _detail: Option<&str>) {}
    /// A streamed reply fragment for message `message` in `chat`.
    fn delta(&self, _chat: Uuid, _message: Uuid, _text: &str) {}
    /// The store changed (a reply began or completed, a proposal was filed).
    fn changed(&self) {}
}

/// The silent sink `hive worker` uses.
pub struct NoSink;
impl TurnSink for NoSink {}

/// Everything a drain pass needs besides the service.
pub struct WorkerCtx<'a> {
    pub cfg: &'a Config,
    pub ws: Uuid,
    pub my_host: &'a str,
    pub host_label: &'a str,
    pub settings: ToolSettings,
}

fn lock(svc: &Mutex<ChatService>) -> MutexGuard<'_, ChatService> {
    svc.lock().unwrap_or_else(|e| e.into_inner())
}

/// Claude Code permission args for a headless worker. **Read-only by default** —
/// a worker that can write files (and, with bypass, run shell commands) is
/// powerful, so write access is opt-in via
/// `HIVE_WORKER_PERMISSION_MODE=acceptEdits|bypassPermissions`. Only then does a
/// worker's claude turn actually edit files (and get worktree-isolated + turned
/// into a review-gated proposal).
fn worker_claude_args() -> Vec<String> {
    match std::env::var("HIVE_WORKER_PERMISSION_MODE").as_deref() {
        Ok("acceptEdits") => vec!["--permission-mode".into(), "acceptEdits".into()],
        Ok("bypassPermissions") => vec!["--permission-mode".into(), "bypassPermissions".into()],
        _ => Vec::new(),
    }
}

/// Whether a keyless runtime is usable: native Ollama, or an OpenAI-wire
/// server on this machine / the LAN / the tailnet (a llama.cpp gateway).
fn keyless_ok(wr: &WorkspaceRuntime) -> bool {
    wr.provider == ModelProviderKind::Ollama
        || (matches!(wr.provider, ModelProviderKind::Custom | ModelProviderKind::HiveDaemon)
            && hive_core::endpoint::endpoint_is_local_server(&wr.endpoint))
}

/// The host-local runtimes file inside the data dir.
pub const LOCAL_RUNTIMES_FILE: &str = "runtimes.toml";

#[derive(serde::Deserialize, Default)]
struct LocalRuntimesToml {
    #[serde(default, rename = "runtime")]
    runtimes: Vec<WorkspaceRuntime>,
}

/// This box's own runtimes (`<data>/runtimes.toml`, the `hive config export`
/// shape: `[[runtime]] id, name, provider, model, endpoint, credential`).
///
/// The desktop binds an agent to one of its *device-local* runtimes, whose id
/// means nothing on another box; a worker only knows the workspace-owned
/// runtimes on the config log. This file maps such ids to what the agent runs
/// on here — typically the lab's keyless llama.cpp gateway — and an entry with
/// `id = "*"` catches every agent sent to this host whose runtime isn't known.
/// It is the box owner's own config, never synced. Workspace runtimes win.
pub fn local_runtimes(data_dir: &std::path::Path) -> Vec<WorkspaceRuntime> {
    let path = data_dir.join(LOCAL_RUNTIMES_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match toml::from_str::<LocalRuntimesToml>(&text) {
        Ok(doc) => doc.runtimes,
        Err(e) => {
            eprintln!("worker: ignoring {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// The runtime an agent runs on here: a workspace runtime with its id, else a
/// host-local one with its id, else the host-local catch-all (`id = "*"`).
/// `runtimes` is workspace runtimes first, then [`local_runtimes`].
pub fn runtime_for<'a>(runtimes: &'a [WorkspaceRuntime], agent: &WorkspaceAgent) -> Option<&'a WorkspaceRuntime> {
    runtimes.iter().find(|r| r.id == agent.runtime_id).or_else(|| runtimes.iter().find(|r| r.id == "*"))
}

/// Build a ResolvedRuntime for an agent from its workspace runtime (or this
/// host's own, see [`local_runtimes`]). A detached agent never runs on a
/// member's personal key (§12.5) — this returns an error if neither knows the
/// agent's runtime.
pub fn agent_workspace_runtime(
    runtimes: &[WorkspaceRuntime],
    agent: &WorkspaceAgent,
) -> Result<(ResolvedRuntime, String)> {
    let wr = runtime_for(runtimes, agent).ok_or_else(|| {
        anyhow!(
            "@{} runs on '{}', which is neither a workspace runtime nor in this host's {LOCAL_RUNTIMES_FILE}",
            agent.name,
            agent.runtime_id
        )
    })?;
    let api_key = resolve_workspace_credential(&wr.credential);
    if api_key.is_none() && !keyless_ok(wr) {
        bail!("workspace runtime '{}' has no secret on this box (provision HIVE_WS_SECRET_…)", wr.name);
    }
    // claude-code honors --permission-mode; other providers ignore args here.
    let args = if wr.provider == ModelProviderKind::ClaudeCode {
        worker_claude_args()
    } else {
        Vec::new()
    };
    let rt = ResolvedRuntime {
        provider: wr.provider,
        model: wr.model.clone(),
        endpoint: wr.endpoint.clone(),
        api_key,
        args,
        model_provider_id: None,
        model_base_url: None,
        context_window_tokens: None,
        keep_alive: None,
        think: None,
        // Only local-capable runtimes act on it (`runs_agent_tools`).
        tools: reply::worker_tools_enabled(),
        max_tool_rounds: None,
        allow_remote_file_tools: false,
    };
    Ok((rt, wr.id.clone()))
}

/// One drain pass: answer every queued mention for an agent this worker hosts
/// that another device hasn't already claimed. Returns `Err` only on a
/// tick-fatal store error (the caller logs and retries) — per-mention failures
/// are isolated so one bad mention can't abort the whole pass.
pub async fn drain_tick(
    ctx: &WorkerCtx<'_>,
    svc: &Mutex<ChatService>,
    failed: &mut HashSet<Uuid>,
    sink: &dyn TurnSink,
) -> Result<()> {
    // Bound the hard-failed set over a long uptime — past a generous cap,
    // drop it so transiently-failed mentions get another chance and memory
    // can't creep unbounded.
    if failed.len() >= 8192 {
        failed.clear();
    }
    let (work, runtimes) = {
        let mut svc = lock(svc);
        // Heartbeat presence (throttled), so peers see this worker as online.
        let _ = svc.touch_host(ctx.ws, ctx.my_host, 60);
        let hosted: HashSet<Uuid> = svc
            .load(workspace_config_session_id(ctx.ws))?
            .map(|s| s.workspace_agents)
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.host_id == ctx.my_host)
            .map(|a| a.id)
            .collect();
        let mut runtimes = svc.list_workspace_runtimes(ctx.ws)?;
        runtimes.extend(local_runtimes(&ctx.cfg.data_dir));
        let mut candidates: Vec<PendingMention> = Vec::new();
        for id in svc.store().list_session_ids()? {
            // Skip a session that fails to load rather than aborting the whole tick.
            let s = match svc.load(id) {
                Ok(Some(s)) => s,
                Ok(None) => continue,
                Err(e) => {
                    eprintln!("worker: load {id} failed: {e}");
                    continue;
                }
            };
            if id == workspace_config_session_id(s.workspace_id) {
                continue;
            }
            candidates.extend(
                pending_mentions(&s)
                    .into_iter()
                    .filter(|pm| hosted.contains(&pm.agent_id) && !failed.contains(&pm.message_id)),
            );
        }
        // Claim each turn so a worker and the agent-owner's desktop (or another
        // worker) don't both answer the same mention. Back off if we lost.
        let mut work = Vec::new();
        for pm in candidates {
            let Some(ws) = svc.load(pm.session_id)?.map(|s| s.workspace_id) else { continue };
            match svc.claim_turn(pm.session_id, ws, pm.message_id) {
                Ok(true) => work.push(pm),
                Ok(false) => {}
                Err(e) => eprintln!("worker: claim {} failed: {e}", pm.message_id),
            }
        }
        (work, runtimes)
    };
    for pm in &work {
        sink.status(pm.session_id, pm.agent_id, TurnState::Queued, None);
    }
    for pm in work {
        if let Err(e) = run_turn(ctx, svc, &runtimes, &pm, sink).await {
            eprintln!("worker: @{} in {} failed: {e}", pm.agent_name, pm.session_id);
            failed.insert(pm.message_id);
            sink.status(pm.session_id, pm.agent_id, TurnState::Error, Some(&e.to_string()));
        }
    }
    Ok(())
}

/// The note a reply cut off by a restart ends with.
pub const INTERRUPTED_NOTE: &str = "[interrupted: the host restarted before this reply finished. Ask again to retry.]";

/// Startup recovery, run before the worker loop: finish every reply *this
/// device* was still streaming when the process stopped (a restart or Ctrl-C
/// mid-turn). The placeholder is stamped `reply_to` the mention, so
/// `pending_mentions` already counts it as the answer and the turn would never
/// run again; left alone it would also stay `streaming` forever on every peer.
/// Each one is completed with the text checkpointed so far plus
/// [`INTERRUPTED_NOTE`], so the asker sees it failed and can ask again.
/// Returns `(chat, message)` for each reply finished this way.
pub fn recover_interrupted_turns(svc: &mut ChatService) -> Result<Vec<(Uuid, Uuid)>> {
    let me = svc.device_id();
    let mut out = Vec::new();
    for id in svc.store().list_session_ids()? {
        let s = match svc.load(id) {
            Ok(Some(s)) => s,
            Ok(None) => continue,
            Err(e) => {
                eprintln!("worker: recovery skipped {id}: {e}");
                continue;
            }
        };
        if id == workspace_config_session_id(s.workspace_id) {
            continue;
        }
        for m in s.messages.iter().filter(|m| m.is_streaming && m.origin_device_id == Some(me)) {
            let body = if m.body.trim().is_empty() {
                format!("[error] {INTERRUPTED_NOTE}")
            } else {
                format!("{}\n\n{INTERRUPTED_NOTE}", m.body.trim_end())
            };
            match svc.complete_assistant_message(s.id, s.workspace_id, m.id, body) {
                Ok(()) => out.push((s.id, m.id)),
                Err(e) => eprintln!("worker: finishing interrupted reply {} failed: {e}", m.id),
            }
        }
    }
    Ok(out)
}

/// Total wall-clock backstop for one turn (the provider's idle timeout is the
/// real "wedged" gate), as in the app: `HIVE_TURN_TIMEOUT_SECS`, default 1 h.
fn turn_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("HIVE_TURN_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&s| s > 0)
            .unwrap_or(3600),
    )
}

/// Answer one claimed mention end to end: placeholder, streamed reply (with
/// throttled crash-recovery checkpoints), final body, and the turn's file
/// changes as a proposal.
async fn run_turn(
    ctx: &WorkerCtx<'_>,
    svc: &Mutex<ChatService>,
    runtimes: &[WorkspaceRuntime],
    pm: &PendingMention,
    sink: &dyn TurnSink,
) -> Result<()> {
    let Some(s) = lock(svc).load(pm.session_id)? else {
        return Ok(());
    };
    let Some(agent) = s.workspace_agents.iter().find(|a| a.id == pm.agent_id).cloned() else {
        return Ok(());
    };
    let (rt, rt_label) = agent_workspace_runtime(runtimes, &agent)?;
    let mut turns = turns_for(&s, Some(agent.id), &agent.name);
    crate::materialize_turns(ctx.cfg, &mut turns).await;
    let system = format!(
        "You are @{}, an agent in a Hive workspace chat. Reply to the latest message concisely and helpfully.\n\n{}",
        agent.name,
        hive_runtime::prompt::runtime_identity_block(&rt, &format!("worker {}", ctx.host_label))
    );
    // H1 gate: resolve the triggering message's author + role and only offer
    // tools (MCP and the built-in file tools) when they clear
    // HIVE_MCP_MIN_ROLE (default Contributor). Unresolved author → fail closed.
    let author_role = s
        .messages
        .iter()
        .find(|m| m.id == pm.message_id)
        .and_then(|m| m.actor_identity.as_ref())
        .map(|a| s.role_of(&a.id));
    let min_role = reply::mcp_min_role();
    let allow_tools = reply::mcp_allowed_for(author_role, min_role);
    if !allow_tools {
        eprintln!(
            "withholding tools for message {} — author role {:?} below minimum {:?}",
            pm.message_id, author_role, min_role
        );
    }

    // The checkout this chat is bound to on this box (see `repos.rs`).
    let repo_root = RepoBindings::load(&ctx.cfg.data_dir).resolve(&repos_dir(&ctx.cfg.data_dir), s.id);

    let mid = lock(svc).begin_assistant_message(
        s.id,
        s.workspace_id,
        agent.name.clone(),
        rt_label.clone(),
        Some(agent.id),
        // Stamp the mention this reply answers, so pending_mentions correlates
        // it precisely even when several tasks target the same agent.
        Some(pm.message_id),
    )?;
    sink.changed();
    sink.status(s.id, agent.id, TurnState::Running, None);
    println!("↳ @{} replying in {} …", agent.name, s.id);

    // A file-mutating subprocess agent runs in an eagerly cut worktree (like
    // the desktop) so its edits return as a review-gated proposal. Only turns
    // that can actually write: a write-mode claude or aider/pi/codex/hermes.
    let write_capable = match rt.provider {
        ModelProviderKind::ClaudeCode => rt.args.iter().any(|a| a == "acceptEdits" || a == "bypassPermissions"),
        ModelProviderKind::Pi | ModelProviderKind::Aider | ModelProviderKind::Codex | ModelProviderKind::Hermes => true,
        _ => false,
    };
    // Subprocess agents fall back to the process's cwd when nothing is bound
    // (the pre-binding behavior of `hive worker`).
    let subprocess_root = repo_root.clone().or_else(|| std::env::current_dir().ok());
    let eager = match subprocess_root.as_deref() {
        Some(root) if write_capable && hive_runtime::git_worktree::is_git_repo(root) => {
            match hive_runtime::git_worktree::cut_turn_worktree(root, &s.id.to_string(), &mid.to_string()) {
                Ok(wt) => Some(wt),
                Err(e) => {
                    eprintln!("worker: worktree unavailable, running in place: {e}");
                    None
                }
            }
        }
        _ => None,
    };
    let work_dir = eager.as_ref().map(|w| w.path.to_string_lossy().into_owned());
    let files = TurnFiles { repo_root, chat_id: s.id.to_string(), turn_key: mid.to_string() };

    // Deltas go to the sink on every fragment; the store gets a throttled
    // checkpoint (~1×/750ms) for crash recovery, as in the app. The final body
    // is written once on completion.
    const FLUSH_EVERY: Duration = Duration::from_millis(750);
    let mut pending = String::new();
    let mut last_flush = Instant::now();
    let on_delta = |text: String| {
        sink.delta(s.id, mid, &text);
        pending.push_str(&text);
        if last_flush.elapsed() >= FLUSH_EVERY {
            let chunk = std::mem::take(&mut pending);
            let _ = lock(svc).append_chunk(s.id, s.workspace_id, mid, chunk);
            last_flush = Instant::now();
        }
    };
    let fut = reply::generate_reply_streaming(
        &rt,
        &system,
        &turns,
        allow_tools,
        work_dir.as_deref(),
        Some(&files),
        &ctx.settings,
        on_delta,
    );
    let result = match tokio::time::timeout(turn_timeout(), fut).await {
        Ok(r) => r,
        Err(_) => Err(anyhow!("runtime timed out after {}s", turn_timeout().as_secs())),
    };

    match result {
        Ok(reply) => {
            let worktree = eager.or(reply.worktree);
            let captured = worktree.as_ref().and_then(|wt| wt.diff().ok()).filter(|d| !d.trim().is_empty());
            {
                let mut svc = lock(svc);
                svc.complete_assistant_message(s.id, s.workspace_id, mid, reply.text)?;
                if let (Some(wt), Some(diff)) = (worktree.as_ref(), captured) {
                    let files = wt.changed_files().unwrap_or_default();
                    let n = files.len();
                    let title =
                        format!("@{} proposed changes to {} file{}", agent.name, n, if n == 1 { "" } else { "s" });
                    // Authorless (like the desktop's): the human reviewing can
                    // approve it — the self-approval guard excludes no one.
                    let mut prop = hive_core::ActionProposal::file_diff(title, "", diff, files);
                    prop.producing_agent_id = Some(agent.id);
                    if let Err(e) = svc.upsert_proposal(s.id, s.workspace_id, prop) {
                        eprintln!("worker: proposal failed: {e}");
                    }
                }
            }
            if let Some(wt) = worktree {
                let _ = wt.remove();
            }
            sink.changed();
            sink.status(s.id, agent.id, TurnState::Done, None);
            println!("   @{} done", agent.name);
            Ok(())
        }
        Err(e) => {
            if let Some(wt) = eager {
                let _ = wt.remove();
            }
            let _ = lock(svc).complete_assistant_message(s.id, s.workspace_id, mid, format!("[error] {e}"));
            sink.changed();
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::{WorkspaceCredential, WorkspaceRuntime};

    fn agent(rt: &str) -> WorkspaceAgent {
        WorkspaceAgent::new("coder", rt)
    }

    #[test]
    fn keyless_local_gateway_resolves_with_tools_on() {
        let mut wr = WorkspaceRuntime::new("gw", "Lab gateway", ModelProviderKind::Custom, "gpt-oss:20b");
        wr.endpoint = "http://100.101.102.103:8080/v1/chat/completions".into();
        let (rt, label) = agent_workspace_runtime(&[wr], &agent("gw")).unwrap();
        assert_eq!(label, "gw");
        assert!(rt.api_key.is_none());
        assert!(rt.runs_agent_tools(), "a tailnet llama.cpp gateway runs the tool loop");
    }

    #[test]
    fn local_runtimes_map_device_local_ids_and_catch_all() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(LOCAL_RUNTIMES_FILE),
            r#"
[[runtime]]
id = "lab"
name = "Lab gateway"
provider = "custom"
model = "gpt-oss:20b"
endpoint = "http://100.101.102.103:8080/v1/chat/completions"

[[runtime]]
id = "*"
name = "Default"
provider = "custom"
model = "gemma4:12b"
endpoint = "http://127.0.0.1:8080/v1/chat/completions"
credential = { kind = "none" }
"#,
        )
        .unwrap();
        let rts = local_runtimes(dir.path());
        assert_eq!(rts.len(), 2);
        assert_eq!(runtime_for(&rts, &agent("lab")).unwrap().model, "gpt-oss:20b");
        assert_eq!(runtime_for(&rts, &agent("desktop-local-id")).unwrap().model, "gemma4:12b");
        let (rt, _) = agent_workspace_runtime(&rts, &agent("desktop-local-id")).unwrap();
        assert!(rt.runs_agent_tools());
        // A workspace runtime with the agent's id wins over the catch-all.
        let mut all = vec![WorkspaceRuntime::new("desktop-local-id", "WS", ModelProviderKind::Ollama, "llama3.2")];
        all.extend(rts);
        assert_eq!(runtime_for(&all, &agent("desktop-local-id")).unwrap().name, "WS");
        assert!(runtime_for(&[], &agent("x")).is_none());
    }

    #[test]
    fn keyless_hosted_api_is_refused() {
        let mut wr = WorkspaceRuntime::new("oa", "OpenAI", ModelProviderKind::OpenAI, "gpt-4o");
        wr.endpoint = "https://api.openai.com/v1/chat/completions".into();
        wr.credential = WorkspaceCredential::SecretRef { secret_ref: "definitely_unset_ref_xyz".into() };
        assert!(agent_workspace_runtime(&[wr], &agent("oa")).is_err());
    }

    #[test]
    fn hosted_api_never_gets_agent_tools() {
        let mut wr = WorkspaceRuntime::new("oa", "OpenAI", ModelProviderKind::OpenAI, "gpt-4o");
        wr.credential = WorkspaceCredential::Inline { secret: "sk-test".into() };
        let (rt, _) = agent_workspace_runtime(&[wr], &agent("oa")).unwrap();
        assert!(!rt.runs_agent_tools());
    }

    #[test]
    fn builtin_tools_need_a_private_endpoint_unless_opted_in() {
        let mut wr = WorkspaceRuntime::new("gw", "GW", ModelProviderKind::Custom, "m");
        wr.endpoint = "https://models.example.com/v1/chat/completions".into();
        wr.credential = WorkspaceCredential::Inline { secret: "k".into() };
        let (rt, _) = agent_workspace_runtime(&[wr], &agent("gw")).unwrap();
        let mut settings = ToolSettings::default();
        assert!(!reply::wants_builtin_tools(&rt, &settings), "public endpoint: no file tools by default");
        settings.allow_remote_file_tools = true;
        assert!(reply::wants_builtin_tools(&rt, &settings));
        settings.builtin_tools = false;
        assert!(!reply::wants_builtin_tools(&rt, &settings));
    }
}
