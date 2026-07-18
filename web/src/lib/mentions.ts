/// Composer @-mention helpers, factored out of ChatView so they're unit-testable.

export interface MentionCandidate {
  handle: string;
  kind: string;
}

/// Detect an `@token` immediately before the caret. The token is a single word
/// (`[\w-]*`) that must start at the beginning of input or after whitespace, so
/// `email@x` doesn't trigger it. Returns the `@`'s index and the partial query.
export function detectMention(value: string, caret: number): { start: number; query: string } | null {
  const before = value.slice(0, caret);
  const m = /(^|\s)@([\w-]*)$/.exec(before);
  if (!m) return null;
  return { start: caret - m[2].length - 1, query: m[2] };
}

/// Candidates whose handle starts with the (case-insensitive) query.
export function filterMentions(
  cands: MentionCandidate[],
  query: string,
  limit = 8,
): MentionCandidate[] {
  const q = query.toLowerCase();
  return cands.filter((c) => c.handle.toLowerCase().startsWith(q)).slice(0, limit);
}
