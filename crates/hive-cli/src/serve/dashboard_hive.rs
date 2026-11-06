//! The `hive` dashboard source: named metrics about the workspace this host
//! serves (agents, proposals, workflow runs, turns, this host's own status),
//! and the built-in "Hive" section made of them.
//!
//! A [`HiveSnapshot`] is computed from the event log's folded state once per
//! evaluation (the serve side caches it for a few seconds) and answers every
//! `metric = "…"` lookup. Everything here is counted from what the workspace
//! records; Hive doesn't record token usage, so there is no cost metric.

use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

use anyhow::{bail, Result};
use hive_core::{ChatSession, MessageRole, ProposalStatus, TranscriptEventKind, WorkflowRunStatus, WorkspaceHost};
use serde_json::{json, Value};
use time::{OffsetDateTime, UtcOffset};
use uuid::Uuid;

use super::dashboard::{Kind, Level, PanelConfig, SectionConfig, SourceKind, Threshold};

/// What a metric yields, which decides the panel kinds it fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// stat, gauge, text, or a row of a bar/list/status panel.
    Number,
    /// text.
    Text,
    /// bar, list, status.
    Rows,
    /// table.
    Table,
}

impl Shape {
    pub fn fits(self, kind: Kind) -> bool {
        match self {
            Shape::Number => matches!(kind, Kind::Stat | Kind::Gauge | Kind::Text),
            Shape::Text => kind == Kind::Text,
            Shape::Rows => matches!(kind, Kind::Bar | Kind::List | Kind::Status),
            Shape::Table => kind == Kind::Table,
        }
    }

    pub fn kinds(self) -> &'static str {
        match self {
            Shape::Number => "stat, gauge or text (or a row of a list/bar/status panel)",
            Shape::Text => "text",
            Shape::Rows => "bar, list or status",
            Shape::Table => "table",
        }
    }
}

pub struct MetricInfo {
    pub name: &'static str,
    pub shape: Shape,
    pub about: &'static str,
}

const fn m(name: &'static str, shape: Shape, about: &'static str) -> MetricInfo {
    MetricInfo { name, shape, about }
}

/// Every metric the `hive` source knows (`hive dashboard metrics` prints it).
pub const METRICS: &[MetricInfo] = &[
    m("agents.total", Shape::Number, "agents in the workspace"),
    m("agents.online", Shape::Number, "agents whose host is online (an agent on its owner's device counts while one of that person's devices is online)"),
    m("agents.busy", Shape::Number, "agents answering a turn right now"),
    m("agents", Shape::Rows, "each agent: up while its host is online; text busy, idle or offline"),
    m("agents.by_host", Shape::Rows, "number of agents per host"),
    m("hosts", Shape::Rows, "each registered host: up while online"),
    m("proposals.open", Shape::Number, "open proposals (not dismissed or superseded)"),
    m("proposals.awaiting_me", Shape::Number, "open proposals this host's identity may vote on and hasn't, its own excluded"),
    m("proposals.list", Shape::Table, "open proposals, newest first: title, chat, approvals"),
    m("workflows.running", Shape::Number, "workflow runs in progress"),
    m("workflows.awaiting_approval", Shape::Number, "workflow runs waiting at an approval gate"),
    m("workflows.failed_today", Shape::Number, "workflow runs that failed since local midnight"),
    m("workflows.completed_today", Shape::Number, "workflow runs that completed since local midnight"),
    m("workflows", Shape::Rows, "running, awaiting approval, failed today and completed today, as rows"),
    m("workflows.list", Shape::Table, "the latest workflow runs: workflow, status, started"),
    m("turns.today", Shape::Number, "agent replies since local midnight"),
    m("turns.7d", Shape::Number, "agent replies in the last 7 days"),
    m("turns.by_agent_7d", Shape::Rows, "agent replies in the last 7 days, per agent"),
    m("host.status", Shape::Rows, "this host: serving, relay sync, remote access, push"),
    m("sync.age", Shape::Number, "seconds since the last successful relay sync"),
    m("sync.last", Shape::Text, "the last relay sync, e.g. \"Synced 4 s ago\""),
    m("phones.paired", Shape::Number, "phones paired for remote access (hive remote)"),
];

pub fn metric_info(name: &str) -> Option<&'static MetricInfo> {
    METRICS.iter().find(|m| m.name == name)
}

// ── Local time ─────────────────────────────────────────────────────────────

static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();

/// Read the host's UTC offset while the process is still single-threaded
/// (the only time it can be read safely on Linux); "today" starts at local
/// midnight. A DST change applies after a restart. Falls back to UTC.
pub fn init_local_offset() {
    let _ = LOCAL_OFFSET.set(UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC));
}

pub fn local_offset() -> UtcOffset {
    LOCAL_OFFSET.get().copied().unwrap_or(UtcOffset::UTC)
}

fn local_midnight(now: OffsetDateTime, offset: UtcOffset) -> OffsetDateTime {
    let local = now.to_offset(offset);
    local.replace_time(time::Time::MIDNIGHT)
}

/// "4 s", "3 min", "2 h", "3 d".
pub fn age(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..=59 => format!("{s} s"),
        60..=3_599 => format!("{} min", s / 60),
        3_600..=86_399 => format!("{} h", s / 3_600),
        _ => format!("{} d", s / 86_400),
    }
}

// ── Inputs ─────────────────────────────────────────────────────────────────

/// The serve process's own state, which the event log doesn't hold.
#[derive(Debug, Clone, Default)]
pub struct HostState {
    /// False under `hive dashboard check`, which reads the store without
    /// serving: the live rows read "unknown".
    pub serving: bool,
    pub relay_configured: bool,
    pub last_sync_ok: Option<OffsetDateTime>,
    /// The latest sync failure since the last success.
    pub sync_error: Option<String>,
    /// Remote access (`hive remote`), when enabled.
    pub remote: Option<RemoteState>,
    /// Pushes through a self-hosted ntfy.
    pub ntfy: bool,
}

#[derive(Debug, Clone, Default)]
pub struct RemoteState {
    pub connected: bool,
    /// The relay can deliver pushes.
    pub push: bool,
    pub phones: usize,
    pub phones_with_push: usize,
}

pub struct HiveInputs<'a> {
    pub now: OffsetDateTime,
    pub offset: UtcOffset,
    /// The identity the phone acts as (this box, or its owner when enrolled).
    pub me: &'a str,
    pub this_host_id: &'a str,
    pub this_host_label: &'a str,
    /// Live chats (config log excluded).
    pub chats: &'a [ChatSession],
    /// The workspace config log: agents, members, workflow definitions.
    pub config: &'a ChatSession,
    /// Registered hosts and whether each is online now.
    pub hosts: &'a [(WorkspaceHost, bool)],
    /// Agents this host's worker is running a turn for.
    pub busy_here: &'a HashSet<Uuid>,
    pub host: HostState,
}

// ── Snapshot ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct AgentInfo {
    name: String,
    host_label: String,
    online: Option<bool>,
    busy: bool,
}

#[derive(Debug, Clone)]
struct ProposalInfo {
    title: String,
    chat: String,
    approvals: String,
    created: OffsetDateTime,
}

#[derive(Debug, Clone)]
struct RunInfo {
    workflow: String,
    status: WorkflowRunStatus,
    started: OffsetDateTime,
    updated: OffsetDateTime,
}

/// Everything the `hive` metrics are computed from, gathered once.
#[derive(Debug, Clone)]
pub struct HiveSnapshot {
    now: OffsetDateTime,
    today: OffsetDateTime,
    agents: Vec<AgentInfo>,
    hosts: Vec<(String, bool)>,
    proposals: Vec<ProposalInfo>,
    awaiting_me: usize,
    runs: Vec<RunInfo>,
    has_workflows: bool,
    turns_today: usize,
    turns_7d: usize,
    turns_by_agent_7d: BTreeMap<String, usize>,
    host: HostState,
}

/// A streaming reply older than this is a leftover (its device went away
/// mid-stream), not a busy agent.
const STREAM_STALE_SECS: i64 = 600;

impl HiveSnapshot {
    pub fn compute(i: &HiveInputs<'_>) -> Self {
        let now = i.now;
        let today = local_midnight(now, i.offset);
        let week_ago = now - time::Duration::days(7);

        // Agents streaming a reply (any host), plus this host's running turns.
        let mut busy: HashSet<Uuid> = i.busy_here.clone();
        for c in i.chats {
            for msg in c.messages.iter().rev().take(20) {
                if msg.is_streaming && (now - msg.created_at.inner()).whole_seconds() < STREAM_STALE_SECS {
                    if let Some(r) = msg.responder_id {
                        busy.insert(r);
                    }
                }
            }
        }
        let online_host = |id: &str| i.hosts.iter().find(|(h, _)| h.id == id).map(|(_, on)| *on);
        let agents: Vec<AgentInfo> = i
            .config
            .workspace_agents
            .iter()
            .map(|a| {
                let (host_label, online) = if a.host_id.is_empty() {
                    // Runs on its owner's own device: online while any of
                    // that person's registered devices is.
                    let mine: Vec<bool> = i
                        .hosts
                        .iter()
                        .filter(|(h, _)| !a.owner_actor_id.is_empty() && h.owner_actor_id == a.owner_actor_id)
                        .map(|(_, on)| *on)
                        .collect();
                    ("Owner's device".to_string(), if mine.is_empty() { None } else { Some(mine.contains(&true)) })
                } else if a.host_id == i.this_host_id {
                    (i.this_host_label.to_string(), Some(true))
                } else {
                    let label = i.hosts.iter().find(|(h, _)| h.id == a.host_id).map(|(h, _)| h.label.clone());
                    (label.unwrap_or_else(|| "Unknown host".into()), Some(online_host(&a.host_id).unwrap_or(false)))
                };
                AgentInfo { name: a.name.clone(), host_label, online, busy: busy.contains(&a.id) }
            })
            .collect();

        let mut proposals = Vec::new();
        let mut awaiting_me = 0;
        for c in i.chats {
            let role = c.role_of(i.me);
            for p in &c.proposals {
                if p.status != ProposalStatus::Open || p.dismissed || p.superseded_by.is_some() {
                    continue;
                }
                let floor = hive_core::authorization::min_role_for(&hive_core::SessionEvent::ProposalVoteCast {
                    proposal_id: p.id,
                    approval: hive_core::proposals::ProposalApproval {
                        actor_id: i.me.to_string(),
                        role,
                        approved: true,
                        created_at: hive_core::Timestamp::now(),
                    },
                });
                let mine = !p.author_actor_id.is_empty() && p.author_actor_id == i.me;
                let voted = p.approvals.iter().any(|a| a.actor_id == i.me);
                if !mine && !voted && role.rank() >= floor.rank() {
                    awaiting_me += 1;
                }
                let yes = p.approvals.iter().filter(|a| a.approved).count();
                let approvals = if p.required_approvals > 0 { format!("{yes}/{}", p.required_approvals) } else { yes.to_string() };
                let chat = if c.title.trim().is_empty() { "Untitled chat".to_string() } else { c.title.clone() };
                proposals.push(ProposalInfo { title: p.title.clone(), chat, approvals, created: p.created_at.inner() });
            }
        }
        proposals.sort_by(|a, b| b.created.cmp(&a.created));

        let mut runs = Vec::new();
        let mut has_workflows = !i.config.workflow_definitions.is_empty();
        for c in i.chats.iter().chain(std::iter::once(i.config)) {
            has_workflows |= !c.workflow_definitions.is_empty() || !c.workflow_runs.is_empty();
            for r in &c.workflow_runs {
                runs.push(RunInfo {
                    workflow: r.definition.name.clone(),
                    status: r.status,
                    started: r.started_at.inner(),
                    updated: r.updated_at.inner(),
                });
            }
        }
        runs.sort_by(|a, b| b.started.cmp(&a.started));

        let (mut turns_today, mut turns_7d) = (0, 0);
        let mut turns_by_agent_7d = BTreeMap::new();
        for c in i.chats {
            for msg in &c.messages {
                if !matches!(msg.role, MessageRole::Assistant | MessageRole::Agent) || msg.kind != TranscriptEventKind::Message {
                    continue;
                }
                let at = msg.created_at.inner();
                if at >= week_ago && at <= now {
                    turns_7d += 1;
                    *turns_by_agent_7d.entry(msg.author.clone()).or_insert(0) += 1;
                }
                if at >= today && at <= now {
                    turns_today += 1;
                }
            }
        }

        Self {
            now,
            today,
            agents,
            hosts: i.hosts.iter().map(|(h, on)| (h.label.clone(), *on || h.id == i.this_host_id)).collect(),
            proposals,
            awaiting_me,
            runs,
            has_workflows,
            turns_today,
            turns_7d,
            turns_by_agent_7d,
            host: i.host.clone(),
        }
    }

    /// Whether the workspace has ever had a workflow (the built-in section
    /// leaves the workflow panels out until then).
    pub fn has_workflows(&self) -> bool {
        self.has_workflows
    }

    pub fn remote_enabled(&self) -> bool {
        self.host.remote.is_some()
    }

    fn runs_with(&self, status: WorkflowRunStatus, since_today: bool) -> usize {
        self.runs.iter().filter(|r| r.status == status && (!since_today || r.updated >= self.today)).count()
    }

    fn sync_age(&self) -> Option<i64> {
        self.host.last_sync_ok.map(|t| (self.now - t).whole_seconds())
    }

    fn sync_row(&self) -> MetricRow {
        if !self.host.serving {
            return MetricRow::status("Relay sync", false, "Unknown (hive serve isn't running here)");
        }
        match (self.sync_age(), &self.host.sync_error) {
            (_, Some(e)) => MetricRow::status("Relay sync", false, &format!("Failing: {}", clip(e, 80))),
            (Some(a), None) => MetricRow::status("Relay sync", a <= 120, &format!("Synced {} ago", age(a))),
            (None, None) => MetricRow::status("Relay sync", false, "Not synced yet"),
        }
    }

    /// Look up a metric by name.
    pub fn metric(&self, name: &str) -> Result<MetricValue> {
        let n = |v: usize| Ok(MetricValue::Num(v as f64));
        match name {
            "agents.total" => n(self.agents.len()),
            "agents.online" => n(self.agents.iter().filter(|a| a.online == Some(true)).count()),
            "agents.busy" => n(self.agents.iter().filter(|a| a.busy).count()),
            "agents" => Ok(MetricValue::Rows(
                self.agents
                    .iter()
                    .map(|a| {
                        let text = match (a.busy, a.online) {
                            (true, _) => "Busy",
                            (false, Some(false)) => "Offline",
                            (false, None) => "Idle (host unknown)",
                            (false, Some(true)) => "Idle",
                        };
                        MetricRow::status(&a.name, a.busy || a.online == Some(true), text)
                    })
                    .collect(),
            )),
            "agents.by_host" => {
                let mut by: BTreeMap<String, usize> = BTreeMap::new();
                for a in &self.agents {
                    *by.entry(a.host_label.clone()).or_insert(0) += 1;
                }
                Ok(MetricValue::Rows(by.into_iter().map(|(h, c)| MetricRow::num(&h, c as f64)).collect()))
            }
            "hosts" => Ok(MetricValue::Rows(
                self.hosts
                    .iter()
                    .map(|(label, on)| MetricRow::status(label, *on, if *on { "Online" } else { "Offline" }))
                    .collect(),
            )),
            "proposals.open" => n(self.proposals.len()),
            "proposals.awaiting_me" => n(self.awaiting_me),
            "proposals.list" => Ok(MetricValue::Table {
                columns: vec!["Proposal".into(), "Chat".into(), "Approvals".into()],
                cells: self.proposals.iter().map(|p| vec![json!(p.title), json!(p.chat), json!(p.approvals)]).collect(),
            }),
            "workflows.running" => n(self.runs_with(WorkflowRunStatus::Running, false)),
            "workflows.awaiting_approval" => n(self.runs_with(WorkflowRunStatus::AwaitingGate, false)),
            "workflows.failed_today" => n(self.runs_with(WorkflowRunStatus::Failed, true)),
            "workflows.completed_today" => n(self.runs_with(WorkflowRunStatus::Completed, true)),
            "workflows" => Ok(MetricValue::Rows(vec![
                MetricRow::num("Running", self.runs_with(WorkflowRunStatus::Running, false) as f64),
                MetricRow::num("Awaiting approval", self.runs_with(WorkflowRunStatus::AwaitingGate, false) as f64),
                MetricRow::num("Failed today", self.runs_with(WorkflowRunStatus::Failed, true) as f64),
                MetricRow::num("Completed today", self.runs_with(WorkflowRunStatus::Completed, true) as f64),
            ])),
            "workflows.list" => Ok(MetricValue::Table {
                columns: vec!["Workflow".into(), "Status".into(), "Started".into()],
                cells: self
                    .runs
                    .iter()
                    .map(|r| {
                        let started = format!("{} ago", age((self.now - r.started).whole_seconds()));
                        vec![json!(r.workflow), json!(run_status(r.status)), json!(started)]
                    })
                    .collect(),
            }),
            "turns.today" => n(self.turns_today),
            "turns.7d" => n(self.turns_7d),
            "turns.by_agent_7d" => {
                let mut rows: Vec<(&String, &usize)> = self.turns_by_agent_7d.iter().collect();
                rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
                Ok(MetricValue::Rows(rows.into_iter().map(|(a, c)| MetricRow::num(a, *c as f64)).collect()))
            }
            "host.status" => {
                let mut rows = vec![if self.host.serving {
                    MetricRow::status("Hive serve", true, "Online")
                } else {
                    MetricRow::status("Hive serve", false, "Not running here")
                }];
                if self.host.relay_configured {
                    rows.push(self.sync_row());
                }
                if let Some(r) = &self.host.remote {
                    rows.push(MetricRow::status("Remote access", r.connected, if r.connected { "Connected" } else { "Disconnected" }));
                }
                let relay_push = self.host.remote.as_ref().is_some_and(|r| r.connected && r.push && r.phones_with_push > 0);
                let text = match (self.host.ntfy, relay_push) {
                    (true, true) => "ntfy and relay".to_string(),
                    (true, false) => "ntfy".to_string(),
                    (false, true) => {
                        let n = self.host.remote.as_ref().map(|r| r.phones_with_push).unwrap_or(0);
                        format!("On, {n} phone{}", if n == 1 { "" } else { "s" })
                    }
                    (false, false) => "Off".to_string(),
                };
                rows.push(MetricRow::status("Push", self.host.ntfy || relay_push, &text));
                Ok(MetricValue::Rows(rows))
            }
            "sync.age" => {
                if !self.host.relay_configured {
                    bail!("this host has no relay configured");
                }
                match self.sync_age() {
                    Some(a) => Ok(MetricValue::Num(a as f64)),
                    None => bail!("not synced yet"),
                }
            }
            "sync.last" => {
                if !self.host.relay_configured {
                    return Ok(MetricValue::Text("No relay configured".into()));
                }
                Ok(MetricValue::Text(self.sync_row().text.unwrap_or_default()))
            }
            "phones.paired" => match &self.host.remote {
                Some(r) => n(r.phones),
                None => bail!("remote access is off (hive remote enable); phones on the direct path aren't counted"),
            },
            other => bail!("unknown hive metric '{other}' (hive dashboard metrics lists them)"),
        }
    }
}

fn run_status(s: WorkflowRunStatus) -> &'static str {
    match s {
        WorkflowRunStatus::Running => "Running",
        WorkflowRunStatus::AwaitingGate => "Awaiting approval",
        WorkflowRunStatus::Completed => "Completed",
        WorkflowRunStatus::Failed => "Failed",
        WorkflowRunStatus::Halted => "Halted",
        WorkflowRunStatus::Canceled => "Canceled",
    }
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// One row of a rows metric.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricRow {
    pub label: String,
    pub value: Option<f64>,
    pub text: Option<String>,
    /// Status rows decide up/down themselves.
    pub up: Option<bool>,
}

impl MetricRow {
    fn num(label: &str, v: f64) -> Self {
        Self { label: label.into(), value: Some(v), text: None, up: None }
    }

    fn status(label: &str, up: bool, text: &str) -> Self {
        Self { label: label.into(), value: None, text: Some(text.into()), up: Some(up) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MetricValue {
    Num(f64),
    Text(String),
    Rows(Vec<MetricRow>),
    Table { columns: Vec<String>, cells: Vec<Vec<Value>> },
}

// ── The built-in section ───────────────────────────────────────────────────

/// Panel ids of the built-in section start with this.
pub const BUILTIN_ID_PREFIX: &str = "hive-";
pub const BUILTIN_TITLE: &str = "Hive";

fn hive_panel(id: &str, title: &str, kind: Kind, metric: &str) -> PanelConfig {
    PanelConfig {
        id: Some(format!("{BUILTIN_ID_PREFIX}{id}")),
        title: title.into(),
        kind,
        source: Some(SourceKind::Hive),
        metric: Some(metric.into()),
        ..PanelConfig::default()
    }
}

fn flag(mut p: PanelConfig, level: Level) -> PanelConfig {
    p.thresholds = vec![Threshold { value: 1.0, level }];
    p
}

/// The "Hive" section every host shows unless `builtin = false`. Workflow
/// panels appear once the workspace has a workflow, the phone count once
/// remote access is on. `None` (no snapshot) gives every panel.
pub fn builtin_section(snap: Option<&HiveSnapshot>) -> SectionConfig {
    let workflows = snap.is_none_or(HiveSnapshot::has_workflows);
    let remote = snap.is_none_or(HiveSnapshot::remote_enabled);
    let mut panels = vec![
        hive_panel("agents-online", "Agents online", Kind::Stat, "agents.online"),
        hive_panel("agents-busy", "Agents busy", Kind::Stat, "agents.busy"),
        flag(hive_panel("awaiting-me", "Awaiting your review", Kind::Stat, "proposals.awaiting_me"), Level::Warn),
        hive_panel("proposals-open", "Open proposals", Kind::Stat, "proposals.open"),
        hive_panel("turns-today", "Turns today", Kind::Stat, "turns.today"),
        hive_panel("turns-7d", "Turns, 7 days", Kind::Stat, "turns.7d"),
    ];
    if workflows {
        panels.push(hive_panel("workflows-running", "Workflows running", Kind::Stat, "workflows.running"));
        panels.push(flag(hive_panel("workflows-gated", "Awaiting approval", Kind::Stat, "workflows.awaiting_approval"), Level::Warn));
        panels.push(flag(hive_panel("workflows-failed", "Failed today", Kind::Stat, "workflows.failed_today"), Level::Critical));
    }
    if remote {
        panels.push(hive_panel("phones", "Phones paired", Kind::Stat, "phones.paired"));
    }
    panels.push(hive_panel("host", "This host", Kind::Status, "host.status"));
    SectionConfig { title: BUILTIN_TITLE.into(), optional: false, show_when: None, panels }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use hive_core::{
        workflow, ActionProposal, ActorIdentity, ActorKind, ChatMessage, HostKind, ProposalKind, Timestamp,
        WorkflowDefinition, WorkspaceAgent, WorkspaceRole,
    };
    use hive_core::identity::WorkspaceMember;
    use hive_core::proposals::ProposalApproval;

    pub const ME: &str = "alex-id";
    pub const THIS_HOST: &str = "host-this";

    pub fn now() -> OffsetDateTime {
        OffsetDateTime::parse("2026-11-04T15:00:00Z", &time::format_description::well_known::Rfc3339).unwrap()
    }

    fn at(hours_ago: i64) -> Timestamp {
        Timestamp(now() - time::Duration::hours(hours_ago))
    }

    fn reply(author: &str, hours_ago: i64) -> ChatMessage {
        let mut m = ChatMessage::new(MessageRole::Assistant, author, "done");
        m.created_at = at(hours_ago);
        m
    }

    /// A workspace with four agents on four kinds of host, five proposals,
    /// five workflow runs and replies spread over two weeks.
    pub struct Fixture {
        pub chats: Vec<ChatSession>,
        pub config: ChatSession,
        pub hosts: Vec<(WorkspaceHost, bool)>,
        pub busy: HashSet<Uuid>,
    }

    pub fn fixture() -> Fixture {
        let ws = Uuid::new_v4();
        let mut config = ChatSession::new("", ws, "");
        let agent = |name: &str, host: &str| {
            let mut a = WorkspaceAgent::new(name, "rt");
            a.host_id = host.into();
            a.owner_actor_id = ME.into();
            a
        };
        let (here, remote_on, remote_off, laptop) =
            (agent("coder", THIS_HOST), agent("writer", "host-on"), agent("tester", "host-off"), agent("helper", ""));
        let busy: HashSet<Uuid> = [here.id].into();
        config.workspace_agents = vec![here, remote_on.clone(), remote_off, laptop];
        let host = |id: &str, kind: HostKind, label: &str, owner: &str| {
            let mut h = WorkspaceHost::new(id, kind, label);
            h.owner_actor_id = owner.into();
            h
        };
        let hosts = vec![
            (host(THIS_HOST, HostKind::Worker, "Home box", ""), true),
            (host("host-on", HostKind::Worker, "Build box", ""), true),
            (host("host-off", HostKind::Worker, "Old box", ""), false),
            (host("laptop", HostKind::Device, "Alex's laptop", ME), true),
        ];

        let mut chat = ChatSession::new("Release", ws, "");
        chat.members = vec![WorkspaceMember {
            id: ME.into(),
            actor: ActorIdentity::new(ME, "Alex", ActorKind::Human),
            role: WorkspaceRole::Contributor,
            title: String::new(),
            index: 1,
            joined_at: Timestamp::now(),
        }];
        chat.messages = vec![
            ChatMessage::new(MessageRole::User, "Alex", "hi"),
            reply("coder", 1),
            reply("coder", 2),
            reply("writer", 48),
            reply("writer", 24 * 10),
        ];
        let mut proposal_note = reply("coder", 1);
        proposal_note.kind = TranscriptEventKind::Proposal;
        chat.messages.push(proposal_note);
        let mut streaming = reply("writer", 0);
        streaming.is_streaming = true;
        streaming.responder_id = Some(remote_on.id);
        chat.messages.push(streaming);

        let open = |title: &str, author: &str, hours_ago: i64| {
            let mut p = ActionProposal::new(title, ProposalKind::Decision, author);
            p.created_at = at(hours_ago);
            p
        };
        let mut voted = open("Voted", "other-id", 3);
        voted.approvals.push(ProposalApproval { actor_id: ME.into(), role: WorkspaceRole::Contributor, approved: true, created_at: Timestamp::now() });
        let mut dismissed = open("Dismissed", "other-id", 4);
        dismissed.dismissed = true;
        let mut applied = open("Applied", "other-id", 5);
        applied.status = ProposalStatus::Applied;
        chat.proposals = vec![open("Older", "other-id", 6), open("Mine", ME, 2), voted, dismissed, applied, open("Newest", "other-id", 1)];

        let def = WorkflowDefinition {
            id: Uuid::new_v4(),
            name: "Ship".into(),
            description: String::new(),
            input_label: None,
            nodes: vec![],
            created_at: Timestamp::now(),
            disabled: false,
        };
        let run = |status: WorkflowRunStatus, hours_ago: i64| {
            let mut r = workflow::new_run(&def, "go", ME);
            r.status = status;
            r.started_at = at(hours_ago + 1);
            r.updated_at = at(hours_ago);
            r
        };
        chat.workflow_runs = vec![
            run(WorkflowRunStatus::Running, 0),
            run(WorkflowRunStatus::AwaitingGate, 1),
            run(WorkflowRunStatus::Failed, 2),
            run(WorkflowRunStatus::Failed, 48),
            run(WorkflowRunStatus::Completed, 3),
        ];
        Fixture { chats: vec![chat], config, hosts, busy }
    }

    pub fn snapshot_of(f: &Fixture, host: HostState) -> HiveSnapshot {
        HiveSnapshot::compute(&HiveInputs {
            now: now(),
            offset: UtcOffset::UTC,
            me: ME,
            this_host_id: THIS_HOST,
            this_host_label: "Home box",
            chats: &f.chats,
            config: &f.config,
            hosts: &f.hosts,
            busy_here: &f.busy,
            host,
        })
    }

    fn num(s: &HiveSnapshot, m: &str) -> f64 {
        match s.metric(m).unwrap() {
            MetricValue::Num(v) => v,
            other => panic!("{m}: {other:?}"),
        }
    }

    fn rows(s: &HiveSnapshot, m: &str) -> Vec<MetricRow> {
        match s.metric(m).unwrap() {
            MetricValue::Rows(r) => r,
            other => panic!("{m}: {other:?}"),
        }
    }

    #[test]
    fn every_catalogued_metric_answers_with_its_shape() {
        let f = fixture();
        let host = HostState {
            serving: true,
            relay_configured: true,
            last_sync_ok: Some(now()),
            remote: Some(RemoteState::default()),
            ..HostState::default()
        };
        let s = snapshot_of(&f, host);
        for m in METRICS {
            let v = s.metric(m.name).unwrap_or_else(|e| panic!("{}: {e}", m.name));
            let shape = match v {
                MetricValue::Num(_) => Shape::Number,
                MetricValue::Text(_) => Shape::Text,
                MetricValue::Rows(_) => Shape::Rows,
                MetricValue::Table { .. } => Shape::Table,
            };
            assert_eq!(shape, m.shape, "{}", m.name);
        }
        assert!(s.metric("tokens.today").unwrap_err().to_string().contains("unknown hive metric"));
    }

    #[test]
    fn agents_online_busy_and_per_host() {
        let s = snapshot_of(&fixture(), HostState::default());
        assert_eq!(num(&s, "agents.total"), 4.0);
        // This host, the online worker, and the owner's-device agent (Alex's
        // laptop is online); the agent on the offline box isn't.
        assert_eq!(num(&s, "agents.online"), 3.0);
        // The local turn plus the reply streaming in from the other box.
        assert_eq!(num(&s, "agents.busy"), 2.0);
        let agents = rows(&s, "agents");
        let tester = agents.iter().find(|r| r.label == "tester").unwrap();
        assert_eq!((tester.up, tester.text.as_deref()), (Some(false), Some("Offline")));
        let writer = agents.iter().find(|r| r.label == "writer").unwrap();
        assert_eq!(writer.text.as_deref(), Some("Busy"));
        let by_host: Vec<(String, Option<f64>)> = rows(&s, "agents.by_host").into_iter().map(|r| (r.label, r.value)).collect();
        assert_eq!(
            by_host,
            [("Build box", 1.0), ("Home box", 1.0), ("Old box", 1.0), ("Owner's device", 1.0)].map(|(l, v)| (l.to_string(), Some(v)))
        );
        let hosts = rows(&s, "hosts");
        assert_eq!(hosts.iter().filter(|r| r.up == Some(true)).count(), 3);
    }

    #[test]
    fn proposals_open_awaiting_me_and_list() {
        let s = snapshot_of(&fixture(), HostState::default());
        // Open, not dismissed: Older, Mine, Voted, Newest.
        assert_eq!(num(&s, "proposals.open"), 4.0);
        // Not my own, not already voted on: Older and Newest.
        assert_eq!(num(&s, "proposals.awaiting_me"), 2.0);
        let MetricValue::Table { columns, cells } = s.metric("proposals.list").unwrap() else { panic!() };
        assert_eq!(columns, ["Proposal", "Chat", "Approvals"]);
        assert_eq!(cells[0], vec![json!("Newest"), json!("Release"), json!("0/1")]);
        assert_eq!(cells.iter().find(|c| c[0] == "Voted").unwrap()[2], "1/1");
    }

    #[test]
    fn a_viewer_has_nothing_awaiting() {
        let mut f = fixture();
        f.chats[0].members[0].role = WorkspaceRole::Viewer;
        let s = snapshot_of(&f, HostState::default());
        let floor = hive_core::authorization::min_role_for(&hive_core::SessionEvent::ProposalVoteCast {
            proposal_id: Uuid::nil(),
            approval: ProposalApproval { actor_id: ME.into(), role: WorkspaceRole::Viewer, approved: true, created_at: Timestamp::now() },
        });
        let expected = if floor == WorkspaceRole::Viewer { 2.0 } else { 0.0 };
        assert_eq!(num(&s, "proposals.awaiting_me"), expected);
    }

    #[test]
    fn workflows_and_turns_count_by_local_day() {
        let s = snapshot_of(&fixture(), HostState::default());
        assert_eq!(num(&s, "workflows.running"), 1.0);
        assert_eq!(num(&s, "workflows.awaiting_approval"), 1.0);
        assert_eq!(num(&s, "workflows.failed_today"), 1.0, "the failure two days ago isn't today's");
        assert_eq!(num(&s, "workflows.completed_today"), 1.0);
        assert!(s.has_workflows());
        // Replies only (not the user's message or the proposal note), the
        // in-flight streaming reply included: 1 h, 2 h, now today; + 48 h in 7 d.
        assert_eq!(num(&s, "turns.today"), 3.0);
        assert_eq!(num(&s, "turns.7d"), 4.0);
        let by_agent: Vec<(String, Option<f64>)> = rows(&s, "turns.by_agent_7d").into_iter().map(|r| (r.label, r.value)).collect();
        assert_eq!(by_agent, [("coder".to_string(), Some(2.0)), ("writer".to_string(), Some(2.0))]);

        // Local midnight decides "today": at UTC-16 it is still the 3rd
        // (23:00), so a reply 2 h ago (21:00 on the 3rd) is today, but one
        // from 48 h ago isn't.
        let f = fixture();
        let west = HiveSnapshot::compute(&HiveInputs {
            now: now(),
            offset: UtcOffset::from_hms(-16, 0, 0).unwrap(),
            me: ME,
            this_host_id: THIS_HOST,
            this_host_label: "Home box",
            chats: &f.chats,
            config: &f.config,
            hosts: &f.hosts,
            busy_here: &f.busy,
            host: HostState::default(),
        });
        assert_eq!(num(&west, "turns.today"), 3.0);
        let mut late = fixture();
        late.chats[0].messages.push(reply("coder", 20)); // 19:00 UTC on the 3rd = 03:00 on the 3rd at UTC-16
        let west_late = HiveSnapshot::compute(&HiveInputs {
            now: now(),
            offset: UtcOffset::from_hms(-16, 0, 0).unwrap(),
            me: ME,
            this_host_id: THIS_HOST,
            this_host_label: "Home box",
            chats: &late.chats,
            config: &late.config,
            hosts: &late.hosts,
            busy_here: &late.busy,
            host: HostState::default(),
        });
        assert_eq!(num(&west_late, "turns.today"), 4.0);
        assert_eq!(num(&snapshot_of(&late, HostState::default()), "turns.today"), 3.0, "in UTC it was yesterday");
    }

    #[test]
    fn host_status_rows() {
        let f = fixture();
        let quiet = snapshot_of(&f, HostState { serving: true, ..HostState::default() });
        let r = rows(&quiet, "host.status");
        let labels: Vec<&str> = r.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["Hive serve", "Push"], "no relay, no remote access: no rows for them");
        assert_eq!(r[1].text.as_deref(), Some("Off"));
        assert!(quiet.metric("sync.age").is_err());
        assert_eq!(quiet.metric("sync.last").unwrap(), MetricValue::Text("No relay configured".into()));
        assert!(quiet.metric("phones.paired").unwrap_err().to_string().contains("remote access is off"));

        let busy = snapshot_of(
            &f,
            HostState {
                serving: true,
                relay_configured: true,
                last_sync_ok: Some(now() - time::Duration::seconds(4)),
                sync_error: None,
                remote: Some(RemoteState { connected: true, push: true, phones: 2, phones_with_push: 1 }),
                ntfy: false,
            },
        );
        let r = rows(&busy, "host.status");
        let shown: Vec<(&str, Option<bool>, Option<&str>)> = r.iter().map(|r| (r.label.as_str(), r.up, r.text.as_deref())).collect();
        assert_eq!(
            shown,
            [
                ("Hive serve", Some(true), Some("Online")),
                ("Relay sync", Some(true), Some("Synced 4 s ago")),
                ("Remote access", Some(true), Some("Connected")),
                ("Push", Some(true), Some("On, 1 phone")),
            ]
        );
        assert_eq!(num(&busy, "sync.age"), 4.0);
        assert_eq!(num(&busy, "phones.paired"), 2.0);

        let failing = snapshot_of(
            &f,
            HostState { serving: true, relay_configured: true, sync_error: Some("relay unreachable".into()), ntfy: true, ..HostState::default() },
        );
        let r = rows(&failing, "host.status");
        assert_eq!((r[1].up, r[1].text.as_deref()), (Some(false), Some("Failing: relay unreachable")));
        assert_eq!(r[2].text.as_deref(), Some("ntfy"));
    }

    #[test]
    fn builtin_section_adapts_to_the_workspace() {
        let mut f = fixture();
        let all = builtin_section(Some(&snapshot_of(&f, HostState { remote: Some(RemoteState::default()), ..HostState::default() })));
        let ids: Vec<&str> = all.panels.iter().map(|p| p.id.as_deref().unwrap()).collect();
        assert_eq!(
            ids,
            [
                "hive-agents-online",
                "hive-agents-busy",
                "hive-awaiting-me",
                "hive-proposals-open",
                "hive-turns-today",
                "hive-turns-7d",
                "hive-workflows-running",
                "hive-workflows-gated",
                "hive-workflows-failed",
                "hive-phones",
                "hive-host"
            ]
        );
        f.chats[0].workflow_runs.clear();
        let lean = builtin_section(Some(&snapshot_of(&f, HostState::default())));
        assert!(lean.panels.iter().all(|p| !p.id.as_deref().unwrap().contains("workflows") && p.id.as_deref() != Some("hive-phones")));
        assert_eq!(builtin_section(None).panels.len(), 11);
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age(4), "4 s");
        assert_eq!(age(125), "2 min");
        assert_eq!(age(7_300), "2 h");
        assert_eq!(age(200_000), "2 d");
        assert_eq!(age(-3), "0 s");
    }
}
