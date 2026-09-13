import type * as Monaco from "monaco-editor";

/// Register grammars Monaco doesn't ship built in. Idempotent — safe to call on
/// every editor mount. Currently just LaTeX (Monaco has no `latex`/`tex`
/// grammar), given a compact Monarch tokenizer: commands, comments, math
/// regions, and \begin/\end environment names.
let done = false;

export function registerCustomLanguages(monaco: typeof Monaco): void {
  if (done) return;
  done = true;
  if (monaco.languages.getLanguages().some((l) => l.id === "latex")) return;

  monaco.languages.register({
    id: "latex",
    extensions: [".tex", ".sty", ".cls", ".ltx"],
    aliases: ["LaTeX", "latex", "TeX"],
  });

  monaco.languages.setLanguageConfiguration("latex", {
    comments: { lineComment: "%" },
    brackets: [
      ["{", "}"],
      ["[", "]"],
      ["(", ")"],
    ],
    autoClosingPairs: [
      { open: "{", close: "}" },
      { open: "[", close: "]" },
      { open: "(", close: ")" },
      { open: "$", close: "$" },
    ],
    surroundingPairs: [
      { open: "{", close: "}" },
      { open: "[", close: "]" },
      { open: "(", close: ")" },
      { open: "$", close: "$" },
    ],
  });

  monaco.languages.setMonarchTokensProvider("latex", {
    defaultToken: "",
    tokenPostfix: ".latex",
    tokenizer: {
      root: [
        [/%.*$/, "comment"],
        // \begin{env} / \end{env} — highlight the environment name as a type.
        [
          /(\\(?:begin|end))(\s*)(\{)([^}]*)(\})/,
          ["keyword", "", "@brackets", "type", "@brackets"],
        ],
        [/\\[a-zA-Z@]+\*?/, "keyword"], // control words: \section, \textbf, …
        [/\\[^a-zA-Z@]/, "keyword"], // control symbols: \\, \{, \$, \[ …
        [/\$\$/, { token: "string", next: "@displaymath" }],
        [/\$/, { token: "string", next: "@math" }],
        [/#\d+/, "variable"], // macro parameters (#1)
        [/[{}[\]()]/, "@brackets"],
        [/&/, "keyword.operator"], // alignment tabs
      ],
      math: [
        [/%.*$/, "comment"],
        [/\$/, { token: "string", next: "@pop" }],
        [/\\[a-zA-Z@]+\*?/, "keyword"],
        [/\\[^a-zA-Z@]/, "keyword"],
        [/[{}[\]()]/, "@brackets"],
        [/[^$\\%]+/, "string"],
      ],
      displaymath: [
        [/%.*$/, "comment"],
        [/\$\$/, { token: "string", next: "@pop" }],
        [/\\[a-zA-Z@]+\*?/, "keyword"],
        [/\\[^a-zA-Z@]/, "keyword"],
        [/[{}[\]()]/, "@brackets"],
        [/[^$\\%]+/, "string"],
      ],
    },
  } as Monaco.languages.IMonarchLanguage);
}
