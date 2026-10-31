//! The SSE event hub and the projection differ that feeds it.
//!
//! [`Hub`] numbers every event, keeps the last [`RING`] of them for
//! `Last-Event-ID` replay, and fans them out over a broadcast channel.
//!
//! Most events are derived, not pushed: after any change — a sync round that
//! pulled events, a phone write, a worker turn starting or finishing — the
//! [`Differ`] compares the projected chats with what it saw last time and
//! emits `message.created` / `message.done` / `proposal.created` /
//! `chat.updated` for the difference. So a reply streamed on the desktop and
//! synced in looks the same to the phone as one this box produced. The
//! worker's own `message.delta` and `turn.status` come straight from it; a
//! reply streaming elsewhere (the desktop) becomes `message.delta` as its
//! checkpointed text syncs in ([`Change::MessageDelta`]).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use hive_core::{ActionProposal, ChatSession, MessageRole, NodeRunStatus, ProposalStatus};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use uuid::Uuid;

/// Events kept for `Last-Event-ID` replay.
pub const RING: usize = 1024;

#[derive(Debug, Clone)]
pub struct SseEvent {
    pub id: u64,
    pub name: &'static str,
    pub data: Value,
}

pub struct Hub {
    tx: broadcast::Sender<SseEvent>,
    /// Replay buffer + id counter; held while sending so a subscriber never
    /// sees an event both replayed and live, or misses one in between.
    ring: Mutex<(u64, VecDeque<SseEvent>)>,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(RING);
        Self { tx, ring: Mutex::new((0, VecDeque::with_capacity(RING))) }
    }

    pub fn emit(&self, name: &'static str, data: Value) -> u64 {
        let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        ring.0 += 1;
        let ev = SseEvent { id: ring.0, name, data };
        if ring.1.len() == RING {
            ring.1.pop_front();
        }
        ring.1.push_back(ev.clone());
        let _ = self.tx.send(ev);
        ring.0
    }

    /// Subscribe, first returning the buffered events after `last_id` (all of
    /// them are dropped if `last_id` is older than the buffer — best effort).
    pub fn subscribe(&self, last_id: Option<u64>) -> (Vec<SseEvent>, broadcast::Receiver<SseEvent>) {
        let ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let backlog = match last_id {
            Some(after) => ring.1.iter().filter(|e| e.id > after).cloned().collect(),
            None => Vec::new(),
        };
        (backlog, self.tx.subscribe())
    }
}

/// What the differ remembers about one chat.
#[derive(Debug, Default, Clone)]
struct ChatSnap {
    title: String,
    /// message id → (still streaming, body length in bytes)
    messages: HashMap<Uuid, (bool, usize)>,
    /// proposal id → what the phone shows of it
    proposals: HashMap<Uuid, ProposalSnap>,
    /// Workflow gates awaiting approval: (run id, node id, attempt).
    gates: HashSet<(Uuid, String, u32)>,
}

/// The parts of a proposal whose change is a `proposal.updated`: its status,
/// votes, refinement thread, revision link and inbox visibility.
#[derive(Debug, Clone, PartialEq)]
struct ProposalSnap {
    status: ProposalStatus,
    votes: Vec<(String, bool)>,
    comments: usize,
    superseded: bool,
    dismissed: bool,
}

fn proposal_snap(p: &ActionProposal) -> ProposalSnap {
    ProposalSnap {
        status: p.status,
        votes: p.approvals.iter().map(|a| (a.actor_id.clone(), a.approved)).collect(),
        comments: p.refinements.len(),
        superseded: p.superseded_by.is_some(),
        dismissed: p.dismissed,
    }
}

/// Something the phone (or ntfy) should hear about.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    MessageCreated { chat: Uuid, message: Uuid },
    MessageDone { chat: Uuid, message: Uuid },
    /// A still-streaming reply's body grew past byte `from` (its text so far
    /// synced in from the device writing it).
    MessageDelta { chat: Uuid, message: Uuid, from: usize },
    ProposalCreated { chat: Uuid, proposal: Uuid },
    /// A known proposal's votes, thread, status or revision link changed.
    ProposalUpdated { chat: Uuid, proposal: Uuid },
    /// A workflow run's gate started waiting for approval (each attempt of a
    /// rerouted gate counts again).
    WorkflowApproval { chat: Uuid, run: Uuid, node: String },
    ChatUpdated { chat: Uuid },
}

#[derive(Default)]
pub struct Differ {
    seeded: bool,
    chats: HashMap<Uuid, ChatSnap>,
}

fn snap(s: &ChatSession) -> ChatSnap {
    ChatSnap {
        title: s.title.clone(),
        messages: s.messages.iter().map(|m| (m.id, (m.is_streaming, m.body.len()))).collect(),
        proposals: s.proposals.iter().map(|p| (p.id, proposal_snap(p))).collect(),
        gates: awaiting_gates(s).collect(),
    }
}

fn awaiting_gates(s: &ChatSession) -> impl Iterator<Item = (Uuid, String, u32)> + '_ {
    s.workflow_runs.iter().flat_map(|r| {
        r.nodes
            .iter()
            .filter(|n| n.status == NodeRunStatus::AwaitingApproval)
            .map(move |n| (r.id, n.node_id.clone(), n.attempts))
    })
}

/// A proposal still waiting for review.
pub fn is_open(p: &ActionProposal) -> bool {
    p.status == ProposalStatus::Open && !p.dismissed && p.superseded_by.is_none()
}

fn is_reply(role: MessageRole) -> bool {
    matches!(role, MessageRole::Assistant | MessageRole::Agent)
}

impl Differ {
    /// Remember `sessions` without reporting anything (startup).
    pub fn seed(&mut self, sessions: &[ChatSession]) {
        self.chats = sessions.iter().map(|s| (s.id, snap(s))).collect();
        self.seeded = true;
    }

    /// The changes since the last call, in transcript order per chat.
    pub fn diff(&mut self, sessions: &[ChatSession]) -> Vec<Change> {
        if !self.seeded {
            self.seed(sessions);
            return Vec::new();
        }
        let mut out = Vec::new();
        for s in sessions {
            let before = self.chats.get(&s.id).cloned().unwrap_or_default();
            let mut touched = !self.chats.contains_key(&s.id) || before.title != s.title;
            for m in &s.messages {
                match before.messages.get(&m.id) {
                    None => {
                        touched = true;
                        out.push(Change::MessageCreated { chat: s.id, message: m.id });
                        if is_reply(m.role) && !m.is_streaming {
                            out.push(Change::MessageDone { chat: s.id, message: m.id });
                        }
                    }
                    Some((true, _)) if !m.is_streaming => {
                        touched = true;
                        out.push(Change::MessageDone { chat: s.id, message: m.id });
                    }
                    Some(&(true, from)) if m.body.len() > from => {
                        out.push(Change::MessageDelta { chat: s.id, message: m.id, from });
                    }
                    _ => {}
                }
            }
            if before.messages.len() > s.messages.len() {
                touched = true; // a message was removed
            }
            for p in &s.proposals {
                match before.proposals.get(&p.id) {
                    // Only open work is "created" for the phone's review inbox;
                    // one first seen already settled (synced in after an outage)
                    // just updates its chat.
                    None if is_open(p) => {
                        touched = true;
                        out.push(Change::ProposalCreated { chat: s.id, proposal: p.id });
                    }
                    None => touched = true,
                    Some(before) => {
                        let now = proposal_snap(p);
                        if *before != now {
                            // A status change still touches the chat (as before);
                            // a vote or note alone is a proposal event only.
                            touched |= before.status != now.status;
                            out.push(Change::ProposalUpdated { chat: s.id, proposal: p.id });
                        }
                    }
                }
            }
            for (run, node, attempt) in awaiting_gates(s) {
                if !before.gates.contains(&(run, node.clone(), attempt)) {
                    out.push(Change::WorkflowApproval { chat: s.id, run, node });
                }
            }
            if touched {
                out.push(Change::ChatUpdated { chat: s.id });
            }
            self.chats.insert(s.id, snap(s));
        }
        out
    }
}

/// `{"message": …}` etc. — the payload shapes of the contract's events.
pub fn wrap(key: &str, v: Value) -> Value {
    json!({ key: v })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::ChatMessage;

    fn chat() -> ChatSession {
        ChatSession::new("t", Uuid::new_v4(), "rt")
    }

    #[test]
    fn replay_after_last_event_id() {
        let hub = Hub::new();
        hub.emit("a", json!(1));
        let second = hub.emit("b", json!(2));
        hub.emit("c", json!(3));
        let (backlog, _rx) = hub.subscribe(Some(second - 1));
        assert_eq!(backlog.iter().map(|e| e.name).collect::<Vec<_>>(), vec!["b", "c"]);
        let (none, _rx) = hub.subscribe(None);
        assert!(none.is_empty());
    }

    #[test]
    fn differ_reports_created_streaming_then_done() {
        let mut d = Differ::default();
        let mut s = chat();
        d.seed(std::slice::from_ref(&s));
        let mut m = ChatMessage::new(MessageRole::Assistant, "coder", "");
        m.is_streaming = true;
        let mid = m.id;
        s.messages.push(m);
        let c = d.diff(std::slice::from_ref(&s));
        assert_eq!(c, vec![Change::MessageCreated { chat: s.id, message: mid }, Change::ChatUpdated { chat: s.id }]);
        assert!(d.diff(std::slice::from_ref(&s)).is_empty(), "no change, no events");
        s.messages[0].is_streaming = false;
        s.messages[0].body = "hi".into();
        let c = d.diff(std::slice::from_ref(&s));
        assert_eq!(c, vec![Change::MessageDone { chat: s.id, message: mid }, Change::ChatUpdated { chat: s.id }]);
    }

    /// A reply streaming on another device: each checkpoint that syncs in is
    /// a delta from where the last one ended; completion is a done, not a delta.
    #[test]
    fn differ_reports_synced_streaming_text_as_deltas() {
        let mut d = Differ::default();
        let mut s = chat();
        let mut m = ChatMessage::new(MessageRole::Assistant, "Hive", "");
        m.is_streaming = true;
        let mid = m.id;
        s.messages.push(m);
        d.seed(std::slice::from_ref(&s));
        s.messages[0].body = "Hel".into();
        assert_eq!(d.diff(std::slice::from_ref(&s)), vec![Change::MessageDelta { chat: s.id, message: mid, from: 0 }]);
        s.messages[0].body = "Hello".into();
        assert_eq!(d.diff(std::slice::from_ref(&s)), vec![Change::MessageDelta { chat: s.id, message: mid, from: 3 }]);
        assert!(d.diff(std::slice::from_ref(&s)).is_empty());
        s.messages[0].body = "Hello there".into();
        s.messages[0].is_streaming = false;
        assert_eq!(
            d.diff(std::slice::from_ref(&s)),
            vec![Change::MessageDone { chat: s.id, message: mid }, Change::ChatUpdated { chat: s.id }]
        );
    }

    #[test]
    fn synced_in_complete_reply_is_created_and_done() {
        let mut d = Differ::default();
        let mut s = chat();
        d.seed(std::slice::from_ref(&s));
        let m = ChatMessage::new(MessageRole::Assistant, "coder", "done already");
        let mid = m.id;
        s.messages.push(m);
        let user = ChatMessage::new(MessageRole::User, "me", "hi");
        let uid = user.id;
        s.messages.push(user);
        let c = d.diff(std::slice::from_ref(&s));
        assert_eq!(
            c,
            vec![
                Change::MessageCreated { chat: s.id, message: mid },
                Change::MessageDone { chat: s.id, message: mid },
                Change::MessageCreated { chat: s.id, message: uid },
                Change::ChatUpdated { chat: s.id },
            ]
        );
    }

    #[test]
    fn settled_proposal_first_seen_is_not_created() {
        let mut d = Differ::default();
        let mut s = chat();
        d.seed(std::slice::from_ref(&s));
        let mut settled = ActionProposal::new("old", hive_core::ProposalKind::FileDiff, "");
        settled.status = ProposalStatus::Applied;
        let mut hidden = ActionProposal::new("hidden", hive_core::ProposalKind::FileDiff, "");
        hidden.dismissed = true;
        let open = ActionProposal::new("new", hive_core::ProposalKind::FileDiff, "");
        let oid = open.id;
        s.proposals = vec![settled, hidden, open];
        let c = d.diff(std::slice::from_ref(&s));
        assert_eq!(c, vec![Change::ProposalCreated { chat: s.id, proposal: oid }, Change::ChatUpdated { chat: s.id }]);
    }

    #[test]
    fn votes_and_notes_are_proposal_updates_status_also_touches_the_chat() {
        let mut d = Differ::default();
        let mut s = chat();
        let p = ActionProposal::new("p", hive_core::ProposalKind::Decision, "");
        let pid = p.id;
        s.proposals.push(p);
        d.seed(std::slice::from_ref(&s));
        s.proposals[0].add_refinement(hive_core::proposals::ProposalRefinement {
            id: Uuid::new_v4(),
            actor_id: "a".into(),
            role: hive_core::WorkspaceRole::Contributor,
            text: "hm".into(),
            request_changes: false,
            created_at: hive_core::Timestamp::now(),
        });
        assert_eq!(d.diff(std::slice::from_ref(&s)), vec![Change::ProposalUpdated { chat: s.id, proposal: pid }]);
        s.proposals[0].cast_vote(hive_core::proposals::ProposalApproval {
            actor_id: "a".into(),
            role: hive_core::WorkspaceRole::Contributor,
            approved: true,
            created_at: hive_core::Timestamp::now(),
        });
        assert_eq!(
            d.diff(std::slice::from_ref(&s)),
            vec![Change::ProposalUpdated { chat: s.id, proposal: pid }, Change::ChatUpdated { chat: s.id }]
        );
        assert!(d.diff(std::slice::from_ref(&s)).is_empty());
    }

    #[test]
    fn workflow_gate_awaiting_approval_is_reported_once_per_attempt() {
        use hive_core::workflow;
        let mut d = Differ::default();
        let mut s = chat();
        let def = workflow::preset_review_gate();
        let mut run = workflow::new_run(&def, "build it", "u1");
        s.workflow_runs.push(run.clone());
        d.seed(std::slice::from_ref(&s));
        let gate = run.node_state_mut("approval").unwrap();
        gate.status = NodeRunStatus::AwaitingApproval;
        s.workflow_runs[0] = run.clone();
        let c = d.diff(std::slice::from_ref(&s));
        assert_eq!(c, vec![Change::WorkflowApproval { chat: s.id, run: run.id, node: "approval".into() }]);
        assert!(d.diff(std::slice::from_ref(&s)).is_empty(), "still waiting: not reported again");
        // Rejected and rerouted: the next attempt waits again → reported again.
        let gate = run.node_state_mut("approval").unwrap();
        gate.attempts = 1;
        s.workflow_runs[0] = run.clone();
        let c = d.diff(std::slice::from_ref(&s));
        assert_eq!(c, vec![Change::WorkflowApproval { chat: s.id, run: run.id, node: "approval".into() }]);
        // Seeding an already-waiting gate reports nothing (restart).
        let mut fresh = Differ::default();
        fresh.seed(std::slice::from_ref(&s));
        assert!(fresh.diff(std::slice::from_ref(&s)).is_empty());
    }

    #[test]
    fn first_diff_seeds_silently_and_new_chat_is_updated() {
        let mut d = Differ::default();
        let a = chat();
        assert!(d.diff(std::slice::from_ref(&a)).is_empty());
        let b = chat();
        let c = d.diff(&[a.clone(), b.clone()]);
        assert_eq!(c, vec![Change::ChatUpdated { chat: b.id }]);
    }
}
