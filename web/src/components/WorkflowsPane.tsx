import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  listAgents,
  listWorkflows,
  listWorkflowRuns,
  removeWorkflow,
  setWorkflowEnabled,
  workflowChangeRequest,
  sendMessage,
  startWorkflowRun,
  cancelWorkflowRun,
  resumeWorkflowRun,
  voteProposal,
  onWorkflowRun,
  onChatStream,
  onWorkspaceSynced,
  type WorkflowDefinitionDto,
  type WorkflowNodeDto,
  type WorkflowRunDto,
  type WorkflowNodeRunDto,
} from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";
import { Button, Section, ErrorState } from "@/components/ui";
import { RailFrame, Stack, EmptyHint, panelStyle, fieldStyle } from "@/components/RightRail";

/// Visual tone for a node/run status chip. Exported for tests.
export function statusTone(status: string): { color: string; background: string } {
  switch (status) {
    case "running":
      return { color: "var(--hive-accent-cool)", background: "color-mix(in srgb, var(--hive-accent-cool) 12%, transparent)" };
    case "awaitingApproval":
    case "awaitingGate":
      return { color: "var(--hive-warn)", background: "color-mix(in srgb, var(--hive-accent-warm) 14%, transparent)" };
    case "succeeded":
    case "completed":
      return { color: "var(--hive-success)", background: "color-mix(in srgb, var(--hive-success) 14%, transparent)" };
    case "failed":
    case "rejected":
    case "halted":
      return { color: "var(--hive-danger)", background: "color-mix(in srgb, var(--hive-danger) 14%, transparent)" };
    default: // pending, skipped, canceled
      return { color: "var(--hive-ink)", background: "var(--hive-overlay)" };
  }
}

/// Human label for a status ("awaitingApproval" → "awaiting approval").
export function statusLabel(status: string): string {
  return status.replace(/([A-Z])/g, " $1").toLowerCase();
}

/// Stages in an order a person reads top to bottom: dependencies first,
/// ties in stored order. A malformed graph (it can't be: definitions are
/// validated) falls back to stored order. Exported for tests.
export function readingOrder(nodes: WorkflowNodeDto[]): WorkflowNodeDto[] {
  const placed = new Set<string>();
  const out: WorkflowNodeDto[] = [];
  const ids = new Set(nodes.map((n) => n.id));
  let progress = true;
  while (out.length < nodes.length && progress) {
    progress = false;
    for (const n of nodes) {
      if (placed.has(n.id)) continue;
      if (n.dependsOn.every((d) => placed.has(d) || !ids.has(d))) {
        placed.add(n.id);
        out.push(n);
        progress = true;
      }
    }
  }
  return out.length === nodes.length ? out : nodes;
}

/// A read-only rendering of a definition's stages: who runs each one, what
/// it waits for, its prompt, and how a gate decides. Shared by the Workflows
/// pane and workflow proposals in the Review pane.
export function WorkflowStages({
  nodes,
  agentName,
}: {
  nodes: WorkflowNodeDto[];
  agentName: (agentId: string) => string | undefined;
}) {
  return (
    <ol className="mt-2 flex flex-col gap-1.5">
      {readingOrder(nodes).map((n, i) => (
        <li key={n.id} className="rounded-lg border px-2.5 py-1.5 text-xs" style={{ borderColor: "var(--hive-line)" }}>
          <div className="flex flex-wrap items-baseline gap-x-1.5">
            <span className="opacity-50">{i + 1}.</span>
            <span className="font-medium">
              {n.kind === "gate" ? "◈ " : ""}
              {n.name}
            </span>
            <span className="opacity-55">
              {n.kind === "gate"
                ? `approval gate · ${n.requiredApprovals ?? 1} approval${(n.requiredApprovals ?? 1) === 1 ? "" : "s"}`
                : n.agentId
                  ? `@${agentName(n.agentId) ?? "agent not in this chat"}`
                  : "primary runtime"}
            </span>
            {n.dependsOn.length > 0 && <span className="opacity-45">after {n.dependsOn.join(", ")}</span>}
          </div>
          {n.kind === "gate" ? (
            <div className="mt-0.5 opacity-70">
              {n.gateTitle}
              {" · "}
              {n.onReject === "routeTo" && n.rejectTarget ? `reject retries from ${n.rejectTarget}` : "reject halts the run"}
            </div>
          ) : (
            n.promptTemplate && (
              <div className="mt-0.5 max-h-32 overflow-auto whitespace-pre-wrap break-words opacity-70">
                {n.promptTemplate}
              </div>
            )
          )}
        </li>
      ))}
    </ol>
  );
}

export function WorkflowsPane({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const workflows = useQuery({
    queryKey: ["workflows", sessionId],
    queryFn: () => listWorkflows(sessionId),
  });
  const runs = useQuery({
    queryKey: ["workflow-runs", sessionId],
    queryFn: () => listWorkflowRuns(sessionId),
  });
  const agents = useQuery({
    queryKey: ["agents", sessionId],
    queryFn: () => listAgents(sessionId),
  });
  const agentName = (id: string) => agents.data?.find((a) => a.id === id)?.name;

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

  // An approved workflow proposal adds a definition (here or on a teammate's
  // device, arriving via sync), so refresh after turns and syncs.
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

  return (
    <RailFrame
      title="Workflows"
      subtitle="Multi-stage agent pipelines with approval gates. Agents write them; you approve, run and pause them."
    >
      <Section title="Definitions">
        <Stack>
          {(workflows.data ?? []).map((wf) => (
            <DefinitionCard key={wf.id} sessionId={sessionId} wf={wf} agentName={agentName} />
          ))}
          {workflows.isError ? (
            <ErrorState text="Couldn't load workflows." onRetry={() => void workflows.refetch()} />
          ) : (
            (workflows.data ?? []).length === 0 && (
              <EmptyHint text="No workflows yet. Ask an agent in this chat to set one up, e.g. “build a workflow that drafts, reviews, and waits for my approval”. It arrives in the Review pane; approve it and it shows up here." />
            )
          )}
        </Stack>
      </Section>

      <Section title="Runs">
        <Stack>
          {(runs.data ?? []).map((run) => (
            <RunCard key={run.id} sessionId={sessionId} run={run} />
          ))}
          {runs.isError ? (
            <ErrorState text="Couldn't load runs." onRetry={() => void runs.refetch()} />
          ) : (
            (runs.data ?? []).length === 0 && <EmptyHint text="No runs yet." />
          )}
        </Stack>
      </Section>
    </RailFrame>
  );
}

type Open = "none" | "run" | "change";

function DefinitionCard({
  sessionId,
  wf,
  agentName,
}: {
  sessionId: string;
  wf: WorkflowDefinitionDto;
  agentName: (agentId: string) => string | undefined;
}) {
  const qc = useQueryClient();
  const [open, setOpen] = useState<Open>("none");
  const [showStages, setShowStages] = useState(false);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);

  function toggle(which: Open) {
    setOpen((o) => (o === which ? "none" : which));
    setText("");
  }

  async function act(fn: () => Promise<unknown>, after?: () => void) {
    setBusy(true);
    try {
      await fn();
      after?.();
      qc.invalidateQueries({ queryKey: ["workflows", sessionId] });
      qc.invalidateQueries({ queryKey: ["workflow-runs", sessionId] });
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  function submit() {
    const value = text.trim();
    if (!value) return;
    if (open === "run") {
      void act(() => startWorkflowRun(sessionId, wf.id, value), () => toggle("run"));
    } else if (open === "change") {
      void act(
        async () => {
          const message = await workflowChangeRequest(sessionId, wf.id, value);
          await sendMessage(sessionId, message);
        },
        () => {
          toggle("change");
          toast.success("Asked the agent. Its revision will arrive in the Review pane.");
          qc.invalidateQueries({ queryKey: ["chat", sessionId] });
        },
      );
    }
  }

  return (
    <div className="rounded-xl border px-4 py-3" style={panelStyle}>
      <div className="flex items-start justify-between gap-2">
        <button
          type="button"
          className="min-w-0 text-left"
          onClick={() => setShowStages((v) => !v)}
          aria-expanded={showStages}
        >
          <div className="flex items-center gap-1.5">
            <span className="truncate font-semibold">{wf.name}</span>
            {wf.disabled && (
              <span className="shrink-0 rounded-md px-1.5 py-0.5 text-[10px] font-medium" style={statusTone("pending")}>
                paused
              </span>
            )}
          </div>
          <div className="text-xs opacity-55">
            {wf.nodes.length} stage{wf.nodes.length === 1 ? "" : "s"} · {showStages ? "hide" : "show"} stages
          </div>
        </button>
        <div className="flex shrink-0 flex-wrap justify-end gap-1 text-xs">
          <Button disabled={busy || wf.disabled} onClick={() => toggle("run")}>
            Run
          </Button>
          <Button
            disabled={busy}
            aria-label={`${wf.disabled ? "Resume" : "Pause"} workflow ${wf.name}`}
            onClick={() => act(() => setWorkflowEnabled(sessionId, wf.id, wf.disabled))}
          >
            {wf.disabled ? "Resume" : "Pause"}
          </Button>
          <Button
            disabled={busy}
            onClick={() =>
              confirmThen(`Delete workflow “${wf.name}”? Past runs stay in the history.`, () =>
                act(() => removeWorkflow(sessionId, wf.id)),
              )
            }
          >
            Delete
          </Button>
        </div>
      </div>
      {wf.description && <p className="mt-1.5 text-xs leading-5 opacity-70">{wf.description}</p>}
      {showStages && <WorkflowStages nodes={wf.nodes} agentName={agentName} />}
      {open !== "none" && (
        <div className="mt-2 flex gap-2">
          <input
            autoFocus
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && submit()}
            placeholder={
              open === "run" ? (wf.inputLabel ?? "What should this run do?") : "What should change?"
            }
            aria-label={open === "run" ? "Run input" : "Requested change"}
            className="min-w-0 flex-1 rounded-xl border px-3 py-1.5 text-sm outline-none"
            style={fieldStyle}
          />
          <Button variant="primary" disabled={busy || !text.trim()} onClick={submit}>
            {open === "run" ? "Start" : "Ask"}
          </Button>
        </div>
      )}
      {open !== "change" && (
        <button
          type="button"
          className="mt-2 text-xs font-medium hover:underline"
          style={{ color: "var(--hive-accent-cool)" }}
          onClick={() => toggle("change")}
          disabled={busy}
        >
          Ask an agent to change this
        </button>
      )}
    </div>
  );
}

function RunCard({ sessionId, run }: { sessionId: string; run: WorkflowRunDto }) {
  const qc = useQueryClient();
  const live = run.status === "running" || run.status === "awaitingGate";
  // A resumable run in an interruptible state whose in-process driver is gone
  // (e.g. after an app restart). When the driver is alive, Resume would only
  // return "already being driven", so hide it.
  const resumable = live && !run.driverAlive;
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
    <div className="rounded-xl border px-4 py-3" style={panelStyle}>
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
              style={{ background: "color-mix(in srgb, var(--hive-success) 20%, transparent)", color: "var(--hive-success)" }}
              onClick={() => act(async () => void (await voteProposal(sessionId, n.proposalId!, true)))}
            >
              Approve
            </button>
            <button
              className="rounded-lg px-2.5 py-1 font-medium"
              style={{ background: "color-mix(in srgb, var(--hive-danger) 18%, transparent)", color: "var(--hive-danger)" }}
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
          {resumable && (
            <Button onClick={() => act(() => resumeWorkflowRun(sessionId, run.id))}>Resume</Button>
          )}
        </div>
      )}
    </div>
  );
}

function NodeChip({ node }: { node: WorkflowNodeRunDto }) {
  const tone = statusTone(node.status);
  const [open, setOpen] = useState(false);
  // A stage's output/error was only reachable via the native hover tooltip.
  // Make the chip click-to-expand so a run is actually readable — the stage's
  // status, attempts, error, and output excerpt inline (a full run-log view).
  const hasDetail = Boolean(node.outputExcerpt || node.error || node.attempts > 1);
  return (
    <span className="inline-flex flex-col">
      <button
        type="button"
        onClick={() => hasDetail && setOpen((o) => !o)}
        className="rounded-lg px-2 py-0.5 text-left text-[11px] font-medium"
        style={{
          ...tone,
          textDecoration: node.status === "skipped" ? "line-through" : undefined,
          cursor: hasDetail ? "pointer" : "default",
        }}
        aria-expanded={hasDetail ? open : undefined}
        title={`${node.name}: ${statusLabel(node.status)}`}
      >
        {node.kind === "gate" ? "◈ " : ""}
        {node.name}
        {node.attempts > 1 ? ` ×${node.attempts}` : ""}
      </button>
      {open && (
        <div
          className="mt-1 max-w-full rounded-lg border p-2 text-[11px] leading-[1.5]"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-overlay)" }}
        >
          <div className="opacity-55">
            {statusLabel(node.status)}
            {node.attempts > 1 ? ` · ${node.attempts} attempts` : ""}
          </div>
          {node.error && (
            <div className="mt-1 whitespace-pre-wrap break-words" style={{ color: "var(--hive-danger)" }}>
              {node.error}
            </div>
          )}
          {node.outputExcerpt && (
            <div className="mt-1 max-h-40 overflow-auto whitespace-pre-wrap break-words opacity-80">
              {node.outputExcerpt}
            </div>
          )}
        </div>
      )}
    </span>
  );
}
