//! `ChatSession` — the projected workspace conversation state.
//!
//! A lean spine over the workspace subsystems (runtime catalog, leases, key
//! rotation, review queue, artifacts, vault exports, …), grown as features need
//! them. The event log + projector are the source of truth.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::agent::WorkspaceAgent;
use crate::channel::Channel;
use crate::workspace_runtime::WorkspaceRuntime;
use crate::chat::ChatMessage;
use crate::identity::{WorkspaceMember, WorkspaceRole};
use crate::proposals::ActionProposal;
use crate::skills::SkillProfile;
use crate::time_util::Timestamp;
use crate::vault::MountedVault;
use crate::workflow::{WorkflowDefinition, WorkflowRun};
use crate::workspace_host::WorkspaceHost;

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
    pub vault_sources: Vec<MountedVault>,
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
    /// Turn-answer claims: `trigger user-message id → winning device id`. The
    /// first `TurnClaimed` in canonical fold order wins, so every device agrees
    /// which one answers a given turn — preventing account-scoped double-answers.
    /// Ephemeral coordination state; empty on legacy sessions.
    #[serde(default)]
    pub turn_claims: BTreeMap<Uuid, Uuid>,
    /// Channels defined on the workspace. Only populated when this session IS
    /// the workspace-config log (§11); empty on ordinary chats.
    #[serde(default)]
    pub channels: Vec<Channel>,
    /// This chat's channel assignment (channel id). Empty = unfiled (pre-channel
    /// or the config log itself).
    #[serde(default)]
    pub channel_id: String,
    /// Workspace-owned runtimes (spec §12.5) — detached/headless agents run on
    /// these. Populated on the config log; overlaid onto chats like other config.
    #[serde(default)]
    pub workspace_runtimes: Vec<WorkspaceRuntime>,
    /// Workspace hosts (spec §12.4) — devices + workers agents run on.
    #[serde(default)]
    pub workspace_hosts: Vec<WorkspaceHost>,
    #[serde(default)]
    pub created_at: Timestamp,
    #[serde(default)]
    pub updated_at: Timestamp,
}

impl ChatSession {
    /// The workspace role `actor_id` holds in this session's projected roster,
    /// or the safe floor (`Viewer`) if they're not a member. Mirrors the
    /// authoring-side floor (`ChatService::actor_role`) so ingest/projection
    /// authz and author-time authz agree on a non-member's privileges.
    pub fn role_of(&self, actor_id: &str) -> WorkspaceRole {
        self.members
            .iter()
            .find(|m| m.id == actor_id)
            .map(|m| m.role)
            .unwrap_or(WorkspaceRole::Viewer)
    }

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
            turn_claims: BTreeMap::new(),
            channels: Vec::new(),
            channel_id: String::new(),
            workspace_runtimes: Vec::new(),
            workspace_hosts: Vec::new(),
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
