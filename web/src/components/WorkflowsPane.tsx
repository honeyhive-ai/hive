import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  listWorkflows,
  listWorkflowRuns,
  removeWorkflow,
  addWorkflowPreset,
  startWorkflowRun,
  cancelWorkflowRun,
  resumeWorkflowRun,
  voteProposal,
  onWorkflowRun,
  onChatStream,
  onWorkspaceSynced,
  type WorkflowDefinitionDto,
  type WorkflowRunDto,
  type WorkflowNodeRunDto,
} from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";
import { Button, Section } from "@/components/ui";
import { RailFrame, Stack, EmptyHint, panelStyle, fieldStyle } from "@/components/RightRail";

/// Visual tone for a node/run status chip. Exported for tests.
export function statusTone(status: string): { color: string; background: string } {
  switch (status) {
    case "running":
      return { color: "var(--hive-accent-cool)", background: "rgba(90,120,255,0.12)" };
    case "awaitingApproval":
    case "awaitingGate":
      return { color: "var(--hive-warn)", background: "rgba(220,160,40,0.14)" };
    case "succeeded":
    case "completed":
      return { color: "var(--hive-success)", background: "rgba(34,160,90,0.14)" };
    case "failed":
    case "rejected":
    case "halted":
      return { color: "var(--hive-danger)", background: "rgba(200,70,70,0.14)" };
    default: // pending, skipped, canceled
      return { color: "var(--hive-ink)", background: "var(--hive-overlay)" };
  }
}

/// Human label for a status ("awaitingApproval" → "awaiting approval").
export function statusLabel(status: string): string {
  return status.replace(/([A-Z])/g, " $1").toLowerCase();
}

function blankDraft(): WorkflowDefinitionDto {
  return {
    id: "",
    name: "",
    description: "",
    inputLabel: null,
    nodes: [
      {
        id: "stage-1",
        name: "Stage 1",
        dependsOn: [],
        kind: "agent",
        agentId: null,
        promptTemplate: "{{input}}",
        gateTitle: null,
        gateBody: null,
        requiredApprovals: null,
        onReject: null,
        rejectTarget: null,
        x: null,
        y: null,
      },
    ],
  };
}

export function WorkflowsPane({
  sessionId,
  onEditWorkflow,
}: {
  sessionId: string;
  /// Opens the DAG editor in the main canvas (App owns that view).
  onEditWorkflow: (def: WorkflowDefinitionDto) => void;
}) {
  const qc = useQueryClient();
  const workflows = useQuery({
    queryKey: ["workflows", sessionId],
    queryFn: () => listWorkflows(sessionId),
  });
  const runs = useQuery({
    queryKey: ["workflow-runs", sessionId],
    queryFn: () => listWorkflowRuns(sessionId),
  });

  const [runTarget, setRunTarget] = useState<string | null>(null);
  const [runInput, setRunInput] = useState("");

  // Live run updates: the engine emits workflow://run on every persisted
  // transition; gates also create proposals and stages post chat messages.
  useEffect(() => {
    const unlisten = onWorkflowRun((e) => {
      if (e.sessionId !== sessionId) return;
      qc.invalidateQueries({ queryKey: ["workflow-runs", sessionId] });
      qc.invalidateQueries({ queryKey: ["proposals", sessionId] });
      qc.invalidateQueries({ queryKey: ["chat", sessionId] });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [sessionId, qc]);

  // Agents can author definitions via a [[workflow: …]] reply directive, so a
  // completed turn may have added one; remote teammates' edits land via sync.
  useEffect(() => {
    const unlistenStream = onChatStream((e) => {
      if (e.sessionId === sessionId && e.phase !== "delta") {
        qc.invalidateQueries({ queryKey: ["workflows", sessionId] });
      }
    });
    const unlistenSync = onWorkspaceSynced(() => {
      qc.invalidateQueries({ queryKey: ["workflows", sessionId] });
      qc.invalidateQueries({ queryKey: ["workflow-runs", sessionId] });
    });
    return () => {
      unlistenStream.then((fn) => fn());
      unlistenSync.then((fn) => fn());
    };
  }, [sessionId, qc]);

  async function start(wf: WorkflowDefinitionDto) {
    const input = runInput.trim();
    if (!input) return;
    try {
      await startWorkflowRun(sessionId, wf.id, input);
      setRunTarget(null);
      setRunInput("");
      qc.invalidateQueries({ queryKey: ["workflow-runs", sessionId] });
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  async function addPreset(preset: string) {
    try {
      await addWorkflowPreset(sessionId, preset);
      qc.invalidateQueries({ queryKey: ["workflows", sessionId] });
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  return (
    <RailFrame
      title="Workflows"
      subtitle="Multi-stage agent pipelines with approval gates — synced with this chat."
    >
      <Section title="Definitions">
        <Stack>
          {(workflows.data ?? []).map((wf) => (
            <div key={wf.id} className="rounded-2xl border px-4 py-3" style={panelStyle}>
              <div className="flex items-center justify-between gap-2">
                <div className="min-w-0">
                  <div className="truncate font-semibold">{wf.name}</div>
                  <div className="text-xs opacity-55">
                    {wf.nodes.length} stage{wf.nodes.length === 1 ? "" : "s"}
                  </div>
                </div>
                <div className="flex shrink-0 gap-1 text-xs">
                  <Button onClick={() => setRunTarget(runTarget === wf.id ? null : wf.id)}>Run</Button>
                  <Button onClick={() => onEditWorkflow(wf)}>Edit</Button>
                  <Button
                    onClick={() =>
                      confirmThen(`Remove workflow “${wf.name}”?`, async () => {
                        await removeWorkflow(sessionId, wf.id);
                        qc.invalidateQueries({ queryKey: ["workflows", sessionId] });
                      })
                    }
                  >
                    Remove
                  </Button>
                </div>
              </div>
              {wf.description && <p className="mt-1.5 text-xs leading-5 opacity-70">{wf.description}</p>}
              {runTarget === wf.id && (
                <div className="mt-2 flex gap-2">
                  <input
                    autoFocus
                    value={runInput}
                    onChange={(e) => setRunInput(e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && start(wf)}
                    placeholder={wf.inputLabel ?? "What should this run do?"}
                    className="min-w-0 flex-1 rounded-xl border px-3 py-1.5 text-sm outline-none"
                    style={fieldStyle}
                  />
                  <Button variant="primary" disabled={!runInput.trim()} onClick={() => start(wf)}>
                    Start
                  </Button>
                </div>
              )}
            </div>
          ))}
          {(workflows.data ?? []).length === 0 && (
            <EmptyHint text="No workflows yet. Start from a preset below, or build your own." />
          )}
        </Stack>
        <div className="mt-2 flex flex-wrap gap-2">
          <Button variant="primary" onClick={() => onEditWorkflow(blankDraft())}>
            New workflow
          </Button>
          <Button onClick={() => addPreset("reviewGate")}>+ Review gate</Button>
          <Button onClick={() => addPreset("fanOutVote")}>+ Fan-out + vote</Button>
        </div>
      </Section>

      <Section title="Runs">
        <Stack>
          {(runs.data ?? []).map((run) => (
            <RunCard key={run.id} sessionId={sessionId} run={run} />
          ))}
          {(runs.data ?? []).length === 0 && <EmptyHint text="No runs yet." />}
        </Stack>
      </Section>
    </RailFrame>
  );
}

function RunCard({ sessionId, run }: { sessionId: string; run: WorkflowRunDto }) {
  const qc = useQueryClient();
  const live = run.status === "running" || run.status === "awaitingGate";
  const tone = statusTone(run.status);

  async function act(fn: () => Promise<void>) {
    try {
      await fn();
      qc.invalidateQueries({ queryKey: ["workflow-runs", sessionId] });
      qc.invalidateQueries({ queryKey: ["proposals", sessionId] });
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  return (
    <div className="rounded-2xl border px-4 py-3" style={panelStyle}>
      <div className="flex items-center justify-between gap-2">
        <div className="min-w-0">
          <div className="truncate font-semibold">{run.definitionName}</div>
          <div className="truncate text-xs opacity-55">{run.input}</div>
        </div>
        <span
          className="shrink-0 rounded-lg px-2 py-0.5 text-[11px] font-medium"
          style={tone}
        >
          {statusLabel(run.status)}
        </span>
      </div>

      <div className="mt-2 flex flex-wrap gap-1.5">
        {run.nodes.map((n) => (
          <NodeChip key={n.nodeId} node={n} />
        ))}
      </div>

      {run.nodes
        .filter((n) => n.status === "awaitingApproval" && n.proposalId)
        .map((n) => (
          <div key={n.nodeId} className="mt-2 flex items-center gap-2 text-xs">
            <span className="opacity-70">“{n.name}” needs your decision:</span>
            <button
              className="rounded-lg px-2.5 py-1 font-medium"
              style={{ background: "rgba(34,160,90,0.2)", color: "var(--hive-success)" }}
              onClick={() => act(async () => void (await voteProposal(sessionId, n.proposalId!, true)))}
            >
              Approve
            </button>
            <button
              className="rounded-lg px-2.5 py-1 font-medium"
              style={{ background: "rgba(200,70,70,0.18)", color: "var(--hive-danger)" }}
              onClick={() => act(async () => void (await voteProposal(sessionId, n.proposalId!, false)))}
            >
              Reject
            </button>
          </div>
        ))}

      {run.nodes.some((n) => n.error) && (
        <div className="mt-2 text-xs" style={{ color: "var(--hive-danger)" }}>
          {run.nodes.find((n) => n.error)?.error}
        </div>
      )}

      {live && (
        <div className="mt-2 flex gap-2 text-xs">
          <Button onClick={() => act(() => cancelWorkflowRun(sessionId, run.id))}>Cancel</Button>
          <Button onClick={() => act(() => resumeWorkflowRun(sessionId, run.id))}>Resume</Button>
        </div>
      )}
    </div>
  );
}

function NodeChip({ node }: { node: WorkflowNodeRunDto }) {
  const tone = statusTone(node.status);
  return (
    <span
      className="rounded-lg px-2 py-0.5 text-[11px] font-medium"
      style={{
        ...tone,
        textDecoration: node.status === "skipped" ? "line-through" : undefined,
      }}
      title={
        node.outputExcerpt
          ? node.outputExcerpt
          : `${node.name}: ${statusLabel(node.status)}${node.error ? ` — ${node.error}` : ""}`
      }
    >
      {node.kind === "gate" ? "◈ " : ""}
      {node.name}
      {node.attempts > 1 ? ` ×${node.attempts}` : ""}
    </span>
  );
}
