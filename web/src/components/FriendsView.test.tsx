import { describe, it, expect } from "vitest";
import { presenceColor, requestOutcomeMessage } from "./FriendsView";

describe("presenceColor", () => {
  it("maps presence states to distinct theme tokens", () => {
    expect(presenceColor("online")).toContain("hive-success");
    expect(presenceColor("away")).toContain("hive-warn");
    expect(presenceColor("offline")).toContain("hive-line");
  });
});

describe("requestOutcomeMessage", () => {
  it("treats only 'sent' as success", () => {
    expect(requestOutcomeMessage("sent", "octocat").ok).toBe(true);
    for (const o of ["alreadyFriends", "capReached", "userNotFound", "invalid"]) {
      expect(requestOutcomeMessage(o, "octocat").ok).toBe(false);
    }
  });

  it("surfaces the cap as an upgrade hint", () => {
    expect(requestOutcomeMessage("capReached", "x").msg.toLowerCase()).toContain("limit");
  });

  it("names the user for not-found", () => {
    expect(requestOutcomeMessage("userNotFound", "octocat").msg).toContain("@octocat");
  });
});
