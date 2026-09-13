// A small, subtle status pill for the active file's language server. It shows
// what the hand-rolled LSP adapter is doing — starting, indexing (rust-analyzer
// can spend a while here, during which the editor otherwise looks dead), ready,
// errored, or "not found" for a language whose server isn't installed — so the
// editor never looks broken while a server warms up.
//
// Graceful degradation is the contract: a null adapter (LSP disabled / discovery
// failed), a language with no mapped server, or a built-in-covered language with
// no server all render nothing. It never throws into the editor.

import { useEffect, useReducer } from "react";
import type { LspAdapter } from "@/lib/lsp";

// Languages Monaco covers with its own bundled workers — we don't nag about a
// missing external server for these (typescript/javascript still map to the
// `typescript` server id, but Monaco's TS worker already provides intelligence).
const MONACO_BUILTIN_LANGUAGES = new Set([
  "typescript",
  "javascript",
  "json",
  "css",
  "scss",
  "less",
  "html",
]);

const PILL_CSS = `
.lsp-pill {
  display: inline-flex; align-items: center; gap: 6px;
  padding: 2px 8px; border-radius: 999px;
  font-size: 0.68rem; line-height: 1.4; font-weight: 500;
  border: 1px solid var(--hive-line);
  background: var(--hive-panel); color: var(--hive-ink-soft);
  box-shadow: 0 1px 3px var(--hive-overlay);
  user-select: none; white-space: nowrap; max-width: 260px;
}
.lsp-pill .lsp-label { overflow: hidden; text-overflow: ellipsis; }
.lsp-dot { width: 7px; height: 7px; border-radius: 50%; flex: none; }
.lsp-spin {
  width: 9px; height: 9px; border-radius: 50%; flex: none;
  border: 1.5px solid var(--hive-ink-faint);
  border-top-color: var(--hive-accent-cool);
  animation: lsp-spin 0.7s linear infinite;
}
@keyframes lsp-spin { to { transform: rotate(360deg); } }
`;

interface Props {
  adapter: LspAdapter | null;
  /** The active file's Monaco language id (e.g. "rust", "typescript"). */
  languageId: string | null;
}

/// Bottom-right status indicator for the active file's language server.
export function LspStatus({ adapter, languageId }: Props) {
  // Re-render whenever the adapter reports a status change.
  const [, bump] = useReducer((n: number) => n + 1, 0);
  useEffect(() => {
    if (!adapter) return;
    return adapter.subscribe(bump);
  }, [adapter]);

  if (!adapter || !languageId) return null;

  let status: ReturnType<LspAdapter["getLanguageStatus"]> = null;
  try {
    status = adapter.getLanguageStatus(languageId);
  } catch {
    return null; // never throw into the editor.
  }
  if (!status) return null;

  const label = status.serverId;

  // "unavailable" only nags for languages Monaco doesn't already cover.
  if (status.status === "unavailable") {
    if (MONACO_BUILTIN_LANGUAGES.has(languageId)) return null;
    return (
      <Pill>
        <span className="lsp-dot" style={{ background: "var(--hive-warn)" }} />
        <span className="lsp-label">{label} not found</span>
      </Pill>
    );
  }

  if (status.status === "starting") {
    return (
      <Pill title={label}>
        <span className="lsp-spin" />
        <span className="lsp-label">starting…</span>
      </Pill>
    );
  }

  if (status.status === "indexing") {
    const pct = typeof status.percent === "number" ? `${Math.round(status.percent)}%` : null;
    const detail = pct ?? status.message ?? "";
    return (
      <Pill title={status.message ?? label}>
        <span className="lsp-spin" />
        <span className="lsp-label">indexing{detail ? `… ${detail}` : "…"}</span>
      </Pill>
    );
  }

  if (status.status === "error") {
    return (
      <Pill title={status.message ?? `${label} stopped`}>
        <span className="lsp-dot" style={{ background: "var(--hive-danger)" }} />
        <span className="lsp-label">error</span>
      </Pill>
    );
  }

  // ready
  return (
    <Pill title={`${label} ready`}>
      <span className="lsp-dot" style={{ background: "var(--hive-success)" }} />
      <span className="lsp-label">{label}</span>
    </Pill>
  );
}

function Pill({ children, title }: { children: React.ReactNode; title?: string }) {
  return (
    <div className="pointer-events-none absolute bottom-2 right-3 z-10">
      <style>{PILL_CSS}</style>
      <span className="lsp-pill" title={title}>
        {children}
      </span>
    </div>
  );
}
