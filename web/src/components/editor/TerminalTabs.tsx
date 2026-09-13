import { terminalStore, useTerminalStore } from "@/state/terminalStore";
import { IconPlus, IconX } from "@/lib/icons";

// A thin tab strip over the terminal store — one chip per open terminal, the
// active one highlighted, each with a close ✕, and an optional trailing +.
// Presentational: it reads the shared store and delegates the actual PTY
// lifecycle to callbacks owned by <TerminalPanel/> (which knows how to spawn
// and dispose xterm sessions). Kept deliberately simple; v1 usually has a
// single terminal, but this scales to a handful without extra wiring.

interface Props {
  /// Switch the active terminal. Defaults to the store's own `setActive`.
  onSelect?: (id: string) => void;
  /// Close a terminal (kill its PTY + drop it). Owned by TerminalPanel.
  onClose?: (id: string) => void;
  /// Spawn a new terminal. Owned by TerminalPanel.
  onNew?: () => void;
}

export function TerminalTabs({ onSelect, onClose, onNew }: Props) {
  const { terminals, activeId } = useTerminalStore();
  const select = onSelect ?? terminalStore.setActive;

  // With a single terminal there's nothing to switch between — hide the strip
  // (the panel header still exposes +new / close), keeping the chrome quiet.
  if (terminals.length <= 1 && !onNew) return null;

  return (
    <div className="flex min-w-0 items-center gap-0.5 overflow-x-auto">
      {terminals.map((t) => {
        const active = t.id === activeId;
        return (
          <div
            key={t.id}
            className="group flex shrink-0 items-center gap-1 rounded-lg px-2 py-0.5 text-[12px] transition-colors"
            style={{
              background: active ? "var(--hive-mist)" : "transparent",
              color: active ? "var(--hive-ink)" : "var(--hive-ink)",
              opacity: active ? 1 : 0.6,
              border: `1px solid ${active ? "var(--hive-line)" : "transparent"}`,
            }}
          >
            <button
              type="button"
              className="max-w-[10rem] truncate"
              onClick={() => select(t.id)}
              title={t.title}
            >
              {t.title}
            </button>
            {onClose && (
              <button
                type="button"
                aria-label={`Close ${t.title}`}
                title="Close terminal"
                className="inline-flex h-4 w-4 items-center justify-center rounded opacity-0 transition-opacity hover:bg-[color:var(--hive-overlay)] group-hover:opacity-70"
                onClick={(e) => {
                  e.stopPropagation();
                  onClose(t.id);
                }}
              >
                <IconX size={11} />
              </button>
            )}
          </div>
        );
      })}
      {onNew && (
        <button
          type="button"
          aria-label="New terminal"
          title="New terminal"
          className="inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-lg opacity-60 transition-all hover:bg-[color:var(--hive-overlay)] hover:opacity-100"
          style={{ color: "var(--hive-ink)" }}
          onClick={onNew}
        >
          <IconPlus size={14} />
        </button>
      )}
    </div>
  );
}
