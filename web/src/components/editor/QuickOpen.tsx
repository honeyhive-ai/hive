import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { listWorkspaceFiles } from "@/lib/ipc";
import { editorStore } from "@/state/editorStore";
import { Modal } from "@/components/ui";

// ⌘P quick-open: a centered modal with a fuzzy filter over the flat workspace
// file list. Enter opens the highlighted file in the editor; Esc closes. The
// file list is fetched once per open and cached by react-query while the modal
// stays mounted (enabled only while `open`).

const MAX_RESULTS = 200;

interface Ranked {
  path: string;
  score: number;
}

/// Subsequence fuzzy match. Returns a score (lower = better) or null when
/// `query` isn't a subsequence of `text`. Ranks by how early and how tightly
/// the query characters land, with a strong bonus for matches in the basename.
function fuzzyScore(query: string, path: string): number | null {
  const q = query.toLowerCase();
  const t = path.toLowerCase();
  const baseStart = t.lastIndexOf("/") + 1; // 0 when no slash

  let ti = 0;
  let first = -1;
  let last = -1;
  let gaps = 0;
  let inBase = 0;
  for (let qi = 0; qi < q.length; qi++) {
    const c = q[qi];
    let found = -1;
    for (; ti < t.length; ti++) {
      if (t[ti] === c) {
        found = ti;
        break;
      }
    }
    if (found === -1) return null;
    if (first < 0) first = found;
    else gaps += found - last - 1;
    if (found >= baseStart) inBase++;
    last = found;
    ti = found + 1;
  }

  // Reward matches that start in / cluster in the basename and land early.
  const baseBonus = inBase === q.length ? -40 : -inBase * 4;
  const startBonus = first === baseStart ? -10 : 0;
  return first * 2 + gaps * 3 + t.length * 0.1 + baseBonus + startBonus;
}

function basename(path: string): string {
  const i = path.lastIndexOf("/");
  return i === -1 ? path : path.slice(i + 1);
}

function dirname(path: string): string {
  const i = path.lastIndexOf("/");
  return i === -1 ? "" : path.slice(0, i);
}

export function QuickOpen({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);

  const files = useQuery({
    queryKey: ["workspace-files-quickopen"],
    queryFn: listWorkspaceFiles,
    enabled: open,
    staleTime: Infinity,
  });

  // Reset per open; Modal autofocuses the input (its first control).
  useEffect(() => {
    if (open) {
      setQuery("");
      setActive(0);
    }
  }, [open]);

  const results = useMemo<Ranked[]>(() => {
    const all = files.data ?? [];
    const q = query.trim();
    if (!q) {
      return all.slice(0, MAX_RESULTS).map((path) => ({ path, score: 0 }));
    }
    const scored: Ranked[] = [];
    for (const path of all) {
      const score = fuzzyScore(q, path);
      if (score !== null) scored.push({ path, score });
    }
    scored.sort((a, b) => a.score - b.score || a.path.length - b.path.length);
    return scored.slice(0, MAX_RESULTS);
  }, [files.data, query]);

  // Keep the active index in range as the result list changes.
  useEffect(() => {
    setActive((a) => Math.min(a, Math.max(0, results.length - 1)));
  }, [results.length]);

  // Keep the active row scrolled into view under arrow navigation.
  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`[data-idx="${active}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [active]);

  if (!open) return null;

  const choose = (path?: string) => {
    if (!path) return;
    editorStore.openFile(path);
    onClose();
  };

  return (
    <Modal
      onClose={onClose}
      overlayClassName="z-[900] flex items-start justify-center pt-[12vh]"
      overlayStyle={{ background: "color-mix(in srgb, var(--hive-ink) 45%, transparent)" }}
      panelClassName="w-full max-w-lg overflow-hidden rounded-2xl border shadow-2xl"
      panelStyle={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)", color: "var(--hive-ink)" }}
    >
      <input
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        placeholder="Go to file…"
        className="w-full border-b bg-transparent px-4 py-3 text-base outline-none"
        style={{ borderColor: "var(--hive-line)" }}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") {
            e.preventDefault();
            setActive((a) => Math.min(a + 1, results.length - 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setActive((a) => Math.max(a - 1, 0));
          } else if (e.key === "Enter") {
            e.preventDefault();
            choose(results[active]?.path);
          }
        }}
      />
      <div ref={listRef} className="max-h-[50vh] overflow-y-auto py-1">
        {files.isLoading && <div className="px-4 py-6 text-center text-sm opacity-50">Loading…</div>}
        {!files.isLoading && results.length === 0 && (
          <div className="px-4 py-6 text-center text-sm opacity-50">No matching files.</div>
        )}
        {results.map((r, i) => {
          const dir = dirname(r.path);
          return (
            <button
              key={r.path}
              data-idx={i}
              onMouseEnter={() => setActive(i)}
              onClick={() => choose(r.path)}
              className="flex w-full items-baseline gap-2 px-4 py-2 text-left text-sm"
              style={{ background: i === active ? "var(--hive-mist)" : "transparent" }}
            >
              <span className="truncate font-medium">{basename(r.path)}</span>
              {dir && <span className="truncate text-xs opacity-45">{dir}</span>}
            </button>
          );
        })}
      </div>
      <div
        className="flex items-center gap-3 border-t px-4 py-2 text-[11px] opacity-45"
        style={{ borderColor: "var(--hive-line)" }}
      >
        <span>↑↓ navigate</span>
        <span>↵ open</span>
        <span>esc close</span>
      </div>
    </Modal>
  );
}
