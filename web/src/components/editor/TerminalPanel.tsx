import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Terminal, type ITheme } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import {
  terminalOpen,
  terminalWrite,
  terminalResize,
  terminalClose,
  onTerminalOutput,
  onTerminalExit,
} from "@/lib/ipc";
import { terminalStore, useTerminalStore } from "@/state/terminalStore";
import { currentScheme } from "@/lib/theme";
import { TerminalTabs } from "@/components/editor/TerminalTabs";
import { IconX } from "@/lib/icons";

// The integrated terminal panel: one real PTY per tab (spawned via the
// `terminal_*` IPC), rendered with xterm.js. Unlike LogsView (read-only), this
// is interactive — keystrokes are piped to the PTY master and its output is
// streamed back. Terminals live in the shared `terminalStore`; this component
// owns the xterm instances + PTY lifecycle. Colours are derived from Hive's
// `--hive-*` tokens and re-applied on the `hive:theme` window event.

// ── xterm theming from Hive tokens ───────────────────────────────────────────
// xterm wants concrete colours (no CSS custom properties), so we read the
// computed `--hive-*` tokens off :root and convert them to hex — the same idiom
// as monacoTheme.ts. The 16 ANSI colours can't be derived from four brand
// tokens, so we keep a legible fixed set per scheme (matching common terminals)
// and only drive background/foreground/cursor/selection from the palette.
function readToken(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function toHex(color: string): string | undefined {
  if (!color) return undefined;
  if (color.startsWith("#")) return color;
  const m = color.match(/rgba?\(([^)]+)\)/i);
  if (!m) return undefined;
  const parts = m[1].split(/[,\s/]+/).filter(Boolean);
  const [r, g, b] = parts.slice(0, 3).map((n) => Math.round(Number(n)));
  if ([r, g, b].some((n) => Number.isNaN(n))) return undefined;
  const a = parts.length > 3 ? Number(parts[3]) : 1;
  const hh = (n: number) => Math.max(0, Math.min(255, n)).toString(16).padStart(2, "0");
  let out = `#${hh(r)}${hh(g)}${hh(b)}`;
  if (Number.isFinite(a) && a < 1) out += hh(Math.round(a * 255));
  return out;
}

const ANSI = {
  dark: {
    black: "#3b4048",
    red: "#e06c75",
    green: "#98c379",
    yellow: "#e5c07b",
    blue: "#61afef",
    magenta: "#c678dd",
    cyan: "#56b6c2",
    white: "#dcdfe4",
    brightBlack: "#5c6370",
    brightRed: "#ef7f88",
    brightGreen: "#a9d68e",
    brightYellow: "#f0cf8f",
    brightBlue: "#79c0ff",
    brightMagenta: "#d79be8",
    brightCyan: "#6fc6d1",
    brightWhite: "#f4f6f8",
  },
  light: {
    black: "#26282b",
    red: "#c6373a",
    green: "#228c50",
    yellow: "#aa7418",
    blue: "#2f6fc4",
    magenta: "#a032b0",
    cyan: "#1288a0",
    white: "#d7d9dc",
    brightBlack: "#5a5d61",
    brightRed: "#d94f52",
    brightGreen: "#2ea35f",
    brightYellow: "#c08a26",
    brightBlue: "#3f83d8",
    brightMagenta: "#b64ac2",
    brightCyan: "#1aa0bb",
    brightWhite: "#f4f6f8",
  },
} as const;

function buildXtermTheme(scheme: "light" | "dark"): ITheme {
  const ink = toHex(readToken("--hive-ink"));
  const canvas = toHex(readToken("--hive-canvas"));
  const accent =
    toHex(readToken("--hive-accent-cool")) ?? toHex(readToken("--hive-accent-warm"));
  const fallback = scheme === "dark" ? { bg: "#1c2129", fg: "#e8eaee" } : { bg: "#f6f5f1", fg: "#26282b" };
  return {
    background: canvas ?? fallback.bg,
    foreground: ink ?? fallback.fg,
    cursor: accent ?? ink ?? fallback.fg,
    cursorAccent: canvas ?? fallback.bg,
    // ~28% accent tint for the selection so it reads over any palette.
    selectionBackground: accent ? accent.slice(0, 7) + "47" : undefined,
    ...ANSI[scheme],
  };
}

// ── One interactive PTY-backed terminal ──────────────────────────────────────
interface SessionProps {
  id: string;
  active: boolean;
}

/// Wires an xterm instance to an already-open PTY (`id`). Does NOT open or close
/// the PTY itself — spawning is owned by <TerminalPanel/> and PTY teardown by
/// its tab-close / unmount paths — so a hidden (inactive) session keeps its
/// scrollback intact while another tab is shown.
function TerminalSession({ id, active }: SessionProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const [exit, setExit] = useState<{ code: number | null } | null>(null);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const term = new Terminal({
      fontSize: 12,
      fontFamily:
        'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace',
      cursorBlink: true,
      theme: buildXtermTheme(currentScheme()),
      allowProposedApi: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    termRef.current = term;
    fitRef.current = fit;

    const syncSize = () => {
      try {
        fit.fit();
      } catch {
        /* host has no size yet (collapsed / hidden) — ignore */
      }
      if (term.cols > 0 && term.rows > 0) {
        void terminalResize(id, term.cols, term.rows).catch(() => {});
      }
    };
    syncSize();

    // Keystrokes → PTY master.
    const dataSub = term.onData((d) => {
      void terminalWrite(id, d).catch(() => {});
    });

    // PTY output → this terminal (filtered to our id — one event channel serves
    // every open terminal).
    const outUnlisten = onTerminalOutput((e) => {
      if (e.id === id) term.write(e.data);
    });
    const exitUnlisten = onTerminalExit((e) => {
      if (e.id !== id) return;
      setExit({ code: e.code });
      const label = e.code === null ? "terminated" : `exited (code ${e.code})`;
      term.write(`\r\n\x1b[2m[process ${label}]\x1b[0m\r\n`);
    });

    // Debounced fit on container resize.
    let resizeTimer: ReturnType<typeof setTimeout> | undefined;
    const ro = new ResizeObserver(() => {
      clearTimeout(resizeTimer);
      resizeTimer = setTimeout(syncSize, 60);
    });
    ro.observe(host);

    // Re-theme in place on palette/scheme flips (no need to recreate).
    const onTheme = () => {
      term.options.theme = buildXtermTheme(currentScheme());
    };
    window.addEventListener("hive:theme", onTheme);

    return () => {
      clearTimeout(resizeTimer);
      window.removeEventListener("hive:theme", onTheme);
      ro.disconnect();
      dataSub.dispose();
      void outUnlisten.then((fn) => fn());
      void exitUnlisten.then((fn) => fn());
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, [id]);

  // Becoming active: the host was display:none, so re-fit + focus now that it
  // has real dimensions.
  useLayoutEffect(() => {
    if (!active) return;
    const term = termRef.current;
    const fit = fitRef.current;
    if (!term || !fit) return;
    try {
      fit.fit();
    } catch {
      /* not laid out yet */
    }
    if (term.cols > 0 && term.rows > 0) {
      void terminalResize(id, term.cols, term.rows).catch(() => {});
    }
    if (!exit) term.focus();
  }, [active, id, exit]);

  return (
    <div
      className="absolute inset-0"
      hidden={!active}
      style={{ background: "var(--hive-canvas)" }}
    >
      <div ref={hostRef} className="h-full w-full p-1.5" />
    </div>
  );
}

// ── The panel ────────────────────────────────────────────────────────────────
interface Props {
  /// Optional parent-owned collapse/hide affordance (CodeView controls height).
  onCollapse?: () => void;
}

/// The integrated terminal panel. Spawns a PTY on first mount, exposes +new /
/// per-tab close via <TerminalTabs/>, and closes every PTY when it unmounts.
export function TerminalPanel({ onCollapse }: Props) {
  const { terminals, activeId } = useTerminalStore();
  const bootRef = useRef(false);
  const seqRef = useRef(0);

  const spawn = useCallback(async () => {
    try {
      // cwd=null → the backend spawns the shell in the workspace root.
      const id = await terminalOpen(null, 80, 24);
      seqRef.current += 1;
      terminalStore.add(id, seqRef.current > 1 ? `Terminal ${seqRef.current}` : "Terminal");
    } catch {
      /* spawn failed (no workspace root / PTY error) — leave the panel empty */
    }
  }, []);

  const closeTerminal = useCallback((id: string) => {
    void terminalClose(id).catch(() => {});
    terminalStore.remove(id);
  }, []);

  // Ensure one terminal exists on mount; close every PTY + clear the store on
  // unmount. `bootRef` (persisted across React 18 strict-mode remounts) keeps
  // the auto-spawn from firing twice.
  useEffect(() => {
    if (!bootRef.current && terminalStore.getState().terminals.length === 0) {
      bootRef.current = true;
      void spawn();
    }
    return () => {
      for (const t of terminalStore.getState().terminals) {
        void terminalClose(t.id).catch(() => {});
        terminalStore.remove(t.id);
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="flex h-full min-h-0 w-full flex-col" style={{ background: "var(--hive-canvas)" }}>
      {/* Header: label · tab strip (+new / close per tab) · collapse */}
      <div
        className="flex h-8 shrink-0 items-center gap-2 px-2"
        style={{
          borderBottom: "1px solid var(--hive-line)",
          background: "var(--hive-panel)",
          color: "var(--hive-ink)",
        }}
      >
        <span className="shrink-0 text-[11px] font-medium uppercase tracking-wide" style={{ opacity: 0.6 }}>
          Terminal
        </span>
        <div className="min-w-0 flex-1">
          <TerminalTabs
            onSelect={terminalStore.setActive}
            onClose={closeTerminal}
            onNew={() => void spawn()}
          />
        </div>
        {onCollapse && (
          <button
            type="button"
            aria-label="Hide terminal"
            title="Hide terminal"
            className="inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-lg opacity-60 transition-all hover:bg-[color:var(--hive-overlay)] hover:opacity-100"
            style={{ color: "var(--hive-ink)" }}
            onClick={onCollapse}
          >
            <IconX size={14} />
          </button>
        )}
      </div>

      {/* Body: one xterm host per terminal; only the active one is shown. */}
      <div className="relative min-h-0 flex-1">
        {terminals.length === 0 ? (
          <div
            className="flex h-full w-full items-center justify-center text-[12px]"
            style={{ color: "var(--hive-ink)", opacity: 0.5 }}
          >
            No terminal. Use ＋ to open one.
          </div>
        ) : (
          terminals.map((t) => (
            <TerminalSession key={t.id} id={t.id} active={t.id === activeId} />
          ))
        )}
      </div>
    </div>
  );
}
