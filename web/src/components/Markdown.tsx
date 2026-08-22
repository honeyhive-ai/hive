import { memo, isValidElement, useEffect, useState, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkBreaks from "remark-breaks";
import remarkGfm from "remark-gfm";
import { highlightCode } from "@/lib/highlight";

/// Pull the fence language ("language-ts" → "ts") off the inner <code> element
/// that react-markdown puts inside <pre>, so the block can label + highlight it.
function codeLanguage(children: ReactNode): string {
  const el = Array.isArray(children) ? children[0] : children;
  if (isValidElement(el)) {
    const cls = (el.props as { className?: string }).className ?? "";
    const m = /language-([\w-]+)/.exec(cls);
    if (m) return m[1];
  }
  return "";
}

/// Recursively collect the text of a React node tree — used to copy a fenced code
/// block's SOURCE (what the user actually wants) rather than reading the rendered
/// DOM's `innerText` (which jsdom doesn't implement, so tests passed vacuously, and
/// which is layout-normalized rather than the literal source). #97
export function nodeText(node: ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(nodeText).join("");
  if (isValidElement(node)) {
    return nodeText((node.props as { children?: ReactNode }).children);
  }
  return "";
}

/// Only open links with a safe scheme. react-markdown's default urlTransform
/// already neutralizes `javascript:`/`data:` hrefs, but this is defense-in-depth
/// so a future config change (adding rehype-raw or a custom urlTransform) can't
/// turn a synced/agent-authored link into a code-execution or exfil vector.
const SAFE_LINK = /^(https?:|mailto:)/i;
function safeHref(href: string | undefined): string | null {
  const h = href?.trim();
  // Require an explicit safe scheme. Anything else — javascript:/data:/vbscript:,
  // scheme-relative, or bare relative — is not opened.
  return h && SAFE_LINK.test(h) ? h : null;
}

/// GitHub-flavored markdown for chat messages. Memoized on `content` so it only
/// re-parses when the text actually changes — the transcript stays cheap during
/// typing/streaming (streaming bubbles render plain text until complete).
///
/// `remark-breaks` keeps single newlines as line breaks. Runtime output (Claude
/// Code especially) uses bare newlines for meaningful line structure, and plain
/// markdown would reflow those into one run-on paragraph — which also made text
/// visibly re-wrap the moment a streaming bubble (rendered `whitespace-pre-wrap`)
/// retired into its persisted copy.
export const Markdown = memo(function Markdown({ content }: { content: string }) {
  return (
    <div className="hive-md text-[0.95rem] leading-7">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkBreaks]}
        components={{
          a: ({ href, children }) => {
            const safe = safeHref(href);
            return (
              <a
                href={safe ?? undefined}
                // Never let a link navigate the app's own webview away; only open
                // links that pass the scheme allowlist in a new window.
                onClick={(e) => {
                  e.preventDefault();
                  if (safe) window.open(safe, "_blank", "noopener,noreferrer");
                }}
                className="underline decoration-dotted underline-offset-2"
                style={{ color: "var(--hive-accent-cool)" }}
              >
                {children}
              </a>
            );
          },
          pre: ({ children }) => <CodeBlock>{children}</CodeBlock>,
          code: ({ className, children }) => (
            // Inline code (block code is handled by the `pre` wrapper above).
            <code
              className={className}
              style={{
                background: "color-mix(in srgb, var(--hive-ink) 18%, transparent)",
                borderRadius: "4px",
                padding: "0.1em 0.35em",
                fontSize: "0.9em",
              }}
            >
              {children}
            </code>
          ),
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
});

function CodeBlock({ children }: { children: ReactNode }) {
  const [copied, setCopied] = useState(false);
  // Copy the block's source text (derived from the node tree), not the rendered
  // DOM — testable and faithful to what was written. #97
  const source = nodeText(children);
  const hint = codeLanguage(children);

  // Lazily syntax-highlight. Renders plain first, then upgrades once
  // highlight.js resolves — so a huge log or a streaming block never blocks.
  const [hl, setHl] = useState<{ html: string; language: string } | null>(null);
  useEffect(() => {
    let alive = true;
    if (!source.trim()) return;
    void highlightCode(source, hint || undefined).then((r) => {
      if (alive && r) setHl(r);
    });
    return () => {
      alive = false;
    };
  }, [source, hint]);

  const language = hl?.language || hint;

  return (
    <div className="group/code relative my-2 overflow-hidden rounded-xl border" style={{ borderColor: "var(--hive-line)" }}>
      {/* Header bar: language label (left) + Copy (right). */}
      <div
        className="flex items-center justify-between px-3 py-1 text-[11px]"
        style={{ background: "var(--hive-panel)", borderBottom: "1px solid var(--hive-line)", color: "var(--hive-ink-soft)" }}
      >
        <span className="font-mono lowercase">{language || "text"}</span>
        <button
          className="rounded-md px-1.5 py-0.5 opacity-0 transition-opacity group-hover/code:opacity-80 group-focus-within/code:opacity-80 focus-visible:!opacity-100 hover:!opacity-100"
          style={{ color: copied ? "var(--hive-success)" : undefined }}
          onClick={() => {
            void navigator.clipboard.writeText(source);
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1500);
          }}
          title="Copy code"
          aria-label="Copy code"
        >
          {copied ? "Copied ✓" : "Copy"}
        </button>
      </div>
      <pre
        className="hljs overflow-x-auto p-3 text-[0.85rem] leading-6"
        style={{ background: "var(--hive-overlay)" }}
      >
        {hl ? (
          <code className={`hljs language-${language}`} dangerouslySetInnerHTML={{ __html: hl.html }} />
        ) : (
          children
        )}
      </pre>
    </div>
  );
}
