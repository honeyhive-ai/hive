// Ordering and grouping for the Review inbox, factored out of RightRail so the
// rules are unit-testable without rendering the pane.
//
// `list_proposals` returns fold order — oldest first, with settled proposals
// interleaved among open ones. That is the wrong order for an inbox twice over:
// the newest item (the one the turn just filed) lands at the bottom, and
// finished work occupies the same space as work that still needs a decision.

import type { ProposalDto } from "@/bindings/ProposalDto";

/// A proposal nobody can act on any further: applied, or rejected by quorum.
///
/// "approved" is deliberately NOT terminal — quorum is met but a human still has
/// to click Implement, which makes it the most actionable state in the pane.
export function isSettled(p: ProposalDto): boolean {
  return p.status === "applied" || p.status === "rejected";
}

/// Whether the vote buttons should show. A settled proposal keeps rendering
/// live Approve/Reject today, which invites a vote that changes nothing.
export function isVotable(p: ProposalDto): boolean {
  return !isSettled(p);
}

/// Sort rank: quorum met and waiting on Implement, then open, then settled.
/// Within a rank the caller sorts newest-first.
function rank(p: ProposalDto): number {
  if (isSettled(p)) return 2;
  return p.quorumMet ? 0 : 1;
}

/// Newest first. An unparseable/missing `createdAt` sorts to the end rather than
/// poisoning the comparator with NaN (which would make the order depend on the
/// input sequence and differ between engines).
function createdMs(p: ProposalDto): number {
  const t = Date.parse(p.createdAt ?? "");
  return Number.isNaN(t) ? -Infinity : t;
}

/// Inbox order: actionable first, newest first within each group.
export function sortProposals(proposals: readonly ProposalDto[]): ProposalDto[] {
  return [...proposals].sort((a, b) => rank(a) - rank(b) || createdMs(b) - createdMs(a));
}

export interface InboxGroups {
  /// Open + approved-awaiting-implement, in inbox order.
  active: ProposalDto[];
  /// Settled and still visible — collapsed behind a disclosure.
  settled: ProposalDto[];
  /// Dismissed, shown only when the reader asks for them.
  dismissed: ProposalDto[];
}

/// Split the inbox into the three shelves the pane renders.
///
/// A dismissed proposal is hidden regardless of status: dismiss is the reader's
/// explicit "I'm done looking at this", and honouring it only for settled items
/// would silently ignore the action on anything else.
export function groupProposals(proposals: readonly ProposalDto[]): InboxGroups {
  const ordered = sortProposals(proposals);
  return {
    active: ordered.filter((p) => !p.dismissed && !isSettled(p)),
    settled: ordered.filter((p) => !p.dismissed && isSettled(p)),
    dismissed: ordered.filter((p) => p.dismissed),
  };
}
