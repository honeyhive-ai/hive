//! `ChatSession` — the projected workspace conversation state.
//!
//! A lean spine over the workspace subsystems (runtime catalog, leases, key
//! rotation, review queue, artifacts, vault exports, …), grown as features need
//! them. The event log + projector are the source of truth.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::WorkspaceAgent;
use crate::chat::ChatMessage;
use crate::identity::WorkspaceMember;
use crate::proposals::ActionProposal;
use crate::skills::SkillProfile;
use crate::time_util::Timestamp;
use crate::vault::VaultSource;
use crate::workflow::{WorkflowDefinition, WorkflowRun};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSession {
    pub id: Uuid,
    pub title: String,
    pub workspace_id: Uuid,
    /// The runtime id that drives the primary (non-agent) turns. Resolved
    /// against the configured runtime catalog.
    pub runtime_id: String,
    #[serde(default)]
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub members: Vec<WorkspaceMember>,
    /// Workspace-canonical agent roster; each chat picks participants from it.
    #[serde(default)]
    pub workspace_agents: Vec<WorkspaceAgent>,
    /// Subset of `workspace_agents.id` active in this chat.
    #[serde(default)]
    pub participant_agent_ids: Vec<Uuid>,
    /// Skills loaded into this session; their instructions are injected into
    /// participants' system prompts.
    #[serde(default)]
    pub loaded_skills: Vec<SkillProfile>,
    /// Action proposals awaiting review / quorum.
    #[serde(default)]
    pub proposals: Vec<ActionProposal>,
    /// Reference-material vault sources mounted into the workspace.
    #[serde(default)]
    pub vault_sources: Vec<VaultSource>,
    /// Agentic workflow definitions available in this chat.
    #[serde(default)]
    pub workflow_definitions: Vec<WorkflowDefinition>,
    /// Workflow runs (live + finished) executed in this chat.
    #[serde(default)]
    pub workflow_runs: Vec<WorkflowRun>,
    /// Soft-delete flag — archived chats are hidden from the sidebar by
    /// default but their events remain (hard delete removes the events).
    #[serde(default)]
    pub archived: bool,
    /// Account id of the device/user that created this chat. Owns the primary
    /// (non-agent) runtime for cross-device dispatch: only the creator's device
    /// answers un-`@mentioned` messages. Empty on legacy sessions (→ local).
    #[serde(default)]
    pub creator_actor_id: String,
    #[serde(default)]
    pub created_at: Timestamp,
    #[serde(default)]
    pub updated_at: Timestamp,
}

impl ChatSession {
    pub fn new(title: impl Into<String>, workspace_id: Uuid, runtime_id: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: Uuid::new_v4(),
            title: title.into(),
            workspace_id,
            runtime_id: runtime_id.into(),
            messages: Vec::new(),
            members: Vec::new(),
            workspace_agents: Vec::new(),
            participant_agent_ids: Vec::new(),
            loaded_skills: Vec::new(),
            proposals: Vec::new(),
            vault_sources: Vec::new(),
            workflow_definitions: Vec::new(),
            workflow_runs: Vec::new(),
            archived: false,
            creator_actor_id: String::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Most-recent message timestamp, or the session's creation time when
    /// empty. Mirrors Swift's `lastActivityAt`.
    pub fn last_activity_at(&self) -> Timestamp {
        self.messages
            .last()
            .map(|m| m.created_at)
            .unwrap_or(self.created_at)
    }

    pub fn message_index(&self, id: Uuid) -> Option<usize> {
        self.messages.iter().position(|m| m.id == id)
    }
}
