// Split a unified diff into per-file sections so the Review card can group,
// collapse, and horizontally scroll each file independently — instead of one
// flat concatenated block. Deliberately tolerant: unknown/preamble lines before
// the first file header attach to a synthetic leading section.

export interface DiffFile {
  /// Best-effort path (from the `+++ b/…` or `diff --git` header).
  path: string;
  /// The raw lines of this file's section (headers + hunks), in order.
  lines: string[];
  added: number;
  removed: number;
}

function pathFromHeader(line: string): string | null {
  // `diff --git a/x b/x`
  let m = /^diff --git a\/(.+?) b\/(.+)$/.exec(line);
  if (m) return m[2];
  // `+++ b/x` (fallback when there's no `diff --git` line)
  m = /^\+\+\+ b\/(.+)$/.exec(line);
  if (m) return m[1];
  m = /^\+\+\+ (.+)$/.exec(line);
  if (m && m[1] !== "/dev/null") return m[1].replace(/^b\//, "");
  return null;
}

export function parseUnifiedDiff(diff: string): DiffFile[] {
  const files: DiffFile[] = [];
  let cur: DiffFile | null = null;
  const push = () => {
    if (cur) files.push(cur);
  };
  for (const line of diff.split("\n")) {
    if (line.startsWith("diff --git ")) {
      push();
      cur = { path: pathFromHeader(line) ?? "file", lines: [], added: 0, removed: 0 };
    } else if (!cur) {
      // Preamble before any header (or a diff that starts straight at ---/+++).
      cur = { path: "", lines: [], added: 0, removed: 0 };
    }
    if (cur) {
      if (!cur.path) {
        const p = pathFromHeader(line);
        if (p) cur.path = p;
      }
      if (line.startsWith("+") && !line.startsWith("+++")) cur.added++;
      else if (line.startsWith("-") && !line.startsWith("---")) cur.removed++;
      cur.lines.push(line);
    }
  }
  push();
  // A diff with no `diff --git` header still yields one section; label it.
  return files.map((f) => ({ ...f, path: f.path || "changes" }));
}
