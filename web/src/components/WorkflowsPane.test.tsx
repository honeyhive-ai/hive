import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { WorkflowsPane, statusTone, statusLabel, readingOrder } from "./WorkflowsPane";
import * as ipc from "@/lib/ipc";

vi.mock("@/lib/ipc", () => ({
  listWorkflows: vi.fn(async () => [
    {
      id: "wf-1",
      name: "Review gate",
      description: "Implement, critique, approve.",
      inputLabel: "What should be implemented?",
      nodes: [
        { id: "approval", name: "Approval", dependsOn: ["implement"], kind: "gate", agentId: null, promptTemplate: null, gateTitle: "Ship it?", gateBody: "", requiredApprovals: 2, onReject: "routeTo", rejectTarget: "implement" },
        { id: "implement", name: "Implement", dependsOn: [], kind: "agent", agentId: "a1", promptTemplate: "Build {{input}}", gateTitle: null, gateBody: null, requiredApprovals: null, onReject: null, rejectTarget: null },
      ],
      disabled: false,
    },
    {
      id: "wf-2",
      name: "Nightly",
      description: "",
      inputLabel: null,
      nodes: [],
      disabled: true,
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
  listAgents: vi.fn(async () => [{ id: "a1", name: "Coder" }]),
  removeWorkflow: vi.fn(async () => {}),
  setWorkflowEnabled: vi.fn(async () => {}),
  workflowChangeRequest: vi.fn(async () => "Please revise the workflow"),
  sendMessage: vi.fn(async () => {}),
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
      <WorkflowsPane sessionId="s1" />
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
    await user.click(screen.getAllByRole("button", { name: "Run" })[0]);
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

  it("is read-only: no editor, no presets, no new-workflow button", async () => {
    setup();
    await screen.findAllByText("Review gate");
    for (const name of ["Edit", "New workflow", "+ Review gate", "+ Fan-out + vote"]) {
      expect(screen.queryByRole("button", { name })).toBeNull();
    }
  });

  it("renders a definition's stages readably in dependency order", async () => {
    const user = userEvent.setup();
    setup();
    await user.click((await screen.findAllByText(/show stages/))[0]);
    expect(await screen.findByText("@Coder")).toBeInTheDocument();
    expect(screen.getByText("Build {{input}}")).toBeInTheDocument();
    expect(screen.getByText(/2 approvals/)).toBeInTheDocument();
    expect(screen.getByText(/reject retries from implement/)).toBeInTheDocument();
    const items = screen.getAllByRole("listitem").map((li) => li.textContent ?? "");
    expect(items[0]).toContain("Implement");
    expect(items[1]).toContain("Approval");
  });

  it("pauses, resumes and blocks running a paused workflow", async () => {
    const user = userEvent.setup();
    setup();
    await screen.findByText("Nightly");
    expect(screen.getByText("paused")).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Run" })[1]).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Pause workflow Review gate" }));
    expect(ipc.setWorkflowEnabled).toHaveBeenCalledWith("s1", "wf-1", false);
    await user.click(screen.getByRole("button", { name: "Resume workflow Nightly" }));
    expect(ipc.setWorkflowEnabled).toHaveBeenCalledWith("s1", "wf-2", true);
  });

  it("asks an agent to change a workflow by sending a chat message", async () => {
    const user = userEvent.setup();
    setup();
    await screen.findAllByText("Review gate");
    await user.click(screen.getAllByRole("button", { name: "Ask an agent to change this" })[0]);
    await user.type(screen.getByLabelText("Requested change"), "add a test stage{Enter}");
    expect(ipc.workflowChangeRequest).toHaveBeenCalledWith("s1", "wf-1", "add a test stage");
    expect(ipc.sendMessage).toHaveBeenCalledWith("s1", "Please revise the workflow");
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

describe("readingOrder", () => {
  it("puts dependencies first and keeps stored order for ties", () => {
    const n = (id: string, dependsOn: string[] = []) =>
      ({ id, name: id, dependsOn, kind: "agent" }) as Parameters<typeof readingOrder>[0][number];
    expect(readingOrder([n("c", ["b"]), n("a"), n("b", ["a"])]).map((x) => x.id)).toEqual(["a", "b", "c"]);
    expect(readingOrder([n("x"), n("y")]).map((x) => x.id)).toEqual(["x", "y"]);
  });
});
