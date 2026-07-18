//! `hive-core` — pure, platform-agnostic domain logic: models, config, context
//! budgeting, git context, crypto/signing, MCP wire types, and the event-sourced
//! session projector. No IO — orchestration lives in `hive-runtime`.

pub mod agent;
pub mod authorization;
pub mod chat;
pub mod config;
pub mod context_budget;
pub mod crypto;
pub mod e2ee;
pub mod events;
pub mod git_context;
pub mod identity;
pub mod policy;
pub mod proposals;
pub mod runtime;
pub mod schedule;
pub mod session;
pub mod skills;
pub mod time_util;
pub mod vault;
pub mod workflow;

// Flat re-exports of the core domain types for ergonomic downstream use.
pub use agent::WorkspaceAgent;
pub use authorization::{evaluate as authorize, AuthzDecision, AuthzReason};
pub use chat::{
    ChatMessage, ChatToolCall, ChatToolResult, MessageReaction, MessageRole, TranscriptEventKind,
};
pub use crypto::{
    sign_envelope, verify, verify_envelope, CryptoError, DeviceCertificate, DeviceIdentity,
    HumanAccount, Platform, SigningKeypair,
};
pub use e2ee::{
    derive_workspace_key, generate_workspace_key, open as seal_open, open_symmetric, seal,
    seal_symmetric, KeyAgreementKeypair, SealedBlob, SealedEnvelope, WorkspaceKeyRotation,
};
pub use events::{
    project, EventScope, MemberRoleChange, SessionEvent, SessionEventEnvelope,
};
pub use git_context::{GitContextReader, GitFileChange, GitFileDiff, GitSnapshot};
pub use identity::{
    ActorIdentity, ActorKind, ActorStamp, HiveUserProfile, WorkspaceMember, WorkspaceRole,
};
pub use policy::{PermissionPolicy, PermissionScope, Workspace};
pub use proposals::{ActionProposal, ProposalApproval, ProposalKind, ProposalStatus};
pub use runtime::{ModelProviderKind, RuntimeCapabilities, RuntimeLocation, RuntimeTarget};
pub use schedule::ScheduleTrigger;
pub use session::ChatSession;
pub use skills::SkillProfile;
pub use time_util::Timestamp;
pub use vault::VaultSource;
pub use workflow::{
    GateRejectPolicy, NodeRunState, NodeRunStatus, WorkflowDefinition, WorkflowNode,
    WorkflowNodeKind, WorkflowRun, WorkflowRunStatus,
};

/// Crate version string, surfaced through the IPC `get_app_info` command.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_non_empty() {
        assert!(!VERSION.is_empty());
    }
}
