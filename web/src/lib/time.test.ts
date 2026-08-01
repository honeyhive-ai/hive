import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { relTime, clockTime, dayKey, dayLabel, absTime } from "./time";

// The helpers normalize designator-less timestamps to UTC, so pin "now" with a
// `Z` too (otherwise the fake clock is local and the message times are UTC).
// relTime is pure epoch math → zone-agnostic. The day helpers use local calendar
// days, so day-boundary cases use *noon-UTC* times, which land on the same local
// date for any UTC/US-zone runner (CI is UTC; a Mac is UTC-5..-8).
const NOW = "2026-08-01T12:00:00Z";

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date(NOW));
});
afterEach(() => {
  vi.useRealTimers();
});

describe("relTime", () => {
  it("shows 'now' under a minute and coarsens upward", () => {
    expect(relTime("2026-08-01T11:59:30Z")).toBe("now");
    expect(relTime("2026-08-01T11:45:00Z")).toBe("15m");
    expect(relTime("2026-08-01T09:00:00Z")).toBe("3h");
    expect(relTime("2026-07-30T12:00:00Z")).toBe("2d");
  });
  it("returns empty for an unparseable value", () => {
    expect(relTime("")).toBe("");
    expect(relTime("not-a-date")).toBe("");
  });
});

describe("clockTime", () => {
  it("formats a wall-clock time (hour:minute)", () => {
    expect(clockTime(NOW)).toMatch(/\d{1,2}:\d{2}/);
  });
  it("is empty for a bad value", () => {
    expect(clockTime("nope")).toBe("");
  });
});

describe("dayKey", () => {
  it("is equal within a day and differs across days", () => {
    expect(dayKey("2026-08-01T10:00:00Z")).toBe(dayKey("2026-08-01T14:00:00Z"));
    expect(dayKey("2026-08-01T12:00:00Z")).not.toBe(dayKey("2026-08-02T12:00:00Z"));
  });
});

describe("dayLabel", () => {
  it("labels today and yesterday", () => {
    expect(dayLabel("2026-08-01T12:00:00Z")).toBe("Today");
    expect(dayLabel("2026-07-31T12:00:00Z")).toBe("Yesterday");
  });
  it("uses a full date for older days", () => {
    const label = dayLabel("2026-07-20T12:00:00Z");
    expect(label).not.toBe("Today");
    expect(label).not.toBe("Yesterday");
    expect(label).toMatch(/2026/);
  });
});

describe("absTime", () => {
  it("renders a non-empty absolute string for a valid time", () => {
    expect(absTime(NOW).length).toBeGreaterThan(0);
    expect(absTime("bad")).toBe("");
  });
});
