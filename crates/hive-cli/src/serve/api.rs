//! The phone-facing HTTP API + SSE stream (contract: `docs/ios-app.md`).
//!
//! JSON, camelCase, ISO-8601 UTC timestamps. Every route but `GET /v1/health`
//! needs `Authorization: Bearer <token>`; health answers `{ok, version}` to
//! anyone and adds `workspace` / `deviceLabel` only when authorized.
//!
//! Handlers lock the shared chat service only inside synchronous helpers, so a
//! guard never lives across an `.await`.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use futures_util::stream::{self, Stream, StreamExt};
use hive_core::{
    ActionProposal, ChatMessage, ChatSession, MessageRole, ProposalKind, ProposalStatus, Timestamp,
};
use serde::Deserialize;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::{lab, pair, Shared};

type S = State<Arc<Shared>>;

// ── Errors ─────────────────────────────────────────────────────────────────

pub struct ApiError(StatusCode, String);

impl ApiError {
    fn not_found(what: &str) -> Self {
        Self(StatusCode::NOT_FOUND, format!("{what} not found"))
    }
    fn bad_request(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}

impl From<hive_runtime::chat_service::ChatError> for ApiError {
    fn from(e: hive_runtime::chat_service::ChatError) -> Self {
        use hive_runtime::chat_service::ChatError;
        match e {
            ChatError::Unauthorized(m) => Self(StatusCode::FORBIDDEN, format!("unauthorized: {m}")),
            ChatError::NotFound(m) => Self(StatusCode::NOT_FOUND, m),
            other => Self(StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

// ── DTOs ───────────────────────────────────────────────────────────────────

/// RFC 3339 / ISO-8601 in UTC (`…Z`).
pub fn iso(ts: Timestamp) -> String {
    ts.inner().to_offset(time::UtcOffset::UTC).format(&Rfc3339).unwrap_or_default()
}

fn role_str(m: &ChatMessage) -> &'static str {
    match m.role {
        MessageRole::User => "user",
        MessageRole::System => "system",
        MessageRole::Agent => "agent",
        // A named agent's reply carries its stable id; the chat's primary
        // runtime ("hive") has none.
        MessageRole::Assistant if m.responder_id.is_some() => "agent",
        MessageRole::Assistant => "assistant",
    }
}

pub fn message_dto(chat: Uuid, m: &ChatMessage) -> Value {
    let mut v = json!({
        "id": m.id,
        "chatId": chat,
        "author": m.author,
        "role": role_str(m),
        "body": m.body,
        "createdAt": iso(m.created_at),
        "streaming": m.is_streaming,
    });
    if let Some(a) = m.responder_id {
        v["agentId"] = json!(a);
    }
    v
}

fn truncate(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>())
    }
}

/// When a chat last changed: its own stamp or its newest message.
fn updated_at(s: &ChatSession) -> Timestamp {
    s.messages.iter().map(|m| m.created_at).chain([s.updated_at, s.created_at]).max().unwrap_or(s.updated_at)
}

pub fn chat_dto(s: &ChatSession, unread: usize) -> Value {
    let preview = s
        .messages
        .iter()
        .rev()
        .find(|m| m.role != MessageRole::System && !m.body.trim().is_empty())
        .map(|m| truncate(&m.body.replace('\n', " "), 140))
        .unwrap_or_default();
    json!({
        "id": s.id,
        "title": if s.title.trim().is_empty() { "Untitled" } else { s.title.as_str() },
        "updatedAt": iso(updated_at(s)),
        "preview": preview,
        "unread": unread,
    })
}

fn kind_str(k: ProposalKind) -> &'static str {
    match k {
        ProposalKind::FileDiff => "fileDiff",
        ProposalKind::Command => "command",
        ProposalKind::Decision => "decision",
    }
}

fn status_str(s: ProposalStatus) -> &'static str {
    match s {
        ProposalStatus::Open => "open",
        ProposalStatus::Approved => "approved",
        ProposalStatus::Rejected => "rejected",
        ProposalStatus::Applied => "applied",
    }
}

/// Who a proposal is from: its author's roster name, else the agent whose
/// turn produced it, else empty.
fn proposal_author(s: &ChatSession, p: &ActionProposal) -> String {
    if !p.author_actor_id.is_empty() {
        if let Some(m) = s.members.iter().find(|m| m.id == p.author_actor_id) {
            return m.actor.display_name.clone();
        }
        return p.author_actor_id.clone();
    }
    p.producing_agent_id
        .and_then(|id| s.workspace_agents.iter().find(|a| a.id == id))
        .map(|a| format!("@{}", a.name))
        .unwrap_or_default()
}

pub fn proposal_dto(s: &ChatSession, p: &ActionProposal) -> Value {
    let mut v = json!({
        "id": p.id,
        "chatId": s.id,
        "title": p.title,
        "body": p.body,
        "kind": kind_str(p.kind),
        "status": status_str(p.status),
        "createdAt": iso(p.created_at),
        "author": proposal_author(s, p),
    });
    if let Some(d) = &p.diff {
        v["diff"] = json!(d);
    }
    v
}

// ── Router + auth ──────────────────────────────────────────────────────────

/// Whether the request carried the right bearer token (set by [`auth`]).
#[derive(Clone, Copy)]
struct Authorized(bool);

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

async fn auth(State(s): S, mut req: Request, next: Next) -> Response {
    let ok = bearer(req.headers()).is_some_and(|t| pair::token_matches(&s.token, t));
    if !ok && req.uri().path() != "/v1/health" {
        return ApiError(StatusCode::UNAUTHORIZED, "missing or invalid bearer token".into()).into_response();
    }
    req.extensions_mut().insert(Authorized(ok));
    next.run(req).await
}

pub fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/chats", get(list_chats).post(create_chat))
        .route("/v1/chats/:id/messages", get(list_messages).post(post_message))
        .route("/v1/agents", get(list_agents))
        .route("/v1/agents/:id/host", put(set_agent_host))
        .route("/v1/hosts", get(list_hosts))
        .route("/v1/proposals", get(list_proposals))
        .route("/v1/proposals/:id/approve", post(approve_proposal))
        .route("/v1/proposals/:id/reject", post(reject_proposal))
        .route("/v1/lab/status", get(lab_status))
        .route("/v1/events", get(events))
        .layer(middleware::from_fn_with_state(shared.clone(), auth))
        .with_state(shared)
}

/// Parse an optional JSON body (empty = `{}`).
fn optional_json<T: for<'de> Deserialize<'de> + Default>(body: &Bytes) -> ApiResult<T> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))
}

fn parse_id(raw: &str, what: &str) -> ApiResult<Uuid> {
    Uuid::parse_str(raw.trim()).map_err(|_| ApiError::not_found(what))
}

// ── Handlers ───────────────────────────────────────────────────────────────

async fn health(State(s): S, Extension(Authorized(ok)): Extension<Authorized>) -> Json<Value> {
    let mut v = json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") });
    if ok {
        v["workspace"] = json!(s.cfg.room);
        v["deviceLabel"] = json!(s.opts.label);
    }
    Json(v)
}

async fn list_chats(State(s): S) -> ApiResult<Json<Value>> {
    let me = s.my_actor_id();
    let mut chats = s.workspace_chats()?;
    chats.sort_by_key(|c| std::cmp::Reverse(updated_at(c)));
    let list: Vec<Value> = chats.iter().map(|c| chat_dto(c, s.unread(c, &me))).collect();
    Ok(Json(json!({ "chats": list })))
}

#[derive(Deserialize, Default)]
struct NewChat {
    #[serde(default)]
    title: Option<String>,
}

async fn create_chat(State(s): S, body: Bytes) -> ApiResult<Json<Value>> {
    let req: NewChat = optional_json(&body)?;
    let title = req.title.map(|t| t.trim().to_string()).unwrap_or_default();
    let chat = {
        let mut svc = s.lock();
        let session = svc.create_chat(title, s.ws, "")?;
        // File it under the workspace's first channel (#general), as the
        // desktop files a new chat; a channel-less workspace leaves it unfiled.
        if let Some(ch) = svc.list_channels(s.ws)?.into_iter().find(|c| !c.archived) {
            svc.set_chat_channel(session.id, s.ws, ch.id)?;
        }
        svc.load(session.id)?.unwrap_or(session)
    };
    s.after_write();
    Ok(Json(json!({ "chat": chat_dto(&chat, 0) })))
}

fn load_chat(s: &Shared, raw: &str) -> ApiResult<ChatSession> {
    let id = parse_id(raw, "chat")?;
    s.workspace_chats()?.into_iter().find(|c| c.id == id).ok_or_else(|| ApiError::not_found("chat"))
}

#[derive(Deserialize)]
struct Page {
    after: Option<String>,
    limit: Option<usize>,
}

async fn list_messages(State(s): S, Path(id): Path<String>, Query(page): Query<Page>) -> ApiResult<Json<Value>> {
    let chat = load_chat(&s, &id)?;
    let limit = page.limit.unwrap_or(100).clamp(1, 1000);
    let msgs = &chat.messages;
    let slice = match page.after.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        Some(after) => {
            let after = parse_id(after, "message")?;
            let i = msgs.iter().position(|m| m.id == after).ok_or_else(|| ApiError::not_found("message"))?;
            &msgs[i + 1..(i + 1 + limit).min(msgs.len())]
        }
        None => &msgs[msgs.len().saturating_sub(limit)..],
    };
    if let Some(last) = slice.last() {
        s.mark_read(&chat, last.id);
    }
    let list: Vec<Value> = slice.iter().map(|m| message_dto(chat.id, m)).collect();
    Ok(Json(json!({ "messages": list })))
}

#[derive(Deserialize)]
struct NewMessage {
    body: String,
}

/// Whether `body` addresses anyone with an `@mention` (an agent of the chat,
/// or `@hive` / a member — anything the desktop would route).
fn has_mention(body: &str, chat: &ChatSession) -> bool {
    let t = hive_runtime::parse_mentions(body, chat);
    !t.is_empty()
}

async fn post_message(State(s): S, Path(id): Path<String>, Json(req): Json<NewMessage>) -> ApiResult<Json<Value>> {
    let chat = load_chat(&s, &id)?;
    let mut body = req.body.trim().to_string();
    if body.is_empty() {
        return Err(ApiError::bad_request("message body is empty"));
    }
    // Opt-in convenience: route an un-addressed phone message to a default agent.
    if let Some(agent) = &s.opts.default_agent {
        if !has_mention(&body, &chat) && chat.workspace_agents.iter().any(|a| a.name.eq_ignore_ascii_case(agent)) {
            body = format!("@{agent} {body}");
        }
    }
    let message = {
        let mut svc = s.lock();
        svc.post_user_message(chat.id, chat.workspace_id, body)?
    };
    s.mark_read_after_post(chat.id, message.id);
    s.after_write();
    Ok(Json(json!({ "message": message_dto(chat.id, &message) })))
}

impl Shared {
    /// The phone has seen its own message.
    fn mark_read_after_post(&self, chat: Uuid, message: Uuid) {
        let loaded = self.lock().load(chat);
        if let Ok(Some(s)) = loaded {
            self.mark_read(&s, message);
        }
    }
}

/// The registered hosts and the workspace-config log. A workspace whose config
/// log hasn't synced in yet has no agents: an empty log stands in, so
/// `GET /v1/agents` answers `[]` rather than 404 (the phone's pairing check
/// calls it).
fn hosts_and_agents(s: &Shared) -> ApiResult<(Vec<hive_core::WorkspaceHost>, ChatSession)> {
    let svc = s.lock();
    let hosts = svc.list_hosts(s.ws)?;
    let config_id = hive_core::workspace_config_session_id(s.ws);
    let config = svc.load(config_id)?.unwrap_or_else(|| {
        let mut empty = ChatSession::new("", s.ws, "");
        empty.id = config_id;
        empty
    });
    Ok((hosts, config))
}

fn agent_dto(s: &Shared, a: &hive_core::WorkspaceAgent, config: &ChatSession, hosts: &[hive_core::WorkspaceHost]) -> Value {
    // What the agent runs on: a workspace runtime, or — when hosted here —
    // this box's own runtime for it (see `worker::local_runtimes`).
    let mut known = config.workspace_runtimes.clone();
    if a.host_id == s.host_id {
        known.extend(crate::worker::local_runtimes(&s.cfg.data_dir));
    }
    let rt = if a.host_id == s.host_id {
        crate::worker::runtime_for(&known, a)
    } else {
        known.iter().find(|r| r.id == a.runtime_id)
    };
    let mut v = json!({
        "id": a.id,
        "name": a.name,
        "runtimeLabel": rt.map(|r| r.name.clone()).unwrap_or_else(|| a.runtime_id.clone()),
    });
    if let Some(r) = rt.filter(|r| !r.model.trim().is_empty()) {
        v["model"] = json!(r.model);
    }
    if !a.host_id.is_empty() {
        v["hostId"] = json!(a.host_id);
        let label = if a.host_id == s.host_id {
            Some(format!("{} (this host)", s.opts.label))
        } else {
            hosts.iter().find(|h| h.id == a.host_id).map(|h| h.label.clone())
        };
        if let Some(l) = label {
            v["hostLabel"] = json!(l);
        }
    } else {
        // Empty host = the agent owner's own device (the desktop's default).
        v["hostLabel"] = json!("Owner's device");
    }
    v
}

async fn list_agents(State(s): S) -> ApiResult<Json<Value>> {
    let (hosts, config) = hosts_and_agents(&s)?;
    let list: Vec<Value> = config.workspace_agents.iter().map(|a| agent_dto(&s, a, &config, &hosts)).collect();
    Ok(Json(json!({ "agents": list })))
}

/// Presence window, as `hive queue` uses.
const ONLINE_WINDOW_SECS: i64 = 90;

async fn list_hosts(State(s): S) -> ApiResult<Json<Value>> {
    let (hosts, _) = hosts_and_agents(&s)?;
    let now = Timestamp::now();
    let list: Vec<Value> = hosts
        .iter()
        .map(|h| {
            let is_this = h.id == s.host_id;
            json!({
                "id": h.id,
                "label": h.label,
                "isThis": is_this,
                "online": is_this || h.is_online(&now, ONLINE_WINDOW_SECS),
            })
        })
        .collect();
    Ok(Json(json!({ "hosts": list })))
}

#[derive(Deserialize)]
struct HostReq {
    #[serde(rename = "hostId")]
    host_id: String,
}

async fn set_agent_host(State(s): S, Path(id): Path<String>, Json(req): Json<HostReq>) -> ApiResult<Json<Value>> {
    let agent_id = parse_id(&id, "agent")?;
    let host_id = req.host_id.trim().to_string();
    let v = {
        let (hosts, config) = hosts_and_agents(&s)?;
        if !config.workspace_agents.iter().any(|a| a.id == agent_id) {
            return Err(ApiError::not_found("agent"));
        }
        // An empty id hands the agent back to its owner's device; anything
        // else must be a registered host.
        if !host_id.is_empty() && !hosts.iter().any(|h| h.id == host_id) {
            return Err(ApiError::bad_request("unknown host"));
        }
        let mut svc = s.lock();
        svc.set_agent_host(s.ws, agent_id, &host_id)?;
        let config = svc
            .load(hive_core::workspace_config_session_id(s.ws))?
            .ok_or_else(|| ApiError::not_found("workspace config"))?;
        drop(svc);
        let agent = config.workspace_agents.iter().find(|a| a.id == agent_id).ok_or_else(|| ApiError::not_found("agent"))?;
        agent_dto(&s, agent, &config, &hosts)
    };
    s.after_write();
    Ok(Json(json!({ "agent": v })))
}

#[derive(Deserialize)]
struct ProposalFilter {
    status: Option<String>,
}

async fn list_proposals(State(s): S, Query(f): Query<ProposalFilter>) -> ApiResult<Json<Value>> {
    let all = match f.status.as_deref().map(str::trim) {
        None | Some("") | Some("open") => false,
        Some("all") => true,
        Some(other) => return Err(ApiError::bad_request(format!("status must be open or all (got '{other}')"))),
    };
    let mut out: Vec<(Timestamp, Value)> = Vec::new();
    for chat in s.workspace_chats()? {
        for p in &chat.proposals {
            let open = p.status == ProposalStatus::Open && !p.dismissed && p.superseded_by.is_none();
            if all || open {
                out.push((p.created_at, proposal_dto(&chat, p)));
            }
        }
    }
    out.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    Ok(Json(json!({ "proposals": out.into_iter().map(|(_, v)| v).collect::<Vec<_>>() })))
}

/// Find a proposal's chat.
fn find_proposal(s: &Shared, raw: &str) -> ApiResult<(ChatSession, ActionProposal)> {
    let pid = parse_id(raw, "proposal")?;
    for chat in s.workspace_chats()? {
        if let Some(p) = chat.proposals.iter().find(|p| p.id == pid).cloned() {
            return Ok((chat, p));
        }
    }
    Err(ApiError::not_found("proposal"))
}

/// Cast this box's vote — the same `ProposalVoteCast` delta the desktop's
/// Review pane emits, at this identity's real roster role, so the proposal's
/// quorum and role floor apply unchanged.
fn vote(s: &Shared, chat: &ChatSession, pid: Uuid, approved: bool) -> ApiResult<ActionProposal> {
    let mut svc = s.lock();
    let actor = svc.author().id.clone();
    let role = chat.role_of(&actor);
    svc.vote_on_proposal(chat.id, chat.workspace_id, pid, actor, role, approved)?
        .ok_or_else(|| ApiError::not_found("proposal"))
}

async fn approve_proposal(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let (chat, p) = find_proposal(&s, &id)?;
    let mut updated = vote(&s, &chat, p.id, true)?;
    if s.opts.apply_on_approve && updated.is_quorum_met() && updated.status != ProposalStatus::Applied {
        if let Some(applied) = apply_on_this_host(&s, &chat, &updated)? {
            updated = applied;
        }
    }
    s.after_write();
    let chat = load_chat(&s, &chat.id.to_string()).unwrap_or(chat);
    Ok(Json(json!({ "proposal": proposal_dto(&chat, &updated) })))
}

#[derive(Deserialize, Default)]
struct RejectReq {
    #[serde(default)]
    reason: Option<String>,
}

async fn reject_proposal(State(s): S, Path(id): Path<String>, body: Bytes) -> ApiResult<Json<Value>> {
    let req: RejectReq = optional_json(&body)?;
    let (chat, p) = find_proposal(&s, &id)?;
    // A reason rides the proposal's refinement thread as a comment, the way a
    // reviewer's note does on the desktop.
    if let Some(reason) = req.reason.map(|r| r.trim().to_string()).filter(|r| !r.is_empty()) {
        let mut svc = s.lock();
        let actor = svc.author().id.clone();
        let role = chat.role_of(&actor);
        svc.refine_proposal(chat.id, chat.workspace_id, p.id, actor, role, reason, false)?;
    }
    let updated = vote(&s, &chat, p.id, false)?;
    s.after_write();
    Ok(Json(json!({ "proposal": proposal_dto(&chat, &updated) })))
}

/// Opt-in (`HIVE_SERVE_APPLY_ON_APPROVE=1`): carry out an approved file-diff
/// proposal on this box, as the desktop's Implement does — apply it into the
/// chat's base worktree of the bound checkout, commit (advancing
/// `hive/chat/<id>`), and mark it Applied. `Ok(None)` when there is nothing to
/// apply here (not a diff, or no checkout bound).
fn apply_on_this_host(s: &Shared, chat: &ChatSession, p: &ActionProposal) -> ApiResult<Option<ActionProposal>> {
    let (ProposalKind::FileDiff, Some(diff)) = (p.kind, p.diff.as_deref()) else {
        return Ok(None);
    };
    let repos = crate::repos::repos_dir(&s.cfg.data_dir);
    let Some(repo) = crate::repos::RepoBindings::load(&s.cfg.data_dir).resolve(&repos, chat.id) else {
        return Ok(None);
    };
    if !hive_runtime::git_worktree::is_git_repo(&repo) {
        return Ok(None);
    }
    let wt = hive_runtime::git_worktree::chat_worktree(&repo, &chat.id.to_string())
        .map_err(|e| ApiError(StatusCode::CONFLICT, format!("chat worktree unavailable: {e}")))?;
    git_apply(&wt, diff).map_err(|e| ApiError(StatusCode::CONFLICT, e))?;
    git_commit(&wt, &format!("Apply proposal: {}", p.title)).map_err(|e| ApiError(StatusCode::CONFLICT, e))?;
    let mut applied = p.clone();
    applied.status = ProposalStatus::Applied;
    s.lock().upsert_proposal(chat.id, chat.workspace_id, applied.clone())?;
    Ok(Some(applied))
}

fn git_apply(dir: &std::path::Path, diff: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = hive_core::process_util::command("git")
        .arg("-C")
        .arg(dir)
        .args(["apply", "--whitespace=nowarn"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git apply: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("git apply: no stdin")?
        .write_all(diff.as_bytes())
        .map_err(|e| format!("write patch: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("git apply: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "couldn't apply the change — the checkout may have moved since it was proposed ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

fn git_commit(dir: &std::path::Path, message: &str) -> Result<(), String> {
    let run = |args: &[&str]| {
        hive_core::process_util::command("git").arg("-C").arg(dir).args(args).output().map_err(|e| e.to_string())
    };
    let add = run(&["add", "-A"])?;
    if !add.status.success() {
        return Err(format!("git add: {}", String::from_utf8_lossy(&add.stderr).trim()));
    }
    // A headless box may have no git identity configured; give the commit one.
    let out = run(&["-c", "user.name=Hive", "-c", "user.email=hive@localhost", "commit", "-m", message])?;
    if !out.status.success() {
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        if text.contains("nothing to commit") {
            return Ok(());
        }
        return Err(format!("git commit: {}", text.trim()));
    }
    Ok(())
}

async fn lab_status(State(s): S) -> Json<Value> {
    Json(lab::status(&s.http, &s.opts.prometheus_url).await)
}

fn to_event(ev: super::hub::SseEvent) -> Event {
    Event::default().id(ev.id.to_string()).event(ev.name).data(ev.data.to_string())
}

async fn events(State(s): S, headers: HeaderMap) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let last = headers.get("last-event-id").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    let (backlog, rx) = s.hub.subscribe(last);
    // Open with a comment so the client sees body bytes at once: URLSession
    // (the phone) doesn't hand back the response until the first body byte, so
    // without this a quiet stream looked "connecting" until the 15 s heartbeat.
    let hello = stream::once(async { Ok(Event::default().comment("connected")) });
    let backlog = hello.chain(stream::iter(backlog.into_iter().map(|e| Ok(to_event(e)))));
    let live = stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => return Some((Ok(to_event(ev)), rx)),
                // A slow client skipped some events; it keeps the stream and
                // can refetch (best effort, like Last-Event-ID).
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(backlog.chain(live)).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("heartbeat"))
}
