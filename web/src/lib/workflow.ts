// Client-side mirror of hive-core's workflow validation, for inline builder
// feedback. The backend re-validates authoritatively on save; this exists so
// the Save button can explain itself without a round-trip.

import type { WorkflowDefinitionDto, WorkflowNodeDto } from "@/lib/ipc";

const SLUG = /^[a-zA-Z0-9_-]+$/;

/// Node ids referenced as `{{nodes.<id>.output}}` in a template.
export function templateRefs(template: string): string[] {
  const refs: string[] = [];
  const re = /\{\{nodes\.([^}]+?)\.output\}\}/g;
  for (const m of template.matchAll(re)) {
    if (m[1] && !refs.includes(m[1])) refs.push(m[1]);
  }
  return refs;
}

/// Transitive upstream closure of a node (exclusive of itself).
function ancestorsOf(nodes: WorkflowNodeDto[], id: string): Set<string> {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const out = new Set<string>();
  const stack = [...(byId.get(id)?.dependsOn ?? [])];
  while (stack.length) {
    const cur = stack.pop()!;
    if (out.has(cur)) continue;
    const node = byId.get(cur);
    if (!node) continue;
    out.add(cur);
    stack.push(...node.dependsOn);
  }
  return out;
}

function hasCycle(nodes: WorkflowNodeDto[]): boolean {
  // Count unique deps so a duplicated "runs after" entry isn't misread as a
  // cycle (mirrors hive-core's topo_order).
  const indegree = new Map(nodes.map((n) => [n.id, new Set(n.dependsOn).size]));
  const queue = nodes.filter((n) => n.dependsOn.length === 0).map((n) => n.id);
  let seen = 0;
  while (queue.length) {
    const id = queue.pop()!;
    seen++;
    for (const n of nodes) {
      if (!n.dependsOn.includes(id)) continue;
      const d = (indegree.get(n.id) ?? 0) - 1;
      indegree.set(n.id, d);
      if (d === 0) queue.push(n.id);
    }
  }
  return seen !== nodes.length;
}

/// All problems with a draft, in display order. Empty ⇒ saveable.
export function validateWorkflowDraft(def: WorkflowDefinitionDto): string[] {
  const errors: string[] = [];
  if (!def.name.trim()) errors.push("Give the workflow a name.");
  if (def.nodes.length === 0) errors.push("Add at least one stage.");

  const ids = new Set<string>();
  for (const n of def.nodes) {
    if (!n.id || !SLUG.test(n.id)) {
      errors.push(`Stage id "${n.id}" must use only letters, digits, "-" or "_".`);
    } else if (ids.has(n.id)) {
      errors.push(`Duplicate stage id "${n.id}".`);
    }
    ids.add(n.id);
  }

  for (const n of def.nodes) {
    for (const dep of n.dependsOn) {
      if (dep === n.id) errors.push(`Stage "${n.name}" depends on itself.`);
      else if (!ids.has(dep)) errors.push(`Stage "${n.name}" depends on unknown stage "${dep}".`);
    }
  }

  if (def.nodes.length > 0 && hasCycle(def.nodes)) {
    errors.push("Stages contain a dependency cycle.");
  }

  if (!def.nodes.some((n) => n.kind === "agent")) {
    errors.push("Add at least one agent stage.");
  }

  for (const n of def.nodes) {
    const ancestors = ancestorsOf(def.nodes, n.id);
    const templates =
      n.kind === "agent" ? [n.promptTemplate ?? ""] : [n.gateTitle ?? "", n.gateBody ?? ""];
    for (const t of templates) {
      for (const ref of templateRefs(t)) {
        if (!ids.has(ref)) {
          errors.push(`Stage "${n.name}" references unknown stage "${ref}" in its template.`);
        } else if (!ancestors.has(ref)) {
          errors.push(
            `Stage "${n.name}" references "${ref}" but doesn't depend on it (add it under "Runs after").`,
          );
        }
      }
    }
    if (n.kind === "gate") {
      if ((n.requiredApprovals ?? 1) < 1) {
        errors.push(`Gate "${n.name}" needs at least one required approval.`);
      }
      if (n.onReject === "routeTo") {
        const target = n.rejectTarget ?? "";
        if (!ids.has(target)) {
          errors.push(`Gate "${n.name}" routes rejection to unknown stage "${target}".`);
        } else if (!ancestors.has(target)) {
          errors.push(`Gate "${n.name}" must route rejection to one of its upstream stages.`);
        }
      }
    }
  }

  return errors;
}

/// Layered DAG layout for the builder canvas: a node's column is its longest
/// dependency depth, its row is its index within that column. Pass-count is
/// capped so a (momentarily) cyclic draft can't loop forever — cycles just
/// render on whatever layer the cap left them.
export function layoutLayers(
  nodes: WorkflowNodeDto[],
): Map<string, { layer: number; row: number }> {
  const layer = new Map(nodes.map((n) => [n.id, 0]));
  for (let pass = 0; pass < nodes.length; pass++) {
    let changed = false;
    for (const n of nodes) {
      const want = Math.max(0, ...n.dependsOn.map((d) => (layer.get(d) ?? -1) + 1));
      if (want > (layer.get(n.id) ?? 0)) {
        layer.set(n.id, want);
        changed = true;
      }
    }
    if (!changed) break;
  }
  const rows = new Map<number, number>();
  const out = new Map<string, { layer: number; row: number }>();
  for (const n of nodes) {
    const l = layer.get(n.id) ?? 0;
    const row = rows.get(l) ?? 0;
    rows.set(l, row + 1);
    out.set(n.id, { layer: l, row });
  }
  return out;
}

/// Slugify a stage name into a template-safe id ("Judge results" → "judge-results").
export function slugify(name: string): string {
  return (
    name
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "") || "stage"
  );
}
