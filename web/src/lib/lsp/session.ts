// One initialized conversation with a single language server (one backend
// session). Owns the `initialize`/`initialized` handshake, document
// open/change/save/close notifications with per-document versions, inbound
// `publishDiagnostics` → Monaco markers, and thin typed request wrappers the
// Monaco providers call. Everything is defensive: a failed handshake leaves the
// session `ready` = false and every request wrapper resolves to null, so the
// editor keeps working with zero language intelligence rather than erroring.

import type * as Monaco from "monaco-editor";
import { JsonRpcConnection } from "./connection";
import {
  diagnosticToMarker,
  toLspPosition,
  toLspRange,
} from "./convert";
import type {
  CompletionItem,
  CompletionList,
  Hover,
  LocationLink,
  LspLocation,
  PublishDiagnosticsParams,
  ServerCapabilities,
  SignatureHelp,
  WorkspaceEdit,
  Range as LspRange,
} from "./protocol";

interface SessionDeps {
  monaco: typeof Monaco;
  rootUri: string | null;
  /** Find an open Monaco model for an LSP document uri (for diagnostics). */
  resolveModel: (uri: string) => Monaco.editor.ITextModel | null;
  /** Called when the session's work-done progress changes (drives status UI). */
  onProgressChange?: () => void;
}

/** The latest snapshot of a single active work-done progress. */
interface ProgressInfo {
  title?: string;
  message?: string;
  percent?: number;
}

export class LspSession {
  readonly serverId: string;
  private conn: JsonRpcConnection;
  private deps: SessionDeps;
  private markerOwner: string;
  private versions = new Map<string, number>();
  private open = new Set<string>();
  private _ready = false;
  private initPromise: Promise<boolean> | null = null;
  private caps: ServerCapabilities = {};
  private disposed = false;
  /** Active work-done progress tokens → their latest reported state. Insertion
   *  order is kept as update order (we re-insert on each update) so the last
   *  entry is the most recently updated one. */
  private activeProgress = new Map<string | number, ProgressInfo>();

  constructor(
    serverId: string,
    send: (body: string) => Promise<void>,
    deps: SessionDeps,
  ) {
    this.serverId = serverId;
    this.deps = deps;
    this.markerOwner = `lsp:${serverId}`;
    this.conn = new JsonRpcConnection(send);
    this.conn.onNotification("textDocument/publishDiagnostics", (params) =>
      this.onPublishDiagnostics(params as PublishDiagnosticsParams),
    );
    // Work-done progress (rust-analyzer's indexing, etc.) drives the status UI.
    this.conn.onNotification("$/progress", (params) => this.onProgress(params));
  }

  get ready(): boolean {
    return this._ready;
  }
  get capabilities(): ServerCapabilities {
    return this.caps;
  }

  /** Whether any work-done progress is active, plus the latest one's detail. */
  get progress(): { active: boolean; title?: string; message?: string; percent?: number } {
    if (this.activeProgress.size === 0) return { active: false };
    let last: ProgressInfo = {};
    for (const info of this.activeProgress.values()) last = info;
    return { active: true, ...last };
  }

  /** Route one raw inbound JSON-RPC message into this session's connection. */
  handleMessage(body: string): void {
    this.conn.handleMessage(body);
  }

  /** Run the `initialize` handshake once; resolves true on success. Idempotent. */
  initialize(): Promise<boolean> {
    if (this.initPromise) return this.initPromise;
    this.initPromise = this.doInitialize().catch(() => false);
    return this.initPromise;
  }

  private async doInitialize(): Promise<boolean> {
    if (this.disposed) return false;
    const params = {
      processId: null,
      clientInfo: { name: "hive-editor" },
      rootUri: this.deps.rootUri,
      rootPath: this.deps.rootUri ? uriToPath(this.deps.rootUri) : null,
      workspaceFolders: this.deps.rootUri
        ? [{ uri: this.deps.rootUri, name: "workspace" }]
        : null,
      capabilities: CLIENT_CAPABILITIES,
      initializationOptions: {},
    };
    const result = (await this.conn.request("initialize", params)) as
      | { capabilities?: ServerCapabilities }
      | undefined;
    if (this.disposed) return false;
    this.caps = result?.capabilities ?? {};
    this.conn.notify("initialized", {});
    this._ready = true;
    return true;
  }

  // --- Document sync --------------------------------------------------------

  didOpen(uri: string, languageId: string, text: string): void {
    if (!this._ready || this.open.has(uri)) return;
    this.open.add(uri);
    this.versions.set(uri, 1);
    this.conn.notify("textDocument/didOpen", {
      textDocument: { uri, languageId, version: 1, text },
    });
  }

  didChange(uri: string, text: string): void {
    if (!this._ready || !this.open.has(uri)) return;
    const version = (this.versions.get(uri) ?? 1) + 1;
    this.versions.set(uri, version);
    // FULL-text sync: one change range-less content event.
    this.conn.notify("textDocument/didChange", {
      textDocument: { uri, version },
      contentChanges: [{ text }],
    });
  }

  didSave(uri: string, text: string): void {
    if (!this._ready || !this.open.has(uri)) return;
    this.conn.notify("textDocument/didSave", {
      textDocument: { uri },
      text,
    });
  }

  didClose(uri: string): void {
    if (!this.open.has(uri)) return;
    this.open.delete(uri);
    this.versions.delete(uri);
    // Clear any markers we set for this document.
    const model = this.deps.resolveModel(uri);
    if (model && !model.isDisposed()) {
      this.deps.monaco.editor.setModelMarkers(model, this.markerOwner, []);
    }
    if (this._ready) this.conn.notify("textDocument/didClose", { textDocument: { uri } });
  }

  // --- Feature requests (raw LSP results; callers convert) ------------------

  completion(uri: string, pos: Monaco.IPosition, triggerCharacter?: string) {
    return this.req<CompletionList | CompletionItem[] | null>("textDocument/completion", {
      textDocument: { uri },
      position: toLspPosition(pos),
      context: triggerCharacter
        ? { triggerKind: 2, triggerCharacter }
        : { triggerKind: 1 },
    });
  }
  resolveCompletion(item: CompletionItem) {
    return this.req<CompletionItem | null>("completionItem/resolve", item);
  }
  hover(uri: string, pos: Monaco.IPosition) {
    return this.req<Hover | null>("textDocument/hover", {
      textDocument: { uri },
      position: toLspPosition(pos),
    });
  }
  definition(uri: string, pos: Monaco.IPosition) {
    return this.req<LspLocation | LspLocation[] | LocationLink[] | null>(
      "textDocument/definition",
      { textDocument: { uri }, position: toLspPosition(pos) },
    );
  }
  references(uri: string, pos: Monaco.IPosition, includeDeclaration: boolean) {
    return this.req<LspLocation[] | null>("textDocument/references", {
      textDocument: { uri },
      position: toLspPosition(pos),
      context: { includeDeclaration },
    });
  }
  prepareRename(uri: string, pos: Monaco.IPosition) {
    return this.req<LspRange | { range: LspRange; placeholder?: string } | null>(
      "textDocument/prepareRename",
      { textDocument: { uri }, position: toLspPosition(pos) },
    );
  }
  rename(uri: string, pos: Monaco.IPosition, newName: string) {
    return this.req<WorkspaceEdit | null>("textDocument/rename", {
      textDocument: { uri },
      position: toLspPosition(pos),
      newName,
    });
  }
  formatting(uri: string, options: { tabSize: number; insertSpaces: boolean }) {
    return this.req<Array<{ range: LspRange; newText: string }> | null>(
      "textDocument/formatting",
      { textDocument: { uri }, options },
    );
  }
  rangeFormatting(
    uri: string,
    range: Monaco.IRange,
    options: { tabSize: number; insertSpaces: boolean },
  ) {
    return this.req<Array<{ range: LspRange; newText: string }> | null>(
      "textDocument/rangeFormatting",
      { textDocument: { uri }, range: toLspRange(range), options },
    );
  }
  signatureHelp(uri: string, pos: Monaco.IPosition) {
    return this.req<SignatureHelp | null>("textDocument/signatureHelp", {
      textDocument: { uri },
      position: toLspPosition(pos),
    });
  }

  async shutdown(): Promise<void> {
    if (this.disposed) return;
    if (this._ready) {
      try {
        await this.conn.request("shutdown");
        this.conn.notify("exit");
      } catch {
        // Server already gone — the backend kills the child on lsp_stop anyway.
      }
    }
  }

  dispose(): void {
    this.disposed = true;
    this._ready = false;
    this.conn.dispose();
    this.open.clear();
    this.versions.clear();
    this.activeProgress.clear();
  }

  /** Guarded request: returns null on any failure or before the handshake. */
  private async req<T>(method: string, params: unknown): Promise<T | null> {
    if (!this._ready || this.disposed) return null;
    try {
      return (await this.conn.request<T>(method, params)) ?? null;
    } catch {
      return null;
    }
  }

  /**
   * Handle a `$/progress` notification. We only track *work-done* progress
   * (value.kind of begin/report/end); other progress payloads (partial results,
   * etc.) carry no `kind` and are ignored. A `begin`/`report` records/updates
   * the token, `end` clears it. While any token is active the manager reports
   * "indexing". Robust to a missing `begin` (a stray `report` starts tracking).
   */
  private onProgress(params: unknown): void {
    if (this.disposed) return;
    const p = params as
      | {
          token?: string | number;
          value?: { kind?: string; title?: string; message?: string; percentage?: number };
        }
      | undefined;
    const token = p?.token;
    const value = p?.value;
    if (token == null || !value || typeof value.kind !== "string") return;
    if (value.kind === "end") {
      if (!this.activeProgress.delete(token)) return; // nothing was tracked.
    } else if (value.kind === "begin" || value.kind === "report") {
      const prev = this.activeProgress.get(token) ?? {};
      // Re-insert (delete first) so this token becomes the most-recent entry.
      this.activeProgress.delete(token);
      this.activeProgress.set(token, {
        title: value.title ?? prev.title,
        message: value.message ?? prev.message,
        percent: value.percentage ?? prev.percent,
      });
    } else {
      return;
    }
    try {
      this.deps.onProgressChange?.();
    } catch {
      /* never surface */
    }
  }

  private onPublishDiagnostics(params: PublishDiagnosticsParams): void {
    if (this.disposed || !params?.uri) return;
    const model = this.deps.resolveModel(params.uri);
    if (!model || model.isDisposed()) return;
    const markers = (params.diagnostics ?? []).map((d) =>
      diagnosticToMarker(this.deps.monaco, d),
    );
    try {
      this.deps.monaco.editor.setModelMarkers(model, this.markerOwner, markers);
    } catch {
      // Model disposed mid-flight — ignore.
    }
  }
}

/** Turn a `file://` uri back into an OS path (best effort, for `rootPath`). */
function uriToPath(uri: string): string {
  try {
    const u = new URL(uri);
    return decodeURIComponent(u.pathname);
  } catch {
    return uri;
  }
}

/** Client capabilities we advertise. Kept conservative + dynamicRegistration-free. */
const CLIENT_CAPABILITIES = {
  general: {
    positionEncodings: ["utf-16"],
  },
  workspace: {
    workspaceFolders: true,
    configuration: true,
    didChangeConfiguration: { dynamicRegistration: false },
  },
  textDocument: {
    synchronization: {
      dynamicRegistration: false,
      didSave: true,
      willSave: false,
      willSaveWaitUntil: false,
    },
    completion: {
      dynamicRegistration: false,
      contextSupport: true,
      completionItem: {
        snippetSupport: true,
        documentationFormat: ["markdown", "plaintext"],
        deprecatedSupport: true,
        insertReplaceSupport: false,
        resolveSupport: { properties: ["documentation", "detail", "additionalTextEdits"] },
      },
      completionItemKind: {
        // Advertise the full 1..25 range so servers don't down-map.
        valueSet: Array.from({ length: 25 }, (_, i) => i + 1),
      },
    },
    hover: {
      dynamicRegistration: false,
      contentFormat: ["markdown", "plaintext"],
    },
    signatureHelp: {
      dynamicRegistration: false,
      signatureInformation: {
        documentationFormat: ["markdown", "plaintext"],
        parameterInformation: { labelOffsetSupport: true },
      },
    },
    definition: { dynamicRegistration: false, linkSupport: true },
    references: { dynamicRegistration: false },
    rename: { dynamicRegistration: false, prepareSupport: true },
    formatting: { dynamicRegistration: false },
    rangeFormatting: { dynamicRegistration: false },
    publishDiagnostics: { relatedInformation: true, tagSupport: { valueSet: [1, 2] } },
  },
};
