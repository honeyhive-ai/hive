import { useSyncExternalStore } from "react";

// External store for the code editor's tab metadata. Monaco models + view
// state live in the editor components (B1); this store only tracks which files
// are open, which is active, and each tab's dirty flag — plus a `pendingOpen`
// slot other views (diff / chat @file / workflow stages) use to request that a
// path be opened in the editor.
//
// A `useSyncExternalStore`-based store (no zustand in this repo). It is
// dependency-light on purpose (no component imports) so it typechecks
// standalone and can be poked from non-React callbacks.

export interface EditorTab {
  path: string;
  dirty: boolean;
}

export interface EditorState {
  tabs: EditorTab[];
  activePath: string | null;
  /// A path another view asked the editor to open; the editor consumes it.
  pendingOpen: string | null;
}

let state: EditorState = { tabs: [], activePath: null, pendingOpen: null };
const listeners = new Set<() => void>();

function emit(next: EditorState) {
  // New object identity so useSyncExternalStore detects the change; the
  // snapshot is the stable `state` reference until the next mutation.
  state = next;
  for (const l of listeners) l();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function getSnapshot(): EditorState {
  return state;
}

/// Open `path` in a tab (or focus it if already open) and make it active.
function openFile(path: string) {
  const exists = state.tabs.some((t) => t.path === path);
  emit({
    tabs: exists ? state.tabs : [...state.tabs, { path, dirty: false }],
    activePath: path,
    pendingOpen: state.pendingOpen,
  });
}

/// Close the tab for `path`; if it was active, fall back to a neighbour.
function closeTab(path: string) {
  const idx = state.tabs.findIndex((t) => t.path === path);
  if (idx === -1) return;
  const tabs = state.tabs.filter((t) => t.path !== path);
  let activePath = state.activePath;
  if (activePath === path) {
    const neighbour = tabs[idx] ?? tabs[idx - 1] ?? null;
    activePath = neighbour ? neighbour.path : null;
  }
  emit({ tabs, activePath, pendingOpen: state.pendingOpen });
}

/// Make `path` the active tab (no-op if it isn't open).
function setActive(path: string) {
  if (state.activePath === path) return;
  if (!state.tabs.some((t) => t.path === path)) return;
  emit({ tabs: state.tabs, activePath: path, pendingOpen: state.pendingOpen });
}

/// Set (or clear) a tab's dirty flag.
function setDirty(path: string, dirty: boolean) {
  let changed = false;
  const tabs = state.tabs.map((t) => {
    if (t.path === path && t.dirty !== dirty) {
      changed = true;
      return { ...t, dirty };
    }
    return t;
  });
  if (!changed) return;
  emit({ tabs, activePath: state.activePath, pendingOpen: state.pendingOpen });
}

/// Ask the editor to open `path` (used by other views). The editor calls
/// `consumePendingOpen()` to read and clear it.
function requestOpen(path: string) {
  emit({ tabs: state.tabs, activePath: state.activePath, pendingOpen: path });
}

/// Read and clear the pending-open path (null when nothing is pending).
function consumePendingOpen(): string | null {
  const pending = state.pendingOpen;
  if (pending !== null) {
    emit({ tabs: state.tabs, activePath: state.activePath, pendingOpen: null });
  }
  return pending;
}

/// Imperative handle — safe to call from anywhere (including non-React code).
export const editorStore = {
  subscribe,
  getSnapshot,
  getState: getSnapshot,
  openFile,
  closeTab,
  setActive,
  setDirty,
  requestOpen,
  consumePendingOpen,
};

/// React hook — subscribes the calling component to editor state.
export function useEditorStore(): EditorState {
  return useSyncExternalStore(subscribe, getSnapshot);
}
