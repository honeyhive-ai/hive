import { useCallback, useEffect, useRef, useState } from "react";
import {
  listWorkspaceTree,
  createWorkspaceEntry,
  renameWorkspaceEntry,
  deleteWorkspaceEntry,
  onFsChanged,
  type FsEntryDto,
} from "@/lib/ipc";
import { editorStore } from "@/state/editorStore";
import { confirmDialog, promptDialog } from "@/components/Dialog";

// Lazy, VS-Code-style workspace file tree. Each directory level is fetched on
// demand via `listWorkspaceTree(relDir)` (root = ""); the backend returns
// entries dirs-first + name-sorted. Clicking a file opens it in the editor via
// `editorStore.openFile`. A context menu (right-click, or the row kebab) wires
// New file / New folder / Rename / Delete to the mutation commands and refreshes
// the affected directory. A debounced `onFsChanged` watcher re-fetches whichever
// loaded directories contain a changed path. Everything is themed with
// `--hive-*` tokens.

const ROOT = ""; // key for the workspace root level

function joinPath(dir: string, name: string): string {
  return dir ? `${dir}/${name}` : name;
}

function parentDir(path: string): string {
  const i = path.lastIndexOf("/");
  return i === -1 ? ROOT : path.slice(0, i);
}

function baseName(path: string): string {
  const i = path.lastIndexOf("/");
  return i === -1 ? path : path.slice(i + 1);
}

// ── icons ────────────────────────────────────────────────────────────────────

function Chevron({ open }: { open: boolean }) {
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
      style={{ transform: open ? "rotate(90deg)" : "none", transition: "transform 120ms", opacity: 0.6 }}
    >
      <path d="M9 6l6 6-6 6" />
    </svg>
  );
}

function FolderIcon({ open }: { open: boolean }) {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden style={{ opacity: 0.85 }}>
      {open ? (
        <path d="M3 8a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2H5l-2 8z" />
      ) : (
        <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
      )}
    </svg>
  );
}

function FileIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden style={{ opacity: 0.6 }}>
      <path d="M6 3h8l4 4v14a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z" />
      <path d="M14 3v4h4" />
    </svg>
  );
}

function KebabIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden>
      <circle cx="12" cy="5" r="1.6" />
      <circle cx="12" cy="12" r="1.6" />
      <circle cx="12" cy="19" r="1.6" />
    </svg>
  );
}

// ── context menu ───────────────────────────────────────────────────────────

interface MenuState {
  x: number;
  y: number;
  /// The directory new entries are created in (a dir node, or a file's parent).
  baseDir: string;
  /// The entry the menu was opened on (null for the root/background menu).
  entry: FsEntryDto | null;
}

// ── tree state ─────────────────────────────────────────────────────────────

interface TreeState {
  // entries loaded per directory (keyed by repo-relative dir, "" = root)
  children: Record<string, FsEntryDto[]>;
  expanded: Set<string>;
  loading: Set<string>;
}

export function FileTree() {
  const [tree, setTree] = useState<TreeState>({
    children: {},
    expanded: new Set(),
    loading: new Set(),
  });
  const [menu, setMenu] = useState<MenuState | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Latest tree snapshot for use inside the fs-changed listener callback.
  const treeRef = useRef(tree);
  treeRef.current = tree;

  const loadDir = useCallback(async (dir: string) => {
    setTree((t) => ({ ...t, loading: new Set(t.loading).add(dir) }));
    try {
      const entries = await listWorkspaceTree(dir || null);
      setTree((t) => {
        const loading = new Set(t.loading);
        loading.delete(dir);
        return { ...t, children: { ...t.children, [dir]: entries }, loading };
      });
    } catch (e) {
      setError(String(e));
      setTree((t) => {
        const loading = new Set(t.loading);
        loading.delete(dir);
        return { ...t, loading };
      });
    }
  }, []);

  // Load the root once on mount.
  useEffect(() => {
    void loadDir(ROOT);
  }, [loadDir]);

  const toggleDir = useCallback(
    (dir: string) => {
      setTree((t) => {
        const expanded = new Set(t.expanded);
        if (expanded.has(dir)) {
          expanded.delete(dir);
        } else {
          expanded.add(dir);
          if (!t.children[dir] && !t.loading.has(dir)) void loadDir(dir);
        }
        return { ...t, expanded };
      });
    },
    [loadDir],
  );

  // Refresh loaded directories that contain a changed path (debounced watcher).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    onFsChanged(({ paths }) => {
      const dirs = new Set<string>();
      for (const p of paths) dirs.add(parentDir(p));
      const loaded = treeRef.current.children;
      for (const d of dirs) {
        if (d in loaded) void loadDir(d);
      }
    })
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => {});
    return () => unlisten?.();
  }, [loadDir]);

  // ── mutations ──────────────────────────────────────────────────────────────

  const refreshDir = useCallback(
    (dir: string) => {
      if (dir in treeRef.current.children) void loadDir(dir);
    },
    [loadDir],
  );

  const ensureExpanded = useCallback(
    (dir: string) => {
      setTree((t) => {
        if (t.expanded.has(dir)) return t;
        const expanded = new Set(t.expanded).add(dir);
        return { ...t, expanded };
      });
      if (!(dir in treeRef.current.children)) void loadDir(dir);
    },
    [loadDir],
  );

  const doNewEntry = useCallback(
    async (baseDir: string, isDir: boolean) => {
      const name = await promptDialog(isDir ? "New folder name" : "New file name", {
        title: isDir ? "New folder" : "New file",
        placeholder: isDir ? "components" : "notes.md",
      });
      if (!name) return;
      const path = joinPath(baseDir, name.trim());
      try {
        await createWorkspaceEntry(path, isDir);
        if (baseDir) ensureExpanded(baseDir);
        refreshDir(baseDir);
        if (!isDir) editorStore.openFile(path);
      } catch (e) {
        setError(String(e));
      }
    },
    [ensureExpanded, refreshDir],
  );

  const doRename = useCallback(
    async (entry: FsEntryDto) => {
      const next = await promptDialog("Rename to", {
        title: `Rename ${baseName(entry.path)}`,
        defaultValue: baseName(entry.path),
      });
      if (!next || next.trim() === baseName(entry.path)) return;
      const dir = parentDir(entry.path);
      const to = joinPath(dir, next.trim());
      try {
        await renameWorkspaceEntry(entry.path, to);
        refreshDir(dir);
      } catch (e) {
        setError(String(e));
      }
    },
    [refreshDir],
  );

  const doDelete = useCallback(
    async (entry: FsEntryDto) => {
      const ok = await confirmDialog(
        `Delete ${baseName(entry.path)}${entry.isDir ? " and everything in it" : ""}? This can't be undone.`,
        { title: "Delete", confirmLabel: "Delete", danger: true },
      );
      if (!ok) return;
      try {
        await deleteWorkspaceEntry(entry.path);
        refreshDir(parentDir(entry.path));
      } catch (e) {
        setError(String(e));
      }
    },
    [refreshDir],
  );

  const openMenu = useCallback((e: React.MouseEvent, entry: FsEntryDto | null) => {
    e.preventDefault();
    e.stopPropagation();
    const baseDir = entry ? (entry.isDir ? entry.path : parentDir(entry.path)) : ROOT;
    setMenu({ x: e.clientX, y: e.clientY, baseDir, entry });
  }, []);

  // Close the menu on any outside click / Escape.
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setMenu(null);
    };
    window.addEventListener("click", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  return (
    <div
      className="flex h-full flex-col text-sm"
      style={{ color: "var(--hive-ink)" }}
      onContextMenu={(e) => openMenu(e, null)}
    >
      <div
        className="flex items-center justify-between gap-2 border-b px-3 py-2"
        style={{ borderColor: "var(--hive-line)" }}
      >
        <span className="text-[11px] font-semibold uppercase tracking-wider opacity-60">Explorer</span>
        <div className="flex items-center gap-1">
          <button
            aria-label="New file"
            title="New file"
            onClick={() => void doNewEntry(ROOT, false)}
            className="inline-flex h-6 w-6 items-center justify-center rounded-md opacity-70 hover:opacity-100"
            style={{ background: "transparent" }}
            onMouseEnter={(e) => (e.currentTarget.style.background = "var(--hive-hover)")}
            onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
          >
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="M6 3h8l4 4v14a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z" />
              <path d="M12 10v6M9 13h6" />
            </svg>
          </button>
          <button
            aria-label="New folder"
            title="New folder"
            onClick={() => void doNewEntry(ROOT, true)}
            className="inline-flex h-6 w-6 items-center justify-center rounded-md opacity-70 hover:opacity-100"
            style={{ background: "transparent" }}
            onMouseEnter={(e) => (e.currentTarget.style.background = "var(--hive-hover)")}
            onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
          >
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
              <path d="M12 10v6M9 13h6" />
            </svg>
          </button>
        </div>
      </div>

      {error && (
        <div className="px-3 py-2 text-xs" style={{ color: "var(--hive-danger)" }}>
          {error}
        </div>
      )}

      <div className="min-h-0 flex-1 overflow-auto py-1">
        <TreeLevel
          dir={ROOT}
          depth={0}
          tree={tree}
          onToggle={toggleDir}
          onOpenFile={(p) => editorStore.openFile(p)}
          onMenu={openMenu}
        />
      </div>

      {menu && (
        <div
          className="fixed z-[950] min-w-[160px] overflow-hidden rounded-lg border py-1 text-sm shadow-2xl"
          style={{
            top: menu.y,
            left: menu.x,
            borderColor: "var(--hive-line)",
            background: "var(--hive-panel)",
            color: "var(--hive-ink)",
          }}
          onClick={(e) => e.stopPropagation()}
        >
          <MenuItem
            label="New file"
            onClick={() => {
              setMenu(null);
              void doNewEntry(menu.baseDir, false);
            }}
          />
          <MenuItem
            label="New folder"
            onClick={() => {
              setMenu(null);
              void doNewEntry(menu.baseDir, true);
            }}
          />
          {menu.entry && (
            <>
              <div className="my-1 border-t" style={{ borderColor: "var(--hive-line)" }} />
              <MenuItem
                label="Rename"
                onClick={() => {
                  const entry = menu.entry!;
                  setMenu(null);
                  void doRename(entry);
                }}
              />
              <MenuItem
                label="Delete"
                danger
                onClick={() => {
                  const entry = menu.entry!;
                  setMenu(null);
                  void doDelete(entry);
                }}
              />
            </>
          )}
        </div>
      )}
    </div>
  );
}

function MenuItem({ label, onClick, danger }: { label: string; onClick: () => void; danger?: boolean }) {
  return (
    <button
      onClick={onClick}
      className="block w-full px-3 py-1.5 text-left"
      style={{ color: danger ? "var(--hive-danger)" : "var(--hive-ink)", background: "transparent" }}
      onMouseEnter={(e) => (e.currentTarget.style.background = "var(--hive-hover)")}
      onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
    >
      {label}
    </button>
  );
}

/// One directory level, rendered recursively for expanded subdirectories.
function TreeLevel({
  dir,
  depth,
  tree,
  onToggle,
  onOpenFile,
  onMenu,
}: {
  dir: string;
  depth: number;
  tree: TreeState;
  onToggle: (dir: string) => void;
  onOpenFile: (path: string) => void;
  onMenu: (e: React.MouseEvent, entry: FsEntryDto) => void;
}) {
  const entries = tree.children[dir];
  const isLoading = tree.loading.has(dir);

  if (!entries) {
    return isLoading ? (
      <Row depth={depth} muted>
        Loading…
      </Row>
    ) : null;
  }
  if (entries.length === 0 && depth > 0) {
    return (
      <Row depth={depth} muted>
        (empty)
      </Row>
    );
  }

  return (
    <>
      {entries.map((entry) => (
        <TreeNode
          key={entry.path}
          entry={entry}
          depth={depth}
          tree={tree}
          onToggle={onToggle}
          onOpenFile={onOpenFile}
          onMenu={onMenu}
        />
      ))}
    </>
  );
}

function TreeNode({
  entry,
  depth,
  tree,
  onToggle,
  onOpenFile,
  onMenu,
}: {
  entry: FsEntryDto;
  depth: number;
  tree: TreeState;
  onToggle: (dir: string) => void;
  onOpenFile: (path: string) => void;
  onMenu: (e: React.MouseEvent, entry: FsEntryDto) => void;
}) {
  const expanded = entry.isDir && tree.expanded.has(entry.path);

  return (
    <>
      <RowButton
        depth={depth}
        onClick={() => (entry.isDir ? onToggle(entry.path) : onOpenFile(entry.path))}
        onContextMenu={(e) => onMenu(e, entry)}
        onKebab={(e) => onMenu(e, entry)}
      >
        <span className="flex w-3.5 shrink-0 items-center justify-center">
          {entry.isDir ? <Chevron open={!!expanded} /> : null}
        </span>
        <span className="flex shrink-0 items-center">
          {entry.isDir ? <FolderIcon open={!!expanded} /> : <FileIcon />}
        </span>
        <span className="truncate">{entry.name}</span>
      </RowButton>
      {expanded && (
        <TreeLevel
          dir={entry.path}
          depth={depth + 1}
          tree={tree}
          onToggle={onToggle}
          onOpenFile={onOpenFile}
          onMenu={onMenu}
        />
      )}
    </>
  );
}

const INDENT = 12; // px per depth level

function RowButton({
  depth,
  onClick,
  onContextMenu,
  onKebab,
  children,
}: {
  depth: number;
  onClick: () => void;
  onContextMenu: (e: React.MouseEvent) => void;
  onKebab: (e: React.MouseEvent) => void;
  children: React.ReactNode;
}) {
  return (
    <div
      className="group flex items-center"
      style={{ background: "transparent" }}
      onMouseEnter={(e) => (e.currentTarget.style.background = "var(--hive-hover)")}
      onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
    >
      <button
        onClick={onClick}
        onContextMenu={onContextMenu}
        className="flex min-w-0 flex-1 items-center gap-1.5 py-1 pr-1 text-left"
        style={{ paddingLeft: 8 + depth * INDENT }}
      >
        {children}
      </button>
      <button
        aria-label="More actions"
        title="More actions"
        onClick={onKebab}
        className="mr-1 hidden h-6 w-6 shrink-0 items-center justify-center rounded-md opacity-60 hover:opacity-100 group-hover:flex"
      >
        <KebabIcon />
      </button>
    </div>
  );
}

function Row({ depth, muted, children }: { depth: number; muted?: boolean; children: React.ReactNode }) {
  return (
    <div
      className="py-1 text-xs"
      style={{ paddingLeft: 8 + depth * INDENT, opacity: muted ? 0.45 : 1 }}
    >
      {children}
    </div>
  );
}
