import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RightRail } from "./RightRail";
import type { ProposalDto } from "@/lib/ipc";

// Everything RightRail calls goes through Tauri's invoke; answer the two the
// Review pane needs and an empty list for the rest.
const proposal: ProposalDto = {
  id: "p1",
  title: "Add workflow “Triage”",
  body: "1. Scan [scan], @Scout",
  authorActorId: "Scout",
  kind: "decision",
  status: "open",
  requiredApprovals: 1,
  qualifyingApprovals: 0,
  quorumMet: false,
  approvals: [],
  createdAt: "2026-11-01T12:00:00Z",
  dismissed: false,
  refinements: [],
  workflow: {
    id: "wf-1",
    name: "Triage",
    description: "Scan, then wait for me.",
    inputLabel: null,
    disabled: false,
    nodes: [
      { id: "scan", name: "Scan", dependsOn: [], kind: "agent", agentId: "a1", promptTemplate: "Scan {{input}}", gateTitle: null, gateBody: null, requiredApprovals: null, onReject: null, rejectTarget: null },
      { id: "ok", name: "Ship?", dependsOn: ["scan"], kind: "gate", agentId: null, promptTemplate: null, gateTitle: "Ship it", gateBody: "", requiredApprovals: 1, onReject: "halt", rejectTarget: null },
    ],
  },
};

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "list_proposals") return [proposal];
    if (cmd === "list_agents") return [{ id: "a1", name: "Scout" }];
    return [];
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

describe("workflow proposals in the Review pane", () => {
  it("render the proposed stages instead of the plain-text body", async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={qc}>
        <RightRail width={400} sessionId="s1" pane="review" activeRuntimeId="" onChangePane={() => {}} onClose={() => {}} />
      </QueryClientProvider>,
    );
    expect(await screen.findByText("Add workflow “Triage”")).toBeInTheDocument();
    expect(await screen.findByText("@Scout")).toBeInTheDocument();
    expect(screen.getByText("Scan {{input}}")).toBeInTheDocument();
    expect(screen.getByText(/Approving adds this workflow/)).toBeInTheDocument();
    expect(screen.queryByText("1. Scan [scan], @Scout")).toBeNull();
    expect(screen.getByRole("button", { name: "Approve" })).toBeInTheDocument();
  });
});
