import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { WorkflowsPane, statusTone, statusLabel } from "./WorkflowsPane";
import * as ipc from "@/lib/ipc";

vi.mock("@/lib/ipc", () => ({
  listWorkflows: vi.fn(async () => [
    {
      id: "wf-1",
      name: "Review gate",
      description: "Implement, critique, approve.",
      inputLabel: "What should be implemented?",
      nodes: [
        { id: "implement", name: "Implement", dependsOn: [], kind: "agent", agentId: null, promptTemplate: "{{input}}", gateTitle: null, gateBody: null, requiredApprovals: null, onReject: null, rejectTarget: null, x: null, y: null },
      ],
    },
  ]),
  listWorkflowRuns: vi.fn(async () => [
    {
      id: "run-1",
      definitionId: "wf-1",
      definitionName: "Review gate",
      input: "add dark mode",
      status: "awaitingGate",
      startedAt: "2026-07-14T12:00:00Z",
      nodes: [
        { nodeId: "implement", name: "Implement", kind: "agent", status: "succeeded", messageId: "m1", proposalId: null, outputExcerpt: "did it", attempts: 0, error: "" },
        { nodeId: "approval", name: "Approval", kind: "gate", status: "awaitingApproval", messageId: null, proposalId: "p1", outputExcerpt: "", attempts: 0, error: "" },
      ],
    },
  ]),
  listAgents: vi.fn(async () => []),
  saveWorkflow: vi.fn(async (_sid: string, d: unknown) => d),
  removeWorkflow: vi.fn(async () => {}),
  addWorkflowPreset: vi.fn(async () => ({})),
  startWorkflowRun: vi.fn(async () => "run-2"),
  cancelWorkflowRun: vi.fn(async () => {}),
  resumeWorkflowRun: vi.fn(async () => {}),
  voteProposal: vi.fn(async () => null),
  onWorkflowRun: vi.fn(() => Promise.resolve(() => {})),
  onChatStream: vi.fn(() => Promise.resolve(() => {})),
  onWorkspaceSynced: vi.fn(() => Promise.resolve(() => {})),
}));

vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

function setup() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={qc}>
      <WorkflowsPane sessionId="s1" onEditWorkflow={vi.fn()} />
    </QueryClientProvider>,
  );
}

describe("WorkflowsPane", () => {
  beforeEach(() => vi.clearAllMocks());

  it("lists definitions and runs with node chips", async () => {
    setup();
    expect((await screen.findAllByText("Review gate")).length).toBeGreaterThan(0);
    expect(await screen.findByText("add dark mode")).toBeInTheDocument();
    expect(screen.getByText(/awaiting gate/)).toBeInTheDocument();
    expect(screen.getByText("Implement")).toBeInTheDocument();
  });

  it("starts a run with the typed input", async () => {
    const user = userEvent.setup();
    setup();
    await screen.findAllByText("Review gate");
    await user.click(screen.getByRole("button", { name: "Run" }));
    await user.type(screen.getByPlaceholderText("What should be implemented?"), "ship it{Enter}");
    expect(ipc.startWorkflowRun).toHaveBeenCalledWith("s1", "wf-1", "ship it");
  });

  it("votes an awaiting gate from the run card", async () => {
    const user = userEvent.setup();
    setup();
    await screen.findByText(/needs your decision/);
    await user.click(screen.getByRole("button", { name: "Approve" }));
    expect(ipc.voteProposal).toHaveBeenCalledWith("s1", "p1", true);
  });

  it("adds presets", async () => {
    const user = userEvent.setup();
    setup();
    await screen.findAllByText("Review gate");
    await user.click(screen.getByRole("button", { name: "+ Fan-out + vote" }));
    expect(ipc.addWorkflowPreset).toHaveBeenCalledWith("s1", "fanOutVote");
  });
});

describe("status helpers", () => {
  it("maps statuses to design-token tones", () => {
    expect(statusTone("succeeded").color).toBe("var(--hive-success)");
    expect(statusTone("failed").color).toBe("var(--hive-danger)");
    expect(statusTone("running").color).toBe("var(--hive-accent-cool)");
    expect(statusTone("pending").color).toBe("var(--hive-ink)");
  });

  it("humanizes camelCase statuses", () => {
    expect(statusLabel("awaitingApproval")).toBe("awaiting approval");
    expect(statusLabel("running")).toBe("running");
  });
});
