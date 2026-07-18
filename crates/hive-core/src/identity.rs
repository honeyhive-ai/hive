//! Actor and membership identity — ported from `HiveModels.swift`
//! (`ActorKind`, `ActorIdentity`, `WorkspaceRole`, `WorkspaceMember`,
//! `ActorStamp`, `HiveUserProfile`). Cryptographic identity (account/device
//! keys, signing) lands in Phase 2; the optional `account_id`/`device_id`
//! fields are carried here now so the wire shape is stable.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::time_util::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActorKind {
    Human,
    Assistant,
    Agent,
    System,
}

impl ActorKind {
    /// Default used by `#[serde(default = ...)]` on reaction/actor fields.
    pub fn human() -> Self {
        ActorKind::Human
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HiveUserProfile {
    pub display_name: String,
    pub handle: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorIdentity {
    pub id: String,
    pub display_name: String,
    pub kind: ActorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<Uuid>,
    /// Git email for commit attribution. Rides on the identity so a host running
    /// an agent on a teammate's behalf can credit them as the commit author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_email: Option<String>,
    /// This actor's device X25519 key-agreement public key (raw 32 bytes). Rides
    /// in the synced roster so an owner can seal a rotated workspace key to every
    /// member's device when revoking access. See `crate::e2ee`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_agreement_public: Option<Vec<u8>>,
}

impl ActorIdentity {
    pub fn new(id: impl Into<String>, display_name: impl Into<String>, kind: ActorKind) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            kind,
            account_id: None,
            device_id: None,
            git_email: None,
            key_agreement_public: None,
        }
    }
}

/// Governance/permission role — drives authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceRole {
    Owner,
    Admin,
    Contributor,
    Viewer,
}

impl WorkspaceRole {
    /// Numeric rank for quorum/role-floor comparisons (higher = more
    /// authority). Mirrors the Swift `roleRank` ordering.
    pub fn rank(self) -> u8 {
        match self {
            WorkspaceRole::Owner => 3,
            WorkspaceRole::Admin => 2,
            WorkspaceRole::Contributor => 1,
            WorkspaceRole::Viewer => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMember {
    pub id: String,
    pub actor: ActorIdentity,
    pub role: WorkspaceRole,
    /// Optional functional title/specialty (e.g. "Lead", "QA"), distinct from
    /// the governance `role`. Empty when unset (flat/equal team).
    #[serde(default)]
    pub title: String,
    /// Per-workspace join index (1-based, unique). Disambiguates members that
    /// share a display name — surfaced as `Name #N`, matchable as `@Name#N`.
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub joined_at: Timestamp,
}

/// Who stamped an event, and when — attached to envelopes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorStamp {
    pub actor: ActorIdentity,
    #[serde(default)]
    pub recorded_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_identity_decodes_without_crypto_fields() {
        let json = r#"{"id":"u1","displayName":"Mara","kind":"human"}"#;
        let actor: ActorIdentity = serde_json::from_str(json).unwrap();
        assert_eq!(actor.kind, ActorKind::Human);
        assert!(actor.account_id.is_none());
        assert!(actor.device_id.is_none());
    }

    #[test]
    fn member_decodes_without_title() {
        let json = r#"{
            "id":"m1",
            "actor":{"id":"u1","displayName":"Mara","kind":"human"},
            "role":"owner"
        }"#;
        let member: WorkspaceMember = serde_json::from_str(json).unwrap();
        assert_eq!(member.role, WorkspaceRole::Owner);
        assert_eq!(member.title, "");
    }

    #[test]
    fn role_rank_orders_owner_highest() {
        assert!(WorkspaceRole::Owner.rank() > WorkspaceRole::Admin.rank());
        assert!(WorkspaceRole::Contributor.rank() > WorkspaceRole::Viewer.rank());
    }
}
