//! Authorization — delta-validating role gate, ported from `Authorization.swift`.
//!
//! Each *local* state change (a `SessionEvent` about to be appended) is checked
//! against the acting member's [`WorkspaceRole`]: governance events (membership,
//! archive/delete) require admin/owner; content (messages, reactions, proposals,
//! agents, skills) requires contributor+; and the **last owner** can't be removed
//! or demoted. Synced foreign events are authorized by their origin device, so
//! this gates the originating side.

use crate::events::{MemberRoleChange, SessionEvent};
use crate::identity::WorkspaceRole;
use crate::session::ChatSession;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthzReason {
    Allowed,
    InsufficientRole,
    TargetIsLastOwner,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthzDecision {
    pub allowed: bool,
    pub reason: AuthzReason,
    pub summary: String,
}

impl AuthzDecision {
    fn allow() -> Self {
        Self {
            allowed: true,
            reason: AuthzReason::Allowed,
            summary: String::new(),
        }
    }
    fn deny(reason: AuthzReason, summary: impl Into<String>) -> Self {
        Self {
            allowed: false,
            reason,
            summary: summary.into(),
        }
    }
}

/// High-frequency streaming events that ride an already-authorized append and so
/// skip the (projection-loading) check.
pub fn requires_authz(event: &SessionEvent) -> bool {
    !matches!(
        event,
        SessionEvent::MessageChunkReceived { .. } | SessionEvent::MessageCompleted { .. }
    )
}

/// The minimum role allowed to emit `event`.
pub fn min_role_for(event: &SessionEvent) -> WorkspaceRole {
    match event {
        // governance
        SessionEvent::MemberAdded { .. }
        | SessionEvent::MemberRemoved { .. }
        | SessionEvent::MemberRoleChanged { .. }
        | SessionEvent::SessionArchivedChanged { .. } => WorkspaceRole::Admin,
        // content / collaboration
        SessionEvent::SessionSnapshot { .. }
        | SessionEvent::MessageAppended { .. }
        | SessionEvent::MessageChunkReceived { .. }
        | SessionEvent::MessageCompleted { .. }
        | SessionEvent::MessageRemoved { .. }
        | SessionEvent::MessageReactionAdded { .. }
        | SessionEvent::MessageReactionRemoved { .. }
        | SessionEvent::AgentRosterUpdated { .. }
        | SessionEvent::SessionRuntimeChanged { .. }
        | SessionEvent::SessionTitleChanged { .. }
        | SessionEvent::SkillsUpdated { .. }
        | SessionEvent::ProposalUpserted { .. }
        | SessionEvent::VaultSourcesUpdated { .. }
        | SessionEvent::WorkflowDefinitionsUpdated { .. }
        | SessionEvent::WorkflowRunUpserted { .. } => WorkspaceRole::Contributor,
        // A newer-client event this build can't author or interpret — require
        // the highest role so it can never be produced locally by accident.
        SessionEvent::Unknown => WorkspaceRole::Owner,
    }
}

fn owner_count(session: &ChatSession) -> usize {
    session
        .members
        .iter()
        .filter(|m| m.role == WorkspaceRole::Owner)
        .count()
}

/// Evaluate whether `actor_role` may emit `event` in `session`.
pub fn evaluate(
    event: &SessionEvent,
    actor_role: WorkspaceRole,
    session: &ChatSession,
) -> AuthzDecision {
    if actor_role.rank() < min_role_for(event).rank() {
        return AuthzDecision::deny(
            AuthzReason::InsufficientRole,
            format!("role {actor_role:?} cannot perform {}", event.kind_str()),
        );
    }

    // Last-owner protection: can't remove or demote the only remaining owner.
    match event {
        SessionEvent::MemberRemoved { member_id } => {
            let removing_owner = session
                .members
                .iter()
                .any(|m| &m.id == member_id && m.role == WorkspaceRole::Owner);
            if removing_owner && owner_count(session) <= 1 {
                return AuthzDecision::deny(
                    AuthzReason::TargetIsLastOwner,
                    "cannot remove the last owner",
                );
            }
        }
        SessionEvent::MemberRoleChanged {
            change: MemberRoleChange { member_id, new_role, .. },
        } => {
            let demoting_owner = *new_role != WorkspaceRole::Owner
                && session
                    .members
                    .iter()
                    .any(|m| &m.id == member_id && m.role == WorkspaceRole::Owner);
            if demoting_owner && owner_count(session) <= 1 {
                return AuthzDecision::deny(
                    AuthzReason::TargetIsLastOwner,
                    "cannot demote the last owner",
                );
            }
        }
        _ => {}
    }

    AuthzDecision::allow()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{ChatMessage, MessageRole};
    use crate::identity::{ActorIdentity, ActorKind, WorkspaceMember};
    use uuid::Uuid;

    fn session_with_members(members: Vec<(&str, WorkspaceRole)>) -> ChatSession {
        let mut s = ChatSession::new("t", Uuid::nil(), "anthropic");
        s.members = members
            .into_iter()
            .map(|(id, role)| WorkspaceMember {
                id: id.to_string(),
                actor: ActorIdentity::new(id, id, ActorKind::Human),
                role,
                title: String::new(),
                index: 0,
                joined_at: Default::default(),
            })
            .collect();
        s
    }

    fn msg() -> SessionEvent {
        SessionEvent::MessageAppended {
            message: ChatMessage::new(MessageRole::User, "u", "hi"),
        }
    }
    fn add_member() -> SessionEvent {
        SessionEvent::MemberAdded {
            member: WorkspaceMember {
                id: "new".into(),
                actor: ActorIdentity::new("new", "New", ActorKind::Human),
                role: WorkspaceRole::Contributor,
                title: String::new(),
                index: 0,
                joined_at: Default::default(),
            },
        }
    }

    #[test]
    fn viewer_cannot_post_or_govern() {
        let s = session_with_members(vec![]);
        assert!(!evaluate(&msg(), WorkspaceRole::Viewer, &s).allowed);
        assert!(evaluate(&msg(), WorkspaceRole::Contributor, &s).allowed);
        assert!(!evaluate(&add_member(), WorkspaceRole::Contributor, &s).allowed);
        assert!(evaluate(&add_member(), WorkspaceRole::Admin, &s).allowed);
    }

    #[test]
    fn cannot_remove_or_demote_last_owner() {
        let s = session_with_members(vec![("o1", WorkspaceRole::Owner)]);
        let remove = SessionEvent::MemberRemoved { member_id: "o1".into() };
        assert_eq!(
            evaluate(&remove, WorkspaceRole::Owner, &s).reason,
            AuthzReason::TargetIsLastOwner
        );
        let demote = SessionEvent::MemberRoleChanged {
            change: MemberRoleChange {
                member_id: "o1".into(),
                old_role: WorkspaceRole::Owner,
                new_role: WorkspaceRole::Admin,
            },
        };
        assert!(!evaluate(&demote, WorkspaceRole::Owner, &s).allowed);
    }

    #[test]
    fn second_owner_lets_you_remove_one() {
        let s = session_with_members(vec![
            ("o1", WorkspaceRole::Owner),
            ("o2", WorkspaceRole::Owner),
        ]);
        let remove = SessionEvent::MemberRemoved { member_id: "o1".into() };
        assert!(evaluate(&remove, WorkspaceRole::Owner, &s).allowed);
    }

    #[test]
    fn streaming_events_skip_the_check() {
        assert!(!requires_authz(&SessionEvent::MessageCompleted {
            message_id: Uuid::nil(),
            body: "x".into()
        }));
        assert!(requires_authz(&msg()));
    }
}
