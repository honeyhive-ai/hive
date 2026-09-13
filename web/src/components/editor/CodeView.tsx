import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { FileTree } from "@/components/editor/FileTree";
import { EditorView } from "@/components/editor/EditorView";
import { QuickOpen } from "@/components/editor/QuickOpen";
import { TerminalPanel } from "@/components/editor/TerminalPanel";
import { editorStore } from "@/state/editorStore";
import { IconPanelLeft } from "@/lib/icons";

// The "code" canvas: a VS-Code-like 3-pane layout composed from the editor
// building blocks — a resizable, collapsible file tree on the left, the Monaco
// editor (with its own tab strip) filling the centre, and a collapsible,
// resizable integrated terminal docked at the bottom. A slim activity strip on
// the far left keeps the tree + terminal toggles reachable even when hidden.
//
// This is the only piece that knows the layout; every child is self-contained
// (no props, or a single collapse callback) and drives the shared stores. The
// resize idiom (pointer-drag handle + persisted size) mirrors DiffView's
// resizable changes list. Everything is themed via `--hive-*` tokens.

// External requests the shell can make of the active code view (from the
// command palette). Consumed once via `onConsumePending`, so a fresh request
// after switching back into the view fires again.
export type CodeViewIntent = "quick-open" | "new-terminal";

const TREE_WIDTH_KEY = "hive.code.treeWidth";
const TREE_VISIBLE_KEY = "hive.code.treeVisible";
const TERM_HEIGHT_KEY = "hive.code.termHeight";
const MIN_TREE_WIDTH = 160;
const MAX_TREE_WIDTH = 480;
const DEFAULT_TREE_WIDTH = 240;
const MIN_TERM_HEIGHT = 120;
const DEFAULT_TERM_HEIGHT = 260;
// Keep at least this much room for the editor above the terminal.
const MIN_EDITOR_HEIGHT = 120;

function loadNumber(key: string, fallback: number, min: number, max: number): number {
  const raw = Number(window.localStorage.getItem(key));
  return Number.isFinite(raw) && raw >= min && raw <= max ? raw : fallback;
}

export function CodeView({
  pending,
  onConsumePending,
}: {
  /// A one-shot request from the shell (command palette). Consumed on arrival.
  pending?: CodeViewIntent | null;
  onConsumePending?: () => void;
}) {
  const [treeVisible, setTreeVisible] = useState(
    () => window.localStorage.getItem(TREE_VISIBLE_KEY) !== "0",
  );
  const [treeWidth, setTreeWidth] = useState(() =>
    loadNumber(TREE_WIDTH_KEY, DEFAULT_TREE_WIDTH, MIN_TREE_WIDTH, MAX_TREE_WIDTH),
  );
  // Terminal starts hidden; height is remembered for when it's shown again.
  const [termVisible, setTermVisible] = useState(false);
  const [termHeight, setTermHeight] = useState(() =>
    loadNumber(TERM_HEIGHT_KEY, DEFAULT_TERM_HEIGHT, MIN_TERM_HEIGHT, 4000),
  );
  const [quickOpen, setQuickOpen] = useState(false);

  // The centre column: measured so the terminal can never crowd the editor out.
  const centerRef = useRef<HTMLDivElement | null>(null);

  const toggleTree = useCallback(() => {
    setTreeVisible((v) => {
      const next = !v;
      window.localStorage.setItem(TREE_VISIBLE_KEY, next ? "1" : "0");
      return next;
    });
  }, []);

  // Honor a file another view asked us to open — on mount (the view becomes
  // active only by mounting here) and whenever a new pending path lands.
  useEffect(() => {
    const consume = () => {
      const path = editorStore.consumePendingOpen();
      if (path) editorStore.openFile(path);
    };
    consume();
    const unsubscribe = editorStore.subscribe(() => {
      if (editorStore.getState().pendingOpen) consume();
    });
    return () => {
      unsubscribe();
    };
  }, []);

  // A shell request (⌘P from the palette, "New terminal") arrives as `pending`.
  useEffect(() => {
    if (!pending) return;
    if (pending === "quick-open") setQuickOpen(true);
    else if (pending === "new-terminal") setTermVisible(true);
    onConsumePending?.();
  }, [pending, onConsumePending]);

  // ⌘P / Ctrl+P opens quick-open. Scoped to when the code view is mounted (it
  // only mounts while active), and yields when a modal input already has focus.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "p") {
        e.preventDefault();
        setQuickOpen(true);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  function startTreeResize(event: ReactPointerEvent<HTMLDivElement>) {
    event.preventDefault();
    const startX = event.clientX;
    const initial = treeWidth;
    const move = (e: PointerEvent) => {
      const next = Math.min(MAX_TREE_WIDTH, Math.max(MIN_TREE_WIDTH, initial + e.clientX - startX));
      setTreeWidth(next);
      window.localStorage.setItem(TREE_WIDTH_KEY, String(next));
    };
    const stop = () => {
      document.body.style.removeProperty("cursor");
      document.body.style.removeProperty("user-select");
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
    };
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop);
  }

  function startTermResize(event: ReactPointerEvent<HTMLDivElement>) {
    event.preventDefault();
    const startY = event.clientY;
    const initial = termHeight;
    // Cap so the editor keeps at least MIN_EDITOR_HEIGHT above the terminal.
    const containerH = centerRef.current?.clientHeight ?? window.innerHeight;
    const maxH = Math.max(MIN_TERM_HEIGHT, containerH - MIN_EDITOR_HEIGHT);
    const move = (e: PointerEvent) => {
      // Dragging up (smaller clientY) grows the terminal.
      const next = Math.min(maxH, Math.max(MIN_TERM_HEIGHT, initial + startY - e.clientY));
      setTermHeight(next);
      window.localStorage.setItem(TERM_HEIGHT_KEY, String(next));
    };
    const stop = () => {
      document.body.style.removeProperty("cursor");
      document.body.style.removeProperty("user-select");
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
    };
    document.body.style.cursor = "row-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop);
  }

  return (
    <div className="flex h-full min-h-0" style={{ background: "var(--hive-canvas)" }}>
      {/* Activity strip: always-present toggles for the tree + terminal. */}
      <div
        className="flex w-9 shrink-0 flex-col items-center gap-1 border-r py-2"
        style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
      >
        <button
          type="button"
          aria-label={treeVisible ? "Hide file tree" : "Show file tree"}
          aria-pressed={treeVisible}
          title="Toggle file tree"
          onClick={toggleTree}
          className="inline-flex h-7 w-7 items-center justify-center rounded-lg transition-all hover:bg-[color:var(--hive-overlay)]"
          style={{ color: "var(--hive-ink)", opacity: treeVisible ? 1 : 0.5 }}
        >
          <IconPanelLeft size={16} />
        </button>
        <button
          type="button"
          aria-label={termVisible ? "Hide terminal" : "Show terminal"}
          aria-pressed={termVisible}
          title="Toggle terminal"
          onClick={() => setTermVisible((v) => !v)}
          className="inline-flex h-7 w-7 items-center justify-center rounded-lg font-mono text-[11px] leading-none transition-all hover:bg-[color:var(--hive-overlay)]"
          style={{ color: "var(--hive-ink)", opacity: termVisible ? 1 : 0.5 }}
        >
          {">_"}
        </button>
      </div>

      {/* Left: file tree (resizable, collapsible). */}
      {treeVisible && (
        <>
          <div
            className="shrink-0 overflow-hidden"
            style={{ width: treeWidth, borderRight: "1px solid var(--hive-line)" }}
          >
            <FileTree />
          </div>
          <div
            role="separator"
            aria-orientation="vertical"
            aria-label="Resize file tree"
            className="relative w-1.5 shrink-0 cursor-col-resize transition-colors hover:bg-[color:var(--hive-overlay)]"
            style={{ touchAction: "none" }}
            onPointerDown={startTreeResize}
          >
            <span
              className="pointer-events-none absolute inset-y-0 left-1/2 w-px -translate-x-1/2"
              style={{ background: "var(--hive-line)" }}
            />
          </div>
        </>
      )}

      {/* Centre: editor above, terminal docked below. */}
      <div ref={centerRef} className="flex min-w-0 flex-1 flex-col">
        <div className="min-h-0 flex-1">
          <EditorView />
        </div>

        {termVisible && (
          <>
            <div
              role="separator"
              aria-orientation="horizontal"
              aria-label="Resize terminal"
              className="relative h-1.5 shrink-0 cursor-row-resize transition-colors hover:bg-[color:var(--hive-overlay)]"
              style={{ touchAction: "none" }}
              onPointerDown={startTermResize}
            >
              <span
                className="pointer-events-none absolute inset-x-0 top-1/2 h-px -translate-y-1/2"
                style={{ background: "var(--hive-line)" }}
              />
            </div>
            <div className="shrink-0" style={{ height: termHeight }}>
              <TerminalPanel onCollapse={() => setTermVisible(false)} />
            </div>
          </>
        )}
      </div>

      <QuickOpen open={quickOpen} onClose={() => setQuickOpen(false)} />
    </div>
  );
}
