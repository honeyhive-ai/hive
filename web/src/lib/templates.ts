/// Saved prompt templates — reusable composer snippets, persisted locally.
/// Pure storage helpers (localStorage-backed) so they're unit-testable.

export interface PromptTemplate {
  id: string;
  name: string;
  body: string;
}

const KEY = "hive.promptTemplates";

export function loadTemplates(): PromptTemplate[] {
  try {
    const raw = window.localStorage.getItem(KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (t): t is PromptTemplate =>
        t && typeof t.id === "string" && typeof t.name === "string" && typeof t.body === "string",
    );
  } catch {
    return [];
  }
}

function persist(list: PromptTemplate[]) {
  window.localStorage.setItem(KEY, JSON.stringify(list));
}

/// Add a template; returns the new list. Empty name/body is rejected (no-op).
export function addTemplate(name: string, body: string): PromptTemplate[] {
  const n = name.trim();
  const b = body.trim();
  if (!n || !b) return loadTemplates();
  const list = loadTemplates();
  const id = `tpl-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`;
  const next = [...list, { id, name: n, body: b }];
  persist(next);
  return next;
}

export function removeTemplate(id: string): PromptTemplate[] {
  const next = loadTemplates().filter((t) => t.id !== id);
  persist(next);
  return next;
}
