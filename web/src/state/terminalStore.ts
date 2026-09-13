import { useSyncExternalStore } from "react";

// External store for the embedded terminal panel's tab metadata. The actual
// xterm instances + PTY wiring live in the terminal components (B3); this store
// only tracks which terminals exist, their titles, and which is active.
//
// A `useSyncExternalStore`-based store, dependency-light (no component imports)
// so it typechecks standalone and can be poked from non-React callbacks.

export interface TerminalMeta {
  id: string;
  title: string;
}

export interface TerminalState {
  terminals: TerminalMeta[];
  activeId: string | null;
}

let state: TerminalState = { terminals: [], activeId: null };
const listeners = new Set<() => void>();

function emit(next: TerminalState) {
  state = next;
  for (const l of listeners) l();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function getSnapshot(): TerminalState {
  return state;
}

/// Register a terminal (or update its title if the id already exists) and make
/// it active.
function add(id: string, title: string) {
  const exists = state.terminals.some((t) => t.id === id);
  const terminals = exists
    ? state.terminals.map((t) => (t.id === id ? { ...t, title } : t))
    : [...state.terminals, { id, title }];
  emit({ terminals, activeId: id });
}

/// Remove a terminal; if it was active, fall back to a neighbour.
function remove(id: string) {
  const idx = state.terminals.findIndex((t) => t.id === id);
  if (idx === -1) return;
  const terminals = state.terminals.filter((t) => t.id !== id);
  let activeId = state.activeId;
  if (activeId === id) {
    const neighbour = terminals[idx] ?? terminals[idx - 1] ?? null;
    activeId = neighbour ? neighbour.id : null;
  }
  emit({ terminals, activeId });
}

/// Make `id` the active terminal (no-op if it isn't registered).
function setActive(id: string) {
  if (state.activeId === id) return;
  if (!state.terminals.some((t) => t.id === id)) return;
  emit({ terminals: state.terminals, activeId: id });
}

/// Imperative handle — safe to call from anywhere (including non-React code).
export const terminalStore = {
  subscribe,
  getSnapshot,
  getState: getSnapshot,
  add,
  remove,
  setActive,
};

/// React hook — subscribes the calling component to terminal state.
export function useTerminalStore(): TerminalState {
  return useSyncExternalStore(subscribe, getSnapshot);
}
