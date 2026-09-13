// Translation between Monaco's model space and LSP. We negotiate the utf-16
// position encoding at `initialize`, so Monaco columns (1-based UTF-16 code
// units) map onto LSP characters (0-based UTF-16 code units) by a simple ∓1 —
// no re-encoding needed. Line numbers differ only by the 1-based ⇄ 0-based base.

import type * as Monaco from "monaco-editor";
import type {
  Position as LspPosition,
  Range as LspRange,
  CompletionItem,
  MarkupContent,
  Diagnostic,
} from "./protocol";

export function toLspPosition(pos: Monaco.IPosition): LspPosition {
  return { line: pos.lineNumber - 1, character: pos.column - 1 };
}

export function fromLspPosition(pos: LspPosition): Monaco.IPosition {
  return { lineNumber: pos.line + 1, column: pos.character + 1 };
}

export function fromLspRange(monaco: typeof Monaco, range: LspRange): Monaco.IRange {
  return new monaco.Range(
    range.start.line + 1,
    range.start.character + 1,
    range.end.line + 1,
    range.end.character + 1,
  );
}

export function toLspRange(range: Monaco.IRange): LspRange {
  return {
    start: { line: range.startLineNumber - 1, character: range.startColumn - 1 },
    end: { line: range.endLineNumber - 1, character: range.endColumn - 1 },
  };
}

/** LSP CompletionItemKind → Monaco CompletionItemKind. */
export function completionKind(
  monaco: typeof Monaco,
  kind: number | undefined,
): Monaco.languages.CompletionItemKind {
  const K = monaco.languages.CompletionItemKind;
  // LSP kinds are 1-based (Text=1 … TypeParameter=25).
  const map: Record<number, Monaco.languages.CompletionItemKind> = {
    1: K.Text,
    2: K.Method,
    3: K.Function,
    4: K.Constructor,
    5: K.Field,
    6: K.Variable,
    7: K.Class,
    8: K.Interface,
    9: K.Module,
    10: K.Property,
    11: K.Unit,
    12: K.Value,
    13: K.Enum,
    14: K.Keyword,
    15: K.Snippet,
    16: K.Color,
    17: K.File,
    18: K.Reference,
    19: K.Folder,
    20: K.EnumMember,
    21: K.Constant,
    22: K.Struct,
    23: K.Event,
    24: K.Operator,
    25: K.TypeParameter,
  };
  return (kind && map[kind]) || K.Property;
}

/** LSP DiagnosticSeverity (1..4) → Monaco MarkerSeverity. */
export function markerSeverity(
  monaco: typeof Monaco,
  severity: number | undefined,
): Monaco.MarkerSeverity {
  const S = monaco.MarkerSeverity;
  switch (severity) {
    case 1:
      return S.Error;
    case 2:
      return S.Warning;
    case 3:
      return S.Info;
    case 4:
      return S.Hint;
    default:
      return S.Error;
  }
}

export function diagnosticToMarker(
  monaco: typeof Monaco,
  d: Diagnostic,
): Monaco.editor.IMarkerData {
  const r = fromLspRange(monaco, d.range);
  return {
    severity: markerSeverity(monaco, d.severity),
    message: d.message,
    startLineNumber: r.startLineNumber,
    startColumn: r.startColumn,
    endLineNumber: r.endLineNumber,
    endColumn: r.endColumn,
    code: d.code === undefined ? undefined : String(d.code),
    source: d.source,
  };
}

/** Normalise documentation (string or MarkupContent) into a Monaco markdown string. */
export function docToMarkdown(
  doc: string | MarkupContent | undefined,
): Monaco.IMarkdownString | string | undefined {
  if (doc === undefined) return undefined;
  if (typeof doc === "string") return doc;
  return { value: doc.value };
}

/** Extract the plain insert text for a completion item (snippet or literal). */
export function completionInsertText(item: CompletionItem): {
  insertText: string;
  isSnippet: boolean;
} {
  const isSnippet = item.insertTextFormat === 2;
  if (item.textEdit && "newText" in item.textEdit) {
    return { insertText: item.textEdit.newText, isSnippet };
  }
  return { insertText: item.insertText ?? item.label, isSnippet };
}
