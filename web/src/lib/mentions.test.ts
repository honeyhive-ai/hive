import { describe, it, expect } from "vitest";
import { detectMention, filterMentions, type MentionCandidate } from "./mentions";

describe("detectMention", () => {
  it("detects a bare @ at the caret", () => {
    expect(detectMention("@", 1)).toEqual({ start: 0, query: "" });
  });

  it("captures the partial handle before the caret", () => {
    expect(detectMention("hey @sa", 7)).toEqual({ start: 4, query: "sa" });
  });

  it("triggers at the start of input", () => {
    expect(detectMention("@mara", 5)).toEqual({ start: 0, query: "mara" });
  });

  it("does NOT trigger inside an email (no leading boundary)", () => {
    expect(detectMention("email me at sam@x", 17)).toBeNull();
  });

  it("does NOT trigger once the token is closed by a space", () => {
    expect(detectMention("@sam ", 5)).toBeNull();
  });

  it("only considers the token immediately before the caret", () => {
    // caret is mid-string after "@bo"
    expect(detectMention("@al said hi @bo", 15)).toEqual({ start: 12, query: "bo" });
  });

  it("allows hyphens in handles", () => {
    expect(detectMention("@code-rev", 9)).toEqual({ start: 0, query: "code-rev" });
  });
});

describe("filterMentions", () => {
  const cands: MentionCandidate[] = [
    { handle: "primary", kind: "Primary runtime" },
    { handle: "all", kind: "Everyone" },
    { handle: "Scout", kind: "Agent" },
    { handle: "Sam", kind: "owner" },
  ];

  it("is a case-insensitive prefix match", () => {
    expect(filterMentions(cands, "s").map((c) => c.handle)).toEqual(["Scout", "Sam"]);
    expect(filterMentions(cands, "SC").map((c) => c.handle)).toEqual(["Scout"]);
  });

  it("returns everything for an empty query", () => {
    expect(filterMentions(cands, "")).toHaveLength(4);
  });

  it("does not match mid-string (prefix only)", () => {
    expect(filterMentions(cands, "out")).toEqual([]);
  });

  it("respects the limit", () => {
    const many = Array.from({ length: 20 }, (_, i) => ({ handle: `a${i}`, kind: "x" }));
    expect(filterMentions(many, "a", 5)).toHaveLength(5);
  });
});
