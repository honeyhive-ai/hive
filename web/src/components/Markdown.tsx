import { memo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkBreaks from "remark-breaks";
import remarkGfm from "remark-gfm";

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
          a: ({ href, children }) => (
            <a
              href={href}
              // Never let a link navigate the app's own webview away.
              onClick={(e) => {
                e.preventDefault();
                if (href) window.open(href, "_blank", "noopener,noreferrer");
              }}
              className="underline decoration-dotted underline-offset-2"
              style={{ color: "var(--hive-accent-cool)" }}
            >
              {children}
            </a>
          ),
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

/// The text the Copy button puts on the clipboard for a `<pre>`.
///
/// Reads `textContent`, not `innerText`, because jsdom implements no layout and
/// returns `undefined` for `innerText` — a test that clicks Copy and asserts on
/// the clipboard passed vacuously, whatever the markup rendered to.
///
/// Inside a `<pre>` the two properties agree on everything that matters here:
/// `white-space: pre` means nothing collapses, and the block has no hidden
/// children for `innerText` to skip. They diverge on exactly one thing — the
/// trailing newline every fenced block carries. `innerText` strips trailing LFs
/// per spec; `textContent` keeps them. Strip it, or copying a shell command and
/// pasting it into a terminal would run it instead of leaving it on the prompt.
export function codeBlockText(el: HTMLElement | null): string {
  return (el?.textContent ?? "").replace(/\n+$/, "");
}

function CodeBlock({ children }: { children: React.ReactNode }) {
  const ref = useRef<HTMLPreElement>(null);
  const [copied, setCopied] = useState(false);
  return (
    <div className="group/code relative my-2">
      <button
        className="absolute right-2 top-2 rounded-lg px-2 py-0.5 text-xs opacity-0 transition-opacity group-hover/code:opacity-80 hover:!opacity-100"
        style={{
          background: "var(--hive-panel)",
          border: "1px solid var(--hive-line)",
          color: copied ? "var(--hive-success)" : undefined,
        }}
        onClick={() => {
          void navigator.clipboard.writeText(codeBlockText(ref.current));
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1500);
        }}
        title="Copy code"
        aria-label="Copy code"
      >
        {copied ? "Copied ✓" : "Copy"}
      </button>
      <pre
        ref={ref}
        className="overflow-x-auto rounded-xl p-3 text-[0.85rem] leading-6"
        style={{ background: "var(--hive-overlay)", border: "1px solid var(--hive-line)" }}
      >
        {children}
      </pre>
    </div>
  );
}
