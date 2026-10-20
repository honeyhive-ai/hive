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
//! synced in looks the same to the phone as one this box produced. Only
//! `message.delta` and `turn.status` come straight from the worker.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use hive_core::{ChatSession, MessageRole, ProposalStatus};
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
    /// message id → still streaming
    messages: HashMap<Uuid, bool>,
    /// proposal id → status
    proposals: HashMap<Uuid, ProposalStatus>,
}

/// Something the phone (or ntfy) should hear about.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    MessageCreated { chat: Uuid, message: Uuid },
    MessageDone { chat: Uuid, message: Uuid },
    ProposalCreated { chat: Uuid, proposal: Uuid },
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
        messages: s.messages.iter().map(|m| (m.id, m.is_streaming)).collect(),
        proposals: s.proposals.iter().map(|p| (p.id, p.status)).collect(),
    }
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
                    Some(true) if !m.is_streaming => {
                        touched = true;
                        out.push(Change::MessageDone { chat: s.id, message: m.id });
                    }
                    _ => {}
                }
            }
            if before.messages.len() > s.messages.len() {
                touched = true; // a message was removed
            }
            for p in &s.proposals {
                match before.proposals.get(&p.id) {
                    None => {
                        touched = true;
                        out.push(Change::ProposalCreated { chat: s.id, proposal: p.id });
                    }
                    Some(st) if *st != p.status => touched = true,
                    _ => {}
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
    fn first_diff_seeds_silently_and_new_chat_is_updated() {
        let mut d = Differ::default();
        let a = chat();
        assert!(d.diff(std::slice::from_ref(&a)).is_empty());
        let b = chat();
        let c = d.diff(&[a.clone(), b.clone()]);
        assert_eq!(c, vec![Change::ChatUpdated { chat: b.id }]);
    }
}
