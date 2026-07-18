import { describe, it, expect } from "vitest";
import { detectSlash } from "./slash";

describe("detectSlash", () => {
  it("activates on a leading slash and captures the token", () => {
    expect(detectSlash("/", 1)).toEqual({ query: "" });
    expect(detectSlash("/exp", 4)).toEqual({ query: "exp" });
  });

  it("does not activate when text doesn't start with /", () => {
    expect(detectSlash("hello /x", 8)).toBeNull();
    expect(detectSlash(" /x", 3)).toBeNull();
  });

  it("deactivates once the caret moves past the first space", () => {
    expect(detectSlash("/export now", 11)).toBeNull();
  });

  it("stays active (with the full leading token) while caret is within it", () => {
    expect(detectSlash("/export now", 4)).toEqual({ query: "export" });
  });
});
