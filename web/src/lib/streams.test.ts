import { describe, it, expect } from "vitest";
import { applyStreamDelta, retireStream, handleTerminal } from "./streams";

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

describe("terminal handling (the 'thinking' lifecycle contract — P2-10/P2-11)", () => {
  it("retiring the last live stream signals clear (single turn)", () => {
    const streams = applyStreamDelta(new Map(), "a", "hi");
    const r = handleTerminal(streams, new Set(), "a");
    expect(r.duplicate).toBe(false);
    expect(r.cleared).toBe(true);
    expect(r.streams.has("a")).toBe(false);
    expect(r.retired.has("a")).toBe(true);
  });

  it("a sibling still in flight does NOT clear (fan-out)", () => {
    let streams = applyStreamDelta(new Map(), "a", "one");
    streams = applyStreamDelta(streams, "b", "two");
    const r = handleTerminal(streams, new Set(), "a");
    expect(r.cleared).toBe(false); // b is still streaming
    expect(r.streams.has("b")).toBe(true);
  });

  it("a zero-delta completion (message never streamed) still clears when it's the only turn", () => {
    const r = handleTerminal(new Map(), new Set(), "a");
    expect(r.duplicate).toBe(false);
    expect(r.cleared).toBe(true);
  });

  it("a DUPLICATE terminal is a no-op — can't collapse a sibling's state", () => {
    // 'a' already retired; 'b' is now the live turn.
    const streams = applyStreamDelta(new Map(), "b", "live");
    const dup = handleTerminal(streams, new Set(["a"]), "a");
    expect(dup.duplicate).toBe(true);
    expect(dup.cleared).toBe(false); // must NOT clear while b is in flight
    expect(dup.streams.has("b")).toBe(true);
  });

  it("does not mutate the input streams/retired sets", () => {
    const streams = applyStreamDelta(new Map(), "a", "x");
    const retired = new Set<string>();
    handleTerminal(streams, retired, "a");
    expect(streams.has("a")).toBe(true);
    expect(retired.size).toBe(0);
  });
});
