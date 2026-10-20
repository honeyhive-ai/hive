//! `hive serve` — the home host for the iOS app.
//!
//! One process on an always-on box (the lab's SFF) that is a normal Hive
//! device in the user's workspace — its E2EE keys never leave the box — and:
//!
//! 1. runs the relay sync loop (push local events, pull + verify peers'),
//! 2. runs the agent worker for the agents hosted here (`worker.rs`), with the
//!    offline harness's tool loop for local model runtimes,
//! 3. exposes a small JSON API + a Server-Sent Events stream to the phone,
//!    bound to a loopback/LAN/tailnet address and gated by a bearer token
//!    (`api.rs`; the contract is in `docs/ios-app.md`),
//! 4. pushes notifications through a self-hosted ntfy (`ntfy.rs`).

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use hive_core::{workspace_config_session_id, ChatSession, HostKind, MessageRole, Timestamp, WorkspaceHost};
use hive_runtime::ChatService;
use serde_json::json;
use tokio::sync::{watch, Notify};
use uuid::Uuid;

use crate::reply::{env_flag, ToolSettings};
use crate::worker::{self, TurnSink, TurnState, WorkerCtx};
use crate::{env_opt, open_service, sync_once, uuid_of_room, Config};

pub mod api;
pub mod hub;
pub mod lab;
pub mod ntfy;
pub mod pair;

#[cfg(test)]
mod tests;

/// How long `hive serve` waits for a turn in flight on SIGTERM / Ctrl-C.
const STOP_GRACE: Duration = Duration::from_secs(15);

/// Replies and proposals created this long before serve started don't push.
const NOTIFY_HISTORY_MINUTES: i64 = 10;

/// Default listen address: loopback only. On the home host the systemd unit
/// passes the box's Tailscale IP.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8787";

/// Everything `hive serve` is configured with besides the workspace [`Config`].
#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub listen: SocketAddr,
    pub token_file: PathBuf,
    pub ntfy: Option<ntfy::Ntfy>,
    /// The URL the phone uses (pairing link); default `http://<listen>`.
    pub public_url: Option<String>,
    /// The host label other members see for this box.
    pub label: String,
    /// Display name this box's identity posts the phone's messages under.
    pub name: Option<String>,
    pub pair: bool,
    pub prometheus_url: String,
    /// `@mention` added to a phone message that names no agent.
    pub default_agent: Option<String>,
    /// Apply an approved file-diff proposal to this box's bound checkout.
    pub apply_on_approve: bool,
    /// Relay sync cadence.
    pub sync_every: Duration,
}

impl ServeOptions {
    /// Flags first, then `HIVE_SERVE_*` / `HIVE_NTFY_*` env, then defaults.
    #[allow(clippy::too_many_arguments)]
    pub fn from_flags(
        cfg: &Config,
        listen: Option<String>,
        token_file: Option<String>,
        ntfy_url: Option<String>,
        ntfy_topic: Option<String>,
        public_url: Option<String>,
        label: Option<String>,
        name: Option<String>,
        pair: bool,
    ) -> Result<Self> {
        let listen_raw = listen.or_else(|| env_opt("HIVE_SERVE_LISTEN")).unwrap_or_else(|| DEFAULT_LISTEN.into());
        let listen: SocketAddr = listen_raw
            .parse()
            .with_context(|| format!("--listen must be IP:PORT (got '{listen_raw}')"))?;
        if !pair::bind_allowed(listen.ip()) && !env_flag("HIVE_SERVE_ALLOW_ANY_BIND", false) {
            bail!(
                "refusing to listen on {listen}: bind to loopback, a private LAN address or the box's Tailscale IP \
                 (set HIVE_SERVE_ALLOW_ANY_BIND=1 to override)"
            );
        }
        let ntfy = match (ntfy_url.or_else(|| env_opt("HIVE_NTFY_URL")), ntfy_topic.or_else(|| env_opt("HIVE_NTFY_TOPIC"))) {
            (Some(url), Some(topic)) => Some(ntfy::Ntfy { url, topic, token: env_opt("HIVE_NTFY_TOKEN") }),
            (Some(_), None) | (None, Some(_)) => bail!("ntfy needs both a URL and a topic (--ntfy-url / --ntfy-topic)"),
            (None, None) => None,
        };
        Ok(Self {
            listen,
            token_file: token_file
                .or_else(|| env_opt("HIVE_SERVE_TOKEN_FILE"))
                .map(PathBuf::from)
                .unwrap_or_else(|| cfg.data_dir.join("serve.token")),
            ntfy,
            public_url: public_url.or_else(|| env_opt("HIVE_SERVE_PUBLIC_URL")),
            label: label
                .or_else(|| env_opt("HIVE_SERVE_LABEL"))
                .or_else(|| env_opt("HOSTNAME"))
                .unwrap_or_else(|| "home host".into()),
            name: name.or_else(|| env_opt("HIVE_SERVE_NAME")),
            pair,
            prometheus_url: env_opt("HIVE_PROMETHEUS_URL").unwrap_or_else(|| "http://127.0.0.1:9090".into()),
            default_agent: env_opt("HIVE_SERVE_DEFAULT_AGENT").map(|a| a.trim().trim_start_matches('@').to_string()),
            apply_on_approve: env_flag("HIVE_SERVE_APPLY_ON_APPROVE", false),
            sync_every: Duration::from_secs(3),
        })
    }

    /// The base URL the phone uses.
    pub fn server_url(&self) -> String {
        self.public_url.clone().unwrap_or_else(|| pair::default_server_url(self.listen))
    }
}

/// State shared by the API handlers, the sync loop and the worker thread.
pub struct Shared {
    pub cfg: Config,
    pub opts: ServeOptions,
    pub ws: Uuid,
    pub token: String,
    pub host_id: String,
    pub svc: Mutex<ChatService>,
    pub hub: hub::Hub,
    differ: Mutex<hub::Differ>,
    /// chat id → id of the last message the phone has seen.
    read_marks: Mutex<HashMap<Uuid, Uuid>>,
    pub sync_kick: Notify,
    pub worker_kick: Notify,
    stopping: AtomicBool,
    /// When this process started (see [`Shared::fresh`]).
    started_at: Timestamp,
    pub http: reqwest::Client,
    rt: tokio::runtime::Handle,
}

/// The device-local serve state file (read marks).
fn state_path(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("serve-state.json")
}

impl Shared {
    pub fn lock(&self) -> MutexGuard<'_, ChatService> {
        self.svc.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// The workspace's live (non-archived) chats, config log excluded.
    pub fn workspace_chats(&self) -> Result<Vec<ChatSession>> {
        let svc = self.lock();
        let config = workspace_config_session_id(self.ws);
        let mut out = Vec::new();
        for id in svc.store().list_session_ids()? {
            if id == config {
                continue;
            }
            if let Ok(Some(s)) = svc.load(id) {
                if s.workspace_id == self.ws && !s.archived && id != workspace_config_session_id(s.workspace_id) {
                    out.push(s);
                }
            }
        }
        Ok(out)
    }

    /// The actor id this box writes as.
    pub fn my_actor_id(&self) -> String {
        self.lock().author().id.clone()
    }

    /// Unread count for `s`: messages after the phone's read mark that this
    /// box didn't write (system notes excluded). No mark = all of them.
    pub fn unread(&self, s: &ChatSession, me: &str) -> usize {
        let marks = self.read_marks.lock().unwrap_or_else(|e| e.into_inner());
        let start = match marks.get(&s.id) {
            Some(mark) => match s.messages.iter().position(|m| m.id == *mark) {
                Some(i) => i + 1,
                None => s.messages.len(),
            },
            None => 0,
        };
        s.messages[start.min(s.messages.len())..]
            .iter()
            .filter(|m| m.role != MessageRole::System)
            .filter(|m| !(m.role == MessageRole::User && m.actor_identity.as_ref().is_some_and(|a| a.id == me)))
            .count()
    }

    /// Advance the read mark for `s` to `message` (never backwards).
    pub fn mark_read(&self, s: &ChatSession, message: Uuid) {
        let mut marks = self.read_marks.lock().unwrap_or_else(|e| e.into_inner());
        let new_idx = s.messages.iter().position(|m| m.id == message);
        let old_idx = marks.get(&s.id).and_then(|old| s.messages.iter().position(|m| m.id == *old));
        if new_idx.is_some() && new_idx >= old_idx {
            marks.insert(s.id, message);
            let body = json!({ "readMarks": &*marks });
            drop(marks);
            let _ = std::fs::write(state_path(&self.cfg), body.to_string());
        }
    }

    /// Diff the projection against the last look and publish the difference
    /// (SSE events, plus ntfy for finished agent replies and new proposals).
    ///
    /// The sync loop, the worker thread and API handlers all call this. The
    /// projection is loaded while holding the differ lock, so each diff runs on
    /// a snapshot at least as new as the one before it; loading first let a
    /// slow caller diff a stale snapshot after a newer one, rolling the differ
    /// back and re-emitting `message.done` (and its ntfy push).
    pub fn refresh(&self) {
        let me = self.my_actor_id();
        let mut notices = Vec::new();
        {
            let mut differ = self.differ.lock().unwrap_or_else(|e| e.into_inner());
            let sessions = match self.workspace_chats() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("serve: refresh failed: {e}");
                    return;
                }
            };
            for change in differ.diff(&sessions) {
                use hub::Change;
                match change {
                    Change::MessageCreated { chat, message } | Change::MessageDone { chat, message } => {
                        let done = matches!(change, Change::MessageDone { .. });
                        let Some(s) = sessions.iter().find(|s| s.id == chat) else { continue };
                        let Some(m) = s.messages.iter().find(|m| m.id == message) else { continue };
                        let name = if done { "message.done" } else { "message.created" };
                        self.hub.emit(name, hub::wrap("message", api::message_dto(chat, m)));
                        if done && matches!(m.role, MessageRole::Assistant | MessageRole::Agent) && self.fresh(m.created_at) {
                            notices.push(ntfy::reply_notice(&m.author, &s.title, chat, m.body.starts_with("[error]")));
                        }
                    }
                    Change::ProposalCreated { chat, proposal } => {
                        let Some(s) = sessions.iter().find(|s| s.id == chat) else { continue };
                        let Some(p) = s.proposals.iter().find(|p| p.id == proposal) else { continue };
                        self.hub.emit("proposal.created", hub::wrap("proposal", api::proposal_dto(s, p)));
                        if self.fresh(p.created_at) {
                            notices.push(ntfy::proposal_notice(&p.title, &s.title, chat));
                        }
                    }
                    Change::ChatUpdated { chat } => {
                        let Some(s) = sessions.iter().find(|s| s.id == chat) else { continue };
                        let unread = self.unread(s, &me);
                        self.hub.emit("chat.updated", hub::wrap("chat", api::chat_dto(s, unread)));
                    }
                }
            }
        }
        if let Some(n) = self.opts.ntfy.clone() {
            for notice in notices {
                let n = n.clone();
                let client = self.http.clone();
                self.rt.spawn(async move { n.send(&client, &notice).await });
            }
        }
    }

    /// Whether something created at `at` is news worth a push: not history
    /// that synced in late (an outage, or a first sync that failed and seeded
    /// the differ from an incomplete store). The phone still lists it.
    fn fresh(&self, at: Timestamp) -> bool {
        at.inner() >= self.started_at.inner() - time::Duration::minutes(NOTIFY_HISTORY_MINUTES)
    }

    /// After a local write: tell the phone, wake the worker, push to the relay.
    pub fn after_write(&self) {
        self.refresh();
        self.worker_kick.notify_one();
        self.sync_kick.notify_one();
    }
}

/// The worker's live events → SSE, plus a refresh + push on store changes.
struct ServeSink(Arc<Shared>);

impl TurnSink for ServeSink {
    fn status(&self, chat: Uuid, agent: Uuid, state: TurnState, detail: Option<&str>) {
        let mut data = json!({ "chatId": chat, "agentId": agent, "state": state.as_str() });
        if let Some(d) = detail {
            data["detail"] = json!(d);
        }
        self.0.hub.emit("turn.status", data);
    }

    fn delta(&self, chat: Uuid, message: Uuid, text: &str) {
        self.0.hub.emit("message.delta", json!({ "chatId": chat, "messageId": message, "text": text }));
    }

    fn changed(&self) {
        self.0.refresh();
        self.0.sync_kick.notify_one();
    }
}

/// A running server. Dropping it (or [`ServeHandle::shutdown`]) stops the API,
/// the sync loop and — between turns — the worker.
pub struct ServeHandle {
    pub addr: SocketAddr,
    pub shared: Arc<Shared>,
    shutdown: watch::Sender<bool>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl ServeHandle {
    pub fn shutdown(&self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        let _ = self.shutdown.send(true);
        self.shared.worker_kick.notify_one();
        self.shared.sync_kick.notify_one();
    }

    /// Stop, then give a turn in flight up to `grace` to finish (the worker
    /// checks `stopping` between turns). Returns whether the worker stopped in
    /// time; if not, the next start finishes the cut-off reply
    /// ([`worker::recover_interrupted_turns`]).
    pub async fn stop(&self, grace: Duration) -> bool {
        self.shutdown();
        let Some(worker) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return true;
        };
        let deadline = tokio::time::Instant::now() + grace;
        while !worker.is_finished() {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = worker.join();
        // The finished turn's last writes go out before we exit.
        if self.shared.cfg.relay_url.is_some() {
            let _ = tokio::time::timeout(Duration::from_secs(3), sync_once(&self.shared.cfg)).await;
        }
        true
    }
}

impl Drop for ServeHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn load_read_marks(cfg: &Config) -> HashMap<Uuid, Uuid> {
    std::fs::read_to_string(state_path(cfg))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| serde_json::from_value(v.get("readMarks")?.clone()).ok())
        .unwrap_or_default()
}

/// Start everything on the current (multi-threaded) tokio runtime and return
/// once the API is listening.
pub async fn start(cfg: Config, opts: ServeOptions) -> Result<ServeHandle> {
    let token = pair::load_or_create_token(&opts.token_file)?;
    let ws = uuid_of_room(&cfg.room);
    // Pull the workspace first, so registering below extends the real config
    // log instead of seeding a competing one.
    if cfg.relay_url.is_some() {
        if let Err(e) = sync_once(&cfg).await {
            // With no local copy of the workspace config yet, registering would
            // seed a competing config log naming this box Owner. Fail instead;
            // systemd restarts us until the relay is reachable.
            let ws_config = workspace_config_session_id(ws);
            if open_service(&cfg)?.load(ws_config)?.is_none() {
                bail!("initial sync failed and this box has no copy of the workspace yet: {e}");
            }
            eprintln!("serve: initial sync failed (continuing, will retry): {e}");
        }
    }
    let mut svc = open_service(&cfg)?;
    if let Some(name) = &opts.name {
        svc.set_author_display_name(name.clone());
    }
    let host_id = svc.device_id().to_string();
    // A turn cut off by the last stop would otherwise stay "streaming" forever
    // and never be answered (its placeholder counts as the reply).
    match worker::recover_interrupted_turns(&mut svc) {
        Ok(done) if !done.is_empty() => eprintln!("serve: finished {} reply(s) interrupted by the last stop", done.len()),
        Ok(_) => {}
        Err(e) => eprintln!("serve: interrupted-turn recovery failed: {e}"),
    }
    // Announce this box in the workspace roster and publish its identity so
    // peers verify its events, then register it as an always-on host.
    if let Err(e) = svc.ensure_self_member(workspace_config_session_id(ws), ws) {
        eprintln!("serve: joining the workspace roster failed: {e}");
    }
    svc.register_host(ws, WorkspaceHost::new(host_id.clone(), HostKind::Worker, opts.label.clone()))
        .context("registering this box as a workspace host")?;

    let shared = Arc::new(Shared {
        ws,
        token,
        host_id,
        svc: Mutex::new(svc),
        hub: hub::Hub::new(),
        differ: Mutex::new(hub::Differ::default()),
        read_marks: Mutex::new(load_read_marks(&cfg)),
        sync_kick: Notify::new(),
        worker_kick: Notify::new(),
        stopping: AtomicBool::new(false),
        started_at: Timestamp::now(),
        http: reqwest::Client::builder().connect_timeout(Duration::from_secs(3)).build()?,
        rt: tokio::runtime::Handle::current(),
        cfg,
        opts,
    });
    // Seed the differ and mark every existing chat read, so a restart neither
    // replays history as events nor shows it all as unread.
    {
        let chats = shared.workspace_chats()?;
        shared.differ.lock().unwrap_or_else(|e| e.into_inner()).seed(&chats);
        let mut marks = shared.read_marks.lock().unwrap_or_else(|e| e.into_inner());
        for s in &chats {
            if let Some(last) = s.messages.last() {
                marks.entry(s.id).or_insert(last.id);
            }
        }
    }

    let (shutdown, rx) = watch::channel(false);
    let listener = tokio::net::TcpListener::bind(shared.opts.listen)
        .await
        .with_context(|| format!("binding {}", shared.opts.listen))?;
    let addr = listener.local_addr()?;
    let app = api::router(shared.clone());
    let mut rx_api = rx.clone();
    tokio::spawn(async move {
        let graceful = async move {
            let _ = rx_api.wait_for(|stop| *stop).await;
        };
        if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(graceful).await {
            eprintln!("serve: API stopped: {e}");
        }
    });
    if shared.cfg.relay_url.is_some() {
        tokio::spawn(sync_loop(shared.clone(), rx.clone()));
    }
    let worker = spawn_worker(shared.clone())?;
    shared.sync_kick.notify_one();
    Ok(ServeHandle { addr, shared, shutdown, worker: Mutex::new(Some(worker)) })
}

/// Relay sync: every few seconds, and immediately after a local write.
async fn sync_loop(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut fails: u32 = 0;
    while !shared.stopping() {
        let wait = match sync_once(&shared.cfg).await {
            Ok((_, pulled)) => {
                fails = 0;
                if pulled > 0 {
                    shared.refresh();
                    shared.worker_kick.notify_one();
                }
                shared.opts.sync_every
            }
            Err(_) if shared.stopping() => return,
            Err(e) => {
                fails = fails.saturating_add(1);
                let secs = crate::sync_backoff_secs(fails);
                eprintln!("sync: {e} — retrying in {secs}s");
                Duration::from_secs(secs)
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = shared.sync_kick.notified(), if fails == 0 => {}
            _ = stop.wait_for(|s| *s) => return,
        }
    }
}

/// The worker runs on its own thread with a current-thread runtime: a turn's
/// tool executors (MCP clients, the lazy worktree) never have to be `Send`, and
/// a long turn never starves the API.
fn spawn_worker(shared: Arc<Shared>) -> Result<std::thread::JoinHandle<()>> {
    Ok(std::thread::Builder::new().name("hive-serve-worker".into()).spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("serve: worker runtime failed: {e}");
                return;
            }
        };
        rt.block_on(async move {
            let ctx = WorkerCtx {
                cfg: &shared.cfg,
                ws: shared.ws,
                my_host: &shared.host_id,
                host_label: &shared.opts.label,
                settings: ToolSettings::from_env(),
            };
            let sink = ServeSink(shared.clone());
            let mut failed: HashSet<Uuid> = HashSet::new();
            while !shared.stopping() {
                if let Err(e) = worker::drain_tick(&ctx, &shared.svc, &mut failed, &sink).await {
                    eprintln!("worker: tick failed (retrying): {e}");
                }
                tokio::select! {
                    _ = shared.worker_kick.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                }
            }
        });
    })?)
}

/// Print the pairing QR code and link.
pub fn print_pairing(server_url: &str, token: &str) -> Result<()> {
    let link = pair::pairing_url(server_url, token);
    println!("\nScan with the iPhone camera (or the Hive app) to pair:\n");
    print!("{}", pair::render_qr(&link)?);
    println!("\n{link}\n");
    println!("This link carries the API token: share it only with your own phone.");
    Ok(())
}

/// `hive serve` — run until SIGTERM / Ctrl-C.
pub fn cmd_serve(cfg: Config, opts: ServeOptions) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let server_url = opts.server_url();
        let pair_now = opts.pair;
        let handle = start(cfg, opts).await?;
        println!(
            "hive serve: listening on http://{} — workspace '{}', host {} ('{}'). Ctrl-C to stop.",
            handle.addr, handle.shared.cfg.room, handle.shared.host_id, handle.shared.opts.label
        );
        if pair_now {
            print_pairing(&server_url, &handle.shared.token)?;
        } else {
            println!("Run `hive pair` to show the phone pairing code.");
        }
        crate::shutdown_signal().await;
        println!("hive serve: stopping.");
        // Under the unit's TimeoutStopSec (20 s): let a turn in flight finish
        // if it can; a longer one is finished as interrupted on the next start.
        if !handle.stop(STOP_GRACE).await {
            eprintln!("hive serve: a turn was still running; it will be marked interrupted on the next start.");
        }
        Ok(())
    })
}

/// `hive pair` — show the pairing code for a configured (possibly already
/// running) `hive serve`, without starting one.
pub fn cmd_pair(opts: &ServeOptions) -> Result<()> {
    let url = opts.server_url();
    if pair::is_loopback_url(&url) {
        anyhow::bail!(
            "the pairing link would point the phone at {url}, which on the phone is the phone itself. \
             Run with the server's settings (HIVE_SERVE_LISTEN / HIVE_DATA_DIR, e.g. from its env file), \
             or pass --listen <tailnet-ip:port> or --public-url <url>"
        );
    }
    let token = pair::load_existing_token(&opts.token_file)?;
    print_pairing(&url, &token)
}
