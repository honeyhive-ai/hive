//! Authorization — delta-validating role gate, ported from `Authorization.swift`.
//!
//! Each *local* state change (a `SessionEvent` about to be appended) is checked
//! against the acting member's [`WorkspaceRole`]: governance events (membership,
//! archive/delete) require admin/owner; content (messages, reactions, proposals,
//! agents, skills) requires contributor+; and the **last owner** can't be removed
//! or demoted. Synced foreign events are authorized by their origin device, so
//! this gates the originating side.

use crate::events::{MemberRoleChange, SessionEvent};
use crate::identity::{WorkspaceMember, WorkspaceRole};
use crate::session::ChatSession;
use crate::workspace_host::WorkspaceHost;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthzReason {
    Allowed,
    InsufficientRole,
    TargetIsLastOwner,
    /// Adding yourself is a permitted bootstrap (empty roster, or a pure
    /// self-enroll at a non-governance role that changes no existing member).
    SelfEnroll,
    /// Managing only hosts you own (e.g. a worker box registering/heartbeating
    /// itself) — a self-scoped presence action a non-member may perform.
    SelfHost,
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
    fn allow_reason(reason: AuthzReason) -> Self {
        Self {
            allowed: true,
            reason,
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
        | SessionEvent::SessionArchivedChanged { .. }
        // Workspace runtimes carry credentials — governance, admin+.
        | SessionEvent::WorkspaceRuntimesUpdated { .. } => WorkspaceRole::Admin,
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
        | SessionEvent::ProposalVoteCast { .. }
        | SessionEvent::VaultSourcesUpdated { .. }
        | SessionEvent::WorkflowDefinitionsUpdated { .. }
        | SessionEvent::WorkflowRunUpserted { .. }
        // Any member may publish their own account key / device certificate; the
        // roster only trusts a self-consistent chain (cert signed by the account
        // key), so a lower floor here can't forge trust.
        | SessionEvent::AccountKeyRegistered { .. }
        | SessionEvent::DeviceCertificateAdded { .. }
        // Channels are cheap organization (spec §11) — any contributor may
        // create, rename, reorder, archive, or file a chat into one.
        | SessionEvent::ChannelCreated { .. }
        | SessionEvent::ChannelRenamed { .. }
        | SessionEvent::ChannelReordered { .. }
        | SessionEvent::ChannelArchived { .. }
        | SessionEvent::ChatChannelChanged { .. }
        | SessionEvent::WorkspaceHostsUpdated { .. } => WorkspaceRole::Contributor,
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

/// The **bootstrap allowance** for `MemberAdded`: the narrow set of self-adds an
/// actor who is *not yet* an Admin+ in the roster may perform. Returns `Some` iff
/// the self-add is allowed; `None` means "no special allowance — fall through to
/// the normal role gate" (so a non-permitted self-add is still denied).
///
/// Scoped hard to `member.id == actor_id` (adding *yourself*). Adding another
/// actor, or removing/demoting anyone, never reaches here and is governed solely
/// by the role gate.
fn self_enroll_allowance(
    member: &WorkspaceMember,
    actor_id: &str,
    session: &ChatSession,
) -> Option<AuthzDecision> {
    // Only a self-add qualifies for the bootstrap allowance.
    if member.id != actor_id {
        return None;
    }
    // First member of a brand-new workspace: the creator seeds themselves.
    if session.members.is_empty() {
        return Some(AuthzDecision::allow_reason(AuthzReason::SelfEnroll));
    }
    match session.members.iter().find(|m| m.id == actor_id) {
        // Already a member: allowed only if the role is unchanged (idempotent
        // re-add). A role bump on yourself is a governance change — needs Admin+,
        // so fall through to the role gate.
        Some(existing) => (existing.role == member.role)
            .then(|| AuthzDecision::allow_reason(AuthzReason::SelfEnroll)),
        // Not yet a member, joining a non-empty workspace: pure self-enroll is
        // allowed only at a *non-governance* floor (Viewer/Contributor). Granting
        // yourself Admin/Owner falls through to the role gate → denied.
        None => (member.role.rank() < WorkspaceRole::Admin.rank())
            .then(|| AuthzDecision::allow_reason(AuthzReason::SelfEnroll)),
    }
}

/// The hosts that differ between the current roster (`old`) and a proposed one
/// (`new`): those added or modified (yielded as their **new** value) and those
/// removed (yielded as their **old** value). An unchanged host is not yielded.
fn hosts_changed<'a>(
    old: &'a [WorkspaceHost],
    new: &'a [WorkspaceHost],
) -> impl Iterator<Item = &'a WorkspaceHost> {
    let added_or_modified =
        new.iter().filter(move |n| !old.iter().any(|o| o.id == n.id && o == *n));
    let removed = old.iter().filter(move |o| !new.iter().any(|n| n.id == o.id));
    added_or_modified.chain(removed)
}

/// The **self-host allowance** for `WorkspaceHostsUpdated`: a change that only
/// adds, removes, or modifies hosts the actor *owns* (`owner_actor_id ==
/// actor_id`) is a self-scoped presence action a non-member may perform — e.g. a
/// worker box registering or heartbeating itself. Returns `Some(allow)` iff every
/// changed host is actor-owned; `None` (→ fall through to the role gate) the
/// moment it touches a host owned by anyone else.
fn self_host_allowance(
    new_hosts: &[WorkspaceHost],
    actor_id: &str,
    session: &ChatSession,
) -> Option<AuthzDecision> {
    let touches_foreign_host =
        hosts_changed(&session.workspace_hosts, new_hosts).any(|h| h.owner_actor_id != actor_id);
    (!touches_foreign_host).then(|| AuthzDecision::allow_reason(AuthzReason::SelfHost))
}

/// Evaluate whether the actor (`actor_id`, holding `actor_role` in the projected
/// roster) may emit `event` in `session`.
pub fn evaluate(
    event: &SessionEvent,
    actor_id: &str,
    actor_role: WorkspaceRole,
    session: &ChatSession,
) -> AuthzDecision {
    // Bootstrap seam: a self-scoped `MemberAdded` can be permitted for an actor
    // who isn't (yet) Admin+ — but *only* the narrow self-enroll cases below.
    // Everything else (adding others, removing/demoting anyone) is role-gated.
    if let SessionEvent::MemberAdded { member } = event {
        if let Some(decision) = self_enroll_allowance(member, actor_id, session) {
            return decision;
        }
    }
    // Presence seam: managing only your *own* hosts (worker self-registration /
    // heartbeat) is permitted for a non-member; touching another actor's host
    // falls through to the Contributor gate.
    if let SessionEvent::WorkspaceHostsUpdated { hosts } = event {
        if let Some(decision) = self_host_allowance(hosts, actor_id, session) {
            return decision;
        }
    }

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
        add_member_role("new", WorkspaceRole::Contributor)
    }
    fn add_member_role(id: &str, role: WorkspaceRole) -> SessionEvent {
        SessionEvent::MemberAdded {
            member: WorkspaceMember {
                id: id.into(),
                actor: ActorIdentity::new(id, id, ActorKind::Human),
                role,
                title: String::new(),
                index: 0,
                joined_at: Default::default(),
            },
        }
    }

    fn host_owned(id: &str, owner: &str) -> WorkspaceHost {
        let mut h = WorkspaceHost::new(id, crate::workspace_host::HostKind::Worker, id);
        h.owner_actor_id = owner.to_string();
        h
    }
    fn hosts_event(hosts: Vec<WorkspaceHost>) -> SessionEvent {
        SessionEvent::WorkspaceHostsUpdated { hosts }
    }

    // ── Self-owned host allowance (worker self-registration; #61 seam) ─────

    #[test]
    fn non_member_may_register_its_own_host() {
        // A worker box (Viewer floor, not a member) registers a host it owns.
        let s = session_with_members(vec![("owner", WorkspaceRole::Owner)]);
        let d = evaluate(&hosts_event(vec![host_owned("box1", "worker")]), "worker",
            WorkspaceRole::Viewer, &s);
        assert!(d.allowed);
        assert_eq!(d.reason, AuthzReason::SelfHost);
    }

    #[test]
    fn non_member_may_heartbeat_its_own_host() {
        let mut s = session_with_members(vec![("owner", WorkspaceRole::Owner)]);
        s.workspace_hosts = vec![host_owned("box1", "worker")];
        let mut beat = host_owned("box1", "worker");
        beat.label = "prod-box".into(); // a heartbeat mutates the host in place
        assert!(evaluate(&hosts_event(vec![beat]), "worker", WorkspaceRole::Viewer, &s).allowed);
    }

    #[test]
    fn non_member_cannot_touch_a_foreign_host() {
        let mut s = session_with_members(vec![("owner", WorkspaceRole::Owner)]);
        s.workspace_hosts = vec![host_owned("box1", "someone-else")];
        // Removing someone else's host (empty new list) is not self-scoped → gated → denied.
        let d = evaluate(&hosts_event(vec![]), "worker", WorkspaceRole::Viewer, &s);
        assert!(!d.allowed);
        assert_eq!(d.reason, AuthzReason::InsufficientRole);
        // Hijacking (modifying) a foreign host is likewise denied.
        let mut hijack = host_owned("box1", "someone-else");
        hijack.label = "stolen".into();
        assert!(!evaluate(&hosts_event(vec![hijack]), "worker", WorkspaceRole::Viewer, &s).allowed);
    }

    #[test]
    fn contributor_manages_any_host_via_the_role_gate() {
        // A Contributor member manages hosts (foreign included) through the gate.
        let mut s = session_with_members(vec![("c", WorkspaceRole::Contributor)]);
        s.workspace_hosts = vec![host_owned("box1", "someone-else")];
        assert!(evaluate(&hosts_event(vec![]), "c", WorkspaceRole::Contributor, &s).allowed);
    }

    #[test]
    fn viewer_cannot_post_or_govern() {
        // Roster is non-empty and the actor ("actor") is adding *another* member
        // ("new") — the self-enroll allowance never applies, so it's role-gated.
        let s = session_with_members(vec![("actor", WorkspaceRole::Viewer)]);
        assert!(!evaluate(&msg(), "actor", WorkspaceRole::Viewer, &s).allowed);
        assert!(evaluate(&msg(), "actor", WorkspaceRole::Contributor, &s).allowed);
        assert!(!evaluate(&add_member(), "actor", WorkspaceRole::Contributor, &s).allowed);
        assert!(evaluate(&add_member(), "actor", WorkspaceRole::Admin, &s).allowed);
    }

    #[test]
    fn cannot_remove_or_demote_last_owner() {
        let s = session_with_members(vec![("o1", WorkspaceRole::Owner)]);
        let remove = SessionEvent::MemberRemoved { member_id: "o1".into() };
        assert_eq!(
            evaluate(&remove, "o1", WorkspaceRole::Owner, &s).reason,
            AuthzReason::TargetIsLastOwner
        );
        let demote = SessionEvent::MemberRoleChanged {
            change: MemberRoleChange {
                member_id: "o1".into(),
                old_role: WorkspaceRole::Owner,
                new_role: WorkspaceRole::Admin,
            },
        };
        assert!(!evaluate(&demote, "o1", WorkspaceRole::Owner, &s).allowed);
    }

    #[test]
    fn second_owner_lets_you_remove_one() {
        let s = session_with_members(vec![
            ("o1", WorkspaceRole::Owner),
            ("o2", WorkspaceRole::Owner),
        ]);
        let remove = SessionEvent::MemberRemoved { member_id: "o1".into() };
        assert!(evaluate(&remove, "o2", WorkspaceRole::Owner, &s).allowed);
    }

    // ── Bootstrap / self-enroll allowance (PR #61 seam) ────────────────────

    #[test]
    fn first_member_of_empty_roster_may_self_seed() {
        // Brand-new workspace: the creator seeds themselves, even as Owner, at the
        // Viewer floor a non-member gets. Only a *self*-add qualifies.
        let s = session_with_members(vec![]);
        let seed_self = add_member_role("creator", WorkspaceRole::Owner);
        assert!(evaluate(&seed_self, "creator", WorkspaceRole::Viewer, &s).allowed);
        // Seeding *someone else* into an empty roster is not a self-enroll → gated.
        let seed_other = add_member_role("victim", WorkspaceRole::Owner);
        assert!(!evaluate(&seed_other, "creator", WorkspaceRole::Viewer, &s).allowed);
    }

    #[test]
    fn non_member_may_self_enroll_at_non_governance_role() {
        // A device joining a non-empty workspace: Viewer/Contributor self-enroll ok.
        let s = session_with_members(vec![("owner", WorkspaceRole::Owner)]);
        let join = add_member_role("newbie", WorkspaceRole::Contributor);
        let d = evaluate(&join, "newbie", WorkspaceRole::Viewer, &s);
        assert!(d.allowed);
        assert_eq!(d.reason, AuthzReason::SelfEnroll);
    }

    #[test]
    fn non_member_cannot_self_enroll_as_owner_or_admin() {
        // No self-escalation: granting yourself governance while joining is denied.
        let s = session_with_members(vec![("owner", WorkspaceRole::Owner)]);
        for role in [WorkspaceRole::Admin, WorkspaceRole::Owner] {
            let escalate = add_member_role("newbie", role);
            let d = evaluate(&escalate, "newbie", WorkspaceRole::Viewer, &s);
            assert!(!d.allowed, "self-enroll at {role:?} must be denied");
            assert_eq!(d.reason, AuthzReason::InsufficientRole);
        }
    }

    #[test]
    fn self_re_add_cannot_change_own_role() {
        // An existing Contributor re-adding self at Owner is a governance change on
        // themselves — the idempotent allowance only covers an unchanged role.
        let s = session_with_members(vec![("me", WorkspaceRole::Contributor)]);
        let bump = add_member_role("me", WorkspaceRole::Owner);
        assert!(!evaluate(&bump, "me", WorkspaceRole::Contributor, &s).allowed);
        // Same role → idempotent no-op is allowed.
        let same = add_member_role("me", WorkspaceRole::Contributor);
        assert!(evaluate(&same, "me", WorkspaceRole::Contributor, &s).allowed);
    }

    #[test]
    fn non_member_cannot_remove_or_demote_others() {
        // Privilege floor: a non-member (Viewer floor) can't govern anyone.
        let s = session_with_members(vec![
            ("owner", WorkspaceRole::Owner),
            ("victim", WorkspaceRole::Contributor),
        ]);
        let remove = SessionEvent::MemberRemoved { member_id: "victim".into() };
        assert!(!evaluate(&remove, "outsider", WorkspaceRole::Viewer, &s).allowed);
        let demote = SessionEvent::MemberRoleChanged {
            change: MemberRoleChange {
                member_id: "victim".into(),
                old_role: WorkspaceRole::Contributor,
                new_role: WorkspaceRole::Viewer,
            },
        };
        assert!(!evaluate(&demote, "outsider", WorkspaceRole::Viewer, &s).allowed);
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
