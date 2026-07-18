/// Slash-command detection for the composer. A command is active when the
/// input begins with `/` and the caret is still within the first token (before
/// any space) — mirroring how `@`-mentions work. Pure + unit-tested.
export function detectSlash(value: string, caret: number): { query: string } | null {
  if (!value.startsWith("/")) return null;
  const firstSpace = value.indexOf(" ");
  // Only active while the caret is inside the leading `/token`.
  if (firstSpace !== -1 && caret > firstSpace) return null;
  const end = firstSpace === -1 ? value.length : firstSpace;
  return { query: value.slice(1, end) };
}
