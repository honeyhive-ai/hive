// The LSP adapter: the single object EditorView drives. It discovers the
// available servers once, starts a session lazily on the first file of a
// language (reusing it across files), registers Monaco providers scoped by
// language, and translates provider callbacks ⇄ LSP requests. Document sync
// (didOpen/didChange/didSave/didClose) is wired per attached model.
//
// Graceful degradation is the whole contract: `create` returns null if the
// discovery call fails; a path with no *available* server attaches nothing; a
// session that fails to start or crashes is dropped and its providers simply
// return no results. Nothing here throws into Monaco — every provider callback
// and event handler is wrapped so the editor behaves exactly as it does without
// the adapter.

import type * as Monaco from "monaco-editor";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  getAppSettings,
  lspServers,
  lspStart,
  lspStop,
  lspSend,
  onLspMessage,
  onLspExit,
} from "@/lib/ipc";
import { LspSession } from "./session";
import {
  SERVER_LANGUAGES,
  serverIdForPath,
  serverIdForLanguage,
  lspLanguageForPath,
} from "./registry";
import { joinFileUri, normalizeUri, pathToFileUri } from "./uri";
import {
  completionInsertText,
  completionKind,
  docToMarkdown,
  fromLspRange,
} from "./convert";
import type {
  CompletionItem,
  CompletionList,
  Hover,
  LocationLink,
  LspLocation,
  MarkupContent,
  ServerCapabilities,
  SignatureHelp,
  TextEdit,
  WorkspaceEdit,
  Range as LspRange,
} from "./protocol";

interface DocState {
  path: string;
  uri: string;
  session: LspSession | null;
  changeSub: Monaco.IDisposable;
  changeTimer: ReturnType<typeof setTimeout> | null;
}

/** The lifecycle state of a single language server, for the status indicator. */
export type LspStatusKind =
  | "unavailable" // registry knows this server, but the binary isn't on PATH.
  | "starting" // session spawned, `initialize` not yet resolved.
  | "indexing" // initialized + at least one active work-done progress.
  | "ready" // initialized, no active progress.
  | "error"; // spawn/initialize failed, or the server exited.

export interface LspStatus {
  serverId: string;
  status: LspStatusKind;
  /** A human-readable detail (progress title/message), when available. */
  message?: string;
  /** Work-done progress percentage (0–100), when the server reports one. */
  percent?: number;
}

const CHANGE_DEBOUNCE_MS = 250;
const COMPLETION_TRIGGERS = [".", ":", "<", '"', "'", "/", "@", "(", ",", " ", ">"];
const SIGNATURE_TRIGGERS = ["(", ","];

export class LspAdapter {
  private monaco: typeof Monaco;
  private rootUri: string | null;
  /** server id → available? (only ids present here may attach). */
  private available = new Set<string>();
  private sessions = new Map<string, LspSession>();
  private sessionRoute = new Map<string, LspSession>(); // backend sessionId → session
  private starting = new Map<string, Promise<LspSession | null>>();
  private docs = new Map<Monaco.editor.ITextModel, DocState>();
  private docsByUri = new Map<string, Monaco.editor.ITextModel>();
  private registeredLangs = new Set<string>();
  private disposables: Monaco.IDisposable[] = [];
  private unlisten: UnlistenFn[] = [];
  private disposed = false;
  /** Per-serverId status, and subscribers notified when any of them changes. */
  private statuses = new Map<string, LspStatus>();
  private statusSubs = new Set<() => void>();

  private constructor(monaco: typeof Monaco, rootUri: string | null) {
    this.monaco = monaco;
    this.rootUri = rootUri;
  }

  /**
   * Build the adapter: discover servers + workspace root, wire the global
   * message/exit listeners. Returns null on any failure (editor degrades).
   */
  static async create(monaco: typeof Monaco): Promise<LspAdapter | null> {
    try {
      const [servers, settings] = await Promise.all([
        lspServers().catch(() => []),
        getAppSettings().catch(() => null),
      ]);
      const root = settings?.workspaceRoot?.trim();
      const rootUri = root ? pathToFileUri(root) : null;
      const adapter = new LspAdapter(monaco, rootUri);
      for (const s of servers) if (s.available) adapter.available.add(s.id);
      // No available servers → still return the adapter (harmless no-op); wiring
      // the listeners is cheap and lets a later state change work.
      adapter.unlisten.push(
        await onLspMessage((e) => {
          try {
            adapter.sessionRoute.get(e.sessionId)?.handleMessage(e.body);
          } catch {
            /* never surface */
          }
        }),
      );
      adapter.unlisten.push(
        await onLspExit((e) => {
          try {
            adapter.onSessionExit(e.sessionId);
          } catch {
            /* never surface */
          }
        }),
      );
      return adapter;
    } catch {
      return null;
    }
  }

  // --- Status (consumed by the LspStatus indicator) ------------------------

  /**
   * The status of the server that serves `languageId`, or null when no server
   * maps to that language (plaintext, markdown, toml, …). A mapped-but-not-on-
   * PATH server reports "unavailable"; a mapped + available server that hasn't
   * started yet reports null (nothing to show until its first file opens).
   */
  getLanguageStatus(languageId: string): LspStatus | null {
    const serverId = serverIdForLanguage(languageId);
    if (!serverId) return null;
    const tracked = this.statuses.get(serverId);
    if (tracked) return tracked;
    if (!this.available.has(serverId)) return { serverId, status: "unavailable" };
    return null; // available, but no session yet.
  }

  /** Subscribe to status changes; returns an unsubscribe fn. */
  subscribe(cb: () => void): () => void {
    this.statusSubs.add(cb);
    return () => {
      this.statusSubs.delete(cb);
    };
  }

  /** Record a server's status and notify subscribers if it actually changed. */
  private setStatus(
    serverId: string,
    status: LspStatusKind,
    extra?: { message?: string; percent?: number },
  ): void {
    const prev = this.statuses.get(serverId);
    const next: LspStatus = { serverId, status, message: extra?.message, percent: extra?.percent };
    if (
      prev &&
      prev.status === next.status &&
      prev.message === next.message &&
      prev.percent === next.percent
    ) {
      return;
    }
    this.statuses.set(serverId, next);
    this.notifyStatus();
  }

  private notifyStatus(): void {
    for (const cb of this.statusSubs) {
      try {
        cb();
      } catch {
        /* a subscriber blew up — never surface. */
      }
    }
  }

  /** Recompute a ready session's status from its live work-done progress. */
  private recomputeStatus(serverId: string): void {
    const session = this.sessions.get(serverId);
    if (!session || !session.ready) return; // still starting / gone → leave as-is.
    const prog = session.progress;
    if (prog.active) {
      const message = prog.message ?? prog.title;
      this.setStatus(serverId, "indexing", { message, percent: prog.percent });
    } else {
      this.setStatus(serverId, "ready");
    }
  }

  // --- Model lifecycle (called by EditorView) -------------------------------

  /** Attach a model + its repo-relative path: open the document with its server. */
  attach(model: Monaco.editor.ITextModel, path: string): void {
    if (this.disposed || this.docs.has(model)) return;
    const serverId = serverIdForPath(path);
    if (!serverId || !this.available.has(serverId)) return; // no server → no-op.

    const uri = this.rootUri ? joinFileUri(this.rootUri, path) : pathToFileUri("/" + path);
    const changeSub = model.onDidChangeContent(() => this.scheduleChange(model));
    const doc: DocState = { path, uri, session: null, changeSub, changeTimer: null };
    this.docs.set(model, doc);
    this.docsByUri.set(normalizeUri(uri), model);

    void this.ensureSession(serverId)
      .then((session) => {
        if (this.disposed || model.isDisposed() || this.docs.get(model) !== doc) return;
        if (!session) return; // server failed to start → degrade.
        doc.session = session;
        this.ensureProviders(serverId);
        session.didOpen(uri, lspLanguageForPath(path), model.getValue());
      })
      .catch(() => {
        /* degrade */
      });
  }

  /** The active model's buffer was saved to disk. */
  notifySave(model: Monaco.editor.ITextModel): void {
    const doc = this.docs.get(model);
    if (!doc?.session || model.isDisposed()) return;
    this.flushChange(model); // ensure the server has the latest text first.
    try {
      doc.session.didSave(doc.uri, model.getValue());
    } catch {
      /* degrade */
    }
  }

  /** A tab closed / model disposed: close the document. */
  detach(model: Monaco.editor.ITextModel): void {
    const doc = this.docs.get(model);
    if (!doc) return;
    if (doc.changeTimer) clearTimeout(doc.changeTimer);
    try {
      doc.changeSub.dispose();
    } catch {
      /* ignore */
    }
    try {
      doc.session?.didClose(doc.uri);
    } catch {
      /* ignore */
    }
    this.docs.delete(model);
    this.docsByUri.delete(normalizeUri(doc.uri));
  }

  /** Tear everything down (editor unmount). */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    for (const model of [...this.docs.keys()]) this.detach(model);
    for (const d of this.disposables) {
      try {
        d.dispose();
      } catch {
        /* ignore */
      }
    }
    this.disposables = [];
    for (const un of this.unlisten) {
      try {
        un();
      } catch {
        /* ignore */
      }
    }
    this.unlisten = [];
    for (const [sessionId, session] of this.sessionRoute) {
      void session.shutdown().finally(() => void lspStop(sessionId).catch(() => {}));
      session.dispose();
    }
    this.sessions.clear();
    this.sessionRoute.clear();
    this.statuses.clear();
    this.statusSubs.clear();
  }

  // --- Session management ---------------------------------------------------

  private ensureSession(serverId: string): Promise<LspSession | null> {
    const existing = this.sessions.get(serverId);
    if (existing) return Promise.resolve(existing);
    const inflight = this.starting.get(serverId);
    if (inflight) return inflight;
    const p = this.startSession(serverId).finally(() => this.starting.delete(serverId));
    this.starting.set(serverId, p);
    return p;
  }

  private async startSession(serverId: string): Promise<LspSession | null> {
    if (this.disposed) return null;
    this.setStatus(serverId, "starting");
    let sessionId: string;
    try {
      sessionId = await lspStart(serverId);
    } catch {
      this.setStatus(serverId, "error"); // couldn't spawn — degrade.
      return null;
    }
    if (this.disposed) {
      void lspStop(sessionId).catch(() => {});
      return null;
    }
    const session = new LspSession(
      serverId,
      (body) => lspSend(sessionId, body),
      {
        monaco: this.monaco,
        rootUri: this.rootUri,
        resolveModel: (uri) => this.modelForUri(uri),
        onProgressChange: () => this.recomputeStatus(serverId),
      },
    );
    // Route inbound messages before the handshake so the initialize response
    // isn't missed.
    this.sessionRoute.set(sessionId, session);
    this.sessions.set(serverId, session);
    const ok = await session.initialize();
    if (this.disposed) {
      this.sessionRoute.delete(sessionId);
      this.sessions.delete(serverId);
      session.dispose();
      void lspStop(sessionId).catch(() => {});
      return null;
    }
    if (!ok) {
      this.setStatus(serverId, "error");
      this.sessionRoute.delete(sessionId);
      this.sessions.delete(serverId);
      session.dispose();
      void lspStop(sessionId).catch(() => {});
      return null;
    }
    // Initialized: ready, unless a work-done progress is already active.
    this.recomputeStatus(serverId);
    return session;
  }

  private onSessionExit(sessionId: string): void {
    const session = this.sessionRoute.get(sessionId);
    if (!session) return;
    this.sessionRoute.delete(sessionId);
    this.sessions.delete(session.serverId);
    this.setStatus(session.serverId, "error"); // the server exited.
    // Detach the crashed session from its documents + clear their markers, but
    // keep the documents open (typing keeps working, just without intelligence).
    for (const [model, doc] of this.docs) {
      if (doc.session === session) {
        doc.session = null;
        if (!model.isDisposed()) {
          try {
            this.monaco.editor.setModelMarkers(model, `lsp:${session.serverId}`, []);
          } catch {
            /* ignore */
          }
        }
      }
    }
    session.dispose();
  }

  // --- Document change debounce ---------------------------------------------

  private scheduleChange(model: Monaco.editor.ITextModel): void {
    const doc = this.docs.get(model);
    if (!doc) return;
    if (doc.changeTimer) clearTimeout(doc.changeTimer);
    doc.changeTimer = setTimeout(() => {
      doc.changeTimer = null;
      if (doc.session && !model.isDisposed()) {
        try {
          doc.session.didChange(doc.uri, model.getValue());
        } catch {
          /* degrade */
        }
      }
    }, CHANGE_DEBOUNCE_MS);
  }

  private flushChange(model: Monaco.editor.ITextModel): void {
    const doc = this.docs.get(model);
    if (!doc?.changeTimer) return;
    clearTimeout(doc.changeTimer);
    doc.changeTimer = null;
    if (doc.session && !model.isDisposed()) {
      try {
        doc.session.didChange(doc.uri, model.getValue());
      } catch {
        /* degrade */
      }
    }
  }

  // --- URI ⇄ model ----------------------------------------------------------

  private modelForUri(uri: string): Monaco.editor.ITextModel | null {
    const m = this.docsByUri.get(normalizeUri(uri));
    return m && !m.isDisposed() ? m : null;
  }

  /** A server-returned uri → a Monaco uri (an open model's, else parsed). */
  private uriToMonaco(uri: string): Monaco.Uri {
    const model = this.modelForUri(uri);
    if (model) return model.uri;
    try {
      return this.monaco.Uri.parse(uri);
    } catch {
      return this.monaco.Uri.parse("file:///unknown");
    }
  }

  // --- Provider registration ------------------------------------------------

  private ensureProviders(serverId: string): void {
    const langs = SERVER_LANGUAGES[serverId] ?? [];
    for (const lang of langs) {
      if (this.registeredLangs.has(lang)) continue;
      this.registeredLangs.add(lang);
      this.registerProvidersForLanguage(lang);
    }
  }

  /** The session owning this model, only if ready + it advertises `cap`. */
  private sessionFor(
    model: Monaco.editor.ITextModel,
    cap: (c: ServerCapabilities) => boolean,
  ): { doc: DocState; session: LspSession } | null {
    const doc = this.docs.get(model);
    if (!doc?.session || !doc.session.ready) return null;
    if (!cap(doc.session.capabilities)) return null;
    return { doc, session: doc.session };
  }

  private registerProvidersForLanguage(lang: string): void {
    const m = this.monaco;
    const d = this.disposables;

    d.push(
      m.languages.registerCompletionItemProvider(lang, {
        triggerCharacters: COMPLETION_TRIGGERS,
        provideCompletionItems: async (model, position, context) => {
          const found = this.sessionFor(model, (c) => !!c.completionProvider);
          if (!found) return undefined;
          try {
            const res = await found.session.completion(
              found.doc.uri,
              position,
              context.triggerCharacter,
            );
            return this.toCompletionList(res, model, position, found.session);
          } catch {
            return undefined;
          }
        },
        resolveCompletionItem: async (item) => {
          const carrier = item as MonacoCompletion;
          const lsp = carrier._lsp;
          const session = carrier._session;
          if (!lsp || !session) return item;
          try {
            const resolved = await session.resolveCompletion(lsp);
            if (!resolved) return item;
            if (resolved.detail) item.detail = resolved.detail;
            const md = docToMarkdown(resolved.documentation);
            if (md) item.documentation = md;
            if (resolved.additionalTextEdits?.length) {
              item.additionalTextEdits = resolved.additionalTextEdits.map((e) => ({
                range: fromLspRange(m, e.range),
                text: e.newText,
              }));
            }
            return item;
          } catch {
            return item;
          }
        },
      }),
    );

    d.push(
      m.languages.registerHoverProvider(lang, {
        provideHover: async (model, position) => {
          const found = this.sessionFor(model, (c) => !!c.hoverProvider);
          if (!found) return undefined;
          try {
            const res = await found.session.hover(found.doc.uri, position);
            return this.toHover(res);
          } catch {
            return undefined;
          }
        },
      }),
    );

    d.push(
      m.languages.registerDefinitionProvider(lang, {
        provideDefinition: async (model, position) => {
          const found = this.sessionFor(model, (c) => !!c.definitionProvider);
          if (!found) return undefined;
          try {
            const res = await found.session.definition(found.doc.uri, position);
            return this.toLocations(res);
          } catch {
            return undefined;
          }
        },
      }),
    );

    d.push(
      m.languages.registerReferenceProvider(lang, {
        provideReferences: async (model, position, ctx) => {
          const found = this.sessionFor(model, (c) => !!c.referencesProvider);
          if (!found) return undefined;
          try {
            const res = await found.session.references(
              found.doc.uri,
              position,
              ctx.includeDeclaration,
            );
            return this.toLocations(res);
          } catch {
            return undefined;
          }
        },
      }),
    );

    d.push(
      m.languages.registerRenameProvider(lang, {
        provideRenameEdits: async (model, position, newName) => {
          const found = this.sessionFor(model, (c) => !!c.renameProvider);
          if (!found) return { edits: [] };
          try {
            const res = await found.session.rename(found.doc.uri, position, newName);
            return this.toWorkspaceEdit(res);
          } catch {
            return { edits: [] };
          }
        },
        resolveRenameLocation: async (model, position) => {
          const found = this.sessionFor(model, (c) => !!c.renameProvider);
          const reject = () =>
            ({ rejectReason: "You cannot rename this element." }) as RenameResult;
          const wordRange = (): RenameResult => {
            const w = model.getWordAtPosition(position);
            if (!w) return reject();
            return {
              range: new m.Range(position.lineNumber, w.startColumn, position.lineNumber, w.endColumn),
              text: w.word,
            } as RenameResult;
          };
          if (!found) return wordRange();
          const caps = found.session.capabilities.renameProvider;
          const canPrepare = typeof caps === "object" && !!caps?.prepareProvider;
          if (!canPrepare) return wordRange();
          try {
            const res = await found.session.prepareRename(found.doc.uri, position);
            if (!res) return reject();
            const range = "range" in res ? fromLspRange(m, res.range) : fromLspRange(m, res as LspRange);
            const placeholder =
              "placeholder" in res && res.placeholder
                ? res.placeholder
                : model.getValueInRange(range);
            return { range, text: placeholder } as RenameResult;
          } catch {
            return wordRange();
          }
        },
      }),
    );

    d.push(
      m.languages.registerDocumentFormattingEditProvider(lang, {
        provideDocumentFormattingEdits: async (model, options) => {
          const found = this.sessionFor(model, (c) => !!c.documentFormattingProvider);
          if (!found) return undefined;
          try {
            const res = await found.session.formatting(found.doc.uri, {
              tabSize: options.tabSize,
              insertSpaces: options.insertSpaces,
            });
            return this.toTextEdits(res);
          } catch {
            return undefined;
          }
        },
      }),
    );

    d.push(
      m.languages.registerSignatureHelpProvider(lang, {
        signatureHelpTriggerCharacters: SIGNATURE_TRIGGERS,
        signatureHelpRetriggerCharacters: [")"],
        provideSignatureHelp: async (model, position) => {
          const found = this.sessionFor(model, (c) => !!c.signatureHelpProvider);
          if (!found) return undefined;
          try {
            const res = await found.session.signatureHelp(found.doc.uri, position);
            return this.toSignatureHelp(res);
          } catch {
            return undefined;
          }
        },
      }),
    );
  }

  // --- Result converters ----------------------------------------------------

  private toCompletionList(
    res: CompletionList | CompletionItem[] | null,
    model: Monaco.editor.ITextModel,
    position: Monaco.IPosition,
    session: LspSession,
  ): Monaco.languages.CompletionList {
    const m = this.monaco;
    if (!res) return { suggestions: [] };
    const items = Array.isArray(res) ? res : res.items;
    const incomplete = Array.isArray(res) ? false : res.isIncomplete;
    const word = model.getWordUntilPosition(position);
    const defaultRange = new m.Range(
      position.lineNumber,
      word.startColumn,
      position.lineNumber,
      word.endColumn,
    );
    const suggestions: MonacoCompletion[] = (items ?? []).map((item) => {
      const { insertText, isSnippet } = completionInsertText(item);
      let range: Monaco.IRange = defaultRange;
      const te = item.textEdit;
      if (te) {
        const r = "range" in te && te.range ? te.range : ("replace" in te ? te.replace : undefined) ?? ("insert" in te ? te.insert : undefined);
        if (r) range = fromLspRange(m, r);
      }
      const sug: MonacoCompletion = {
        label: item.label,
        kind: completionKind(m, item.kind),
        insertText,
        range,
        detail: item.detail,
        documentation: docToMarkdown(item.documentation),
        sortText: item.sortText,
        filterText: item.filterText,
        preselect: item.preselect,
        _lsp: item,
        _session: session,
      };
      if (isSnippet) sug.insertTextRules = m.languages.CompletionItemInsertTextRule.InsertAsSnippet;
      if (item.additionalTextEdits?.length) {
        sug.additionalTextEdits = item.additionalTextEdits.map((e) => ({
          range: fromLspRange(m, e.range),
          text: e.newText,
        }));
      }
      return sug;
    });
    return { suggestions, incomplete };
  }

  private toHover(res: Hover | null): Monaco.languages.Hover | undefined {
    if (!res || res.contents == null) return undefined;
    const contents = normalizeHoverContents(res.contents);
    if (contents.length === 0) return undefined;
    return {
      contents,
      range: res.range ? fromLspRange(this.monaco, res.range) : undefined,
    };
  }

  private toLocations(
    res: LspLocation | LspLocation[] | LocationLink[] | null,
  ): Monaco.languages.Location[] {
    if (!res) return [];
    const arr = Array.isArray(res) ? res : [res];
    const out: Monaco.languages.Location[] = [];
    for (const item of arr) {
      if (!item) continue;
      if ("targetUri" in item) {
        out.push({
          uri: this.uriToMonaco(item.targetUri),
          range: fromLspRange(this.monaco, item.targetSelectionRange ?? item.targetRange),
        });
      } else if ("uri" in item) {
        out.push({ uri: this.uriToMonaco(item.uri), range: fromLspRange(this.monaco, item.range) });
      }
    }
    return out;
  }

  private toWorkspaceEdit(res: WorkspaceEdit | null): Monaco.languages.WorkspaceEdit {
    const m = this.monaco;
    const edits: Monaco.languages.IWorkspaceTextEdit[] = [];
    if (!res) return { edits };
    const push = (uri: string, textEdits: TextEdit[]) => {
      const resource = this.uriToMonaco(uri);
      for (const e of textEdits) {
        edits.push({
          resource,
          versionId: undefined,
          textEdit: { range: fromLspRange(m, e.range), text: e.newText },
        });
      }
    };
    if (res.documentChanges) {
      for (const dc of res.documentChanges) push(dc.textDocument.uri, dc.edits);
    } else if (res.changes) {
      for (const [uri, textEdits] of Object.entries(res.changes)) push(uri, textEdits);
    }
    return { edits };
  }

  private toTextEdits(
    res: Array<{ range: LspRange; newText: string }> | null,
  ): Monaco.languages.TextEdit[] | undefined {
    if (!res) return undefined;
    return res.map((e) => ({ range: fromLspRange(this.monaco, e.range), text: e.newText }));
  }

  private toSignatureHelp(
    res: SignatureHelp | null,
  ): Monaco.languages.SignatureHelpResult | undefined {
    if (!res || !res.signatures?.length) return undefined;
    const value: Monaco.languages.SignatureHelp = {
      signatures: res.signatures.map((s) => ({
        label: s.label,
        documentation: docToMarkdown(s.documentation),
        parameters: (s.parameters ?? []).map((p) => ({
          label: p.label,
          documentation: docToMarkdown(p.documentation),
        })),
        activeParameter: s.activeParameter,
      })),
      activeSignature: res.activeSignature ?? 0,
      activeParameter: res.activeParameter ?? 0,
    };
    return { value, dispose: () => {} };
  }
}

/** A Monaco suggestion carrying the originating LSP item for resolve. */
type MonacoCompletion = Monaco.languages.CompletionItem & {
  _lsp?: CompletionItem;
  _session?: LspSession;
};

/** Monaco types the prepare-rename result as the intersection of both shapes. */
type RenameResult = Monaco.languages.RenameLocation & Monaco.languages.Rejection;

function normalizeHoverContents(
  contents: Hover["contents"],
): Monaco.IMarkdownString[] {
  const toMd = (
    c: string | MarkupContent | { language: string; value: string },
  ): Monaco.IMarkdownString | null => {
    if (c == null) return null;
    if (typeof c === "string") return c.trim() ? { value: c } : null;
    if ("language" in c) {
      return c.value.trim() ? { value: "```" + c.language + "\n" + c.value + "\n```" } : null;
    }
    return c.value?.trim() ? { value: c.value } : null;
  };
  if (Array.isArray(contents)) {
    return contents.map(toMd).filter((x): x is Monaco.IMarkdownString => x !== null);
  }
  const single = toMd(contents);
  return single ? [single] : [];
}
