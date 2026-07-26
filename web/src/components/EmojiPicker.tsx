import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";

// Full offline emoji picker for message reactions. The ~1900-emoji dataset
// (unicode-emoji-json) is loaded via dynamic import so it lands in its own
// chunk and only downloads the first time someone opens the picker — the main
// bundle stays lean. Emojis are opaque strings end-to-end (see toggle_reaction),
// so nothing here is special-cased per emoji.

type EmojiItem = { emoji: string; name: string; slug: string };
type EmojiGroup = { name: string; slug: string; emojis: EmojiItem[] };

const RECENT_KEY = "hive.emoji.recent";
const RECENT_SEED = ["👍", "👎", "🎉", "👀", "❤️"];
const RECENT_MAX = 24;

function loadRecent(): string[] {
  try {
    const raw = localStorage.getItem(RECENT_KEY);
    if (raw) return JSON.parse(raw) as string[];
  } catch {
    /* corrupt/absent — fall through to the seed */
  }
  return RECENT_SEED;
}

function pushRecent(emoji: string): string[] {
  const next = [emoji, ...loadRecent().filter((e) => e !== emoji)].slice(0, RECENT_MAX);
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(next));
  } catch {
    /* storage full/blocked — recents are best-effort */
  }
  return next;
}

export function EmojiPicker({
  onPick,
  align = "left",
}: {
  onPick: (emoji: string) => void;
  align?: "left" | "right";
}) {
  const [groups, setGroups] = useState<EmojiGroup[] | null>(null);
  const [query, setQuery] = useState("");
  const [recent, setRecent] = useState<string[]>(loadRecent);
  const [dir, setDir] = useState<"up" | "down">("up");
  const inputRef = useRef<HTMLInputElement>(null);
  const rootRef = useRef<HTMLDivElement>(null);

  // Default to opening upward (reactions usually happen near the bottom of the
  // log); flip downward when the panel would clip the top of the viewport
  // (message near the top). Re-measured when the dataset loads and grows it.
  useLayoutEffect(() => {
    const r = rootRef.current?.getBoundingClientRect();
    if (r && r.top < 8) setDir("down");
  }, [groups]);

  useEffect(() => {
    let alive = true;
    import("unicode-emoji-json/data-by-group.json")
      .then((m) => {
        if (alive) setGroups((m.default ?? m) as unknown as EmojiGroup[]);
      })
      .catch(() => {
        if (alive) setGroups([]);
      });
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const pick = (emoji: string) => {
    setRecent(pushRecent(emoji));
    onPick(emoji);
  };

  // Search matches emoji name + slug ("fire", "grinning", "heart"). Capped so a
  // one-letter query doesn't try to render all 1900 at once.
  const results = useMemo(() => {
    if (!groups) return null;
    const q = query.trim().toLowerCase();
    if (!q) return null;
    const seen = new Set<string>();
    const out: EmojiItem[] = [];
    for (const g of groups) {
      for (const e of g.emojis) {
        if ((e.name.includes(q) || e.slug.includes(q)) && !seen.has(e.emoji)) {
          seen.add(e.emoji);
          out.push(e);
          if (out.length >= 240) return out;
        }
      }
    }
    return out;
  }, [groups, query]);

  return (
    <div
      ref={rootRef}
      className={`absolute z-30 flex w-[288px] flex-col rounded-xl border shadow-xl ${
        dir === "up" ? "bottom-full mb-1" : "top-full mt-1"
      }`}
      style={{
        borderColor: "var(--hive-line)",
        background: "var(--hive-panel)",
        ...(align === "right" ? { right: 0 } : { left: 0 }),
      }}
    >
      <div className="p-2">
        <input
          ref={inputRef}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search emoji"
          className="w-full rounded-lg border px-2.5 py-1.5 text-sm outline-none"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)", color: "var(--hive-ink)" }}
        />
      </div>

      <div className="max-h-[260px] overflow-y-auto px-2 pb-2">
        {groups === null && <div className="p-3 text-center text-xs opacity-50">Loading…</div>}

        {results !== null &&
          (results.length === 0 ? (
            <div className="p-3 text-center text-xs opacity-50">No emoji found</div>
          ) : (
            <Grid items={results} onPick={pick} />
          ))}

        {results === null && groups && (
          <>
            {recent.length > 0 && (
              <Section title="Frequently used">
                <Grid items={recent.map((e) => ({ emoji: e, name: e, slug: e }))} onPick={pick} />
              </Section>
            )}
            {groups.map((g) => (
              <Section key={g.slug} title={g.name}>
                <Grid items={g.emojis} onPick={pick} />
              </Section>
            ))}
          </>
        )}
      </div>
    </div>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="mt-1">
      <div
        className="sticky top-0 z-10 px-1 py-1 text-[11px] font-semibold uppercase tracking-wide"
        style={{ color: "var(--hive-ink-soft)", background: "var(--hive-panel)" }}
      >
        {title}
      </div>
      {children}
    </div>
  );
}

function Grid({ items, onPick }: { items: { emoji: string; name: string }[]; onPick: (e: string) => void }) {
  return (
    <div className="grid grid-cols-8 gap-0.5">
      {items.map((it, i) => (
        <button
          key={it.emoji + i}
          title={it.name}
          aria-label={it.name}
          onClick={() => onPick(it.emoji)}
          className="flex h-8 w-8 items-center justify-center rounded-md text-xl leading-none transition-transform hover:scale-110"
          onMouseEnter={(e) => (e.currentTarget.style.background = "var(--hive-hover)")}
          onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
        >
          {it.emoji}
        </button>
      ))}
    </div>
  );
}
