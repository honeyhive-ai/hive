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
}

fn default_floor() -> WorkspaceRole {
    WorkspaceRole::Viewer
}

impl ActionProposal {
    pub fn new(title: impl Into<String>, kind: ProposalKind) -> Self {
        Self {
            id: Uuid::new_v4(),
            title: title.into(),
            body: String::new(),
            kind,
            status: ProposalStatus::Open,
            required_approvals: 1,
            approval_role_floor: WorkspaceRole::Viewer,
            approvals: Vec::new(),
            created_at: Timestamp::now(),
        }
    }

    /// Record a vote, replacing any prior vote by the same actor (latest wins).
    pub fn cast_vote(&mut self, vote: ProposalApproval) {
        self.approvals.retain(|a| a.actor_id != vote.actor_id);
        self.approvals.push(vote);
        self.recompute_status();
    }

    /// Up-votes from members whose role meets the floor.
    pub fn qualifying_approvals(&self) -> usize {
        let floor = self.approval_role_floor.rank();
        self.approvals
            .iter()
            .filter(|a| a.approved && a.role.rank() >= floor)
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
        let mut p = ActionProposal::new("Ship it", ProposalKind::Decision);
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
        let mut p = ActionProposal::new("X", ProposalKind::Command);
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
        let mut p = ActionProposal::new("X", ProposalKind::FileDiff);
        p.required_approvals = 1;
        p.approval_role_floor = WorkspaceRole::Admin;
        p.cast_vote(vote("a1", WorkspaceRole::Admin, true));
        assert_eq!(p.status, ProposalStatus::Approved);
        p.cast_vote(vote("o1", WorkspaceRole::Owner, false));
        assert_eq!(p.status, ProposalStatus::Rejected);
    }
}
