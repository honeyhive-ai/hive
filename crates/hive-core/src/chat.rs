//! Transcript models — ported from `HiveModels.swift` (`ChatMessage` and
//! friends). Optional/defaulted fields use `#[serde(default)]` so older stored
//! JSON decodes unchanged, mirroring the Swift `decodeIfPresent` decoders.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::identity::{ActorIdentity, ActorKind};
use crate::time_util::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptEventKind {
    Message,
    Proposal,
    ExecutionResult,
    HandoffSummary,
}

impl Default for TranscriptEventKind {
    fn default() -> Self {
        Self::Message
    }
}

/// First-class tool-call block emitted by an assistant message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatToolCall {
    pub id: String,
    pub name: String,
    pub input_json: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
}

/// First-class tool-result block returned by a user-role message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatToolResult {
    pub call_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

/// A single emoji reaction on a message by one actor (human or agent).
/// Identity is `(actor_id, emoji)` so toggling the same emoji is idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageReaction {
    pub emoji: String,
    pub actor_id: String,
    pub actor_display_name: String,
    #[serde(default = "ActorKind::human")]
    pub actor_kind: ActorKind,
    #[serde(default)]
    pub created_at: Timestamp,
}

impl MessageReaction {
    /// Two reactions are "the same vote" when the same actor used the same
    /// emoji — used for toggling and dedupe (timestamp/name ignored).
    pub fn is_same_vote(&self, other: &MessageReaction) -> bool {
        self.actor_id == other.actor_id && self.emoji == other.emoji
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: Uuid,
    pub role: MessageRole,
    pub author: String,
    pub body: String,
    #[serde(default)]
    pub created_at: Timestamp,
    #[serde(default)]
    pub is_streaming: bool,
    #[serde(default)]
    pub kind: TranscriptEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_proposal_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_identity: Option<ActorIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_persona_id: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ChatToolCall>,
    #[serde(default)]
    pub tool_results: Vec<ChatToolResult>,
    #[serde(default)]
    pub reactions: Vec<MessageReaction>,
    #[serde(default)]
    pub reaction_options: Vec<String>,
}

impl ChatMessage {
    /// Convenience constructor for the common case (role + author + body).
    pub fn new(role: MessageRole, author: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            role,
            author: author.into(),
            body: body.into(),
            created_at: Timestamp::now(),
            is_streaming: false,
            kind: TranscriptEventKind::Message,
            related_proposal_id: None,
            related_agent_id: None,
            actor_identity: None,
            runtime_id: None,
            runtime_label: None,
            agent_persona_id: None,
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            reactions: Vec::new(),
            reaction_options: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_minimal_legacy_message_with_defaults() {
        // A message written before tool/reaction fields existed must still
        // decode, with the new collections defaulting to empty.
        let json = r#"{
            "id": "00000000-0000-0000-0000-000000000001",
            "role": "user",
            "author": "Mara",
            "body": "hello",
            "createdAt": "2026-01-01T00:00:00Z"
        }"#;
        let msg: ChatMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.body, "hello");
        assert!(!msg.is_streaming);
        assert_eq!(msg.kind, TranscriptEventKind::Message);
        assert!(msg.tool_calls.is_empty());
        assert!(msg.reactions.is_empty());
    }

    #[test]
    fn reaction_same_vote_ignores_timestamp_and_name() {
        let a = MessageReaction {
            emoji: "👍".into(),
            actor_id: "u1".into(),
            actor_display_name: "A".into(),
            actor_kind: ActorKind::Human,
            created_at: Timestamp::epoch(),
        };
        let b = MessageReaction {
            emoji: "👍".into(),
            actor_id: "u1".into(),
            actor_display_name: "different".into(),
            actor_kind: ActorKind::Human,
            created_at: Timestamp::now(),
        };
        assert!(a.is_same_vote(&b));
    }
}
