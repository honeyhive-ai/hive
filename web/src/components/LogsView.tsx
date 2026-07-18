import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { onChatStream } from "@/lib/ipc";
import { useColorScheme } from "@/lib/theme";

// xterm needs concrete colors (no CSS custom properties). One readable pair
// per scheme; the terminal is recreated on theme flips (logs are ephemeral).
const XTERM_THEMES = {
  dark: { background: "#1c2129", foreground: "#e8eaee" },
  light: { background: "#f6f5f1", foreground: "#26282b" },
} as const;

/// The Logs canvas: an xterm.js terminal that tails runtime activity. Phase 4
/// streams chat lifecycle (deltas/completions/errors); agent/tool/command
/// output is added as those subsystems land.
export function LogsView() {
  const hostRef = useRef<HTMLDivElement>(null);
  const scheme = useColorScheme();

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const term = new Terminal({
      convertEol: true,
      fontSize: 12,
      theme: XTERM_THEMES[scheme],
      cursorBlink: false,
      disableStdin: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    fit.fit();
    term.writeln("\x1b[2mhive runtime log — streaming chat activity\x1b[0m");

    const ro = new ResizeObserver(() => fit.fit());
    ro.observe(host);

    const unlisten = onChatStream((e) => {
      const ts = new Date().toLocaleTimeString();
      if (e.phase === "delta") {
        term.write(e.text);
      } else if (e.phase === "completed") {
        term.writeln(`\r\n\x1b[32m[${ts}] ✓ completed (${e.messageId.slice(0, 8)})\x1b[0m`);
      } else {
        term.writeln(`\r\n\x1b[31m[${ts}] ✗ error: ${e.text}\x1b[0m`);
      }
    });

    return () => {
      unlisten.then((fn) => fn());
      ro.disconnect();
      term.dispose();
    };
  }, [scheme]);

  return (
    <div
      ref={hostRef}
      className="h-full w-full"
      style={{ background: XTERM_THEMES[scheme].background }}
    />
  );
}
