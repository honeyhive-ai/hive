//! Workspace agents — ported from `HiveModels.swift` (`WorkspaceAgent`). An
//! agent is a named participant bound to a runtime and owned by an actor; the
//! `role` is a short human-set specialty shared with the whole workspace.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::time_util::Timestamp;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAgent {
    pub id: Uuid,
    pub name: String,
    pub runtime_id: String,
    #[serde(default)]
    pub owner_actor_id: String,
    /// Short human-set role/specialty (e.g. "Reviewer", "Researcher"),
    /// surfaced to every participant so the workspace shares a roster of who
    /// does what. Empty when unset.
    #[serde(default)]
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_color_hex: Option<String>,
    /// Optional avatar as a bounded `data:image/…` URL. Rides in the synced
    /// roster (`AgentRosterUpdated`), so every member renders the agent's
    /// picture. Size-capped at the edit boundary. `None` = fall back to
    /// `avatar_color_hex`, then to initials-on-color.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    /// Host this agent runs on (spec §12.4) — a device or worker id. Empty =
    /// the owner's device (default). A worker id makes the agent detached.
    #[serde(default)]
    pub host_id: String,
    #[serde(default)]
    pub created_at: Timestamp,
}

impl WorkspaceAgent {
    pub fn new(name: impl Into<String>, runtime_id: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            runtime_id: runtime_id.into(),
            owner_actor_id: String::new(),
            role: String::new(),
            avatar_color_hex: None,
            avatar_url: None,
            host_id: String::new(),
            created_at: Timestamp::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_minimal_agent_with_defaults() {
        let json = r#"{
            "id":"00000000-0000-0000-0000-0000000000aa",
            "name":"Scout",
            "runtimeId":"local-qwen"
        }"#;
        let agent: WorkspaceAgent = serde_json::from_str(json).unwrap();
        assert_eq!(agent.name, "Scout");
        assert_eq!(agent.runtime_id, "local-qwen");
        assert_eq!(agent.role, "");
        assert_eq!(agent.owner_actor_id, "");
    }
}
