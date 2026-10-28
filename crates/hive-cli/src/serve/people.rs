//! Who and where, for the phone: people (the workspace roster with avatars),
//! channels, and one chat's details — the desktop's right-hand panel (People,
//! Tools → answering runtime + agents, Context, Workflows, Skills/Vaults),
//! read-only. Contract: `docs/ios-app.md`.
//!
//! Avatars ride the synced roster as bounded `data:image/…` URLs (members on
//! `MemberAdded`, agents on `AgentRosterUpdated`); they are sent only on
//! `/v1/people`, `/v1/agents` and chat details, never per chat row, so a chat
//! list stays small. A chat row names its recent speakers by id instead.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use hive_core::context_budget::{model_context_window, token_estimator};
use hive_core::{
    ChatSession, HostKind, MessageRole, Timestamp, WorkspaceAgent, WorkspaceHost, WorkspaceMember,
    WorkspaceRuntime,
};
use serde_json::{json, Value};

use super::api::{agent_dto, hosts_and_agents, load_chat, ApiResult, ONLINE_WINDOW_SECS};
use super::Shared;

type S = State<Arc<Shared>>;

/// How many workflow runs chat details lists (newest first).
const RECENT_RUNS: usize = 10;

fn camel<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn clip(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>())
    }
}

fn non_empty(s: Option<&String>) -> Option<&String> {
    s.filter(|v| !v.trim().is_empty())
}

// ── Agents: avatar + where it runs ─────────────────────────────────────────

/// Where an agent runs, as the desktop's host badge says it: `ownerDevice`
/// (no host: its owner's own machine), `thisHost` (this `hive serve` box),
/// `worker` or `device` (another registered host).
fn host_kind(s: &Shared, a: &WorkspaceAgent, hosts: &[WorkspaceHost]) -> (&'static str, Option<bool>) {
    if a.host_id.is_empty() {
        return ("ownerDevice", None);
    }
    if a.host_id == s.host_id {
        return ("thisHost", Some(true));
    }
    let now = Timestamp::now();
    match hosts.iter().find(|h| h.id == a.host_id) {
        Some(h) => (
            if h.kind == HostKind::Worker { "worker" } else { "device" },
            Some(h.is_online(&now, ONLINE_WINDOW_SECS)),
        ),
        None => ("worker", Some(false)),
    }
}

/// Adds the avatar, role and host kind to an agent DTO (`GET /v1/agents`).
pub fn decorate_agent(v: &mut Value, s: &Shared, a: &WorkspaceAgent, hosts: &[WorkspaceHost]) {
    if !a.role.trim().is_empty() {
        v["role"] = json!(a.role);
    }
    if let Some(u) = non_empty(a.avatar_url.as_ref()) {
        v["avatarUrl"] = json!(u);
    }
    if let Some(c) = non_empty(a.avatar_color_hex.as_ref()) {
        v["avatarColorHex"] = json!(c);
    }
    let (kind, online) = host_kind(s, a, hosts);
    v["hostKind"] = json!(kind);
    if let Some(o) = online {
        v["hostOnline"] = json!(o);
    }
}

// ── People ─────────────────────────────────────────────────────────────────

fn person_dto(m: &WorkspaceMember, me: &str) -> Value {
    let mut v = json!({
        "actorId": m.actor.id,
        "displayName": m.actor.display_name,
        "kind": camel(&m.actor.kind),
        "role": camel(&m.role),
        "isSelf": m.actor.id == me,
    });
    if !m.title.trim().is_empty() {
        v["title"] = json!(m.title);
    }
    if let Some(u) = non_empty(m.actor.avatar_url.as_ref()) {
        v["avatarUrl"] = json!(u);
    }
    v
}

/// The workspace roster, people only (agents are `GET /v1/agents`).
fn roster(config: &ChatSession) -> impl Iterator<Item = &WorkspaceMember> {
    config.members.iter().filter(|m| m.actor.kind == hive_core::ActorKind::Human)
}

/// `GET /v1/people` → `{people: [...]}`.
pub async fn list_people(State(s): S) -> ApiResult<Json<Value>> {
    let (_, config) = hosts_and_agents(&s)?;
    let me = s.my_actor_id();
    let list: Vec<Value> = roster(&config).map(|m| person_dto(m, &me)).collect();
    Ok(Json(json!({ "people": list })))
}

// ── Channels ───────────────────────────────────────────────────────────────

fn channel_dto(c: &hive_core::Channel) -> Value {
    let mut v = json!({
        "id": c.id,
        "name": c.name,
        "position": c.position,
        "archived": c.archived,
        "defaultChatId": c.default_chat_id,
    });
    if let Some(p) = non_empty(c.purpose.as_ref()) {
        v["purpose"] = json!(p);
    }
    v
}

/// `GET /v1/channels` → `{channels: [...]}`, in sidebar order.
pub async fn list_channels(State(s): S) -> ApiResult<Json<Value>> {
    let channels = s.lock().list_channels(s.ws)?;
    Ok(Json(json!({ "channels": channels.iter().map(channel_dto).collect::<Vec<_>>() })))
}

// ── Chat participants (chat rows) ──────────────────────────────────────────

/// Who spoke in the chat, newest first, at most `cap`: `{kind, id, name}`
/// where kind is `person` (id = actor id), `agent` (id = agent id) or
/// `primary` (the chat's own `@hive`, id `hive`).
pub fn participants(s: &ChatSession, cap: usize) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in s.messages.iter().rev() {
        if out.len() >= cap {
            break;
        }
        let (kind, id, name) = match m.role {
            MessageRole::System => continue,
            MessageRole::User => match &m.actor_identity {
                Some(a) => ("person", a.id.clone(), a.display_name.clone()),
                None => ("person", m.author.clone(), m.author.clone()),
            },
            _ => match m.responder_id {
                Some(id) => {
                    let name = s.workspace_agents.iter().find(|a| a.id == id).map(|a| a.name.clone());
                    ("agent", id.to_string(), name.unwrap_or_else(|| m.author.clone()))
                }
                None if m.role == MessageRole::Assistant => ("primary", "hive".to_string(), "hive".to_string()),
                None => ("agent", m.author.clone(), m.author.clone()),
            },
        };
        if id.is_empty() || !seen.insert((kind, id.clone())) {
            continue;
        }
        out.push(json!({ "kind": kind, "id": id, "name": name }));
    }
    out
}

// ── Chat details ───────────────────────────────────────────────────────────

/// What runs the chat's primary (`@hive`) turns: a workspace runtime, else
/// this box's own (`worker::local_runtimes`).
fn primary_runtime<'a>(chat: &ChatSession, known: &'a [WorkspaceRuntime]) -> Option<&'a WorkspaceRuntime> {
    known.iter().find(|r| r.id == chat.runtime_id)
}

fn default_responder(s: &Shared, chat: &ChatSession, agents: &[Value], known: &[WorkspaceRuntime]) -> Value {
    // `hive serve --default-agent` routes an un-addressed phone message to that
    // agent; otherwise the chat's `@hive` answers it, as on the desktop.
    if let Some(name) = &s.opts.default_agent {
        if let Some(a) = agents.iter().find(|a| a["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name))) {
            let mut v = json!({ "kind": "agent", "name": a["name"], "agentId": a["id"], "via": "serveDefault" });
            for k in ["runtimeLabel", "model", "hostLabel", "hostKind"] {
                if !a[k].is_null() {
                    v[k] = a[k].clone();
                }
            }
            return v;
        }
    }
    let rt = primary_runtime(chat, known);
    let mut v = json!({
        "kind": "primary",
        "name": "hive",
        "via": "untagged",
        "runtimeLabel": rt.map(|r| r.name.clone()).unwrap_or_else(|| {
            if chat.runtime_id.trim().is_empty() { "Default runtime".into() } else { chat.runtime_id.clone() }
        }),
    });
    if let Some(r) = rt.filter(|r| !r.model.trim().is_empty()) {
        v["model"] = json!(r.model);
    }
    v
}

/// A coarse context-budget estimate (the desktop's Context pane plans the real
/// prompt; this box only knows the transcript, skills and the model name).
fn context_estimate(chat: &ChatSession, model: Option<&str>) -> Value {
    let history = token_estimator::estimate_messages(&chat.messages) as u64;
    let skills: u64 = chat.loaded_skills.iter().map(|k| token_estimator::estimate_text(&k.instructions) as u64).sum();
    let window = model
        .filter(|m| !m.trim().is_empty())
        .map(model_context_window::tokens_for_model)
        .unwrap_or(model_context_window::FALLBACK) as u64;
    let used = history + skills;
    let mut v = json!({
        "estimated": true,
        "windowTokens": window,
        "usedTokens": used,
        "historyTokens": history,
        "skillTokens": skills,
        "messageCount": chat.messages.len(),
        // Over budget the oldest turns are summarized, as on the desktop.
        "compacting": used > window,
    });
    if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
        v["model"] = json!(m);
    }
    v
}

fn workflow_run_dto(chat: &ChatSession, r: &hive_core::WorkflowRun) -> Value {
    use hive_core::workflow::NodeRunStatus;
    let done = r.nodes.iter().filter(|n| n.status == NodeRunStatus::Succeeded || n.status == NodeRunStatus::Skipped).count();
    let initiator = chat
        .members
        .iter()
        .find(|m| m.actor.id == r.initiator_actor_id)
        .map(|m| m.actor.display_name.clone())
        .unwrap_or_default();
    let mut v = json!({
        "id": r.id,
        "name": r.definition.name,
        "status": camel(&r.status),
        "startedAt": super::api::iso(r.started_at),
        "updatedAt": super::api::iso(r.updated_at),
        "stepsDone": done,
        "stepsTotal": r.definition.nodes.len().max(r.nodes.len()),
    });
    if !initiator.is_empty() {
        v["initiator"] = json!(initiator);
    }
    v
}

/// `GET /v1/chats/:id/details`: the chat's right-hand panel, read-only.
pub async fn chat_details(State(s): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let chat = load_chat(&s, &id)?;
    let (hosts, config) = hosts_and_agents(&s)?;
    let me = s.my_actor_id();
    let unread = s.unread(&chat, &me);

    let spoke: HashSet<String> =
        participants(&chat, usize::MAX).iter().filter_map(|p| p["id"].as_str().map(str::to_string)).collect();

    let agents: Vec<Value> = config
        .workspace_agents
        .iter()
        .map(|a| {
            let mut v = agent_dto(&s, a, &config, &hosts);
            v["inChat"] = json!(spoke.contains(&a.id.to_string()) || chat.participant_agent_ids.contains(&a.id));
            v
        })
        .collect();
    let people: Vec<Value> = roster(&config)
        .map(|m| {
            let mut v = person_dto(m, &me);
            v["inChat"] = json!(spoke.contains(&m.actor.id));
            v
        })
        .collect();

    let mut known = config.workspace_runtimes.clone();
    known.extend(crate::worker::local_runtimes(&s.cfg.data_dir));
    let responder = default_responder(&s, &chat, &agents, &known);
    let model = responder["model"].as_str().map(str::to_string);

    let channel = s.lock().list_channels(s.ws)?.into_iter().find(|c| c.id == chat.channel_id);

    let mut runs: Vec<&hive_core::WorkflowRun> = chat.workflow_runs.iter().collect();
    runs.sort_by_key(|r| std::cmp::Reverse(r.started_at));
    let runs: Vec<Value> = runs.into_iter().take(RECENT_RUNS).map(|r| workflow_run_dto(&chat, r)).collect();

    let skills: Vec<Value> = chat
        .loaded_skills
        .iter()
        .map(|k| {
            let summary = k.instructions.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
            let mut v = json!({ "id": k.id, "name": k.name, "summary": clip(summary.trim_start_matches('#').trim(), 140) });
            if let Some(u) = non_empty(k.source_url.as_ref()) {
                v["sourceUrl"] = json!(u);
            }
            if !k.agent_ids.is_empty() {
                v["agentIds"] = json!(k.agent_ids);
            }
            v
        })
        .collect();
    let vaults: Vec<Value> = chat
        .vault_sources
        .iter()
        .map(|vs| {
            let mut v = json!({ "label": vs.label(), "url": vs.raw_url() });
            if !vs.agent_ids.is_empty() {
                v["agentIds"] = json!(vs.agent_ids);
            }
            v
        })
        .collect();

    let mut out = json!({
        "chat": super::api::chat_dto(&chat, unread),
        "people": people,
        "agents": agents,
        "defaultResponder": responder,
        "context": context_estimate(&chat, model.as_deref()),
        "workflowRuns": runs,
        "skills": skills,
        "vaults": vaults,
    });
    if let Some(c) = channel {
        out["channel"] = channel_dto(&c);
    }
    Ok(Json(out))
}
