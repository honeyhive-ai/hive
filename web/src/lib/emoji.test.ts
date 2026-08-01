import { describe, it, expect } from "vitest";
import { searchEmoji, exactEmoji, detectShortcode, closingShortcodeWord, type EmojiEntry } from "./emoji";

const INDEX: EmojiEntry[] = [
  { emoji: "🔥", name: "fire", slug: "fire" },
  { emoji: "❤️", name: "red heart", slug: "red_heart" },
  { emoji: "😀", name: "grinning face", slug: "grinning_face" },
  { emoji: "🎉", name: "party popper", slug: "party_popper" },
  { emoji: "🚒", name: "fire engine", slug: "fire_engine" },
];

describe("searchEmoji", () => {
  it("returns nothing for an empty query", () => {
    expect(searchEmoji(INDEX, "")).toEqual([]);
    expect(searchEmoji(INDEX, "   ")).toEqual([]);
  });

  it("ranks slug/name prefix matches before substring matches", () => {
    const out = searchEmoji(INDEX, "fire");
    // both "fire" and "fire_engine" prefix-match; "heart" substring 'ire' doesn't.
    expect(out.map((e) => e.emoji)).toEqual(["🔥", "🚒"]);
  });

  it("matches on the human name too", () => {
    expect(searchEmoji(INDEX, "grinning").map((e) => e.emoji)).toEqual(["😀"]);
  });

  it("is case-insensitive and respects the limit", () => {
    expect(searchEmoji(INDEX, "FIRE").length).toBe(2);
    expect(searchEmoji(INDEX, "fire", 1).length).toBe(1);
  });
});

describe("exactEmoji", () => {
  it("resolves an exact slug", () => {
    expect(exactEmoji(INDEX, "fire")).toBe("🔥");
    expect(exactEmoji(INDEX, "red_heart")).toBe("❤️");
  });
  it("falls back to an exact name", () => {
    expect(exactEmoji(INDEX, "party popper")).toBe("🎉");
  });
  it("is case-insensitive and returns null when nothing matches exactly", () => {
    expect(exactEmoji(INDEX, "FIRE")).toBe("🔥");
    expect(exactEmoji(INDEX, "fir")).toBeNull(); // prefix, not exact
    expect(exactEmoji(INDEX, "")).toBeNull();
  });
});

describe("detectShortcode", () => {
  it("detects a :word at the end of the text", () => {
    expect(detectShortcode("hello :fire")).toEqual({ start: 6, query: "fire" });
  });
  it("detects a :word at the start of the text", () => {
    expect(detectShortcode(":party")).toEqual({ start: 0, query: "party" });
  });
  it("requires a word boundary before the colon (no mid-word / URL false-fire)", () => {
    expect(detectShortcode("http://x")).toBeNull();
    expect(detectShortcode("a:fire")).toBeNull();
  });
  it("needs at least two chars (single char doesn't open the menu)", () => {
    expect(detectShortcode(":f")).toBeNull();
    expect(detectShortcode(":fi")).toEqual({ start: 0, query: "fi" });
  });
  it("stops matching once the closing colon is typed", () => {
    expect(detectShortcode(":fire:")).toBeNull();
  });
});

describe("closingShortcodeWord", () => {
  it("returns the inner word when a :word: is completed", () => {
    expect(closingShortcodeWord("nice :fire:")).toBe("fire");
    expect(closingShortcodeWord(":tada:")).toBe("tada");
  });
  it("returns null for an unclosed shortcode or mid-word colon", () => {
    expect(closingShortcodeWord("nice :fire")).toBeNull();
    expect(closingShortcodeWord("http://x:")).toBeNull();
  });
});
