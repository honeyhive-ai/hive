import { describe, expect, it } from "vitest";
import type { ProposalDto } from "@/bindings/ProposalDto";
import { groupProposals, isSettled, isVotable, sortProposals } from "./proposals";

function proposal(over: Partial<ProposalDto> & { id: string }): ProposalDto {
  return {
    title: over.id,
    body: "",
    authorActorId: "",
    kind: "decision",
    status: "open",
    requiredApprovals: 1,
    qualifyingApprovals: 0,
    quorumMet: false,
    approvals: [],
    createdAt: "2026-08-01T00:00:00Z",
    dismissed: false,
    ...over,
  };
}

const at = (id: string, iso: string, over: Partial<ProposalDto> = {}) =>
  proposal({ id, createdAt: iso, ...over });

describe("sortProposals", () => {
  // The defect: `list_proposals` returns fold order, so the proposal a turn just
  // filed lands at the bottom of the pane, under everything that came before it.
  it("puts the newest proposal first, not last", () => {
    const fold = [
      at("oldest", "2026-08-01T10:00:00Z"),
      at("middle", "2026-08-02T10:00:00Z"),
      at("newest", "2026-08-03T10:00:00Z"),
    ];
    expect(sortProposals(fold).map((p) => p.id)).toEqual(["newest", "middle", "oldest"]);
  });

  it("negative control: fold order is the reverse, which is what shipped", () => {
    const fold = [
      at("oldest", "2026-08-01T10:00:00Z"),
      at("newest", "2026-08-03T10:00:00Z"),
    ];
    expect(fold.map((p) => p.id)).toEqual(["oldest", "newest"]);
  });

  // Settled proposals interleave with open ones in fold order, so finished work
  // sits between two things that still need a decision.
  it("sinks settled proposals below anything still actionable", () => {
    const fold = [
      at("open-old", "2026-08-01T10:00:00Z"),
      at("applied-new", "2026-08-05T10:00:00Z", { status: "applied" }),
      at("rejected-new", "2026-08-04T10:00:00Z", { status: "rejected" }),
    ];
    expect(sortProposals(fold).map((p) => p.id)).toEqual([
      "open-old",
      "applied-new",
      "rejected-new",
    ]);
  });

  // Quorum is met and the only thing left is a human clicking Implement — the
  // most actionable state in the pane, so it outranks an unvoted proposal even
  // when the unvoted one is newer.
  it("lifts quorum-met-awaiting-implement above merely open", () => {
    const fold = [
      at("awaiting", "2026-08-01T10:00:00Z", { status: "approved", quorumMet: true }),
      at("open-newer", "2026-08-09T10:00:00Z"),
    ];
    expect(sortProposals(fold).map((p) => p.id)).toEqual(["awaiting", "open-newer"]);
  });

  it("does not mutate its input", () => {
    const fold = [at("a", "2026-08-01T10:00:00Z"), at("b", "2026-08-03T10:00:00Z")];
    sortProposals(fold);
    expect(fold.map((p) => p.id)).toEqual(["a", "b"]);
  });

  // A NaN comparator result makes the order depend on the input sequence and can
  // differ between engines, so a bad timestamp must not poison the sort.
  it("keeps a total order when a timestamp is unparseable", () => {
    const fold = [
      at("bad", "not-a-date"),
      at("good-old", "2026-08-01T10:00:00Z"),
      at("good-new", "2026-08-03T10:00:00Z"),
    ];
    expect(sortProposals(fold).map((p) => p.id)).toEqual(["good-new", "good-old", "bad"]);
  });
});

describe("isSettled / isVotable", () => {
  it("treats applied and rejected as settled", () => {
    expect(isSettled(proposal({ id: "a", status: "applied" }))).toBe(true);
    expect(isSettled(proposal({ id: "r", status: "rejected" }))).toBe(true);
  });

  // Quorum is met but a human still has to click Implement, so it is not done.
  it("does not treat approved as settled — Implement is still outstanding", () => {
    expect(isSettled(proposal({ id: "ap", status: "approved", quorumMet: true }))).toBe(false);
  });

  // A terminal proposal rendered live Approve/Reject buttons, inviting a vote
  // that changes nothing.
  it("hides the vote buttons once a proposal is terminal", () => {
    expect(isVotable(proposal({ id: "a", status: "applied" }))).toBe(false);
    expect(isVotable(proposal({ id: "r", status: "rejected" }))).toBe(false);
    expect(isVotable(proposal({ id: "o", status: "open" }))).toBe(true);
  });
});

describe("groupProposals", () => {
  it("shelves active, settled and dismissed separately", () => {
    const groups = groupProposals([
      at("open", "2026-08-01T10:00:00Z"),
      at("applied", "2026-08-02T10:00:00Z", { status: "applied" }),
      at("filed-away", "2026-08-03T10:00:00Z", { status: "rejected", dismissed: true }),
    ]);
    expect(groups.active.map((p) => p.id)).toEqual(["open"]);
    expect(groups.settled.map((p) => p.id)).toEqual(["applied"]);
    expect(groups.dismissed.map((p) => p.id)).toEqual(["filed-away"]);
  });

  // Dismiss is the reader's explicit "I'm done with this". Honouring it only for
  // settled items would silently ignore the action on anything else.
  it("honours a dismiss regardless of status", () => {
    const groups = groupProposals([at("open-but-dismissed", "2026-08-01T10:00:00Z", { dismissed: true })]);
    expect(groups.active).toEqual([]);
    expect(groups.dismissed.map((p) => p.id)).toEqual(["open-but-dismissed"]);
  });

  it("orders each shelf newest-first", () => {
    const groups = groupProposals([
      at("old", "2026-08-01T10:00:00Z"),
      at("new", "2026-08-05T10:00:00Z"),
      at("settled-old", "2026-08-02T10:00:00Z", { status: "applied" }),
      at("settled-new", "2026-08-06T10:00:00Z", { status: "applied" }),
    ]);
    expect(groups.active.map((p) => p.id)).toEqual(["new", "old"]);
    expect(groups.settled.map((p) => p.id)).toEqual(["settled-new", "settled-old"]);
  });

  it("empties cleanly", () => {
    const groups = groupProposals([]);
    expect(groups).toEqual({ active: [], settled: [], dismissed: [] });
  });
});
