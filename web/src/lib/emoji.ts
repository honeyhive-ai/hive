/// Shared emoji index for `:shortcode:` autocomplete in the composer and the
/// reaction picker. The ~1900-emoji dataset (unicode-emoji-json) is loaded once,
/// lazily (dynamic import → its own chunk), and cached. Names/slugs are the
/// match keys (e.g. `:fire` → 🔥, `:heart` → ❤️).

export type EmojiEntry = { emoji: string; name: string; slug: string };

let cache: EmojiEntry[] | null = null;
let loading: Promise<EmojiEntry[]> | null = null;

export function loadEmojiIndex(): Promise<EmojiEntry[]> {
  if (cache) return Promise.resolve(cache);
  if (loading) return loading;
  loading = import("unicode-emoji-json/data-by-group.json")
    .then((m) => {
      const groups = ((m as { default?: unknown }).default ?? m) as { emojis: EmojiEntry[] }[];
      cache = groups.flatMap((g) => g.emojis ?? []);
      return cache;
    })
    .catch(() => {
      cache = [];
      return cache;
    });
  return loading;
}

/// Rank matches: slug/name prefix hits first, then substring hits. Capped.
export function searchEmoji(index: EmojiEntry[], query: string, limit = 8): EmojiEntry[] {
  const q = query.trim().toLowerCase();
  if (!q) return [];
  const starts: EmojiEntry[] = [];
  const contains: EmojiEntry[] = [];
  for (const e of index) {
    if (e.slug.startsWith(q) || e.name.startsWith(q)) starts.push(e);
    else if (e.slug.includes(q) || e.name.includes(q)) contains.push(e);
    if (starts.length >= limit) break;
  }
  return [...starts, ...contains].slice(0, limit);
}

/// Exact shortcode → emoji (for converting a completed `:fire:` on the closing
/// colon). Matches the slug exactly, else a unique name match.
export function exactEmoji(index: EmojiEntry[], word: string): string | null {
  const q = word.trim().toLowerCase();
  if (!q) return null;
  const bySlug = index.find((e) => e.slug === q);
  if (bySlug) return bySlug.emoji;
  const byName = index.find((e) => e.name === q);
  return byName ? byName.emoji : null;
}
