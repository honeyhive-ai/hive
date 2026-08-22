// Lazy syntax highlighting for chat/diff code blocks. highlight.js/lib/common
// (~35 common languages) is imported dynamically so it lands in its own chunk
// and never bloats the initial bundle — the block renders plain first, then
// upgrades to highlighted markup once the module resolves.
import type { HLJSApi } from "highlight.js";

let hljsPromise: Promise<HLJSApi> | null = null;

function loadHljs(): Promise<HLJSApi> {
  if (!hljsPromise) {
    hljsPromise = import("highlight.js/lib/common").then((m) => m.default);
  }
  return hljsPromise;
}

export interface Highlighted {
  /// HTML with highlight.js token spans (styled by the hljs theme in styles.css).
  html: string;
  /// The language actually used (given hint, or auto-detected), for the header.
  language: string;
}

/// Highlight `code`. If `lang` is a known language it's used directly; otherwise
/// highlight.js auto-detects. Resolves to null if the module can't load, so the
/// caller keeps its plain-text fallback.
export async function highlightCode(code: string, lang?: string): Promise<Highlighted | null> {
  try {
    const hljs = await loadHljs();
    if (lang && hljs.getLanguage(lang)) {
      const r = hljs.highlight(code, { language: lang, ignoreIllegals: true });
      return { html: r.value, language: lang };
    }
    const r = hljs.highlightAuto(code);
    return { html: r.value, language: r.language ?? "" };
  } catch {
    return null;
  }
}
