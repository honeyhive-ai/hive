import { describe, it, expect } from "vitest";
import { validateWorkflowDraft, templateRefs, slugify, layoutLayers } from "./workflow";
import type { WorkflowDefinitionDto, WorkflowNodeDto } from "@/lib/ipc";

function agent(id: string, deps: string[] = [], template = "{{input}}"): WorkflowNodeDto {
  return {
    id,
    name: id,
    dependsOn: deps,
    kind: "agent",
    agentId: null,
    promptTemplate: template,
    gateTitle: null,
    gateBody: null,
    requiredApprovals: null,
    onReject: null,
    rejectTarget: null,
    x: null,
    y: null,
  };
}

function gate(id: string, deps: string[], overrides: Partial<WorkflowNodeDto> = {}): WorkflowNodeDto {
  return {
    ...agent(id, deps),
    kind: "gate",
    promptTemplate: null,
    gateTitle: "Approve: {{input}}",
    gateBody: "",
    requiredApprovals: 1,
    onReject: "halt",
    ...overrides,
  };
}

function def(nodes: WorkflowNodeDto[]): WorkflowDefinitionDto {
  return { id: "", name: "test", description: "", inputLabel: null, nodes };
}

describe("validateWorkflowDraft", () => {
  it("accepts a valid chain with a gate", () => {
    const d = def([
      agent("a"),
      agent("b", ["a"], "Review: {{nodes.a.output}}"),
      gate("g", ["b"], { onReject: "routeTo", rejectTarget: "a" }),
    ]);
    expect(validateWorkflowDraft(d)).toEqual([]);
  });

  it("flags cycles", () => {
    const d = def([agent("a", ["b"]), agent("b", ["a"])]);
    expect(validateWorkflowDraft(d).join(" ")).toMatch(/cycle/i);
  });

  it("flags duplicate and malformed ids", () => {
    expect(validateWorkflowDraft(def([agent("a"), agent("a")])).join(" ")).toMatch(/duplicate/i);
    expect(validateWorkflowDraft(def([agent("bad id!")])).join(" ")).toMatch(/letters/i);
  });

  it("flags template refs to stages that are not upstream", () => {
    const d = def([agent("a"), agent("b", [], "{{nodes.a.output}}")]);
    expect(validateWorkflowDraft(d).join(" ")).toMatch(/doesn't depend/);
    const unknown = def([agent("a", [], "{{nodes.ghost.output}}")]);
    expect(validateWorkflowDraft(unknown).join(" ")).toMatch(/unknown stage/i);
  });

  it("flags gate rules", () => {
    const noAgent = def([gate("g", [])]);
    expect(validateWorkflowDraft(noAgent).join(" ")).toMatch(/agent stage/i);
    const badRoute = def([agent("a"), agent("b"), gate("g", ["a"], { onReject: "routeTo", rejectTarget: "b" })]);
    expect(validateWorkflowDraft(badRoute).join(" ")).toMatch(/upstream/i);
  });
});

describe("templateRefs", () => {
  it("extracts and dedupes refs", () => {
    expect(templateRefs("{{nodes.a.output}} {{nodes.b-2.output}} {{nodes.a.output}}")).toEqual([
      "a",
      "b-2",
    ]);
  });
});

describe("layoutLayers", () => {
  it("columns follow longest dependency depth; rows count within a column", () => {
    const layout = layoutLayers([
      agent("a"),
      agent("b"),
      agent("c", ["a", "b"]),
      agent("d", ["a", "c"]), // longest path a→c→d ⇒ layer 2
    ]);
    expect(layout.get("a")).toEqual({ layer: 0, row: 0 });
    expect(layout.get("b")).toEqual({ layer: 0, row: 1 });
    expect(layout.get("c")).toEqual({ layer: 1, row: 0 });
    expect(layout.get("d")).toEqual({ layer: 2, row: 0 });
  });

  it("terminates on a (momentarily) cyclic draft", () => {
    const layout = layoutLayers([agent("a", ["b"]), agent("b", ["a"])]);
    expect(layout.size).toBe(2); // no hang; exact layers don't matter mid-edit
  });
});

describe("slugify", () => {
  it("makes template-safe ids", () => {
    expect(slugify("Judge results!")).toBe("judge-results");
    expect(slugify("  ")).toBe("stage");
  });
});
