import { describe, it, expect } from "vitest";
import { applyStreamDelta, retireStream } from "./streams";

describe("stream accumulation", () => {
  it("accumulates interleaved deltas per message id independently", () => {
    let m = new Map<string, string>();
    m = applyStreamDelta(m, "a", "Hello");
    m = applyStreamDelta(m, "b", "Bonjour");
    m = applyStreamDelta(m, "a", " world");
    m = applyStreamDelta(m, "b", " le monde");
    expect(m.get("a")).toBe("Hello world");
    expect(m.get("b")).toBe("Bonjour le monde");
  });

  it("retiring one stream leaves the others in flight", () => {
    let m = new Map<string, string>();
    m = applyStreamDelta(m, "a", "one");
    m = applyStreamDelta(m, "b", "two");
    m = retireStream(m, "a");
    expect(m.has("a")).toBe(false);
    expect(m.get("b")).toBe("two");
    expect(m.size).toBe(1);
  });

  it("does not mutate the previous map (safe for React state updaters)", () => {
    const first = applyStreamDelta(new Map(), "a", "x");
    const second = applyStreamDelta(first, "a", "y");
    expect(first.get("a")).toBe("x");
    expect(second.get("a")).toBe("xy");
  });
});
