import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import { DiffEditor } from "@monaco-editor/react";
// Importing setup here (not in main.tsx) keeps Monaco + its worker inside the
// lazily-loaded diff chunk, off the startup path.
import "@/lib/monaco-setup";
import {
  detectEditors,
  getFileDiffSides,
  getWorkspaceDiffs,
  openPathInEditor,
  type GitFileDiffDto,
} from "@/lib/ipc";
import { useColorScheme } from "@/lib/theme";
import { toast, errMsg } from "@/components/Toast";
import { Button, IconButton } from "@/components/ui";
import { IconRegenerate, IconChevronDown } from "@/lib/icons";

/// Monaco language id for a file path — drives diff syntax highlighting.
export function languageForPath(path: string): string {
  const ext = path.slice(path.lastIndexOf(".") + 1).toLowerCase();
  const map: Record<string, string> = {
    ts: "typescript",
    tsx: "typescript",
    mts: "typescript",
    js: "javascript",
    jsx: "javascript",
    mjs: "javascript",
    cjs: "javascript",
    rs: "rust",
    py: "python",
    go: "go",
    rb: "ruby",
    java: "java",
    kt: "kotlin",
    swift: "swift",
    c: "c",
    h: "c",
    cc: "cpp",
    cpp: "cpp",
    hpp: "cpp",
    cs: "csharp",
    php: "php",
    sh: "shell",
    bash: "shell",
    zsh: "shell",
    json: "json",
    jsonc: "json",
    toml: "ini",
    ini: "ini",
    yml: "yaml",
    yaml: "yaml",
    md: "markdown",
    mdx: "markdown",
    html: "html",
    htm: "html",
    css: "css",
    scss: "scss",
    less: "less",
    sql: "sql",
    xml: "xml",
    svg: "xml",
    dockerfile: "dockerfile",
    graphql: "graphql",
    lua: "lua",
    vue: "html",
  };
  if (path.toLowerCase().endsWith("dockerfile")) return "dockerfile";
  return map[ext] ?? "plaintext";
}

const KIND_BADGE: Record<string, { label: string; color: string }> = {
  added: { label: "added", color: "var(--hive-success)" },
  untracked: { label: "new", color: "var(--hive-success)" },
  deleted: { label: "deleted", color: "var(--hive-danger)" },
  modified: { label: "modified", color: "var(--hive-accent-cool)" },
  renamed: { label: "renamed", color: "var(--hive-accent-cool)" },
  conflicted: { label: "conflict", color: "var(--hive-warn)" },
};

const LIST_WIDTH_KEY = "hive.diff.listWidth";
const LAST_EDITOR_KEY = "hive.diff.lastEditor";
const MIN_LIST_WIDTH = 180;
const MAX_LIST_WIDTH = 480;

function loadListWidth(): number {
  const raw = Number(window.localStorage.getItem(LIST_WIDTH_KEY));
  return Number.isFinite(raw) && raw >= MIN_LIST_WIDTH && raw <= MAX_LIST_WIDTH ? raw : 288;
}

/// The Diff canvas: uncommitted changes on the left (resizable); the selected
/// file as a syntax-highlighted side-by-side/inline Monaco diff on the right,
/// with an "Open in <installed editor>" escape hatch.
export function DiffView() {
  const diffs = useQuery({ queryKey: ["diffs"], queryFn: getWorkspaceDiffs });
  const editors = useQuery({ queryKey: ["editors"], queryFn: detectEditors, staleTime: Infinity });
  const [selected, setSelected] = useState<string | null>(null);
  const [sideBySide, setSideBySide] = useState(true);
  const [listWidth, setListWidth] = useState(loadListWidth);
  const [editorMenuOpen, setEditorMenuOpen] = useState(false);
  const scheme = useColorScheme();
  // Two Monaco quirks make a naive toggle appear dead:
  // 1. renderSideBySide only applies via updateOptions on the live editor
  //    (the options prop is inert after mount).
  // 2. useInlineViewWhenSpaceIsLimited defaults to true with a ~900px
  //    breakpoint — our diff pane is almost always narrower (sidebar +
  //    changes list + right rail), so Monaco silently forces inline anyway.
  //    Disable it so the user's toggle is authoritative.
  const diffEditorRef = useRef<{ updateOptions: (o: object) => void } | null>(null);
  useEffect(() => {
    diffEditorRef.current?.updateOptions({
      renderSideBySide: sideBySide,
      useInlineViewWhenSpaceIsLimited: false,
    });
  }, [sideBySide]);

  const files: GitFileDiffDto[] = diffs.data ?? [];
  const active = files.find((f) => f.path === selected) ?? files[0];

  const sides = useQuery({
    queryKey: ["file-diff", active?.path],
    queryFn: () => getFileDiffSides(active!.path),
    enabled: Boolean(active),
  });

  // Preferred editor = last used, else the first detected.
  const editorList = editors.data ?? [];
  const preferred =
    editorList.find((e) => e.id === window.localStorage.getItem(LAST_EDITOR_KEY)) ?? editorList[0];

  async function openIn(editorId: string) {
    if (!active) return;
    setEditorMenuOpen(false);
    window.localStorage.setItem(LAST_EDITOR_KEY, editorId);
    try {
      await openPathInEditor(editorId, active.path);
    } catch (e) {
      toast.error(`Couldn't open editor: ${errMsg(e)}`);
    }
  }

  function startListResize(event: ReactPointerEvent<HTMLDivElement>) {
    event.preventDefault();
    const startX = event.clientX;
    const initial = listWidth;
    const move = (e: PointerEvent) => {
      const next = Math.min(MAX_LIST_WIDTH, Math.max(MIN_LIST_WIDTH, initial + e.clientX - startX));
      setListWidth(next);
      window.localStorage.setItem(LIST_WIDTH_KEY, String(next));
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

  return (
    <div className="flex h-full">
      <div
        className="shrink-0 overflow-y-auto border-r p-2 text-sm"
        style={{ width: listWidth, borderColor: "var(--hive-line)" }}
      >
        <div className="flex items-center justify-between px-2 py-1">
          <span className="font-medium opacity-70">Changes</span>
          <IconButton label="Refresh changes" size={24} onClick={() => diffs.refetch()}>
            <IconRegenerate size={13} />
          </IconButton>
        </div>
        {files.length === 0 && <p className="px-2 py-4 opacity-50">No uncommitted changes.</p>}
        {files.map((f) => (
          <button
            key={f.path}
            onClick={() => setSelected(f.path)}
            className={`flex w-full items-center justify-between gap-2 rounded-lg px-2 py-1 text-left hover:opacity-100 ${
              active?.path === f.path ? "opacity-100" : "opacity-75"
            }`}
            style={{ background: active?.path === f.path ? "var(--hive-mist)" : "transparent" }}
            title={f.path}
          >
            <span className="truncate font-mono text-xs">{f.path}</span>
            <span className="shrink-0 text-xs">
              <span style={{ color: "var(--hive-success)" }}>+{f.addedLines}</span>{" "}
              <span style={{ color: "var(--hive-danger)" }}>−{f.removedLines}</span>
            </span>
          </button>
        ))}
      </div>

      {/* Drag to resize the changes list. */}
      <div
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize changes list"
        className="relative w-1.5 shrink-0 cursor-col-resize transition-colors hover:bg-[color:var(--hive-overlay)]"
        style={{ touchAction: "none" }}
        onPointerDown={startListResize}
      >
        <span
          className="pointer-events-none absolute inset-y-0 left-1/2 w-px -translate-x-1/2"
          style={{ background: "var(--hive-line)" }}
        />
      </div>

      <div className="flex min-w-0 flex-1 flex-col">
        {active ? (
          <>
            <div
              className="flex items-center gap-2 border-b px-3 py-1.5 text-xs"
              style={{ borderColor: "var(--hive-line)" }}
            >
              <span className="truncate font-mono opacity-80">{active.path}</span>
              {KIND_BADGE[active.kind] && (
                <span
                  className="shrink-0 rounded-full px-2 py-0.5 text-[0.7rem] font-medium"
                  style={{ color: KIND_BADGE[active.kind].color, background: "var(--hive-mist)" }}
                >
                  {KIND_BADGE[active.kind].label}
                </span>
              )}
              <span className="flex-1" />

              {/* Segmented view toggle — the familiar GitHub/VS Code pattern. */}
              <div
                className="flex shrink-0 items-center gap-0.5 rounded-xl border p-0.5"
                style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
                role="tablist"
                aria-label="Diff layout"
              >
                {(
                  [
                    { id: true, label: "Split" },
                    { id: false, label: "Inline" },
                  ] as const
                ).map((t) => (
                  <button
                    key={t.label}
                    role="tab"
                    aria-selected={sideBySide === t.id}
                    onClick={() => setSideBySide(t.id)}
                    className="rounded-[9px] px-2.5 py-0.5 font-medium transition-colors"
                    style={{
                      background: sideBySide === t.id ? "var(--hive-panel)" : "transparent",
                      opacity: sideBySide === t.id ? 1 : 0.55,
                      boxShadow: sideBySide === t.id ? "0 1px 2px rgba(0,0,0,0.08)" : undefined,
                    }}
                  >
                    {t.label}
                  </button>
                ))}
              </div>

              {preferred && (
                <div className="relative flex shrink-0">
                  <Button
                    size="sm"
                    className={editorList.length > 1 ? "rounded-r-none" : ""}
                    onClick={() => void openIn(preferred.id)}
                  >
                    Open in {preferred.label}
                  </Button>
                  {editorList.length > 1 && (
                    <>
                      <Button
                        size="sm"
                        className="rounded-l-none border-l-0 !px-1.5"
                        aria-label="Choose editor"
                        aria-expanded={editorMenuOpen}
                        onClick={() => setEditorMenuOpen((o) => !o)}
                      >
                        <IconChevronDown size={13} />
                      </Button>
                      {editorMenuOpen && (
                        <>
                          <div
                            className="fixed inset-0 z-40"
                            onClick={() => setEditorMenuOpen(false)}
                          />
                          <div
                            className="absolute right-0 top-full z-50 mt-1 min-w-40 overflow-hidden rounded-xl border p-1 shadow-xl"
                            style={{
                              borderColor: "var(--hive-line)",
                              background: "var(--hive-panel)",
                            }}
                          >
                            {editorList.map((ed) => (
                              <button
                                key={ed.id}
                                onClick={() => void openIn(ed.id)}
                                className="block w-full rounded-lg px-2.5 py-1.5 text-left text-xs transition-colors hover:bg-[color:var(--hive-mist)]"
                              >
                                Open in {ed.label}
                              </button>
                            ))}
                          </div>
                        </>
                      )}
                    </>
                  )}
                </div>
              )}
            </div>

            <div className="min-h-0 flex-1">
              {sides.data?.isBinary ? (
                <div className="flex h-full items-center justify-center text-sm opacity-50">
                  Binary file — no text diff.
                </div>
              ) : sides.data ? (
                <DiffEditor
                  key={active.path}
                  height="100%"
                  language={languageForPath(active.path)}
                  original={sides.data.original}
                  modified={sides.data.modified}
                  onMount={(editor) => {
                    diffEditorRef.current = editor;
                    editor.updateOptions({
                      renderSideBySide: sideBySide,
                      useInlineViewWhenSpaceIsLimited: false,
                    });
                  }}
                  options={{
                    readOnly: true,
                    renderSideBySide: sideBySide,
                    useInlineViewWhenSpaceIsLimited: false,
                    minimap: { enabled: false },
                    fontSize: 12,
                    wordWrap: "on",
                    automaticLayout: true,
                    hideUnchangedRegions: { enabled: true },
                  }}
                  theme={scheme === "dark" ? "vs-dark" : "light"}
                />
              ) : (
                <div className="flex h-full items-center justify-center text-sm opacity-50">
                  {sides.isError ? `Couldn't load diff: ${errMsg(sides.error)}` : "Loading diff…"}
                </div>
              )}
            </div>
          </>
        ) : (
          <div className="flex h-full items-center justify-center opacity-50">
            Select a file to view its diff.
          </div>
        )}
      </div>
    </div>
  );
}
