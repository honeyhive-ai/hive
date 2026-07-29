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

/// The subset of events that must be **re-authorized during projection**, not
/// only at authoring time — because a malicious member can sign one as themselves
/// (passing the envelope verifier) and sync it to every peer. Without an ingest
/// check, a Viewer's forged `MemberRoleChanged{self→Owner}` (or a forged
/// `MemberRemoved` of the real owner, or planted agents/skills/runtimes) is
/// projected by everyone → workspace takeover / prompt injection. Gating these in
/// the canonical fold makes every device reach the identical drop decision, so
/// convergence is preserved.
///
/// Scope is deliberately narrow: **governance + workspace-config** only. Chat
/// content (messages/reactions/proposals/title) is excluded — impersonation is
/// already caught by the envelope verifier, and gating it would risk dropping
/// legitimate collaboration. Trust-bootstrap events (`AccountKeyRegistered`,
/// `DeviceCertificateAdded`) are excluded — they are validated by the cert chain,
/// not by role, and gating them would break a joiner establishing trust.
/// `SessionSnapshot` is handled out of band, not here: the base *seed* is gated by
/// `events::is_self_consistent_seed` and a mid-stream (non-seed) snapshot
/// by [`nonseed_snapshot_authorized`], because both need the projected-so-far
/// state / the embedded snapshot to decide — which the generic role check lacks
/// (security residual B2, closed in `events::project`).
pub fn requires_projection_authz(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::MemberAdded { .. }
            | SessionEvent::MemberRemoved { .. }
            | SessionEvent::MemberRoleChanged { .. }
            | SessionEvent::SessionArchivedChanged { .. }
            | SessionEvent::WorkspaceRuntimesUpdated { .. }
            | SessionEvent::WorkspaceRuntimeUpserted { .. }
            | SessionEvent::WorkspaceRuntimeRemoved { .. }
            | SessionEvent::WorkspaceHostsUpdated { .. }
            | SessionEvent::WorkspaceHostUpserted { .. }
            | SessionEvent::WorkspaceHostRemoved { .. }
            | SessionEvent::AgentRosterUpdated { .. }
            | SessionEvent::AgentUpserted { .. }
            | SessionEvent::AgentRemoved { .. }
            | SessionEvent::SkillsUpdated { .. }
            | SessionEvent::VaultSourcesUpdated { .. }
            | SessionEvent::ChannelCreated { .. }
            | SessionEvent::ChannelRenamed { .. }
            | SessionEvent::ChannelReordered { .. }
            | SessionEvent::ChannelArchived { .. }
            | SessionEvent::ChatChannelChanged { .. }
    )
}

/// Whether a **non-seed** `SessionSnapshot` — one folded *after* the base seed,
/// i.e. a compaction checkpoint — may be applied during projection. A snapshot is
/// a full-state overwrite, so a mid-stream one must be constrained to what its
/// author could already express through governance deltas, or a malicious member
/// could rewrite the roster wholesale from a single event. Authorized iff:
///   * the author is an Owner in the roster projected *so far*, AND
///   * it preserves `creator_actor_id` (compaction never re-founds the workspace),
///     AND
///   * it promotes no member to Owner who was not already one (no silent
///     elevation of the author or a confederate).
///
/// A plain compaction snapshot by an Owner that preserves the roster/creator
/// passes; a forged one that re-founds or elevates is dropped.
///
/// Determinism: this reads only `new_state`, `author`, and the fold-so-far
/// `projected`, so every device reaches the identical decision ⇒ convergence.
///
/// NOTE: `ChatSession::apply` currently folds a non-seed snapshot as a no-op, so
/// this gate is defense-in-depth — it becomes load-bearing only if compaction
/// gains subsuming semantics. The live B2 risk is *seed* selection (see
/// `crate::events::project`).
pub fn nonseed_snapshot_authorized(
    new_state: &ChatSession,
    author: &str,
    projected: &ChatSession,
) -> bool {
    if projected.role_of(author) != WorkspaceRole::Owner {
        return false;
    }
    if new_state.creator_actor_id != projected.creator_actor_id {
        return false;
    }
    // Reject any Owner in the incoming state who was not already an Owner in the
    // roster projected so far (a member absent so far counts as "not an Owner").
    let elevates_a_new_owner = new_state.members.iter().any(|m| {
        m.role == WorkspaceRole::Owner && projected.role_of(&m.id) != WorkspaceRole::Owner
    });
    !elevates_a_new_owner
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
        | SessionEvent::WorkspaceRuntimesUpdated { .. }
        | SessionEvent::WorkspaceRuntimeUpserted { .. }
        | SessionEvent::WorkspaceRuntimeRemoved { .. } => WorkspaceRole::Admin,
        // content / collaboration
        SessionEvent::SessionSnapshot { .. }
        | SessionEvent::MessageAppended { .. }
        | SessionEvent::MessageChunkReceived { .. }
        | SessionEvent::MessageCompleted { .. }
        | SessionEvent::MessageRemoved { .. }
        | SessionEvent::TurnClaimed { .. }
        | SessionEvent::MessageReactionAdded { .. }
        | SessionEvent::MessageReactionRemoved { .. }
        | SessionEvent::AgentRosterUpdated { .. }
        | SessionEvent::AgentUpserted { .. }
        | SessionEvent::AgentRemoved { .. }
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
        | SessionEvent::WorkspaceHostsUpdated { .. }
        | SessionEvent::WorkspaceHostUpserted { .. }
        | SessionEvent::WorkspaceHostRemoved { .. } => WorkspaceRole::Contributor,
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
    // falls through to the Contributor gate. Covers both the per-item deltas
    // (new writes) and the legacy whole-list event (backward-compat).
    match event {
        // A single-host upsert (register/heartbeat) of a host the actor owns.
        SessionEvent::WorkspaceHostUpserted { host } if host.owner_actor_id == actor_id => {
            return AuthzDecision::allow_reason(AuthzReason::SelfHost);
        }
        // Removing a host the actor owns — look the current owner up by id. An
        // unknown id (nothing owned) falls through to the role gate.
        SessionEvent::WorkspaceHostRemoved { host_id } => {
            let owns = session
                .workspace_hosts
                .iter()
                .any(|h| &h.id == host_id && h.owner_actor_id == actor_id);
            if owns {
                return AuthzDecision::allow_reason(AuthzReason::SelfHost);
            }
        }
        // Legacy whole-list carve-out (old clients still emit this).
        SessionEvent::WorkspaceHostsUpdated { hosts } => {
            if let Some(decision) = self_host_allowance(hosts, actor_id, session) {
                return decision;
            }
        }
        _ => {}
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

    // ── Per-item config deltas: gating + self-host carve-out ───────────────

    #[test]
    fn non_member_may_upsert_its_own_host_delta_but_not_a_foreign_one() {
        // The self-host carve-out extends to the per-item `WorkspaceHostUpserted`
        // (worker self-registration / heartbeat as a single-host delta).
        let s = session_with_members(vec![("owner", WorkspaceRole::Owner)]);
        let own = SessionEvent::WorkspaceHostUpserted { host: host_owned("box1", "worker") };
        let d = evaluate(&own, "worker", WorkspaceRole::Viewer, &s);
        assert!(d.allowed);
        assert_eq!(d.reason, AuthzReason::SelfHost);

        // A non-member registering someone else's host is gated → denied.
        let foreign = SessionEvent::WorkspaceHostUpserted { host: host_owned("box2", "someone-else") };
        let d = evaluate(&foreign, "worker", WorkspaceRole::Viewer, &s);
        assert!(!d.allowed);
        assert_eq!(d.reason, AuthzReason::InsufficientRole);
    }

    #[test]
    fn non_member_may_remove_its_own_host_delta_but_not_a_foreign_one() {
        let mut s = session_with_members(vec![("owner", WorkspaceRole::Owner)]);
        s.workspace_hosts = vec![host_owned("box1", "worker"), host_owned("box2", "someone-else")];
        // Removing a host the actor owns → self-scoped, allowed.
        let own = SessionEvent::WorkspaceHostRemoved { host_id: "box1".into() };
        assert_eq!(evaluate(&own, "worker", WorkspaceRole::Viewer, &s).reason, AuthzReason::SelfHost);
        // Removing a foreign host → gated → denied for a non-member.
        let foreign = SessionEvent::WorkspaceHostRemoved { host_id: "box2".into() };
        assert!(!evaluate(&foreign, "worker", WorkspaceRole::Viewer, &s).allowed);
    }

    #[test]
    fn agent_delta_needs_contributor_runtime_delta_needs_admin() {
        use crate::runtime::ModelProviderKind;
        use crate::workspace_runtime::WorkspaceRuntime;
        let s = session_with_members(vec![("a", WorkspaceRole::Viewer)]);
        let agent = SessionEvent::AgentUpserted {
            agent: crate::agent::WorkspaceAgent::new("r", "ws-claude"),
        };
        // Agent upsert/remove: Contributor floor (same as list `AgentRosterUpdated`).
        assert!(!evaluate(&agent, "a", WorkspaceRole::Viewer, &s).allowed);
        assert!(evaluate(&agent, "a", WorkspaceRole::Contributor, &s).allowed);
        let agent_rm = SessionEvent::AgentRemoved { agent_id: uuid::Uuid::nil() };
        assert!(!evaluate(&agent_rm, "a", WorkspaceRole::Viewer, &s).allowed);
        assert!(evaluate(&agent_rm, "a", WorkspaceRole::Contributor, &s).allowed);

        // Runtime upsert/remove: Admin floor (credentials — same as the list form).
        let rt = SessionEvent::WorkspaceRuntimeUpserted {
            runtime: WorkspaceRuntime::new("rt1", "One", ModelProviderKind::Anthropic, "m"),
        };
        assert!(!evaluate(&rt, "a", WorkspaceRole::Contributor, &s).allowed);
        assert!(evaluate(&rt, "a", WorkspaceRole::Admin, &s).allowed);
        let rt_rm = SessionEvent::WorkspaceRuntimeRemoved { runtime_id: "rt1".into() };
        assert!(!evaluate(&rt_rm, "a", WorkspaceRole::Contributor, &s).allowed);
        assert!(evaluate(&rt_rm, "a", WorkspaceRole::Admin, &s).allowed);
    }

    #[test]
    fn config_deltas_are_projection_gated() {
        // All six deltas must be re-authorized during projection (governance/
        // config), same as the whole-list events they replace.
        for ev in [
            SessionEvent::AgentUpserted { agent: crate::agent::WorkspaceAgent::new("r", "ws-claude") },
            SessionEvent::AgentRemoved { agent_id: uuid::Uuid::nil() },
            SessionEvent::WorkspaceHostUpserted { host: host_owned("b", "o") },
            SessionEvent::WorkspaceHostRemoved { host_id: "b".into() },
            SessionEvent::WorkspaceRuntimeUpserted {
                runtime: crate::workspace_runtime::WorkspaceRuntime::new(
                    "rt", "n", crate::runtime::ModelProviderKind::Anthropic, "m",
                ),
            },
            SessionEvent::WorkspaceRuntimeRemoved { runtime_id: "rt".into() },
        ] {
            assert!(requires_projection_authz(&ev), "{} must be projection-gated", ev.kind_str());
        }
    }

    #[test]
    fn nonseed_snapshot_gate() {
        // Projected-so-far: alice Owner, bob Contributor, founded by alice.
        let mut projected = session_with_members(vec![
            ("alice", WorkspaceRole::Owner),
            ("bob", WorkspaceRole::Contributor),
        ]);
        projected.creator_actor_id = "alice".into();

        // A compaction by the owner preserving roster + creator: allowed.
        let mut snap = projected.clone();
        assert!(nonseed_snapshot_authorized(&snap, "alice", &projected));
        // Authored by a non-owner: rejected.
        assert!(!nonseed_snapshot_authorized(&snap, "bob", &projected));
        // Re-founding (creator changed): rejected even by the owner.
        snap.creator_actor_id = "bob".into();
        assert!(!nonseed_snapshot_authorized(&snap, "alice", &projected));
        // Silent elevation (bob → Owner): rejected even by the owner.
        let mut elevate = projected.clone();
        if let Some(b) = elevate.members.iter_mut().find(|m| m.id == "bob") {
            b.role = WorkspaceRole::Owner;
        }
        assert!(!nonseed_snapshot_authorized(&elevate, "alice", &projected));
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
