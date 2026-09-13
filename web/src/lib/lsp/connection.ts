// A JSON-RPC 2.0 conversation over one language-server session. It owns the
// request-id counter + pending-promise map, dispatches server notifications to
// registered handlers, and answers the handful of server→client requests that
// would otherwise stall a server (registerCapability, workspace/configuration,
// progress creation, …). Every failure degrades quietly — a rejected request
// resolves callers to a graceful null upstream; nothing throws into Monaco.

import {
  isNotification,
  isRequest,
  isResponse,
  type RpcMessage,
  type RpcResponse,
} from "./protocol";

type NotificationHandler = (params: unknown) => void;

interface Pending {
  resolve: (value: unknown) => void;
  reject: (reason: unknown) => void;
  timer: ReturnType<typeof setTimeout>;
}

/** How long to wait for a response before giving up (and degrading). */
const REQUEST_TIMEOUT_MS = 15_000;

export class JsonRpcConnection {
  private nextId = 1;
  private pending = new Map<number, Pending>();
  private notificationHandlers = new Map<string, NotificationHandler>();
  private closed = false;

  /**
   * @param send Writes one complete JSON-RPC message string to the server's
   *   stdin (the backend adds Content-Length framing). May reject; we swallow.
   */
  constructor(private send: (body: string) => Promise<void>) {}

  /** Register a handler for a server→client notification method. */
  onNotification(method: string, handler: NotificationHandler): void {
    this.notificationHandlers.set(method, handler);
  }

  /** Fire-and-forget client→server notification. */
  notify(method: string, params?: unknown): void {
    if (this.closed) return;
    void this.write({ jsonrpc: "2.0", method, params });
  }

  /** Client→server request; resolves with the result or rejects on error/timeout. */
  request<T = unknown>(method: string, params?: unknown): Promise<T> {
    if (this.closed) return Promise.reject(new Error("connection closed"));
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`LSP request ${method} timed out`));
      }, REQUEST_TIMEOUT_MS);
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject, timer });
      void this.write({ jsonrpc: "2.0", id, method, params }).catch((e) => {
        const p = this.pending.get(id);
        if (p) {
          clearTimeout(p.timer);
          this.pending.delete(id);
          reject(e);
        }
      });
    });
  }

  /** Feed one raw inbound JSON-RPC message string (from `lsp://message`). */
  handleMessage(body: string): void {
    if (this.closed) return;
    let msg: RpcMessage;
    try {
      msg = JSON.parse(body) as RpcMessage;
    } catch {
      return; // malformed frame — ignore rather than crash the adapter.
    }
    try {
      if (isResponse(msg)) {
        this.handleResponse(msg);
      } else if (isRequest(msg)) {
        this.handleServerRequest(msg.id, msg.method, msg.params);
      } else if (isNotification(msg)) {
        this.notificationHandlers.get(msg.method)?.(msg.params);
      }
    } catch {
      // A handler blew up — never let it escape into the event listener.
    }
  }

  /** Reject every in-flight request and stop accepting new ones. */
  dispose(): void {
    this.closed = true;
    for (const p of this.pending.values()) {
      clearTimeout(p.timer);
      p.reject(new Error("connection disposed"));
    }
    this.pending.clear();
    this.notificationHandlers.clear();
  }

  private handleResponse(msg: RpcResponse): void {
    const id = typeof msg.id === "number" ? msg.id : Number(msg.id);
    const p = this.pending.get(id);
    if (!p) return;
    clearTimeout(p.timer);
    this.pending.delete(id);
    if (msg.error) p.reject(new Error(msg.error.message || `LSP error ${msg.error.code}`));
    else p.resolve(msg.result);
  }

  /**
   * Answer server→client requests so servers don't stall. We keep this minimal:
   * capability (un)registration and progress creation get an empty/null ack;
   * `workspace/configuration` returns one `null` per requested item (servers
   * fall back to their own defaults); everything else gets a null result.
   */
  private handleServerRequest(id: number | string, method: string, params: unknown): void {
    let result: unknown = null;
    if (method === "workspace/configuration") {
      const items = (params as { items?: unknown[] } | undefined)?.items;
      result = Array.isArray(items) ? items.map(() => null) : [];
    }
    // client/registerCapability, client/unregisterCapability,
    // window/workDoneProgress/create, workspace/*Refresh, etc. → null.
    void this.write({ jsonrpc: "2.0", id, result });
  }

  private async write(msg: object): Promise<void> {
    try {
      await this.send(JSON.stringify(msg));
    } catch {
      // stdin write failed (server gone) — pending requests time out on their
      // own; nothing to surface.
    }
  }
}
