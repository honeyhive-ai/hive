// Static mapping between file extensions, our server ids (matching the backend
// registry), and the Monaco language ids each server serves. The backend is the
// source of truth for *availability* (which binaries exist on PATH); this table
// only routes a file to a candidate server id and tells us which Monaco language
// selectors to register that server's providers under.

/** Server id (matches the backend `LspServerDto.id`) → Monaco language ids. */
export const SERVER_LANGUAGES: Record<string, string[]> = {
  typescript: ["typescript", "javascript"],
  "rust-analyzer": ["rust"],
  pyright: ["python"],
  gopls: ["go"],
};

/** File extension (lowercase, no dot) → server id. */
const EXT_TO_SERVER: Record<string, string> = {
  ts: "typescript",
  tsx: "typescript",
  mts: "typescript",
  cts: "typescript",
  js: "typescript",
  jsx: "typescript",
  mjs: "typescript",
  cjs: "typescript",
  rs: "rust-analyzer",
  py: "pyright",
  pyi: "pyright",
  go: "gopls",
};

/** The LSP `languageId` a server expects for a file (per the spec's language ids). */
const EXT_TO_LSP_LANGUAGE: Record<string, string> = {
  ts: "typescript",
  mts: "typescript",
  cts: "typescript",
  tsx: "typescriptreact",
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  jsx: "javascriptreact",
  rs: "rust",
  py: "python",
  pyi: "python",
  go: "go",
};

function extOf(path: string): string {
  const lower = path.toLowerCase();
  const slash = Math.max(lower.lastIndexOf("/"), lower.lastIndexOf("\\"));
  const dot = lower.lastIndexOf(".");
  if (dot <= slash) return "";
  return lower.slice(dot + 1);
}

/** The candidate server id for a path, or null if no server serves it. */
export function serverIdForPath(path: string): string | null {
  return EXT_TO_SERVER[extOf(path)] ?? null;
}

/** Reverse of SERVER_LANGUAGES: Monaco language id → the server that serves it. */
const LANGUAGE_TO_SERVER: Record<string, string> = (() => {
  const out: Record<string, string> = {};
  for (const [serverId, langs] of Object.entries(SERVER_LANGUAGES)) {
    for (const lang of langs) out[lang] = serverId;
  }
  return out;
})();

/** The server id registered for a Monaco language id, or null if none. */
export function serverIdForLanguage(languageId: string): string | null {
  return LANGUAGE_TO_SERVER[languageId] ?? null;
}

/** The LSP `languageId` for a path (defaults to plaintext). */
export function lspLanguageForPath(path: string): string {
  return EXT_TO_LSP_LANGUAGE[extOf(path)] ?? "plaintext";
}
