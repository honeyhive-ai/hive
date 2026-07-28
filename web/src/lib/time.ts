// Timestamp helpers shared across the chat surfaces.
//
// Hive's own messages are always UTC RFC 3339 with a `Z` suffix, but a stray
// timestamp from seed data, a migration, or an older/non-Hive client can arrive
// *without* a timezone designator. JS parses such a string as LOCAL time, which
// at a +8/9h offset renders a reply as ~8h earlier than the message it answers
// (time appears to run backwards). Normalizing a designator-less value to UTC
// before parsing makes that impossible.

/// Append `Z` to a timestamp that carries no timezone designator (no trailing
/// `Z` and no `±hh:mm` offset), so `new Date` treats it as UTC, not local.
export function normalizeTimestamp(iso: string): string {
  return /(?:Z|[+-]\d{2}:\d{2})$/.test(iso) ? iso : `${iso}Z`;
}

/// Parse a (possibly designator-less) timestamp to a Date, or null if invalid.
export function parseTimestamp(iso: string | undefined | null): Date | null {
  if (!iso) return null;
  const d = new Date(normalizeTimestamp(iso));
  return Number.isNaN(d.getTime()) ? null : d;
}

/// Compact relative age ("now", "12m", "2h", "3d", …) — the in-thread label.
export function relTime(iso: string): string {
  const d = parseTimestamp(iso);
  if (!d) return "";
  const s = Math.floor((Date.now() - d.getTime()) / 1000);
  if (s < 60) return "now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  const day = Math.floor(h / 24);
  if (day < 7) return `${day}d`;
  const w = Math.floor(day / 7);
  if (w < 5) return `${w}w`;
  const mo = Math.floor(day / 30);
  if (mo < 12) return `${mo}mo`;
  return `${Math.floor(day / 365)}y`;
}

/// Absolute local date+time, for the hover tooltip on a relative label.
export function absTime(iso: string): string {
  const d = parseTimestamp(iso);
  return d ? d.toLocaleString() : "";
}
