//! Action proposals + quorum voting — ported from `ActionProposal` /
//! `ProposalApproval` in `HiveModels.swift`. A proposal is something a
//! participant wants the workspace to approve (a file diff, a command, a
//! decision). Approval requires a quorum: at least `required_approvals`
//! up-votes from members whose role meets `approval_role_floor`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::identity::WorkspaceRole;
use crate::time_util::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProposalKind {
    FileDiff,
    Command,
    Decision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProposalStatus {
    Open,
    Approved,
    Rejected,
    Applied,
}

/// One actor's vote on a proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposalApproval {
    pub actor_id: String,
    pub role: WorkspaceRole,
    /// `true` = approve, `false` = reject.
    pub approved: bool,
    #[serde(default)]
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionProposal {
    pub id: Uuid,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Actor who created the proposal. Their own approval does not count toward
    /// quorum (no self-approval). Empty for authorless proposals (e.g. workflow
    /// gates), where the guard excludes no one.
    #[serde(default)]
    pub author_actor_id: String,
    pub kind: ProposalKind,
    pub status: ProposalStatus,
    /// How many qualifying up-votes are needed (0 = no quorum required).
    #[serde(default)]
    pub required_approvals: u32,
    /// Minimum role an approval must carry to count.
    #[serde(default = "default_floor")]
    pub approval_role_floor: WorkspaceRole,
    #[serde(default)]
    pub approvals: Vec<ProposalApproval>,
    #[serde(default)]
    pub created_at: Timestamp,
    /// For a [`ProposalKind::FileDiff`]: the unified diff the agent produced in
    /// its isolated worktree, applied to the workspace root with `git apply` on
    /// Implement. Stored on the proposal (not a branch ref) so it's portable —
    /// every reviewer sees it, and any device whose tree matches the base can
    /// apply it, not just the one that ran the turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// Paths the diff touches — a compact summary without parsing the patch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
}

fn default_floor() -> WorkspaceRole {
    WorkspaceRole::Viewer
}

impl ActionProposal {
    pub fn new(
        title: impl Into<String>,
        kind: ProposalKind,
        author_actor_id: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            title: title.into(),
            body: String::new(),
            author_actor_id: author_actor_id.into(),
            kind,
            status: ProposalStatus::Open,
            required_approvals: 1,
            approval_role_floor: WorkspaceRole::Viewer,
            approvals: Vec::new(),
            created_at: Timestamp::now(),
            diff: None,
            changed_files: Vec::new(),
        }
    }

    /// A file-diff proposal carrying the patch an agent produced in an isolated
    /// worktree. `Implement` applies it to the workspace root.
    pub fn file_diff(
        title: impl Into<String>,
        author_actor_id: impl Into<String>,
        diff: String,
        changed_files: Vec<String>,
    ) -> Self {
        Self {
            diff: Some(diff),
            changed_files,
            ..Self::new(title, ProposalKind::FileDiff, author_actor_id)
        }
    }

    /// Record a vote, replacing any prior vote by the same actor (latest wins).
    pub fn cast_vote(&mut self, vote: ProposalApproval) {
        self.approvals.retain(|a| a.actor_id != vote.actor_id);
        self.approvals.push(vote);
        self.recompute_status();
    }

    /// Whether `actor_id` authored this proposal (never true for an authorless
    /// proposal, so the self-approval guard excludes no one there).
    fn is_author(&self, actor_id: &str) -> bool {
        !self.author_actor_id.is_empty() && self.author_actor_id == actor_id
    }

    /// Up-votes from members whose role meets the floor. The author's own
    /// approval does not count — a proposal can't self-satisfy its quorum.
    pub fn qualifying_approvals(&self) -> usize {
        let floor = self.approval_role_floor.rank();
        self.approvals
            .iter()
            .filter(|a| a.approved && a.role.rank() >= floor && !self.is_author(&a.actor_id))
            .count()
    }

    /// Whether enough qualifying up-votes exist.
    pub fn is_quorum_met(&self) -> bool {
        self.required_approvals > 0 && self.qualifying_approvals() >= self.required_approvals as usize
    }

    /// A qualifying down-vote rejects the proposal outright.
    fn has_qualifying_rejection(&self) -> bool {
        let floor = self.approval_role_floor.rank();
        self.approvals
            .iter()
            .any(|a| !a.approved && a.role.rank() >= floor)
    }

    fn recompute_status(&mut self) {
        // Don't override a terminal "applied" state.
        if self.status == ProposalStatus::Applied {
            return;
        }
        self.status = if self.has_qualifying_rejection() {
            ProposalStatus::Rejected
        } else if self.is_quorum_met() {
            ProposalStatus::Approved
        } else {
            ProposalStatus::Open
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vote(actor: &str, role: WorkspaceRole, approved: bool) -> ProposalApproval {
        ProposalApproval {
            actor_id: actor.into(),
            role,
            approved,
            created_at: Timestamp::epoch(),
        }
    }

    #[test]
    fn quorum_counts_only_qualifying_roles() {
        let mut p = ActionProposal::new("Ship it", ProposalKind::Decision, "");
        p.required_approvals = 2;
        p.approval_role_floor = WorkspaceRole::Contributor;

        p.cast_vote(vote("viewer", WorkspaceRole::Viewer, true)); // below floor
        assert_eq!(p.qualifying_approvals(), 0);
        assert!(!p.is_quorum_met());
        assert_eq!(p.status, ProposalStatus::Open);

        p.cast_vote(vote("c1", WorkspaceRole::Contributor, true));
        p.cast_vote(vote("a1", WorkspaceRole::Admin, true));
        assert_eq!(p.qualifying_approvals(), 2);
        assert!(p.is_quorum_met());
        assert_eq!(p.status, ProposalStatus::Approved);
    }

    #[test]
    fn latest_vote_per_actor_wins() {
        let mut p = ActionProposal::new("X", ProposalKind::Command, "");
        p.required_approvals = 1;
        p.cast_vote(vote("c1", WorkspaceRole::Contributor, true));
        assert_eq!(p.status, ProposalStatus::Approved);
        // same actor changes their mind → reject
        p.cast_vote(vote("c1", WorkspaceRole::Contributor, false));
        assert_eq!(p.approvals.len(), 1);
        assert_eq!(p.status, ProposalStatus::Rejected);
    }

    #[test]
    fn qualifying_rejection_blocks() {
        let mut p = ActionProposal::new("X", ProposalKind::FileDiff, "");
        p.required_approvals = 1;
        p.approval_role_floor = WorkspaceRole::Admin;
        p.cast_vote(vote("a1", WorkspaceRole::Admin, true));
        assert_eq!(p.status, ProposalStatus::Approved);
        p.cast_vote(vote("o1", WorkspaceRole::Owner, false));
        assert_eq!(p.status, ProposalStatus::Rejected);
    }

    #[test]
    fn author_self_approval_does_not_reach_quorum() {
        let mut p = ActionProposal::new("Mine", ProposalKind::Decision, "author");
        p.required_approvals = 1;
        // Author approves their own proposal — does not count toward quorum.
        p.cast_vote(vote("author", WorkspaceRole::Owner, true));
        assert_eq!(p.qualifying_approvals(), 0);
        assert!(!p.is_quorum_met());
        assert_eq!(p.status, ProposalStatus::Open);
        // A second, non-author approval satisfies the gate.
        p.cast_vote(vote("other", WorkspaceRole::Contributor, true));
        assert_eq!(p.qualifying_approvals(), 1);
        assert_eq!(p.status, ProposalStatus::Approved);
    }

    #[test]
    fn role_floor_enforced_against_voter_role() {
        let mut p = ActionProposal::new("Floor", ProposalKind::Decision, "author");
        p.required_approvals = 1;
        p.approval_role_floor = WorkspaceRole::Contributor;
        // Viewer up-vote is below the floor → doesn't qualify.
        p.cast_vote(vote("v1", WorkspaceRole::Viewer, true));
        assert_eq!(p.qualifying_approvals(), 0);
        assert_eq!(p.status, ProposalStatus::Open);
        // Contributor up-vote qualifies.
        p.cast_vote(vote("c1", WorkspaceRole::Contributor, true));
        assert_eq!(p.qualifying_approvals(), 1);
        assert_eq!(p.status, ProposalStatus::Approved);
    }

    #[test]
    fn author_downvote_still_vetoes() {
        let mut p = ActionProposal::new("Mine", ProposalKind::Decision, "author");
        p.required_approvals = 1;
        p.cast_vote(vote("other", WorkspaceRole::Contributor, true));
        assert_eq!(p.status, ProposalStatus::Approved);
        // A qualifying down-vote vetoes regardless of who casts it.
        p.cast_vote(vote("veto", WorkspaceRole::Contributor, false));
        assert_eq!(p.status, ProposalStatus::Rejected);
    }

    #[test]
    fn empty_author_excludes_no_one() {
        // Authorless (e.g. workflow gate) proposal: any qualifying approval counts.
        let mut p = ActionProposal::new("Gate", ProposalKind::Decision, "");
        p.required_approvals = 1;
        p.cast_vote(vote("someone", WorkspaceRole::Contributor, true));
        assert_eq!(p.qualifying_approvals(), 1);
        assert_eq!(p.status, ProposalStatus::Approved);
    }
}
