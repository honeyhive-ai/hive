import { useMemo, useState } from "react";
import {
  ReactFlow,
  Background,
  BackgroundVariant,
  Handle,
  Position,
  MarkerType,
  type Node,
  type Edge,
  type NodeChange,
  type EdgeChange,
  type Connection,
  type NodeProps,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import type { WorkflowDefinitionDto, WorkflowNodeDto, WorkspaceAgentDto } from "@/lib/ipc";
import { Button, IconButton } from "@/components/ui";
import { IconX } from "@/lib/icons";
import { confirmThen } from "@/lib/confirm";
import { fieldStyle, SubtleSelectField } from "@/components/RightRail";
import { validateWorkflowDraft, layoutLayers, slugify } from "@/lib/workflow";

// Auto-layout spacing for nodes the user hasn't dragged yet: execution flows
// top-to-bottom (rows = dependency depth), parallel siblings spread
// horizontally. Dragged positions persist on the definition (x/y).
const NODE_W = 176;
const NODE_H = 64;
const SIBLING_GAP = 48;
const LAYER_GAP = 72;
const PAD = 28;

type StageNodeData = { node: WorkflowNodeDto };
type StageFlowNode = Node<StageNodeData, "stage">;

/// The node card rendered on the canvas. Handles sit top (in) / bottom (out)
/// so execution reads top-to-bottom; everything is design-token styled.
function StageNode({ data, selected }: NodeProps<StageFlowNode>) {
  const { node } = data;
  const handleStyle = {
    width: 9,
    height: 9,
    background: "var(--hive-accent-cool)",
    border: "2px solid var(--hive-panel)",
  };
  return (
    <div
      className="rounded-2xl border px-3 py-2 shadow-sm"
      style={{
        width: NODE_W,
        background: "var(--hive-panel)",
        color: "var(--hive-ink)",
        borderColor: selected ? "var(--hive-accent-cool)" : "var(--hive-line)",
        boxShadow: selected ? "0 0 0 1px var(--hive-accent-cool)" : undefined,
      }}
    >
      <Handle type="target" position={Position.Top} style={handleStyle} />
      <div className="flex items-center gap-1.5">
        <span
          className="rounded px-1 text-[9px] font-semibold uppercase tracking-wider"
          style={{
            color: node.kind === "gate" ? "var(--hive-warn)" : "var(--hive-accent-cool)",
            background: "var(--hive-overlay)",
          }}
        >
          {node.kind === "gate" ? "gate" : "agent"}
        </span>
        <span className="truncate text-sm font-medium">{node.name}</span>
      </div>
      <div className="mt-1 truncate font-mono text-[10px] opacity-45">{node.id}</div>
      <Handle type="source" position={Position.Bottom} style={handleStyle} />
    </div>
  );
}

const nodeTypes = { stage: StageNode };

/// One-click template variables: `{{input}}` plus each dependency's output.
/// A bare dependency edge is valid control flow, but when the point *is* the
/// data, these make wiring it explicit without typing template syntax.
function TemplateChips({
  node,
  onInsert,
}: {
  node: WorkflowNodeDto;
  onInsert: (snippet: string) => void;
}) {
  const chips = ["{{input}}", ...node.dependsOn.map((dep) => `{{nodes.${dep}.output}}`)];
  return (
    <div className="flex flex-wrap gap-1">
      {chips.map((c) => (
        <button
          key={c}
          type="button"
          onClick={() => onInsert(c)}
          title={`Insert ${c}`}
          className="rounded-lg border px-1.5 py-0.5 font-mono text-[10px] opacity-70 transition-opacity hover:opacity-100"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-overlay)" }}
        >
          {c}
        </button>
      ))}
    </div>
  );
}

/// Visual DAG editor on react-flow: drag nodes (positions persist on the
/// definition), drag handle-to-handle to add a dependency edge, select an
/// edge and press Delete/Backspace to remove it, pan/zoom freely. The
/// inspector edits the selected stage; "Runs after" and the canvas are two
/// views of the same dependsOn data.
///
/// Renders as a main-canvas view (the DiffView slot), not a dialog — a
/// canvas tool wants the full area, and an unsaved draft shouldn't be one
/// stray Escape/overlay-click away from vanishing. Closing with unsaved
/// changes asks first.
export function WorkflowBuilder({
  initial,
  agents,
  saving,
  onSave,
  onClose,
}: {
  initial: WorkflowDefinitionDto;
  agents: WorkspaceAgentDto[];
  saving: boolean;
  onSave: (draft: WorkflowDefinitionDto) => void;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState<WorkflowDefinitionDto>(() => structuredClone(initial));
  const [selectedId, setSelectedId] = useState<string | null>(draft.nodes[0]?.id ?? null);
  const errors = useMemo(() => validateWorkflowDraft(draft), [draft]);

  const rfNodes = useMemo<StageFlowNode[]>(() => {
    const auto = layoutLayers(draft.nodes);
    return draft.nodes.map((n) => {
      const a = auto.get(n.id) ?? { layer: 0, row: 0 };
      return {
        id: n.id,
        type: "stage" as const,
        position: {
          x: n.x ?? PAD + a.row * (NODE_W + SIBLING_GAP),
          y: n.y ?? PAD + a.layer * (NODE_H + LAYER_GAP),
        },
        data: { node: n },
        selected: n.id === selectedId,
      };
    });
  }, [draft.nodes, selectedId]);

  const rfEdges = useMemo<Edge[]>(
    () =>
      draft.nodes.flatMap((n) =>
        n.dependsOn.map((dep) => ({
          id: `${dep}->${n.id}`,
          source: dep,
          target: n.id,
        })),
      ),
    [draft.nodes],
  );

  function onNodesChange(changes: NodeChange[]) {
    for (const c of changes) {
      if (c.type === "position" && c.position) {
        const { x, y } = c.position;
        setDraft((d) => ({
          ...d,
          nodes: d.nodes.map((n) =>
            n.id === c.id ? { ...n, x: Math.round(x), y: Math.round(y) } : n,
          ),
        }));
      } else if (c.type === "remove") {
        removeStageById(c.id);
      }
    }
  }

  function onEdgesChange(changes: EdgeChange[]) {
    for (const c of changes) {
      if (c.type !== "remove") continue;
      const [dep, target] = c.id.split("->");
      setDraft((d) => ({
        ...d,
        nodes: d.nodes.map((n) =>
          n.id === target ? { ...n, dependsOn: n.dependsOn.filter((x) => x !== dep) } : n,
        ),
      }));
    }
  }

  function onConnect(conn: Connection) {
    if (!conn.source || !conn.target || conn.source === conn.target) return;
    setDraft((d) => ({
      ...d,
      nodes: d.nodes.map((n) =>
        n.id === conn.target && !n.dependsOn.includes(conn.source)
          ? { ...n, dependsOn: [...n.dependsOn, conn.source] }
          : n,
      ),
    }));
  }

  const selected = draft.nodes.find((n) => n.id === selectedId) ?? null;
  const selectedIndex = draft.nodes.findIndex((n) => n.id === selectedId);

  function patch(update: Partial<WorkflowDefinitionDto>) {
    setDraft((d) => ({ ...d, ...update }));
  }

  function patchNode(index: number, update: Partial<WorkflowNodeDto>) {
    setDraft((d) => ({
      ...d,
      nodes: d.nodes.map((n, i) => (i === index ? { ...n, ...update } : n)),
    }));
  }

  /// Renaming a stage id rewrites every reference to it — dependencies,
  /// reject-routes, and `{{nodes.<id>.output}}` templates — so a rename never
  /// silently strands the rest of the DAG.
  function renameNodeId(index: number, rawId: string) {
    const newId = rawId.trim();
    const oldId = draft.nodes[index].id;
    if (!newId || newId === oldId) return;
    const rewrite = (t: string | null) =>
      t === null ? null : t.split(`{{nodes.${oldId}.output}}`).join(`{{nodes.${newId}.output}}`);
    setDraft((d) => ({
      ...d,
      nodes: d.nodes.map((n, i) => ({
        ...n,
        id: i === index ? newId : n.id,
        dependsOn: n.dependsOn.map((dep) => (dep === oldId ? newId : dep)),
        rejectTarget: n.rejectTarget === oldId ? newId : n.rejectTarget,
        promptTemplate: rewrite(n.promptTemplate),
        gateTitle: rewrite(n.gateTitle),
        gateBody: rewrite(n.gateBody),
      })),
    }));
    if (selectedId === oldId) setSelectedId(newId);
  }

  function addStage(kind: "agent" | "gate") {
    let id = `${kind === "agent" ? "stage" : "gate"}-${draft.nodes.length + 1}`;
    while (draft.nodes.some((n) => n.id === id)) id = `${id}x`;
    const node: WorkflowNodeDto = {
      id,
      name: kind === "agent" ? `Stage ${draft.nodes.length + 1}` : "Approval",
      dependsOn: draft.nodes.length > 0 ? [draft.nodes[draft.nodes.length - 1].id] : [],
      kind,
      agentId: null,
      promptTemplate: kind === "agent" ? "{{input}}" : null,
      gateTitle: kind === "gate" ? "Approve: {{input}}" : null,
      gateBody: kind === "gate" ? "" : null,
      requiredApprovals: kind === "gate" ? 1 : null,
      onReject: kind === "gate" ? "halt" : null,
      rejectTarget: null,
      x: null,
      y: null,
    };
    setDraft((d) => ({ ...d, nodes: [...d.nodes, node] }));
    setSelectedId(id);
  }

  function removeStageById(id: string) {
    setDraft((d) => ({
      ...d,
      nodes: d.nodes
        .filter((n) => n.id !== id)
        .map((n) => ({ ...n, dependsOn: n.dependsOn.filter((dep) => dep !== id) })),
    }));
    if (selectedId === id) setSelectedId(null);
  }

  function toggleDep(index: number, dep: string) {
    setDraft((d) => ({
      ...d,
      nodes: d.nodes.map((n, i) =>
        i === index
          ? {
              ...n,
              dependsOn: n.dependsOn.includes(dep)
                ? n.dependsOn.filter((x) => x !== dep)
                : [...n.dependsOn, dep],
            }
          : n,
      ),
    }));
  }

  const inputClass = "w-full rounded-xl border px-3 py-2 text-sm outline-none";

  /// Discard-guard: closing is an explicit action now, but still confirm if
  /// the draft has unsaved edits.
  function requestClose() {
    if (JSON.stringify(draft) === JSON.stringify(initial)) {
      onClose();
      return;
    }
    confirmThen("Discard unsaved workflow changes?", onClose);
  }

  return (
    <div
      className="flex h-full min-h-0 flex-col"
      style={{ background: "var(--hive-panel)", color: "var(--hive-ink)" }}
    >
      <div
        className="flex items-center gap-2 border-b px-5 py-3"
        style={{ borderColor: "var(--hive-line)" }}
      >
        <h2 className="shrink-0 text-base font-semibold">
          {initial.id ? "Edit workflow" : "New workflow"}
        </h2>
        <input
          value={draft.name}
          onChange={(e) => patch({ name: e.target.value })}
          placeholder="Workflow name"
          className="min-w-0 flex-1 rounded-xl border px-3 py-1.5 text-sm outline-none"
          style={fieldStyle}
        />
        <input
          value={draft.inputLabel ?? ""}
          onChange={(e) => patch({ inputLabel: e.target.value || null })}
          placeholder="Input prompt label (optional)"
          className="hidden min-w-0 flex-1 rounded-xl border px-3 py-1.5 text-sm outline-none sm:block"
          style={fieldStyle}
        />
        <IconButton label="Close editor" onClick={requestClose}>
          <IconX size={16} />
        </IconButton>
      </div>

      <div className="flex min-h-0 flex-1">
        {/* Canvas */}
        <div className="workflow-canvas min-w-0 flex-1">
          <ReactFlow
            nodes={rfNodes}
            edges={rfEdges}
            nodeTypes={nodeTypes}
            onNodesChange={onNodesChange}
            onEdgesChange={onEdgesChange}
            onConnect={onConnect}
            onNodeClick={(_, node) => setSelectedId(node.id)}
            onPaneClick={() => setSelectedId(null)}
            fitView
            fitViewOptions={{ padding: 0.25, maxZoom: 1 }}
            deleteKeyCode={["Backspace", "Delete"]}
            defaultEdgeOptions={{
              style: { stroke: "var(--hive-line)", strokeWidth: 1.5 },
              markerEnd: { type: MarkerType.ArrowClosed, color: "var(--hive-line)" },
            }}
            style={{ background: "transparent" }}
          >
            <Background variant={BackgroundVariant.Dots} gap={22} size={1} color="var(--hive-line)" />
          </ReactFlow>
        </div>

        {/* Inspector */}
        <div
          className="flex w-80 shrink-0 flex-col overflow-y-auto border-l px-4 py-3"
          style={{ borderColor: "var(--hive-line)" }}
        >
          {selected && selectedIndex >= 0 ? (
            <div className="space-y-2.5">
              <input
                value={selected.name}
                onChange={(e) => patchNode(selectedIndex, { name: e.target.value })}
                onBlur={() => {
                  // Seed a readable id from the name while the id is still auto.
                  if (/^(stage|gate)-\d+x*$/.test(selected.id)) {
                    renameNodeId(selectedIndex, slugify(selected.name));
                  }
                }}
                placeholder="Stage name"
                className={`${inputClass} font-medium`}
                style={fieldStyle}
              />
              <div className="flex items-center gap-2 text-xs opacity-70">
                <span>id</span>
                <input
                  value={selected.id}
                  onChange={(e) => renameNodeId(selectedIndex, e.target.value)}
                  className="min-w-0 flex-1 rounded-lg border px-2 py-1 font-mono text-xs outline-none"
                  style={fieldStyle}
                  aria-label="Stage id"
                />
              </div>

              <SubtleSelectField
                label="Kind"
                value={selected.kind}
                onChange={(v) =>
                  patchNode(
                    selectedIndex,
                    v === "gate"
                      ? {
                          kind: "gate",
                          gateTitle: selected.gateTitle ?? "Approve: {{input}}",
                          requiredApprovals: selected.requiredApprovals ?? 1,
                          onReject: selected.onReject ?? "halt",
                        }
                      : { kind: "agent", promptTemplate: selected.promptTemplate ?? "{{input}}" },
                  )
                }
              >
                <option value="agent">Agent</option>
                <option value="gate">Approval gate</option>
              </SubtleSelectField>

              {draft.nodes.length > 1 && (
                <div>
                  <div className="mb-1 text-[10px] font-semibold uppercase tracking-[0.12em] opacity-45">
                    Runs after
                  </div>
                  <div className="space-y-1 text-xs">
                    {draft.nodes
                      .filter((n) => n.id !== selected.id)
                      .map((other) => (
                        <label key={other.id} className="flex items-center gap-1.5">
                          <input
                            type="checkbox"
                            checked={selected.dependsOn.includes(other.id)}
                            onChange={() => toggleDep(selectedIndex, other.id)}
                          />
                          <span className="truncate">{other.name}</span>
                        </label>
                      ))}
                    {selected.dependsOn.length === 0 && (
                      <div className="opacity-50">(starts immediately)</div>
                    )}
                  </div>
                </div>
              )}

              {selected.kind === "agent" ? (
                <>
                  <SubtleSelectField
                    label="Agent"
                    value={selected.agentId ?? ""}
                    onChange={(v) => patchNode(selectedIndex, { agentId: v || null })}
                  >
                    <option value="">Primary runtime</option>
                    {agents.map((a) => (
                      <option key={a.id} value={a.id}>
                        {a.name}
                        {a.role ? ` — ${a.role}` : ""}
                      </option>
                    ))}
                  </SubtleSelectField>
                  <TemplateChips
                    node={selected}
                    onInsert={(snippet) =>
                      patchNode(selectedIndex, {
                        promptTemplate: `${selected.promptTemplate ?? ""}${
                          (selected.promptTemplate ?? "").endsWith("\n") ||
                          !(selected.promptTemplate ?? "")
                            ? ""
                            : "\n"
                        }${snippet}`,
                      })
                    }
                  />
                  <textarea
                    value={selected.promptTemplate ?? ""}
                    onChange={(e) => patchNode(selectedIndex, { promptTemplate: e.target.value })}
                    placeholder="Prompt — use {{input}} and {{nodes.<id>.output}}"
                    rows={6}
                    className="w-full resize-y rounded-xl border px-3 py-2 font-mono text-xs outline-none"
                    style={fieldStyle}
                  />
                </>
              ) : (
                <>
                  <input
                    value={selected.gateTitle ?? ""}
                    onChange={(e) => patchNode(selectedIndex, { gateTitle: e.target.value })}
                    placeholder="Proposal title template"
                    className={inputClass}
                    style={fieldStyle}
                  />
                  <TemplateChips
                    node={selected}
                    onInsert={(snippet) =>
                      patchNode(selectedIndex, {
                        gateBody: `${selected.gateBody ?? ""}${
                          (selected.gateBody ?? "").endsWith("\n") || !(selected.gateBody ?? "")
                            ? ""
                            : "\n"
                        }${snippet}`,
                      })
                    }
                  />
                  <textarea
                    value={selected.gateBody ?? ""}
                    onChange={(e) => patchNode(selectedIndex, { gateBody: e.target.value })}
                    placeholder="Proposal body template (optional)"
                    rows={3}
                    className="w-full resize-y rounded-xl border px-3 py-2 font-mono text-xs outline-none"
                    style={fieldStyle}
                  />
                  <label className="flex items-center justify-between gap-2 text-xs">
                    <span className="opacity-70">Approvals needed</span>
                    <input
                      type="number"
                      min={1}
                      value={selected.requiredApprovals ?? 1}
                      onChange={(e) =>
                        patchNode(selectedIndex, {
                          requiredApprovals: Math.max(1, Number(e.target.value) || 1),
                        })
                      }
                      className="w-16 rounded-lg border px-2 py-1 outline-none"
                      style={fieldStyle}
                    />
                  </label>
                  <SubtleSelectField
                    label="On reject"
                    value={selected.onReject ?? "halt"}
                    onChange={(v) => patchNode(selectedIndex, { onReject: v })}
                  >
                    <option value="halt">Halt the run</option>
                    <option value="routeTo">Retry from stage…</option>
                  </SubtleSelectField>
                  {selected.onReject === "routeTo" && (
                    <SubtleSelectField
                      label="Retry from"
                      value={selected.rejectTarget ?? ""}
                      onChange={(v) => patchNode(selectedIndex, { rejectTarget: v || null })}
                    >
                      <option value="">Choose a stage…</option>
                      {draft.nodes
                        .filter((n) => n.id !== selected.id)
                        .map((other) => (
                          <option key={other.id} value={other.id}>
                            {other.name}
                          </option>
                        ))}
                    </SubtleSelectField>
                  )}
                </>
              )}

              <Button className="w-full" onClick={() => removeStageById(selected.id)}>
                Remove stage
              </Button>
            </div>
          ) : (
            <div className="space-y-2.5">
              <div className="text-sm opacity-60">
                Select a stage to configure it. Drag from a stage's bottom
                handle to another's top handle to add a dependency; select an
                edge and press Delete to remove it.
              </div>
              <input
                value={draft.description}
                onChange={(e) => patch({ description: e.target.value })}
                placeholder="What does this workflow do?"
                className={inputClass}
                style={fieldStyle}
              />
            </div>
          )}

          {errors.length > 0 && (
            <ul
              className="mt-3 space-y-1 rounded-xl border px-3 py-2 text-xs"
              style={{ borderColor: "rgba(200,70,70,0.35)", color: "var(--hive-danger)" }}
            >
              {errors.map((e, i) => (
                <li key={i}>{e}</li>
              ))}
            </ul>
          )}
        </div>
      </div>

      <div
        className="flex items-center gap-2 border-t px-5 py-3"
        style={{ borderColor: "var(--hive-line)" }}
      >
        <Button onClick={() => addStage("agent")}>+ Agent stage</Button>
        <Button onClick={() => addStage("gate")}>+ Approval gate</Button>
        <Button
          onClick={() =>
            patch({ nodes: draft.nodes.map((n) => ({ ...n, x: null, y: null })) })
          }
        >
          Auto-layout
        </Button>
        <div className="flex-1" />
        <Button onClick={requestClose}>Cancel</Button>
        <Button variant="primary" disabled={errors.length > 0 || saving} onClick={() => onSave(draft)}>
          {saving ? "Saving…" : "Save workflow"}
        </Button>
      </div>
    </div>
  );
}
