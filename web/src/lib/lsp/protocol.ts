// Minimal LSP protocol types + JSON-RPC envelopes. Only the subset the adapter
// speaks is modelled; everything else is treated as opaque JSON. Positions here
// are LSP-native: 0-based line, 0-based UTF-16 character (we negotiate utf-16 so
// they map 1:1 onto Monaco columns minus one).

export interface Position {
  line: number;
  character: number;
}

export interface Range {
  start: Position;
  end: Position;
}

export interface LspLocation {
  uri: string;
  range: Range;
}

/** LSP `LocationLink` (returned by servers that advertise link support). */
export interface LocationLink {
  targetUri: string;
  targetRange: Range;
  targetSelectionRange: Range;
  originSelectionRange?: Range;
}

export interface TextEdit {
  range: Range;
  newText: string;
}

export interface MarkupContent {
  kind: "plaintext" | "markdown";
  value: string;
}

export interface Hover {
  contents: MarkupContent | string | Array<MarkupContent | string | { language: string; value: string }>;
  range?: Range;
}

export interface Diagnostic {
  range: Range;
  severity?: 1 | 2 | 3 | 4; // Error | Warning | Information | Hint
  code?: string | number;
  source?: string;
  message: string;
  tags?: number[];
}

export interface PublishDiagnosticsParams {
  uri: string;
  version?: number;
  diagnostics: Diagnostic[];
}

export interface CompletionItem {
  label: string;
  kind?: number;
  detail?: string;
  documentation?: string | MarkupContent;
  sortText?: string;
  filterText?: string;
  insertText?: string;
  insertTextFormat?: 1 | 2; // PlainText | Snippet
  textEdit?: TextEdit | { range: Range; insert?: Range; replace?: Range; newText: string };
  additionalTextEdits?: TextEdit[];
  command?: unknown;
  data?: unknown;
  preselect?: boolean;
  deprecated?: boolean;
  tags?: number[];
}

export interface CompletionList {
  isIncomplete: boolean;
  items: CompletionItem[];
}

export interface SignatureInformation {
  label: string;
  documentation?: string | MarkupContent;
  parameters?: Array<{ label: string | [number, number]; documentation?: string | MarkupContent }>;
  activeParameter?: number;
}

export interface SignatureHelp {
  signatures: SignatureInformation[];
  activeSignature?: number;
  activeParameter?: number;
}

export interface WorkspaceEdit {
  changes?: Record<string, TextEdit[]>;
  documentChanges?: Array<{ textDocument: { uri: string; version?: number | null }; edits: TextEdit[] }>;
}

/** The slice of the server's `initialize` result the adapter consults. */
export interface ServerCapabilities {
  completionProvider?: { resolveProvider?: boolean; triggerCharacters?: string[] } | boolean;
  hoverProvider?: boolean | object;
  definitionProvider?: boolean | object;
  referencesProvider?: boolean | object;
  renameProvider?: boolean | { prepareProvider?: boolean };
  documentFormattingProvider?: boolean | object;
  signatureHelpProvider?: { triggerCharacters?: string[] };
  [key: string]: unknown;
}

// --- JSON-RPC envelopes -----------------------------------------------------

export interface RpcRequest {
  jsonrpc: "2.0";
  id: number | string;
  method: string;
  params?: unknown;
}
export interface RpcNotification {
  jsonrpc: "2.0";
  method: string;
  params?: unknown;
}
export interface RpcResponse {
  jsonrpc: "2.0";
  id: number | string;
  result?: unknown;
  error?: { code: number; message: string; data?: unknown };
}

export type RpcMessage = RpcRequest | RpcNotification | RpcResponse;

export function isRequest(m: RpcMessage): m is RpcRequest {
  return "id" in m && "method" in m && (m as RpcRequest).id !== undefined;
}
export function isResponse(m: RpcMessage): m is RpcResponse {
  return "id" in m && !("method" in m) && (m as RpcResponse).id !== undefined;
}
export function isNotification(m: RpcMessage): m is RpcNotification {
  return !("id" in m) && "method" in m;
}
