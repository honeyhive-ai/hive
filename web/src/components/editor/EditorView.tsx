import { useCallback, useEffect, useRef, useState } from "react";
import Editor, { type OnMount } from "@monaco-editor/react";
import type * as Monaco from "monaco-editor";
// Importing setup here (as DiffView does) keeps Monaco + its worker inside this
// lazily-loaded chunk, off the app's startup path.
import "@/lib/monaco-setup";
import { editorStore, useEditorStore } from "@/state/editorStore";
import { readWorkspaceFile, writeWorkspaceFile, getFileDiffSides, onFsChanged } from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { Button } from "@/components/ui";
import { IconFile, IconAlertTriangle } from "@/lib/icons";
import { EditorTabs } from "@/components/editor/EditorTabs";
import { applyMonacoTheme, watchMonacoTheme, MONACO_THEME } from "@/components/editor/monacoTheme";
import { LspAdapter } from "@/lib/lsp";
import { LspStatus } from "@/components/editor/LspStatus";
import { registerCustomLanguages } from "./customLangs";

type CodeEditor = Monaco.editor.IStandaloneCodeEditor;
type TextModel = Monaco.editor.ITextModel;
type ViewState = Monaco.editor.ICodeEditorViewState;

/// Monaco language id for a file path (drives syntax highlighting). Kept local
/// so the editor doesn't couple to DiffView.
function languageForPath(path: string): string {
  const lower = path.toLowerCase();
  if (lower.endsWith("dockerfile")) return "dockerfile";
  const ext = lower.slice(lower.lastIndexOf(".") + 1);
  const map: Record<string, string> = {
    ts: "typescript", tsx: "typescript", mts: "typescript", cts: "typescript",
    js: "javascript", jsx: "javascript", mjs: "javascript", cjs: "javascript",
    rs: "rust", py: "python", go: "go", rb: "ruby", java: "java", kt: "kotlin",
    swift: "swift", c: "c", h: "c", cc: "cpp", cpp: "cpp", hpp: "cpp",
    cs: "csharp", php: "php", sh: "shell", bash: "shell", zsh: "shell",
    json: "json", jsonc: "json", toml: "ini", ini: "ini", yml: "yaml",
    yaml: "yaml", md: "markdown", mdx: "markdown", html: "html", htm: "html",
    css: "css", scss: "scss", less: "less", sql: "sql", xml: "xml", svg: "xml",
    graphql: "graphql", lua: "lua", vue: "html",
    scala: "scala", sc: "scala", sbt: "scala",
    tf: "hcl", tfvars: "hcl", hcl: "hcl", proto: "proto", rst: "restructuredtext",
    tex: "latex", sty: "latex", cls: "latex", ltx: "latex",
  };
  return map[ext] ?? "plaintext";
}

// --- Git-changed gutter -----------------------------------------------------
// We mark lines that differ from HEAD (the last commit) in the editor gutter,
// VS Code-style: added (green), modified (blue), and a caret where lines were
// removed. The only data source is `getFileDiffSides(path)` — the HEAD
// ("original") side of the working-tree diff — so this is GIT change, not agent
// attribution. Per-line *who* (which agent/turn edited a line) has NO data
// source in the app today, so we deliberately don't fake one.
//
// Large files skip the per-line pass (the LCS below is O(n·m)); the toolbar
// still shows a plain "changed vs HEAD" count for files within the cap.
const GUTTER_LINE_CAP = 2000;

const GUTTER_CSS = `
.hive-gl { width: 3px !important; left: 3px !important; border-radius: 2px; }
.hive-gl-add { background: var(--hive-success); }
.hive-gl-mod { background: var(--hive-accent-cool); }
.hive-gl-del { background: transparent; }
.hive-gl-del::after {
  content: ""; position: absolute; left: -1px; bottom: -2px; width: 0; height: 0;
  border-left: 4px solid transparent; border-right: 4px solid transparent;
  border-top: 5px solid var(--hive-danger);
}
`;

interface LineDiff {
  added: number[];
  modified: number[];
  deleted: number[];
  changed: number;
}

/// Classify each current line vs its HEAD version via an LCS alignment. Added =
/// a new line with no removed counterpart in its change group; modified = a new
/// line paired with a removed one; deleted = a line-number anchor where HEAD
/// lines were removed with no replacement. 1-based line numbers.
function computeLineDiff(head: string[], cur: string[]): LineDiff {
  const n = head.length;
  const m = cur.length;
  const W = m + 1;
  // dp[i*W+j] = LCS length of head[i:] and cur[j:]. Uint16 is enough (LCS ≤ min
  // length ≤ GUTTER_LINE_CAP < 65535).
  const dp = new Uint16Array((n + 1) * W);
  for (let i = n - 1; i >= 0; i--) {
    const rowi = i * W;
    const rown = (i + 1) * W;
    for (let j = m - 1; j >= 0; j--) {
      dp[rowi + j] =
        head[i] === cur[j] ? dp[rown + j + 1] + 1 : Math.max(dp[rown + j], dp[rowi + j + 1]);
    }
  }
  const added: number[] = [];
  const modified: number[] = [];
  const deleted: number[] = [];
  let pendingDel = 0;
  let groupAdds: number[] = [];
  const flush = (boundary: number) => {
    const modCount = Math.min(pendingDel, groupAdds.length);
    for (let k = 0; k < groupAdds.length; k++) {
      (k < modCount ? modified : added).push(groupAdds[k]);
    }
    if (pendingDel > groupAdds.length && m > 0) {
      const anchor = groupAdds.length ? groupAdds[groupAdds.length - 1] : boundary;
      deleted.push(Math.min(Math.max(1, anchor), m));
    }
    pendingDel = 0;
    groupAdds = [];
  };
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (head[i] === cur[j]) {
      flush(j + 1);
      i++;
      j++;
    } else if (dp[(i + 1) * W + j] >= dp[i * W + j + 1]) {
      pendingDel++;
      i++;
    } else {
      groupAdds.push(j + 1);
      j++;
    }
  }
  while (i < n) {
    pendingDel++;
    i++;
  }
  while (j < m) {
    groupAdds.push(j + 1);
    j++;
  }
  flush(m);
  return { added, modified, deleted, changed: added.length + modified.length + deleted.length };
}

/// The editor canvas: a tab strip over a single Monaco `Editor` that swaps its
/// model per active tab. One model is kept per open path (created on first open,
/// disposed on tab close), and each tab's cursor/scroll (view state) is
/// preserved across switches. ⌘S / Ctrl+S writes the active buffer to disk.
/// Mounted by another agent (no props).
export function EditorView() {
  const { tabs, activePath } = useEditorStore();

  const editorRef = useRef<CodeEditor | null>(null);
  const monacoRef = useRef<typeof Monaco | null>(null);
  const models = useRef<Map<string, TextModel>>(new Map());
  const viewStates = useRef<Map<string, ViewState>>(new Map());
  // Path currently attached to the editor (to save its view state on switch).
  const mountedPath = useRef<string | null>(null);
  // Suppress the dirty flag while we set content programmatically (load/reload).
  const suppressDirty = useRef(false);
  const initialModel = useRef<TextModel | null>(null);
  const [ready, setReady] = useState(false);
  // Open files that changed on disk while dirty — we never clobber them; the
  // banner lets the user reload or keep their edits.
  const [conflicts, setConflicts] = useState<Set<string>>(new Set());
  // Git-changed gutter: the HEAD side per path (undefined = not fetched, null =
  // no diff available — binary/untracked/error), the live decorations, a
  // debounce timer for content edits, and the active file's changed-line count.
  const headCache = useRef<Map<string, string[] | null>>(new Map());
  const decoRef = useRef<Monaco.editor.IEditorDecorationsCollection | null>(null);
  const gutterTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [changedLines, setChangedLines] = useState(0);
  // Optional LSP language-intelligence adapter. Null until it's built (async,
  // after mount) and stays null if discovery fails or no server is available —
  // in which case every hook below is a no-op and the editor behaves as before.
  const lspRef = useRef<LspAdapter | null>(null);
  // A render-visible mirror of lspRef so the status indicator can subscribe once
  // the adapter is built (refs don't trigger re-renders). Stays null when LSP is
  // disabled / discovery fails → the indicator renders nothing.
  const [lspAdapter, setLspAdapter] = useState<LspAdapter | null>(null);

  const clearConflict = useCallback((path: string) => {
    setConflicts((prev) => {
      if (!prev.has(path)) return prev;
      const next = new Set(prev);
      next.delete(path);
      return next;
    });
  }, []);

  /// Reload a model's content from disk (only when not clobbering user edits).
  const reloadFromDisk = useCallback(
    async (path: string) => {
      const model = models.current.get(path);
      if (!model) return;
      try {
        const content = await readWorkspaceFile(path);
        if (model.isDisposed()) return;
        if (model.getValue() !== content) {
          suppressDirty.current = true;
          model.setValue(content);
          suppressDirty.current = false;
        }
        editorStore.setDirty(path, false);
        clearConflict(path);
      } catch {
        // A deleted/unreadable file just leaves the buffer as-is.
      }
    },
    [clearConflict],
  );

  /// Create (or fetch the existing) model for `path`, loading its content.
  const ensureModel = useCallback(async (path: string): Promise<TextModel | null> => {
    const monaco = monacoRef.current;
    if (!monaco) return null;
    const existing = models.current.get(path);
    if (existing && !existing.isDisposed()) return existing;
    let content = "";
    try {
      content = await readWorkspaceFile(path);
    } catch (e) {
      toast.error(`Couldn't open ${path}: ${errMsg(e)}`);
      return null;
    }
    const model = monaco.editor.createModel(content, languageForPath(path));
    model.onDidChangeContent(() => {
      if (suppressDirty.current) return;
      editorStore.setDirty(path, true);
    });
    models.current.set(path, model);
    // Hand the fresh model to the LSP adapter (opens the document with its
    // language server). No-op when the adapter is absent or the language has no
    // available server; guarded so a failure never blocks opening the file.
    try {
      lspRef.current?.attach(model, path);
    } catch {
      /* LSP is best-effort — never block opening a file. */
    }
    return model;
  }, []);

  const save = useCallback(async () => {
    const path = mountedPath.current;
    if (!path) return;
    const model = models.current.get(path);
    if (!model) return;
    try {
      await writeWorkspaceFile(path, model.getValue());
      editorStore.setDirty(path, false);
      clearConflict(path); // our version is now on disk
      try {
        lspRef.current?.notifySave(model);
      } catch {
        /* LSP notification is best-effort. */
      }
    } catch (e) {
      toast.error(`Couldn't save ${path}: ${errMsg(e)}`);
    }
  }, [clearConflict]);

  /// Recompute the git-changed gutter for `path`, but only while it's the active
  /// (mounted) file. Fetches the HEAD side once per path (cached), diffs it
  /// against the live buffer, and paints add/mod/del decorations. Cheap no-op
  /// when the diff is unavailable or the file is too large.
  const refreshGutter = useCallback(async (path: string | null) => {
    const editor = editorRef.current;
    const monaco = monacoRef.current;
    if (!editor || !monaco || !path || path !== mountedPath.current) return;
    const model = models.current.get(path);
    if (!model || model.isDisposed()) return;

    let head = headCache.current.get(path);
    if (head === undefined) {
      head = null;
      try {
        const sides = await getFileDiffSides(path);
        if (!sides.isBinary) head = sides.original.split(/\r?\n/);
      } catch {
        head = null; // untracked / no git / read error — nothing to mark.
      }
      headCache.current.set(path, head);
    }
    // The active file (or the editor) may have changed during the await.
    if (path !== mountedPath.current || editorRef.current !== editor || model.isDisposed()) return;

    if (head === null) {
      decoRef.current?.clear();
      setChangedLines(0);
      return;
    }
    const cur = model.getValue().split(/\r?\n/);
    if (head.length > GUTTER_LINE_CAP || cur.length > GUTTER_LINE_CAP) {
      // Too large for the O(n·m) pass — skip the gutter rather than jank typing.
      decoRef.current?.clear();
      setChangedLines(0);
      return;
    }
    const diff = computeLineDiff(head, cur);
    const line = (ln: number) => new monaco.Range(ln, 1, ln, 1);
    const decos: Monaco.editor.IModelDeltaDecoration[] = [
      ...diff.added.map((ln) => ({ range: line(ln), options: { linesDecorationsClassName: "hive-gl hive-gl-add" } })),
      ...diff.modified.map((ln) => ({ range: line(ln), options: { linesDecorationsClassName: "hive-gl hive-gl-mod" } })),
      ...diff.deleted.map((ln) => ({ range: line(ln), options: { linesDecorationsClassName: "hive-gl hive-gl-del" } })),
    ];
    if (!decoRef.current) decoRef.current = editor.createDecorationsCollection();
    decoRef.current.set(decos);
    setChangedLines(diff.changed);
  }, []);

  const handleMount: OnMount = useCallback(
    (editor, monaco) => {
      editorRef.current = editor;
      monacoRef.current = monaco;
      initialModel.current = editor.getModel();
      registerCustomLanguages(monaco); // LaTeX — Monaco ships no built-in grammar
      applyMonacoTheme(monaco);
      // Build the LSP adapter (best-effort, async). If it never resolves or
      // returns null (no servers / discovery failed), the editor just runs
      // without language intelligence. Any models opened before it's ready are
      // attached retroactively here.
      void LspAdapter.create(monaco)
        .then((adapter) => {
          if (!adapter) return;
          if (!editorRef.current) {
            // Editor unmounted while we were building — throw the adapter away.
            adapter.dispose();
            return;
          }
          lspRef.current = adapter;
          setLspAdapter(adapter);
          for (const [path, model] of models.current) {
            if (!model.isDisposed()) {
              try {
                adapter.attach(model, path);
              } catch {
                /* best-effort */
              }
            }
          }
        })
        .catch(() => {
          /* language intelligence is optional — degrade silently. */
        });
      // ⌘S / Ctrl+S saves the active buffer.
      editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, () => {
        void save();
      });
      // Re-mark the git-changed gutter as the buffer changes (debounced). We
      // diff against the LIVE buffer, so unsaved edits update the gutter here —
      // which is also why no separate save-time refresh is needed (a save
      // doesn't change HEAD-vs-buffer). External changes are handled via
      // onFsChanged below. (Disposed with the editor on unmount.)
      editor.onDidChangeModelContent(() => {
        if (gutterTimer.current) clearTimeout(gutterTimer.current);
        gutterTimer.current = setTimeout(() => void refreshGutter(mountedPath.current), 350);
      });
      setReady(true);
    },
    [save, refreshGutter],
  );

  // Re-theme Monaco when the shell palette/scheme flips.
  useEffect(() => watchMonacoTheme(), []);

  // Swap the editor's model to the active tab, preserving per-tab view state.
  useEffect(() => {
    const editor = editorRef.current;
    if (!ready || !editor || !activePath) return;
    // Save the outgoing tab's cursor/scroll before switching.
    const prev = mountedPath.current;
    if (prev && prev !== activePath) {
      const vs = editor.saveViewState();
      if (vs) viewStates.current.set(prev, vs);
    }
    let cancelled = false;
    void ensureModel(activePath).then((model) => {
      if (cancelled || !model || editorRef.current !== editor) return;
      editor.setModel(model);
      const vs = viewStates.current.get(activePath);
      if (vs) editor.restoreViewState(vs);
      mountedPath.current = activePath;
      // Drop the throwaway model @monaco-editor/react created on mount.
      const init = initialModel.current;
      if (init && init !== model && !init.isDisposed()) {
        init.dispose();
        initialModel.current = null;
      }
      editor.focus();
      // Repaint the gutter for the newly-active file (clear the outgoing file's
      // marks first so they don't linger during the async recompute).
      decoRef.current?.clear();
      setChangedLines(0);
      void refreshGutter(activePath);
    });
    return () => {
      cancelled = true;
    };
  }, [activePath, ready, ensureModel, refreshGutter]);

  // Dispose models (+ their view state) for tabs that were closed.
  useEffect(() => {
    const open = new Set(tabs.map((t) => t.path));
    for (const [path, model] of models.current) {
      if (open.has(path)) continue;
      try {
        lspRef.current?.detach(model); // close the LSP document first.
      } catch {
        /* best-effort */
      }
      if (!model.isDisposed()) model.dispose();
      models.current.delete(path);
      viewStates.current.delete(path);
      clearConflict(path);
      if (mountedPath.current === path) mountedPath.current = null;
    }
  }, [tabs, clearConflict]);

  // Dispose every model when the editor unmounts (leaving the code view).
  useEffect(
    () => () => {
      // Tear down the LSP adapter first (closes documents, stops the language
      // servers, drops listeners) before the models it references go away.
      try {
        lspRef.current?.dispose();
      } catch {
        /* best-effort */
      }
      lspRef.current = null;
      for (const model of models.current.values()) {
        if (!model.isDisposed()) model.dispose();
      }
      models.current.clear();
      viewStates.current.clear();
    },
    [],
  );

  // React to on-disk changes: silently reload open, clean files; flag dirty
  // ones so the user chooses (never silently clobber their edits).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void onFsChanged(({ paths }) => {
      const changed = new Set(paths);
      // The file — and possibly HEAD — moved on disk, so the cached HEAD side is
      // stale; drop it so the gutter refetches.
      for (const p of paths) headCache.current.delete(p);
      for (const tab of editorStore.getState().tabs) {
        if (!changed.has(tab.path)) continue;
        if (!models.current.has(tab.path)) continue;
        if (tab.dirty) {
          setConflicts((prev) => {
            if (prev.has(tab.path)) return prev;
            const next = new Set(prev);
            next.add(tab.path);
            return next;
          });
        } else {
          void reloadFromDisk(tab.path);
        }
      }
      // Repaint the active file's gutter (a clean file's reload triggers the
      // content listener too, but this also covers a dirty active file whose
      // HEAD moved).
      const active = editorStore.getState().activePath;
      if (active && changed.has(active)) void refreshGutter(active);
    }).then((un) => {
      if (disposed) un();
      else unlisten = un;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [reloadFromDisk, refreshGutter]);

  // Stop the pending gutter recompute when the editor unmounts.
  useEffect(() => () => {
    if (gutterTimer.current) clearTimeout(gutterTimer.current);
  }, []);

  const empty = tabs.length === 0;
  const conflictActive = activePath !== null && conflicts.has(activePath);

  /// "Open in Diff": reveal the Diff canvas focused on the active file. Routed
  /// through a window event because the editor is nested under CodeView (App
  /// can't hand it a callback); App turns the event into a mode switch.
  const openInDiff = useCallback(() => {
    const path = mountedPath.current;
    if (!path) return;
    window.dispatchEvent(new CustomEvent("hive:editor-open-in-diff", { detail: { path } }));
  }, []);

  /// "Send selection to chat": drop the current selection (or the caret's line
  /// when nothing is selected) into the chat composer as a fenced block prefixed
  /// with the repo-relative path — e.g. ```ts // path/to/file.
  const sendSelectionToChat = useCallback(() => {
    const editor = editorRef.current;
    const path = mountedPath.current;
    const model = editor?.getModel();
    if (!editor || !path || !model) return;
    const sel = editor.getSelection();
    let snippet = sel && !sel.isEmpty() ? model.getValueInRange(sel) : "";
    if (!snippet) {
      const ln = sel?.startLineNumber ?? editor.getPosition()?.lineNumber ?? 1;
      snippet = model.getLineContent(ln);
    }
    snippet = snippet.replace(/\s+$/, "");
    if (!snippet) {
      toast.error("Nothing to send — the selection is empty.");
      return;
    }
    const ext = path.slice(path.lastIndexOf(".") + 1);
    const lang = ext && ext !== path ? `${ext} ` : "";
    const block = "```" + lang + "// " + path + "\n" + snippet + "\n```";
    window.dispatchEvent(new CustomEvent("hive:editor-send-to-chat", { detail: { text: block } }));
    toast.success("Selection sent to chat.");
  }, []);

  return (
    <div className="flex h-full min-h-0 flex-col" style={{ background: "var(--hive-canvas)" }}>
      {/* Gutter decoration styles (theme tokens only, works in every palette). */}
      <style>{GUTTER_CSS}</style>
      <EditorTabs />

      {/* Wedge toolbar: the changed-vs-HEAD count plus the editor→chat/diff
          bridges. Only shown with a file open. */}
      {!empty && activePath && (
        <div
          className="flex items-center gap-2 border-b px-3 py-1"
          style={{ borderColor: "var(--hive-line)" }}
        >
          {changedLines > 0 && (
            <span
              className="shrink-0 rounded-full px-2 py-0.5 text-[0.7rem] font-medium"
              style={{ color: "var(--hive-accent-cool)", background: "var(--hive-mist)" }}
              title="Lines changed vs the last commit (HEAD)"
            >
              {changedLines} changed vs HEAD
            </span>
          )}
          <span className="flex-1" />
          <Button size="sm" onClick={sendSelectionToChat} title="Send the selected code to the chat composer">
            Send selection to chat
          </Button>
          <Button size="sm" onClick={openInDiff} title="Show this file's changes in the Diff view">
            Open in Diff
          </Button>
        </div>
      )}

      {conflictActive && activePath && (
        <div
          className="flex items-center gap-3 border-b px-3 py-1.5 text-xs"
          style={{
            borderColor: "var(--hive-line)",
            background: "var(--hive-mist)",
            color: "var(--hive-ink)",
          }}
        >
          <span style={{ color: "var(--hive-warn)" }}>
            <IconAlertTriangle size={14} />
          </span>
          <span className="flex-1">
            <span className="font-mono">{activePath}</span> changed on disk. You have unsaved edits.
          </span>
          <Button size="sm" onClick={() => void reloadFromDisk(activePath)}>
            Reload from disk
          </Button>
          <Button size="sm" onClick={() => clearConflict(activePath)}>
            Keep mine
          </Button>
        </div>
      )}

      <div className="relative min-h-0 flex-1">
        {/* The Editor stays mounted (its instance + models persist); the empty
            state is an overlay so closing the last tab doesn't churn Monaco. */}
        <div className="absolute inset-0" style={{ visibility: empty ? "hidden" : "visible" }}>
          <Editor
            height="100%"
            theme={MONACO_THEME}
            defaultValue=""
            keepCurrentModel
            onMount={handleMount}
            options={{
              automaticLayout: true,
              fontSize: 13,
              minimap: { enabled: true },
              wordWrap: "off",
              scrollBeyondLastLine: false,
              smoothScrolling: true,
              renderWhitespace: "selection",
              tabSize: 2,
              // Language-intelligence surface: right-click menu, hover cards,
              // and Go-to/Peek. These only show data once a provider exists —
              // Monaco's built-in workers (TS/JS/JSON/CSS/HTML) or the LSP
              // adapter for other languages — but the affordances are enabled here.
              contextmenu: true,
              hover: { enabled: true, sticky: true },
              quickSuggestions: true,
              suggestOnTriggerCharacters: true,
              parameterHints: { enabled: true },
              links: true,
              occurrencesHighlight: "singleFile",
              definitionLinkOpensInPeek: false,
              gotoLocation: {
                multipleDefinitions: "gotoAndPeek",
                multipleTypeDefinitions: "gotoAndPeek",
                multipleDeclarations: "gotoAndPeek",
                multipleImplementations: "gotoAndPeek",
                multipleReferences: "gotoAndPeek",
              },
            }}
          />
        </div>
        {/* Language-server status for the active file (bottom-right). Renders
            nothing when LSP is off, the language has no server, or the server is
            a Monaco-built-in-covered one with nothing to report. */}
        {!empty && activePath && (
          <LspStatus adapter={lspAdapter} languageId={languageForPath(activePath)} />
        )}
        {empty && (
          <div
            className="absolute inset-0 flex flex-col items-center justify-center gap-3 text-sm"
            style={{ color: "var(--hive-ink)", opacity: 0.5, background: "var(--hive-canvas)" }}
          >
            <IconFile size={28} />
            <p>Open a file to start editing.</p>
          </div>
        )}
      </div>
    </div>
  );
}
