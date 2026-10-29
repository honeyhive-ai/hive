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
//! 4. pushes notifications through a self-hosted ntfy (`ntfy.rs`),
//! 5. optionally keeps a relay tunnel open so the phone reaches the API
//!    without a VPN (`tunnel/`, spec `docs/relay-tunnel.md`; off by default),
//! 6. serves the owner-defined dashboard (`dashboard.rs`).

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use hive_core::{workspace_config_session_id, ChatSession, HostKind, MessageRole, Timestamp, WorkspaceHost};
use hive_runtime::device_enroll::Standing;
use hive_runtime::ChatService;
use serde_json::json;
use tokio::sync::{watch, Notify};
use uuid::Uuid;

use crate::reply::{env_flag, ToolSettings};
use crate::worker::{self, TurnSink, TurnState, WorkerCtx};
use crate::{env_opt, open_service, sync_once, uuid_of_room, Config};

pub mod api;
pub mod appearance;
pub mod dashboard;
pub mod hub;
pub mod lab;
pub mod notify;
pub mod ntfy;
pub mod pair;
pub mod people;
pub mod tunnel;

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
    /// Whether `HIVE_PROMETHEUS_URL` was set: with no `dashboard.toml`, only
    /// then is the default (lab) dashboard generated.
    pub prometheus_configured: bool,
    /// `dashboard.toml` (`HIVE_DASHBOARD_FILE`, default in the data dir).
    pub dashboard_file: PathBuf,
    /// `@mention` added to a phone message that names no agent.
    pub default_agent: Option<String>,
    /// The member this box belongs to (`HIVE_SERVE_OWNER`: an actor id or a
    /// display name). Set, the box registers its host as theirs, so desktops
    /// treat it as one of that person's own devices: it doesn't count as a
    /// second human, and the owner's desktop answers `@hive` in chats the phone
    /// creates. Honored once the box is an Admin (`hive_core::DeviceLinks`).
    pub owner: Option<String>,
    /// Apply an approved file-diff proposal to this box's bound checkout.
    pub apply_on_approve: bool,
    /// Relay sync cadence.
    pub sync_every: Duration,
    /// Remote access through a relay tunnel (`None` = off, the default).
    pub remote: Option<tunnel::RelaySettings>,
    /// Where `GET /v1/appearance` finds the host's theme (`appearance.rs`).
    pub appearance: appearance::AppearanceConfig,
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
            prometheus_configured: env_opt("HIVE_PROMETHEUS_URL").is_some(),
            dashboard_file: dashboard::config_path(&cfg.data_dir),
            default_agent: env_opt("HIVE_SERVE_DEFAULT_AGENT").map(|a| a.trim().trim_start_matches('@').to_string()),
            owner: env_opt("HIVE_SERVE_OWNER").map(|o| o.trim().trim_start_matches('@').to_string()),
            apply_on_approve: env_flag("HIVE_SERVE_APPLY_ON_APPROVE", false),
            sync_every: Duration::from_secs(3),
            remote: None,
            appearance: appearance::AppearanceConfig::from_env(),
        })
    }

    /// The base URL the phone uses.
    pub fn server_url(&self) -> String {
        self.public_url.clone().unwrap_or_else(|| pair::default_server_url(self.listen))
    }
}

/// Resolve `HIVE_SERVE_OWNER` against the workspace roster: a human member other
/// than this box, by actor id or (unique, case-insensitive) display name.
/// Returns their actor id and whether this box is already an Admin or Owner.
fn resolve_owner(svc: &ChatService, ws: Uuid, wanted: &str) -> Result<(String, bool)> {
    let config = svc
        .load(workspace_config_session_id(ws))?
        .ok_or_else(|| anyhow::anyhow!("the workspace roster hasn't synced yet"))?;
    let me = svc.author().id.clone();
    let humans: Vec<_> = config
        .members
        .iter()
        .filter(|m| m.actor.kind == hive_core::ActorKind::Human && m.actor.id != me)
        .collect();
    let by_id: Vec<_> = humans.iter().filter(|m| m.actor.id == wanted).collect();
    let found: Vec<_> = if by_id.is_empty() {
        humans.iter().filter(|m| m.actor.display_name.eq_ignore_ascii_case(wanted)).collect()
    } else {
        by_id
    };
    let owner = match found.as_slice() {
        [one] => one.actor.id.clone(),
        [] => bail!("no member named '{wanted}'"),
        _ => bail!("more than one member is named '{wanted}' (use their actor id)"),
    };
    let admin = config
        .members
        .iter()
        .find(|m| m.actor.id == me)
        .is_some_and(|m| m.role.rank() >= hive_core::WorkspaceRole::Admin.rank());
    Ok((owner, admin))
}

/// Pre-enrollment start: this box acts as its own identity. Without
/// `HIVE_SERVE_OWNER`, it joins the roster as its own member (the original
/// model). With it, it asks to be enrolled as the owner's device (the code the
/// owner approves on their desktop) and keeps today's behavior meanwhile: an
/// existing member registers its host as the owner's (the dispatch stopgap); a
/// box that isn't a member doesn't join, it waits for approval.
fn register_own(svc: &mut ChatService, ws: Uuid, host_id: &str, opts: &ServeOptions) -> Result<()> {
    let config = svc.load(workspace_config_session_id(ws))?;
    let me = svc.author().clone();
    let is_member = config.as_ref().is_some_and(|c| c.members.iter().any(|m| m.actor.id == me.id));
    let Some(wanted) = &opts.owner else {
        // Announce this box in the workspace roster and publish its identity so
        // peers verify its events, then register it as an always-on host.
        if let Err(e) = svc.ensure_self_member(workspace_config_session_id(ws), ws) {
            eprintln!("serve: joining the workspace roster failed: {e}");
        }
        let host = WorkspaceHost::new(host_id.to_string(), HostKind::Worker, opts.label.clone());
        return svc.register_host(ws, host).context("registering this box as a workspace host");
    };
    print_enrollment(svc, ws, &opts.label, Some(wanted));
    if !is_member {
        println!("hive serve: waiting for approval; the phone can read the workspace but not write until then.");
        svc.set_write_block(Some(PENDING.into()));
        return Ok(());
    }
    if let Err(e) = svc.ensure_self_member(workspace_config_session_id(ws), ws) {
        eprintln!("serve: refreshing this box's identity failed: {e}");
    }
    let mut host = WorkspaceHost::new(host_id.to_string(), HostKind::Worker, opts.label.clone());
    match resolve_owner(svc, ws, wanted) {
        Ok((owner, admin)) => {
            if !admin {
                eprintln!(
                    "serve: HIVE_SERVE_OWNER: until you approve this box as your device, desktops treat it as {wanted}'s \
                     only once it is an Admin"
                );
            }
            host.owner_actor_id = owner;
        }
        Err(e) => eprintln!("serve: HIVE_SERVE_OWNER ignored: {e}"),
    }
    svc.register_host(ws, host).context("registering this box as a workspace host")
}

/// Print the enrollment code the owner approves on their desktop.
pub fn print_enrollment(svc: &ChatService, ws: Uuid, label: &str, owner: Option<&str>) {
    let request = svc.enrollment_request(ws, label);
    let whose = owner.map(|o| format!("{o}'s")).unwrap_or_else(|| "your".into());
    println!(
        "\nTo make this box {whose} own device (everything the phone does is then authored as its owner),\n\
         approve it on {whose} desktop: Settings > Account > My devices > Add a device, and paste:\n\n  {}\n\n\
         Check the desktop shows this fingerprint: {}  (label \"{}\")\n",
        request.encode(),
        request.fingerprint(),
        request.label
    );
}

/// `hive serve enroll [--new-device]`: print this box's enrollment code. With
/// `--new-device` (needed after a revocation: a revoked device id stays
/// revoked), first replace the box's device key with a fresh one.
pub fn cmd_enroll(cfg: &Config, opts: &ServeOptions, new_device: bool) -> Result<()> {
    let ws = uuid_of_room(&cfg.room);
    let svc = open_service(cfg)?;
    let standing = svc.device_standing(ws, opts.owner.as_deref())?;
    let svc = match (&standing, new_device) {
        (Standing::Revoked, false) => bail!(
            "this box's device was revoked; run `hive serve enroll --new-device` to make a new device key, then approve its code"
        ),
        (_, true) => {
            drop(svc);
            let ids = hive_runtime::IdentityStore::new(&cfg.data_dir, hive_runtime::FileKeyVault::new(&cfg.data_dir));
            let rotated = ids.rotate_device()?;
            println!(
                "This box has a new device key ({}). Restart hive serve so it uses it (e.g. systemctl --user restart hive-serve).",
                rotated.device.id
            );
            open_service(cfg)?
        }
        (Standing::OwnerDevice(o), false) => {
            println!("This box is already enrolled as {}'s device.", o.display_name);
            return Ok(());
        }
        (Standing::Own, false) => svc,
    };
    print_enrollment(&svc, ws, &opts.label, opts.owner.as_deref());
    Ok(())
}

/// State shared by the API handlers, the sync loop and the worker thread.
pub struct Shared {
    pub cfg: Config,
    pub opts: ServeOptions,
    pub ws: Uuid,
    token: RwLock<String>,
    pub host_id: String,
    /// The relay tunnel, when remote access is enabled.
    pub tunnel: Option<Arc<tunnel::TunnelHost>>,
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
    /// Panel results, each kept for its `refresh`.
    pub dashboard_cache: dashboard::Cache,
    rt: tokio::runtime::Handle,
    /// "Send to agent to revise" requests awaiting the agent's new version,
    /// by the proposal they revise (`api::request_changes`). In memory: a
    /// restart before the revision lands leaves it unlinked.
    pub revisions: Mutex<HashMap<Uuid, api::PendingRevision>>,
    /// Whose device this box is (see [`Standing`]): its own identity, its
    /// owner's enrolled device, or revoked. Re-read after every pull.
    standing: Mutex<Standing>,
}

/// Why a revoked box refuses writes.
const REVOKED: &str = "this box's device was revoked by its owner; run `hive serve enroll --new-device` on it and approve the new code on your desktop";

/// Why a box waiting for enrollment (and not a member) refuses writes.
const PENDING: &str = "this box is waiting for its owner to approve it as their device (the code is in hive serve's log, or run `hive serve enroll`)";

/// The device-local serve state file (read marks).
fn state_path(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("serve-state.json")
}

impl Shared {
    pub fn lock(&self) -> MutexGuard<'_, ChatService> {
        self.svc.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current direct-path bearer token (`hive phones revoke` rotates it).
    pub fn token(&self) -> String {
        self.token.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Re-read the token file; a rotated token takes effect at once.
    fn reload_token(&self) {
        let Ok(t) = std::fs::read_to_string(&self.opts.token_file) else { return };
        let t = t.trim();
        if t.len() >= 32 && *self.token.read().unwrap_or_else(|e| e.into_inner()) != t {
            *self.token.write().unwrap_or_else(|e| e.into_inner()) = t.to_string();
            println!("hive serve: the bearer token changed on disk; using the new one.");
        }
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

    /// Whose device this box currently is.
    pub fn standing(&self) -> Standing {
        self.standing.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// `identity` for `GET /v1/health`: who the phone posts as.
    ///
    /// `mode`: `owner` (enrolled as the owner's device: the phone acts as the
    /// owner), `member` (its own workspace member, the pre-enrollment model),
    /// `pending` (`HIVE_SERVE_OWNER` set, waiting for the owner's approval) or
    /// `revoked` (read-only until re-enrolled).
    pub fn identity_json(&self) -> serde_json::Value {
        let (actor, fingerprint) = {
            let svc = self.lock();
            (svc.author().clone(), hive_runtime::device_enroll::fingerprint(&svc.device_public_key()))
        };
        let mode = match self.standing() {
            Standing::OwnerDevice(_) => "owner",
            Standing::Revoked => "revoked",
            Standing::Own if self.opts.owner.is_some() => "pending",
            Standing::Own => "member",
        };
        json!({ "mode": mode, "actorId": actor.id, "displayName": actor.display_name, "deviceFingerprint": fingerprint })
    }

    /// Re-read this box's standing after a pull, switching identity when the
    /// owner approves it (or revokes it) while it runs. Returns whether it changed.
    pub fn refresh_standing(&self) -> bool {
        let next = match self.lock().device_standing(self.ws, self.opts.owner.as_deref()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("serve: reading this box's device standing failed: {e}");
                return false;
            }
        };
        let prev = self.standing();
        if next == prev {
            return false;
        }
        {
            let mut svc = self.lock();
            match (&prev, &next) {
                (_, Standing::Revoked) => {
                    svc.set_write_block(Some(REVOKED.into()));
                    eprintln!("hive serve: {REVOKED}.");
                }
                (_, Standing::OwnerDevice(owner)) => {
                    let first = !matches!(prev, Standing::OwnerDevice(_));
                    svc.act_as_owner_device(owner.clone());
                    svc.set_write_block(None);
                    if first {
                        println!("hive serve: approved as {}'s device; the phone now acts as {0}.", owner.display_name);
                        let host = WorkspaceHost::new(self.host_id.clone(), HostKind::Worker, self.opts.label.clone());
                        if let Err(e) = svc.register_host(self.ws, host) {
                            eprintln!("serve: re-registering this host as the owner's failed: {e}");
                        }
                    }
                }
                (Standing::OwnerDevice(_), Standing::Own) => {
                    // The owner's certificate no longer counts (the owner left the
                    // workspace): don't keep writing as them.
                    svc.set_write_block(Some("this box is no longer enrolled as its owner's device; restart hive serve".into()));
                    eprintln!("hive serve: no longer enrolled as the owner's device; writes are paused until a restart.");
                }
                _ => {}
            }
        }
        *self.standing.lock().unwrap_or_else(|e| e.into_inner()) = next;
        true
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
    /// (SSE events, plus ntfy and relay pushes for what [`notify`] picks).
    ///
    /// The sync loop, the worker thread and API handlers all call this. The
    /// projection is loaded while holding the differ lock, so each diff runs on
    /// a snapshot at least as new as the one before it; loading first let a
    /// slow caller diff a stale snapshot after a newer one, rolling the differ
    /// back and re-emitting `message.done` (and its ntfy push).
    pub fn refresh(&self) {
        let (author, links) = {
            let svc = self.lock();
            (svc.author().clone(), svc.device_links(self.ws).unwrap_or_default())
        };
        let me = author.id.clone();
        let mut notices = Vec::new();
        let mut created = Vec::new();
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
                        // Agent replies notify when done; other members'
                        // messages when they appear (only if they mention you).
                        if self.fresh(m.created_at) {
                            let me = notify::Me::in_chat(&author, s, &links);
                            notices.extend(notify::for_message(s, m, done, &me));
                        }
                    }
                    Change::ProposalCreated { chat, proposal } => {
                        let Some(s) = sessions.iter().find(|s| s.id == chat) else { continue };
                        let Some(p) = s.proposals.iter().find(|p| p.id == proposal) else { continue };
                        created.push((chat, proposal));
                        self.hub.emit("proposal.created", hub::wrap("proposal", api::proposal_dto_as(s, p, &me)));
                        if self.fresh(p.created_at) {
                            notices.extend(notify::for_proposal(s, p));
                        }
                    }
                    Change::WorkflowApproval { chat, run, node } => {
                        let Some(s) = sessions.iter().find(|s| s.id == chat) else { continue };
                        let Some(r) = s.workflow_runs.iter().find(|r| r.id == run) else { continue };
                        if self.fresh(r.updated_at) {
                            notices.extend(notify::for_workflow_gate(s, r, &node));
                        }
                    }
                    Change::ProposalUpdated { chat, proposal } => {
                        let Some(s) = sessions.iter().find(|s| s.id == chat) else { continue };
                        let Some(p) = s.proposals.iter().find(|p| p.id == proposal) else { continue };
                        self.hub.emit("proposal.updated", hub::wrap("proposal", api::proposal_dto_as(s, p, &me)));
                    }
                    Change::ChatUpdated { chat } => {
                        let Some(s) = sessions.iter().find(|s| s.id == chat) else { continue };
                        let unread = self.unread(s, &me);
                        self.hub.emit("chat.updated", hub::wrap("chat", api::chat_dto(s, unread)));
                    }
                }
            }
        }
        // A revision the agent just filed: link it to the version it revises
        // (a write of its own), then publish that change too.
        if self.link_revisions(&created) {
            self.sync_kick.notify_one();
            self.refresh();
        }
        if let Some(t) = &self.tunnel {
            for (_, push) in &notices {
                t.push_all(push);
            }
        }
        if let Some(n) = self.opts.ntfy.clone() {
            for (notice, _) in notices {
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
    let host_id = svc.device_id().to_string();
    // Whose device is this box? Enrolled as its owner's (a certificate from the
    // owner's account key is in the workspace), its own member, or revoked.
    let standing = svc.device_standing(ws, opts.owner.as_deref()).unwrap_or(Standing::Own);
    match &standing {
        Standing::OwnerDevice(owner) => {
            svc.act_as_owner_device(owner.clone());
            println!("hive serve: enrolled as {}'s device; the phone acts as {0}.", owner.display_name);
        }
        Standing::Revoked => {
            svc.set_write_block(Some(REVOKED.into()));
            eprintln!("hive serve: {REVOKED}.");
        }
        Standing::Own => {
            if let Some(name) = &opts.name {
                svc.set_author_display_name(name.clone());
            }
        }
    }
    // A turn cut off by the last stop would otherwise stay "streaming" forever
    // and never be answered (its placeholder counts as the reply).
    if standing != Standing::Revoked {
        match worker::recover_interrupted_turns(&mut svc) {
            Ok(done) if !done.is_empty() => eprintln!("serve: finished {} reply(s) interrupted by the last stop", done.len()),
            Ok(_) => {}
            Err(e) => eprintln!("serve: interrupted-turn recovery failed: {e}"),
        }
    }
    match &standing {
        Standing::OwnerDevice(_) => {
            // The owner is already a member; this box only (re)registers its host.
            let host = WorkspaceHost::new(host_id.clone(), HostKind::Worker, opts.label.clone());
            svc.register_host(ws, host).context("registering this box as a workspace host")?;
        }
        Standing::Revoked => {}
        Standing::Own => register_own(&mut svc, ws, &host_id, &opts)?,
    }
    let tunnel = match &opts.remote {
        Some(settings) => Some(tunnel::TunnelHost::open(&cfg.data_dir, settings.clone()).context("opening the relay tunnel state")?),
        None => None,
    };
    let shared = Arc::new(Shared {
        ws,
        token: RwLock::new(token),
        host_id,
        tunnel,
        svc: Mutex::new(svc),
        hub: hub::Hub::new(),
        differ: Mutex::new(hub::Differ::default()),
        read_marks: Mutex::new(load_read_marks(&cfg)),
        sync_kick: Notify::new(),
        worker_kick: Notify::new(),
        stopping: AtomicBool::new(false),
        started_at: Timestamp::now(),
        http: reqwest::Client::builder().connect_timeout(Duration::from_secs(3)).build()?,
        dashboard_cache: dashboard::Cache::default(),
        rt: tokio::runtime::Handle::current(),
        revisions: Mutex::new(HashMap::new()),
        standing: Mutex::new(standing),
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
    tokio::spawn(token_watch(shared.clone(), rx.clone()));
    if let Some(t) = shared.tunnel.clone() {
        tokio::spawn(tunnel::pending_loop(t.clone(), rx.clone()));
        tokio::spawn(tunnel::control::run(t, api::router(shared.clone()), rx.clone(), Default::default()));
    }
    let worker = spawn_worker(shared.clone())?;
    shared.sync_kick.notify_one();
    Ok(ServeHandle { addr, shared, shutdown, worker: Mutex::new(Some(worker)) })
}

/// Pick up a token rotated by `hive phones revoke` (§13.2 step 4).
async fn token_watch(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(tunnel::PENDING_POLL) => shared.reload_token(),
            _ = stop.wait_for(|s| *s) => return,
        }
    }
}

/// Relay sync: every few seconds, and immediately after a local write.
async fn sync_loop(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut fails: u32 = 0;
    while !shared.stopping() {
        let wait = match sync_once(&shared.cfg).await {
            Ok((_, pulled)) => {
                fails = 0;
                if pulled > 0 {
                    shared.refresh_standing();
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

/// Print the pairing QR code and link. With remote access enabled the link is
/// v2 (§4): it also opens a one-time enrollment window (10 min) that a
/// running `hive serve` announces to the relay. `direct` = `None` prints a
/// relay-only link (no `url`/`token`).
pub fn print_pairing(data_dir: &std::path::Path, direct: Option<(&str, &str)>, remote: Option<&tunnel::RelaySettings>) -> Result<()> {
    let link = match remote {
        Some(r) => {
            r.validate()?;
            let dir = tunnel::tunnel_dir(data_dir);
            let Some(keys) = tunnel::HostKeys::load(&dir)? else {
                bail!(
                    "remote access is enabled but there are no host keys in {}: run `hive remote enable` or start `hive serve` first",
                    dir.display()
                );
            };
            let (secret, exp) = tunnel::create_window(&dir, tunnel::PAIR_WINDOW_SECS)?;
            tunnel::pairing_link_v2(direct, &keys, &r.relay, &secret.ps, exp)
        }
        None => {
            let Some((url, token)) = direct else { bail!("nothing to pair: no direct URL and remote access is off") };
            pair::pairing_url(url, token)
        }
    };
    println!("\nScan with the iPhone camera (or the Hive app) to pair:\n");
    print!("{}", pair::render_qr(&link)?);
    println!("\n{link}\n");
    if direct.is_some() {
        println!("This link carries the API token: share it only with your own phone.");
    }
    if remote.is_some() {
        println!("The remote pairing secret in it works once, for 10 minutes, while `hive serve` is running.");
    }
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
        if let Some(t) = &handle.shared.tunnel {
            println!("hive serve: remote access through {} (host id {}).", t.settings.relay, t.host_id());
        }
        if pair_now {
            let token = handle.shared.token();
            let direct = (!pair::is_loopback_url(&server_url) || handle.shared.tunnel.is_none()).then_some((server_url.as_str(), token.as_str()));
            print_pairing(&handle.shared.cfg.data_dir, direct, handle.shared.opts.remote.as_ref())?;
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
pub fn cmd_pair(data_dir: &std::path::Path, opts: &ServeOptions, relay_only: bool) -> Result<()> {
    let url = opts.server_url();
    if let Some(remote) = &opts.remote {
        // A loopback direct URL is useless on the phone: the link goes relay-only.
        if relay_only || pair::is_loopback_url(&url) {
            if !relay_only {
                println!("(The direct URL {url} is loopback; printing a relay-only link.)");
            }
            return print_pairing(data_dir, None, Some(remote));
        }
        let token = pair::load_existing_token(&opts.token_file)?;
        return print_pairing(data_dir, Some((&url, &token)), Some(remote));
    }
    if relay_only {
        bail!("--relay-only needs remote access: `hive remote enable --relay <url>` first");
    }
    if pair::is_loopback_url(&url) {
        anyhow::bail!(
            "the pairing link would point the phone at {url}, which on the phone is the phone itself. \
             Run with the server's settings (HIVE_SERVE_LISTEN / HIVE_DATA_DIR, e.g. from its env file), \
             or pass --listen <tailnet-ip:port> or --public-url <url>"
        );
    }
    let token = pair::load_existing_token(&opts.token_file)?;
    print_pairing(data_dir, Some((&url, &token)), None)
}
