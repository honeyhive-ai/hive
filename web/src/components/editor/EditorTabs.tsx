import type { MouseEvent as ReactMouseEvent } from "react";
import { editorStore, useEditorStore } from "@/state/editorStore";
import { IconX, IconFile } from "@/lib/icons";

/// The editor's tab strip: one tab per open file, active highlight, a dirty dot
/// (unsaved edits) and a close affordance. Middle-click closes too — the usual
/// editor idiom. Self-contained: reads/writes `editorStore` directly, so
/// `EditorView` just renders `<EditorTabs/>`.
export function EditorTabs() {
  const { tabs, activePath } = useEditorStore();

  if (tabs.length === 0) return null;

  function baseName(path: string): string {
    const i = path.lastIndexOf("/");
    return i === -1 ? path : path.slice(i + 1);
  }

  function onAuxClick(e: ReactMouseEvent, path: string) {
    // Middle-click (button 1) closes the tab.
    if (e.button === 1) {
      e.preventDefault();
      editorStore.closeTab(path);
    }
  }

  return (
    <div
      role="tablist"
      aria-label="Open files"
      className="flex shrink-0 items-stretch overflow-x-auto border-b"
      style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
    >
      {tabs.map((t) => {
        const active = t.path === activePath;
        return (
          <div
            key={t.path}
            role="tab"
            aria-selected={active}
            title={t.path}
            onMouseDown={(e) => onAuxClick(e, t.path)}
            onClick={() => editorStore.setActive(t.path)}
            className="group flex min-w-0 cursor-pointer select-none items-center gap-1.5 border-r px-3 py-1.5 text-xs transition-colors"
            style={{
              borderColor: "var(--hive-line)",
              background: active ? "var(--hive-canvas)" : "transparent",
              opacity: active ? 1 : 0.65,
              boxShadow: active
                ? "inset 0 -2px 0 0 var(--hive-accent-cool)"
                : undefined,
            }}
          >
            <span className="shrink-0 opacity-60">
              <IconFile size={13} />
            </span>
            <span className="max-w-[14rem] truncate font-mono">{baseName(t.path)}</span>
            {/* Dirty dot swaps to a close X on hover, VS Code-style. */}
            <span className="relative ml-1 flex h-4 w-4 shrink-0 items-center justify-center">
              {t.dirty && (
                <span
                  aria-label="Unsaved changes"
                  className="h-2 w-2 rounded-full group-hover:opacity-0"
                  style={{ background: "var(--hive-ink)" }}
                />
              )}
              <button
                aria-label={`Close ${baseName(t.path)}`}
                onClick={(e) => {
                  e.stopPropagation();
                  editorStore.closeTab(t.path);
                }}
                className={`absolute inset-0 flex items-center justify-center rounded transition-opacity hover:bg-[color:var(--hive-overlay)] ${
                  t.dirty ? "opacity-0 group-hover:opacity-100" : "opacity-50 group-hover:opacity-100"
                }`}
                style={{ color: "var(--hive-ink)" }}
                tabIndex={active ? 0 : -1}
              >
                <IconX size={12} />
              </button>
            </span>
          </div>
        );
      })}
    </div>
  );
}
