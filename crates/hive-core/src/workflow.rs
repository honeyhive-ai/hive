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
    /// Canvas position (px) set by dragging in the builder; `None` means the
    /// editor auto-lays the node out. Purely presentational.
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
}
