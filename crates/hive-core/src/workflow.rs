//! Agentic workflows — user-defined DAG pipelines over the existing chat
//! primitives. A definition is a set of nodes (agent turns and approval
//! gates) linked by `depends_on` edges; a run is the frozen definition plus
//! per-node execution state. Everything here is pure: validation, the
//! ready-set scheduler, gate outcome/reroute logic, and prompt-template
//! rendering. Driving IO (running turns, creating proposals) lives in the
//! app layer.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::time_util::Timestamp;

/// Total gate attempts before a rejecting RouteTo gate halts the run instead
/// of rerouting again (so `3` = the initial attempt plus two reroutes).
pub const MAX_NODE_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDefinition {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Placeholder shown when asking the user for the run input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_label: Option<String>,
    pub nodes: Vec<WorkflowNode>,
    #[serde(default)]
    pub created_at: Timestamp,
    /// Paused: still listed (read-only), but no new run can start. Runs
    /// already in flight are unaffected.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowNode {
    /// Slug-stable id, unique within the definition; referenced by
    /// `depends_on` and `{{nodes.<id>.output}}` templates.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub kind: WorkflowNodeKind,
    /// Canvas position (px) from the retired visual builder. Kept so older
    /// definitions round-trip unchanged; nothing reads it any more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
pub enum WorkflowNodeKind {
    /// One assistant turn. `agent_id: None` runs the session's primary
    /// runtime, so definitions work before any named agents exist.
    Agent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<Uuid>,
        prompt_template: String,
    },
    /// Human/quorum approval, backed by an `ActionProposal`.
    Gate {
        title_template: String,
        #[serde(default)]
        body_template: String,
        required_approvals: u32,
        #[serde(default)]
        on_reject: GateRejectPolicy,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "policy")]
pub enum GateRejectPolicy {
    /// Rejection halts the run.
    #[default]
    Halt,
    /// Rejection resets `node` (which must be an ancestor of the gate) and
    /// everything downstream of it — including the gate — for another
    /// attempt, bounded by [`MAX_NODE_ATTEMPTS`].
    RouteTo { node: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowRunStatus {
    Running,
    AwaitingGate,
    Completed,
    Failed,
    Halted,
    Canceled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeRunStatus {
    Pending,
    Running,
    AwaitingApproval,
    Succeeded,
    Failed,
    Rejected,
    Skipped,
}

impl NodeRunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            NodeRunStatus::Succeeded
                | NodeRunStatus::Failed
                | NodeRunStatus::Rejected
                | NodeRunStatus::Skipped
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRunState {
    pub node_id: String,
    pub status: NodeRunStatus,
    /// Transcript message carrying this node's full output (agent nodes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<Uuid>,
    /// Backing proposal (gate nodes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<Uuid>,
    /// First ~400 chars of the output, for run cards; the full text lives in
    /// the transcript message.
    #[serde(default)]
    pub output_excerpt: String,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub error: String,
}

impl NodeRunState {
    fn fresh(node_id: &str) -> Self {
        Self {
            node_id: node_id.to_string(),
            status: NodeRunStatus::Pending,
            message_id: None,
            proposal_id: None,
            output_excerpt: String::new(),
            attempts: 0,
            error: String::new(),
        }
    }

    /// Reset for another attempt, keeping the attempt counter.
    fn reset(&mut self) {
        let attempts = self.attempts;
        *self = Self::fresh(&self.node_id.clone());
        self.attempts = attempts;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRun {
    pub id: Uuid,
    pub definition_id: Uuid,
    /// Frozen copy taken at start so later edits can't corrupt an in-flight run.
    pub definition: WorkflowDefinition,
    pub input: String,
    pub status: WorkflowRunStatus,
    pub nodes: Vec<NodeRunState>,
    pub initiator_actor_id: String,
    #[serde(default)]
    pub started_at: Timestamp,
    #[serde(default)]
    pub updated_at: Timestamp,
}

impl WorkflowRun {
    pub fn node_state(&self, node_id: &str) -> Option<&NodeRunState> {
        self.nodes.iter().find(|n| n.node_id == node_id)
    }

    pub fn node_state_mut(&mut self, node_id: &str) -> Option<&mut NodeRunState> {
        self.nodes.iter_mut().find(|n| n.node_id == node_id)
    }
}

pub fn new_run(
    definition: &WorkflowDefinition,
    input: impl Into<String>,
    initiator_actor_id: impl Into<String>,
) -> WorkflowRun {
    WorkflowRun {
        id: Uuid::new_v4(),
        definition_id: definition.id,
        definition: definition.clone(),
        input: input.into(),
        status: WorkflowRunStatus::Running,
        nodes: definition.nodes.iter().map(|n| NodeRunState::fresh(&n.id)).collect(),
        initiator_actor_id: initiator_actor_id.into(),
        started_at: Timestamp::now(),
        updated_at: Timestamp::now(),
    }
}

/// Structural validation. Returns the first problem found; the builder UI
/// mirrors these checks client-side for inline feedback.
pub fn validate(def: &WorkflowDefinition) -> Result<(), String> {
    if def.name.trim().is_empty() {
        return Err("workflow needs a name".into());
    }
    if def.nodes.is_empty() {
        return Err("workflow needs at least one stage".into());
    }

    let mut ids = HashSet::new();
    for node in &def.nodes {
        if node.id.is_empty()
            || !node.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!(
                "stage id {:?} must be a non-empty slug (letters, digits, '-', '_')",
                node.id
            ));
        }
        if !ids.insert(node.id.as_str()) {
            return Err(format!("duplicate stage id {:?}", node.id));
        }
    }

    for node in &def.nodes {
        for dep in &node.depends_on {
            if dep == &node.id {
                return Err(format!("stage {:?} depends on itself", node.id));
            }
            if !ids.contains(dep.as_str()) {
                return Err(format!("stage {:?} depends on unknown stage {:?}", node.id, dep));
            }
        }
    }

    if topo_order(def).is_none() {
        return Err("stages contain a dependency cycle".into());
    }

    if !def
        .nodes
        .iter()
        .any(|n| matches!(n.kind, WorkflowNodeKind::Agent { .. }))
    {
        return Err("workflow needs at least one agent stage".into());
    }

    for node in &def.nodes {
        let ancestors = ancestors_of(def, &node.id);
        let check_refs = |template: &str| -> Result<(), String> {
            for referenced in template_refs(template) {
                if !ids.contains(referenced.as_str()) {
                    return Err(format!(
                        "stage {:?} references unknown stage {:?} in its template",
                        node.id, referenced
                    ));
                }
                if !ancestors.contains(referenced.as_str()) {
                    return Err(format!(
                        "stage {:?} references {:?} but does not (transitively) depend on it",
                        node.id, referenced
                    ));
                }
            }
            Ok(())
        };
        match &node.kind {
            WorkflowNodeKind::Agent { prompt_template, .. } => check_refs(prompt_template)?,
            WorkflowNodeKind::Gate {
                title_template,
                body_template,
                required_approvals,
                on_reject,
            } => {
                check_refs(title_template)?;
                check_refs(body_template)?;
                if *required_approvals == 0 {
                    return Err(format!("gate {:?} needs at least one required approval", node.id));
                }
                if let GateRejectPolicy::RouteTo { node: target } = on_reject {
                    if !ids.contains(target.as_str()) {
                        return Err(format!(
                            "gate {:?} routes rejection to unknown stage {:?}",
                            node.id, target
                        ));
                    }
                    if !ancestors.contains(target.as_str()) {
                        return Err(format!(
                            "gate {:?} must route rejection to one of its upstream stages, not {:?}",
                            node.id, target
                        ));
                    }
                }
            }
        }
    }

    Ok(())
}

/// Kahn's algorithm; `None` on a cycle.
fn topo_order(def: &WorkflowDefinition) -> Option<Vec<&str>> {
    // Count *unique* dependencies: the decrement below fires once per (node,
    // upstream id), so a duplicate entry in `depends_on` would otherwise leave
    // indegree stuck above zero and be misreported as a cycle.
    let mut indegree: HashMap<&str, usize> = def
        .nodes
        .iter()
        .map(|n| {
            let unique: HashSet<&str> = n.depends_on.iter().map(String::as_str).collect();
            (n.id.as_str(), unique.len())
        })
        .collect();
    let mut order = Vec::with_capacity(def.nodes.len());
    let mut queue: Vec<&str> = indegree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(id, _)| *id)
        .collect();
    while let Some(id) = queue.pop() {
        order.push(id);
        for node in &def.nodes {
            if node.depends_on.iter().any(|d| d == id) {
                let d = indegree.get_mut(node.id.as_str()).unwrap();
                *d -= 1;
                if *d == 0 {
                    queue.push(node.id.as_str());
                }
            }
        }
    }
    (order.len() == def.nodes.len()).then_some(order)
}

/// Transitive upstream closure of `node_id` (exclusive of itself).
fn ancestors_of<'a>(def: &'a WorkflowDefinition, node_id: &str) -> HashSet<&'a str> {
    let mut out = HashSet::new();
    let mut stack: Vec<&str> = def
        .nodes
        .iter()
        .find(|n| n.id == node_id)
        .map(|n| n.depends_on.iter().map(String::as_str).collect())
        .unwrap_or_default();
    while let Some(id) = stack.pop() {
        if let Some(node) = def.nodes.iter().find(|n| n.id == id) {
            if out.insert(node.id.as_str()) {
                stack.extend(node.depends_on.iter().map(String::as_str));
            }
        }
    }
    out
}

/// Transitive downstream closure of `node_id` (exclusive of itself).
fn descendants_of<'a>(def: &'a WorkflowDefinition, node_id: &str) -> HashSet<&'a str> {
    let mut out: HashSet<&str> = HashSet::new();
    let mut grew = true;
    while grew {
        grew = false;
        for node in &def.nodes {
            if out.contains(node.id.as_str()) {
                continue;
            }
            if node
                .depends_on
                .iter()
                .any(|d| d == node_id || out.contains(d.as_str()))
            {
                out.insert(node.id.as_str());
                grew = true;
            }
        }
    }
    out
}

/// Nodes that can execute right now: `Pending` with every dependency `Succeeded`.
pub fn ready_nodes<'a>(run: &'a WorkflowRun) -> Vec<&'a WorkflowNode> {
    run.definition
        .nodes
        .iter()
        .filter(|node| {
            run.node_state(&node.id)
                .is_some_and(|s| s.status == NodeRunStatus::Pending)
                && node.depends_on.iter().all(|dep| {
                    run.node_state(dep)
                        .is_some_and(|s| s.status == NodeRunStatus::Succeeded)
                })
        })
        .collect()
}

/// Cascade `Skipped` onto pending nodes whose dependencies can no longer succeed.
pub fn propagate_skips(run: &mut WorkflowRun) {
    loop {
        let mut to_skip = Vec::new();
        for node in &run.definition.nodes {
            let pending = run
                .node_state(&node.id)
                .is_some_and(|s| s.status == NodeRunStatus::Pending);
            if !pending {
                continue;
            }
            let dead_dep = node.depends_on.iter().any(|dep| {
                run.node_state(dep).is_some_and(|s| {
                    matches!(
                        s.status,
                        NodeRunStatus::Failed | NodeRunStatus::Rejected | NodeRunStatus::Skipped
                    )
                })
            });
            if dead_dep {
                to_skip.push(node.id.clone());
            }
        }
        if to_skip.is_empty() {
            break;
        }
        for id in to_skip {
            if let Some(s) = run.node_state_mut(&id) {
                s.status = NodeRunStatus::Skipped;
            }
        }
    }
}

/// Fold a gate vote into the run. On rejection with `RouteTo`, resets the
/// target and its downstream (which includes the gate) for another attempt,
/// halting once the gate has been rejected [`MAX_NODE_ATTEMPTS`] times.
pub fn apply_gate_outcome(run: &mut WorkflowRun, node_id: &str, approved: bool) {
    let policy = match run.definition.nodes.iter().find(|n| n.id == node_id) {
        Some(WorkflowNode { kind: WorkflowNodeKind::Gate { on_reject, .. }, .. }) => {
            on_reject.clone()
        }
        _ => return,
    };

    if approved {
        if let Some(s) = run.node_state_mut(node_id) {
            s.status = NodeRunStatus::Succeeded;
        }
        return;
    }

    match policy {
        GateRejectPolicy::Halt => {
            if let Some(s) = run.node_state_mut(node_id) {
                s.status = NodeRunStatus::Rejected;
            }
        }
        GateRejectPolicy::RouteTo { node: target } => {
            let attempts = run.node_state(node_id).map(|s| s.attempts).unwrap_or(0);
            if attempts + 1 >= MAX_NODE_ATTEMPTS {
                if let Some(s) = run.node_state_mut(node_id) {
                    s.status = NodeRunStatus::Rejected;
                    s.attempts = attempts + 1;
                }
                return;
            }
            let mut affected = descendants_of(&run.definition, &target)
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>();
            affected.push(target.clone());
            for id in affected {
                if let Some(s) = run.node_state_mut(&id) {
                    s.reset();
                }
            }
            if let Some(s) = run.node_state_mut(node_id) {
                s.attempts = attempts + 1;
            }
        }
    }
}

/// Derive the run's status from its nodes. `Canceled` is set explicitly by
/// the driver and never derived. Call [`propagate_skips`] first so stranded
/// pending nodes are already `Skipped`.
pub fn derive_run_status(run: &WorkflowRun) -> WorkflowRunStatus {
    let states = || run.nodes.iter().map(|n| n.status);
    if states().any(|s| s == NodeRunStatus::Running) {
        return WorkflowRunStatus::Running;
    }
    if states().any(|s| s == NodeRunStatus::AwaitingApproval) {
        return WorkflowRunStatus::AwaitingGate;
    }
    if states().any(|s| s == NodeRunStatus::Pending) {
        return WorkflowRunStatus::Running;
    }
    if states().any(|s| s == NodeRunStatus::Failed) {
        return WorkflowRunStatus::Failed;
    }
    if states().any(|s| s == NodeRunStatus::Rejected) {
        return WorkflowRunStatus::Halted;
    }
    WorkflowRunStatus::Completed
}

/// Node ids referenced as `{{nodes.<id>.output}}` in a template.
pub fn template_refs(template: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{nodes.") {
        rest = &rest[start + "{{nodes.".len()..];
        if let Some(end) = rest.find(".output}}") {
            let id = &rest[..end];
            if !id.is_empty() && !refs.iter().any(|r| r == id) {
                refs.push(id.to_string());
            }
            rest = &rest[end + ".output}}".len()..];
        } else {
            break;
        }
    }
    refs
}

/// Substitute `{{input}}` and `{{nodes.<id>.output}}`. Unresolvable node
/// refs render a visible placeholder rather than leaking template syntax.
pub fn render_template(template: &str, input: &str, outputs: &HashMap<String, String>) -> String {
    let mut out = template.replace("{{input}}", input);
    for id in template_refs(template) {
        let pattern = format!("{{{{nodes.{id}.output}}}}");
        let replacement = outputs
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("[no output from {id}]"));
        out = out.replace(&pattern, &replacement);
    }
    out
}

/// Add `def` to a definition set, or replace the one with the same id in
/// place (an approved revision). Idempotent: applying the same definition
/// twice leaves one copy.
pub fn upsert_definition(defs: &mut Vec<WorkflowDefinition>, def: WorkflowDefinition) {
    match defs.iter_mut().find(|d| d.id == def.id) {
        Some(slot) => *slot = def,
        None => defs.push(def),
    }
}

/// The stages in an order a person can read top to bottom: dependency
/// order where the graph allows it, else as stored.
fn reading_order(def: &WorkflowDefinition) -> Vec<&WorkflowNode> {
    match topo_order(def) {
        Some(ids) => ids.iter().filter_map(|id| def.nodes.iter().find(|n| n.id == *id)).collect(),
        None => def.nodes.iter().collect(),
    }
}

/// A plain-text rendering of a definition: what a reviewer reads in a
/// workflow proposal (and what older peers and the phone show as its body).
/// `agent_name` resolves a stage's agent id to its roster name.
pub fn describe_definition(def: &WorkflowDefinition, agent_name: &dyn Fn(Uuid) -> Option<String>) -> String {
    let mut out = String::new();
    if !def.description.trim().is_empty() {
        out.push_str(def.description.trim());
        out.push_str("\n\n");
    }
    if let Some(label) = def.input_label.as_deref().filter(|l| !l.trim().is_empty()) {
        out.push_str(&format!("Input: {label}\n\n"));
    }
    for (i, node) in reading_order(def).into_iter().enumerate() {
        let after = if node.depends_on.is_empty() {
            String::new()
        } else {
            format!(" (after {})", node.depends_on.join(", "))
        };
        match &node.kind {
            WorkflowNodeKind::Agent { agent_id, prompt_template } => {
                let who = match agent_id {
                    None => "primary runtime".to_string(),
                    Some(id) => agent_name(*id)
                        .map(|n| format!("@{n}"))
                        .unwrap_or_else(|| "an agent not in this chat".into()),
                };
                out.push_str(&format!("{}. {} [{}], {who}{after}\n", i + 1, node.name, node.id));
                for line in prompt_template.lines() {
                    out.push_str("   > ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
            WorkflowNodeKind::Gate { title_template, required_approvals, on_reject, .. } => {
                let reject = match on_reject {
                    GateRejectPolicy::Halt => "reject halts the run".to_string(),
                    GateRejectPolicy::RouteTo { node } => format!("reject retries from {node}"),
                };
                let n = *required_approvals;
                out.push_str(&format!(
                    "{}. Approval gate {} [{}]{after}: \"{title_template}\", {n} approval{}, {reject}\n",
                    i + 1,
                    node.name,
                    node.id,
                    if n == 1 { "" } else { "s" },
                ));
            }
        }
    }
    out.trim_end().to_string()
}

/// A definition as the `[[workflow: …]]` JSON an agent authors (agents by
/// roster name), with `revises` set to its id: what "ask an agent to change
/// this" hands the agent to edit.
pub fn directive_json(def: &WorkflowDefinition, agent_name: &dyn Fn(Uuid) -> Option<String>) -> serde_json::Value {
    use serde_json::{json, Map, Value};
    let stages: Vec<Value> = def
        .nodes
        .iter()
        .map(|n| {
            let mut s = Map::new();
            s.insert("id".into(), json!(n.id));
            s.insert("name".into(), json!(n.name));
            match &n.kind {
                WorkflowNodeKind::Agent { agent_id, prompt_template } => {
                    s.insert("kind".into(), json!("agent"));
                    if let Some(name) = agent_id.and_then(agent_name) {
                        s.insert("agent".into(), json!(name));
                    }
                    s.insert("prompt".into(), json!(prompt_template));
                }
                WorkflowNodeKind::Gate { title_template, body_template, required_approvals, on_reject } => {
                    s.insert("kind".into(), json!("gate"));
                    s.insert("title".into(), json!(title_template));
                    if !body_template.is_empty() {
                        s.insert("body".into(), json!(body_template));
                    }
                    s.insert("approvals".into(), json!(required_approvals));
                    s.insert(
                        "onReject".into(),
                        match on_reject {
                            GateRejectPolicy::Halt => json!("halt"),
                            GateRejectPolicy::RouteTo { node } => json!({ "retryFrom": node }),
                        },
                    );
                }
            }
            if !n.depends_on.is_empty() {
                s.insert("after".into(), json!(n.depends_on));
            }
            Value::Object(s)
        })
        .collect();
    let mut out = Map::new();
    out.insert("revises".into(), json!(def.id.to_string()));
    out.insert("name".into(), json!(def.name));
    if !def.description.is_empty() {
        out.insert("description".into(), json!(def.description));
    }
    if let Some(label) = &def.input_label {
        out.insert("inputLabel".into(), json!(label));
    }
    out.insert("stages".into(), Value::Array(stages));
    Value::Object(out)
}

/// Preset: implement → critique → human approval gate (reject retries the
/// implementation, bounded by [`MAX_NODE_ATTEMPTS`]).
pub fn preset_review_gate() -> WorkflowDefinition {
    WorkflowDefinition {
        id: Uuid::new_v4(),
        name: "Review gate".into(),
        description: "One agent implements, a second critiques, and you approve before the run \
                      completes. Rejecting the gate sends it back for another attempt."
            .into(),
        input_label: Some("What should be implemented?".into()),
        created_at: Timestamp::now(),
        disabled: false,
        nodes: vec![
            WorkflowNode {
                id: "implement".into(),
                name: "Implement".into(),
                depends_on: vec![],
                x: None,
                y: None,
                kind: WorkflowNodeKind::Agent {
                    agent_id: None,
                    prompt_template:
                        "Implement the following request. Be concrete and complete.\n\n{{input}}"
                            .into(),
                },
            },
            WorkflowNode {
                id: "critique".into(),
                name: "Critique".into(),
                depends_on: vec!["implement".into()],
                x: None,
                y: None,
                kind: WorkflowNodeKind::Agent {
                    agent_id: None,
                    prompt_template: "You are a critical reviewer. Assess the implementation \
                                      below against the original request. Point out gaps, risks, \
                                      and concrete improvements — be specific, not polite.\n\n\
                                      Original request:\n{{input}}\n\n\
                                      Implementation:\n{{nodes.implement.output}}"
                        .into(),
                },
            },
            WorkflowNode {
                id: "approval".into(),
                name: "Approval".into(),
                depends_on: vec!["critique".into()],
                x: None,
                y: None,
                kind: WorkflowNodeKind::Gate {
                    title_template: "Review gate: {{input}}".into(),
                    body_template: "Critique of the current implementation:\n\n\
                                    {{nodes.critique.output}}"
                        .into(),
                    required_approvals: 1,
                    on_reject: GateRejectPolicy::RouteTo { node: "implement".into() },
                },
            },
        ],
    }
}

/// Preset: three parallel attempts → a judge declares a winner.
pub fn preset_fan_out_vote() -> WorkflowDefinition {
    let attempt = |n: usize| WorkflowNode {
        id: format!("attempt-{n}"),
        name: format!("Attempt {n}"),
        depends_on: vec![],
        x: None,
        y: None,
        kind: WorkflowNodeKind::Agent {
            agent_id: None,
            prompt_template: format!(
                "Attempt the task below (you are variant {n} of 3 working independently). \
                 Prefer an approach a different variant might not take.\n\n{{{{input}}}}"
            ),
        },
    };
    WorkflowDefinition {
        id: Uuid::new_v4(),
        name: "Fan-out + vote".into(),
        description: "Three agents attempt the task in parallel; a judge compares the results \
                      and declares a winner."
            .into(),
        input_label: Some("What task should the agents attempt?".into()),
        created_at: Timestamp::now(),
        disabled: false,
        nodes: vec![
            attempt(1),
            attempt(2),
            attempt(3),
            WorkflowNode {
                id: "judge".into(),
                name: "Judge".into(),
                depends_on: vec!["attempt-1".into(), "attempt-2".into(), "attempt-3".into()],
                x: None,
                y: None,
                kind: WorkflowNodeKind::Agent {
                    agent_id: None,
                    prompt_template: "Three independent attempts at the task below follow. \
                                      Compare them, declare a single winner, and justify the \
                                      choice briefly. Quote the winning answer in full at the \
                                      end.\n\nTask:\n{{input}}\n\n\
                                      Attempt 1:\n{{nodes.attempt-1.output}}\n\n\
                                      Attempt 2:\n{{nodes.attempt-2.output}}\n\n\
                                      Attempt 3:\n{{nodes.attempt-3.output}}"
                        .into(),
                },
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, deps: &[&str], template: &str) -> WorkflowNode {
        WorkflowNode {
            id: id.into(),
            name: id.into(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            kind: WorkflowNodeKind::Agent { agent_id: None, prompt_template: template.into() },
            x: None,
            y: None,
        }
    }

    fn gate(id: &str, deps: &[&str], on_reject: GateRejectPolicy) -> WorkflowNode {
        WorkflowNode {
            id: id.into(),
            name: id.into(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            x: None,
            y: None,
            kind: WorkflowNodeKind::Gate {
                title_template: format!("Gate {id}"),
                body_template: String::new(),
                required_approvals: 1,
                on_reject,
            },
        }
    }

    fn def(nodes: Vec<WorkflowNode>) -> WorkflowDefinition {
        WorkflowDefinition {
            id: Uuid::new_v4(),
            name: "test".into(),
            description: String::new(),
            input_label: None,
            nodes,
            created_at: Timestamp::now(),
            disabled: false,
        }
    }

    fn set_status(run: &mut WorkflowRun, id: &str, status: NodeRunStatus) {
        run.node_state_mut(id).unwrap().status = status;
    }

    // ---- validate ----

    #[test]
    fn validate_accepts_both_presets() {
        validate(&preset_review_gate()).unwrap();
        validate(&preset_fan_out_vote()).unwrap();
    }

    #[test]
    fn validate_rejects_duplicate_and_malformed_ids() {
        let d = def(vec![agent("a", &[], "x"), agent("a", &[], "y")]);
        assert!(validate(&d).unwrap_err().contains("duplicate"));
        let d = def(vec![agent("bad id!", &[], "x")]);
        assert!(validate(&d).unwrap_err().contains("slug"));
    }

    #[test]
    fn validate_rejects_bad_dependencies() {
        let d = def(vec![agent("a", &["ghost"], "x")]);
        assert!(validate(&d).unwrap_err().contains("unknown stage"));
        let d = def(vec![agent("a", &["a"], "x")]);
        assert!(validate(&d).unwrap_err().contains("depends on itself"));
    }

    #[test]
    fn validate_rejects_cycles() {
        let d = def(vec![agent("a", &["b"], "x"), agent("b", &["a"], "y")]);
        assert!(validate(&d).unwrap_err().contains("cycle"));
        let d = def(vec![
            agent("a", &["c"], "x"),
            agent("b", &["a"], "y"),
            agent("c", &["b"], "z"),
        ]);
        assert!(validate(&d).unwrap_err().contains("cycle"));
    }

    #[test]
    fn duplicate_dependency_is_not_a_false_cycle() {
        // A repeated "after" entry (agent directives can emit these) must not
        // be misreported as a cycle.
        let d = def(vec![agent("a", &[], "x"), agent("b", &["a", "a"], "y")]);
        validate(&d).unwrap();
    }

    #[test]
    fn validate_rejects_unresolvable_template_refs() {
        let d = def(vec![agent("a", &[], "{{nodes.ghost.output}}")]);
        assert!(validate(&d).unwrap_err().contains("unknown stage"));
        // Referencing a node that exists but isn't upstream is also an error.
        let d = def(vec![agent("a", &[], "x"), agent("b", &[], "{{nodes.a.output}}")]);
        assert!(validate(&d).unwrap_err().contains("depend"));
    }

    #[test]
    fn validate_gate_rules() {
        let mut g = gate("g", &["a"], GateRejectPolicy::Halt);
        if let WorkflowNodeKind::Gate { required_approvals, .. } = &mut g.kind {
            *required_approvals = 0;
        }
        let d = def(vec![agent("a", &[], "x"), g]);
        assert!(validate(&d).unwrap_err().contains("approval"));

        // RouteTo must point upstream of the gate.
        let d = def(vec![
            agent("a", &[], "x"),
            agent("b", &[], "y"),
            gate("g", &["a"], GateRejectPolicy::RouteTo { node: "b".into() }),
        ]);
        assert!(validate(&d).unwrap_err().contains("upstream"));

        // A gate-only workflow is rejected.
        let d = def(vec![gate("g", &[], GateRejectPolicy::Halt)]);
        assert!(validate(&d).unwrap_err().contains("agent stage"));
    }

    // ---- scheduling ----

    #[test]
    fn ready_nodes_walks_a_chain_one_at_a_time() {
        let d = def(vec![agent("a", &[], "x"), agent("b", &["a"], "y"), agent("c", &["b"], "z")]);
        let mut run = new_run(&d, "in", "actor");
        let ready: Vec<_> = ready_nodes(&run).iter().map(|n| n.id.clone()).collect();
        assert_eq!(ready, vec!["a"]);
        set_status(&mut run, "a", NodeRunStatus::Succeeded);
        let ready: Vec<_> = ready_nodes(&run).iter().map(|n| n.id.clone()).collect();
        assert_eq!(ready, vec!["b"]);
    }

    #[test]
    fn ready_nodes_fans_out_roots_and_joins() {
        let d = preset_fan_out_vote();
        let mut run = new_run(&d, "in", "actor");
        assert_eq!(ready_nodes(&run).len(), 3);
        set_status(&mut run, "attempt-1", NodeRunStatus::Succeeded);
        set_status(&mut run, "attempt-2", NodeRunStatus::Succeeded);
        // Judge is not ready until every attempt has succeeded.
        assert!(ready_nodes(&run).is_empty() || ready_nodes(&run)[0].id != "judge");
        set_status(&mut run, "attempt-3", NodeRunStatus::Succeeded);
        let ready: Vec<_> = ready_nodes(&run).iter().map(|n| n.id.clone()).collect();
        assert_eq!(ready, vec!["judge"]);
    }

    #[test]
    fn skips_cascade_transitively_and_fail_beats_halt_in_status() {
        let d = def(vec![agent("a", &[], "x"), agent("b", &["a"], "y"), agent("c", &["b"], "z")]);
        let mut run = new_run(&d, "in", "actor");
        set_status(&mut run, "a", NodeRunStatus::Failed);
        propagate_skips(&mut run);
        assert_eq!(run.node_state("b").unwrap().status, NodeRunStatus::Skipped);
        assert_eq!(run.node_state("c").unwrap().status, NodeRunStatus::Skipped);
        assert_eq!(derive_run_status(&run), WorkflowRunStatus::Failed);
    }

    #[test]
    fn derive_run_status_matrix() {
        let d = def(vec![agent("a", &[], "x"), agent("b", &["a"], "y")]);
        let mut run = new_run(&d, "in", "actor");
        assert_eq!(derive_run_status(&run), WorkflowRunStatus::Running);
        set_status(&mut run, "a", NodeRunStatus::Running);
        assert_eq!(derive_run_status(&run), WorkflowRunStatus::Running);
        set_status(&mut run, "a", NodeRunStatus::AwaitingApproval);
        assert_eq!(derive_run_status(&run), WorkflowRunStatus::AwaitingGate);
        set_status(&mut run, "a", NodeRunStatus::Succeeded);
        set_status(&mut run, "b", NodeRunStatus::Succeeded);
        assert_eq!(derive_run_status(&run), WorkflowRunStatus::Completed);
        set_status(&mut run, "b", NodeRunStatus::Rejected);
        assert_eq!(derive_run_status(&run), WorkflowRunStatus::Halted);
    }

    // ---- gates ----

    #[test]
    fn approved_gate_unlocks_dependents() {
        let d = def(vec![
            agent("a", &[], "x"),
            gate("g", &["a"], GateRejectPolicy::Halt),
            agent("b", &["g"], "y"),
        ]);
        let mut run = new_run(&d, "in", "actor");
        set_status(&mut run, "a", NodeRunStatus::Succeeded);
        set_status(&mut run, "g", NodeRunStatus::AwaitingApproval);
        apply_gate_outcome(&mut run, "g", true);
        let ready: Vec<_> = ready_nodes(&run).iter().map(|n| n.id.clone()).collect();
        assert_eq!(ready, vec!["b"]);
    }

    #[test]
    fn rejected_halt_gate_halts_the_run() {
        let d = def(vec![agent("a", &[], "x"), gate("g", &["a"], GateRejectPolicy::Halt)]);
        let mut run = new_run(&d, "in", "actor");
        set_status(&mut run, "a", NodeRunStatus::Succeeded);
        set_status(&mut run, "g", NodeRunStatus::AwaitingApproval);
        apply_gate_outcome(&mut run, "g", false);
        propagate_skips(&mut run);
        assert_eq!(derive_run_status(&run), WorkflowRunStatus::Halted);
    }

    #[test]
    fn rejected_route_to_resets_subtree_and_caps_attempts() {
        let d = preset_review_gate();
        let mut run = new_run(&d, "in", "actor");
        // Simulate a full pass reaching the gate.
        set_status(&mut run, "implement", NodeRunStatus::Succeeded);
        set_status(&mut run, "critique", NodeRunStatus::Succeeded);
        set_status(&mut run, "approval", NodeRunStatus::AwaitingApproval);
        run.node_state_mut("implement").unwrap().message_id = Some(Uuid::new_v4());

        // Rejection 1: implement + critique + the gate reset to Pending.
        apply_gate_outcome(&mut run, "approval", false);
        assert_eq!(run.node_state("implement").unwrap().status, NodeRunStatus::Pending);
        assert_eq!(run.node_state("implement").unwrap().message_id, None);
        assert_eq!(run.node_state("critique").unwrap().status, NodeRunStatus::Pending);
        assert_eq!(run.node_state("approval").unwrap().status, NodeRunStatus::Pending);
        assert_eq!(run.node_state("approval").unwrap().attempts, 1);
        let ready: Vec<_> = ready_nodes(&run).iter().map(|n| n.id.clone()).collect();
        assert_eq!(ready, vec!["implement"]);

        // Rejection 2 still reroutes; rejection 3 halts.
        set_status(&mut run, "implement", NodeRunStatus::Succeeded);
        set_status(&mut run, "critique", NodeRunStatus::Succeeded);
        set_status(&mut run, "approval", NodeRunStatus::AwaitingApproval);
        apply_gate_outcome(&mut run, "approval", false);
        assert_eq!(run.node_state("approval").unwrap().attempts, 2);
        assert_eq!(run.node_state("approval").unwrap().status, NodeRunStatus::Pending);

        set_status(&mut run, "implement", NodeRunStatus::Succeeded);
        set_status(&mut run, "critique", NodeRunStatus::Succeeded);
        set_status(&mut run, "approval", NodeRunStatus::AwaitingApproval);
        apply_gate_outcome(&mut run, "approval", false);
        assert_eq!(run.node_state("approval").unwrap().status, NodeRunStatus::Rejected);
        propagate_skips(&mut run);
        assert_eq!(derive_run_status(&run), WorkflowRunStatus::Halted);
    }

    // ---- templates ----

    #[test]
    fn render_template_substitutes_input_and_outputs() {
        let mut outputs = HashMap::new();
        outputs.insert("a".to_string(), "ALPHA".to_string());
        let rendered = render_template(
            "Task: {{input}}\nPrev: {{nodes.a.output}}\nMissing: {{nodes.b.output}}",
            "do it",
            &outputs,
        );
        assert_eq!(rendered, "Task: do it\nPrev: ALPHA\nMissing: [no output from b]");
    }

    #[test]
    fn template_refs_are_extracted_and_deduped() {
        let refs =
            template_refs("{{nodes.a.output}} {{nodes.b-2.output}} {{nodes.a.output}} {{input}}");
        assert_eq!(refs, vec!["a".to_string(), "b-2".to_string()]);
    }

    // ---- serde ----

    #[test]
    fn definition_and_run_round_trip_with_camel_case_tags() {
        let d = preset_review_gate();
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"kind\":\"agent\""));
        assert!(json.contains("\"kind\":\"gate\""));
        assert!(json.contains("\"promptTemplate\""));
        assert!(json.contains("\"requiredApprovals\""));
        let back: WorkflowDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);

        let run = new_run(&d, "input", "actor-1");
        let json = serde_json::to_string(&run).unwrap();
        assert!(json.contains("\"status\":\"running\""));
        let back: WorkflowRun = serde_json::from_str(&json).unwrap();
        assert_eq!(back, run);
    }

    #[test]
    fn upsert_definition_adds_then_replaces_by_id() {
        let mut defs = vec![];
        let mut d = preset_review_gate();
        upsert_definition(&mut defs, d.clone());
        upsert_definition(&mut defs, d.clone());
        assert_eq!(defs.len(), 1, "same id twice keeps one copy");
        d.name = "Renamed".into();
        upsert_definition(&mut defs, d.clone());
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "Renamed");
        upsert_definition(&mut defs, preset_fan_out_vote());
        assert_eq!(defs.len(), 2);
    }

    #[test]
    fn describe_definition_reads_in_dependency_order() {
        let agent_id = Uuid::new_v4();
        let mut d = def(vec![
            gate("ok", &["fix"], GateRejectPolicy::RouteTo { node: "fix".into() }),
            agent("fix", &["scan"], "Fix {{nodes.scan.output}}"),
            agent("scan", &[], "Scan {{input}}"),
        ]);
        if let WorkflowNodeKind::Agent { agent_id: a, .. } = &mut d.nodes[1].kind {
            *a = Some(agent_id);
        }
        let text = describe_definition(&d, &|id| (id == agent_id).then(|| "Coder".to_string()));
        let scan = text.find("[scan]").unwrap();
        let fix = text.find("[fix]").unwrap();
        let ok = text.find("[ok]").unwrap();
        assert!(scan < fix && fix < ok, "{text}");
        assert!(text.contains("@Coder"));
        assert!(text.contains("primary runtime"));
        assert!(text.contains("reject retries from fix"));
        assert!(text.contains("   > Scan {{input}}"));
    }

    #[test]
    fn directive_json_round_trips_names_and_revises() {
        let d = preset_review_gate();
        let v = directive_json(&d, &|_| None);
        assert_eq!(v["revises"], d.id.to_string());
        assert_eq!(v["stages"].as_array().unwrap().len(), d.nodes.len());
        let gate = v["stages"].as_array().unwrap().iter().find(|s| s["kind"] == "gate").unwrap();
        assert_eq!(gate["onReject"]["retryFrom"], "implement");
    }

    #[test]
    fn disabled_defaults_off_and_is_omitted_when_off() {
        let d = preset_review_gate();
        let json = serde_json::to_string(&d).unwrap();
        assert!(!json.contains("disabled"), "an older peer sees the same shape");
        let mut paused = d.clone();
        paused.disabled = true;
        let back: WorkflowDefinition = serde_json::from_str(&serde_json::to_string(&paused).unwrap()).unwrap();
        assert!(back.disabled);
    }

    #[test]
    fn legacy_definitions_and_proposals_still_decode() {
        // A definition saved by the retired builder: canvas positions, no
        // `disabled`. It must load unchanged and validate.
        let legacy = r#"{"id":"6f1c1f3e-8d0b-4a39-9d55-0c1f0b8f2a11","name":"Old","description":"",
            "nodes":[{"id":"a","name":"A","dependsOn":[],"kind":{"kind":"agent","promptTemplate":"{{input}}"},"x":40,"y":80}],
            "createdAt":"2026-01-01T00:00:00Z"}"#;
        let d: WorkflowDefinition = serde_json::from_str(legacy).unwrap();
        assert!(!d.disabled);
        assert_eq!(d.nodes[0].x, Some(40));
        assert!(validate(&d).is_ok());

        // A proposal from before workflow proposals existed decodes with none.
        let p = crate::proposals::ActionProposal::new("Ship", crate::proposals::ProposalKind::Decision, "a");
        let mut v = serde_json::to_value(&p).unwrap();
        v.as_object_mut().unwrap().remove("workflow");
        let back: crate::proposals::ActionProposal = serde_json::from_value(v).unwrap();
        assert!(back.workflow.is_none());

        // A workflow proposal round-trips, and still reads as a decision.
        let wp = crate::proposals::ActionProposal::workflow("Add", "a", "body", d.clone());
        let json = serde_json::to_string(&wp).unwrap();
        assert!(json.contains("\"kind\":\"decision\""));
        let back: crate::proposals::ActionProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(back.workflow.as_deref(), Some(&d));
    }
}
