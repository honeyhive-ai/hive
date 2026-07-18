import { memo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

/// GitHub-flavored markdown for chat messages. Memoized on `content` so it only
/// re-parses when the text actually changes — the transcript stays cheap during
/// typing/streaming (streaming bubbles render plain text until complete).
export const Markdown = memo(function Markdown({ content }: { content: string }) {
  return (
    <div className="hive-md text-[0.95rem] leading-7">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
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
                background: "rgba(127,127,127,0.18)",
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
          const text = ref.current?.innerText ?? "";
          void navigator.clipboard.writeText(text);
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
