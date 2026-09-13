import { describe, it, expect } from "vitest";
import { serverIdForPath, lspLanguageForPath } from "./registry";
import { pathToFileUri, joinFileUri, normalizeUri } from "./uri";
import { toLspPosition, fromLspPosition, completionInsertText } from "./convert";
import { JsonRpcConnection } from "./connection";

describe("lsp registry", () => {
  it("routes extensions to server ids", () => {
    expect(serverIdForPath("src/a.ts")).toBe("typescript");
    expect(serverIdForPath("a.tsx")).toBe("typescript");
    expect(serverIdForPath("a.js")).toBe("typescript");
    expect(serverIdForPath("main.rs")).toBe("rust-analyzer");
    expect(serverIdForPath("x/y.py")).toBe("pyright");
    expect(serverIdForPath("cmd/main.go")).toBe("gopls");
  });

  it("returns null for unserved / extension-less paths", () => {
    expect(serverIdForPath("README.md")).toBeNull();
    expect(serverIdForPath("Makefile")).toBeNull();
    expect(serverIdForPath("noext")).toBeNull();
    expect(serverIdForPath(".gitignore")).toBeNull();
  });

  it("maps to LSP language ids (react variants distinguished)", () => {
    expect(lspLanguageForPath("a.ts")).toBe("typescript");
    expect(lspLanguageForPath("a.tsx")).toBe("typescriptreact");
    expect(lspLanguageForPath("a.jsx")).toBe("javascriptreact");
    expect(lspLanguageForPath("a.py")).toBe("python");
    expect(lspLanguageForPath("a.unknown")).toBe("plaintext");
  });
});

describe("lsp uri helpers", () => {
  it("builds file uris and encodes segments", () => {
    expect(pathToFileUri("/home/me/proj")).toBe("file:///home/me/proj");
    expect(pathToFileUri("/a b/c")).toBe("file:///a%20b/c");
    expect(pathToFileUri("C:\\Users\\me")).toBe("file:///C%3A/Users/me");
  });

  it("joins root + relative path", () => {
    expect(joinFileUri("file:///home/me/proj", "src/a.ts")).toBe(
      "file:///home/me/proj/src/a.ts",
    );
    expect(joinFileUri("file:///home/me/proj/", "/src/a.ts")).toBe(
      "file:///home/me/proj/src/a.ts",
    );
    expect(joinFileUri("file:///root", "a b.ts")).toBe("file:///root/a%20b.ts");
  });

  it("normalizes percent-encoding for comparison", () => {
    expect(normalizeUri("file:///a%20b/c.ts")).toBe(normalizeUri("file:///a b/c.ts"));
  });
});

describe("lsp position mapping", () => {
  it("maps Monaco (1-based) ⇄ LSP (0-based)", () => {
    expect(toLspPosition({ lineNumber: 1, column: 1 })).toEqual({ line: 0, character: 0 });
    expect(toLspPosition({ lineNumber: 5, column: 3 })).toEqual({ line: 4, character: 2 });
    expect(fromLspPosition({ line: 0, character: 0 })).toEqual({ lineNumber: 1, column: 1 });
    expect(fromLspPosition({ line: 4, character: 2 })).toEqual({ lineNumber: 5, column: 3 });
  });

  it("extracts completion insert text + snippet flag", () => {
    expect(completionInsertText({ label: "foo" })).toEqual({ insertText: "foo", isSnippet: false });
    expect(completionInsertText({ label: "foo", insertText: "foo()" })).toEqual({
      insertText: "foo()",
      isSnippet: false,
    });
    expect(
      completionInsertText({ label: "foo", insertText: "foo($1)", insertTextFormat: 2 }),
    ).toEqual({ insertText: "foo($1)", isSnippet: true });
    expect(
      completionInsertText({
        label: "foo",
        textEdit: { range: { start: { line: 0, character: 0 }, end: { line: 0, character: 3 } }, newText: "bar" },
      }),
    ).toEqual({ insertText: "bar", isSnippet: false });
  });
});

describe("JsonRpcConnection", () => {
  it("resolves a request with the matching response", async () => {
    const sent: string[] = [];
    const conn = new JsonRpcConnection(async (b) => void sent.push(b));
    const p = conn.request<number>("ping");
    const req = JSON.parse(sent[0]);
    expect(req.method).toBe("ping");
    expect(req.id).toBeTypeOf("number");
    conn.handleMessage(JSON.stringify({ jsonrpc: "2.0", id: req.id, result: 42 }));
    await expect(p).resolves.toBe(42);
  });

  it("rejects a request whose response carries an error", async () => {
    const conn = new JsonRpcConnection(async () => {});
    const p = conn.request("boom").catch((e) => (e as Error).message);
    // Grab the id by sending through a capturing connection instead:
    const sent: string[] = [];
    const conn2 = new JsonRpcConnection(async (b) => void sent.push(b));
    const p2 = conn2.request("boom").catch((e) => (e as Error).message);
    const id = JSON.parse(sent[0]).id;
    conn2.handleMessage(
      JSON.stringify({ jsonrpc: "2.0", id, error: { code: -1, message: "nope" } }),
    );
    await expect(p2).resolves.toBe("nope");
    // Clean up the dangling first promise so the test doesn't leak a timer.
    conn.dispose();
    await expect(p).resolves.toMatch(/disposed/);
  });

  it("dispatches notifications to handlers", () => {
    const conn = new JsonRpcConnection(async () => {});
    let got: unknown = null;
    conn.onNotification("textDocument/publishDiagnostics", (params) => (got = params));
    conn.handleMessage(
      JSON.stringify({
        jsonrpc: "2.0",
        method: "textDocument/publishDiagnostics",
        params: { uri: "file:///a.ts", diagnostics: [] },
      }),
    );
    expect(got).toEqual({ uri: "file:///a.ts", diagnostics: [] });
  });

  it("answers server→client requests so servers don't stall", () => {
    const sent: string[] = [];
    const conn = new JsonRpcConnection(async (b) => void sent.push(b));
    // workspace/configuration → one null per item.
    conn.handleMessage(
      JSON.stringify({
        jsonrpc: "2.0",
        id: 100,
        method: "workspace/configuration",
        params: { items: [{}, {}] },
      }),
    );
    expect(JSON.parse(sent[0])).toEqual({ jsonrpc: "2.0", id: 100, result: [null, null] });
    // Unknown request → null result (never left unanswered).
    conn.handleMessage(
      JSON.stringify({ jsonrpc: "2.0", id: 101, method: "client/registerCapability", params: {} }),
    );
    expect(JSON.parse(sent[1])).toEqual({ jsonrpc: "2.0", id: 101, result: null });
  });

  it("ignores malformed frames without throwing", () => {
    const conn = new JsonRpcConnection(async () => {});
    expect(() => conn.handleMessage("{not json")).not.toThrow();
  });
});
