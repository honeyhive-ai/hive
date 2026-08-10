// Byte-for-byte port of web/src/lib/avatar.ts.
//
// Avatar colour is NOT a theme token. It is a fixed seven-entry palette picked
// by a hash of the name, identical in every accent family and every scheme —
// and it is an identity signal, so the same person must get the same colour on
// the phone as on the desktop. Any drift in the hash, the multiplier, or the
// order of `AV_COLORS` silently re-colours people. Do not tidy this.

const AV_COLORS = ["#3f72a8", "#b5673a", "#5a8f6b", "#8a5a9e", "#c08438", "#4c8aa6", "#a85a6a"];

/// Deterministic colour from a name/handle — stable across sessions and devices.
export function avColor(seed: string): string {
  let h = 0;
  for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) >>> 0;
  return AV_COLORS[h % AV_COLORS.length];
}

/// Up to two letters, `@` stripped, hyphen/underscore = word break
/// ("@alice-claude" → "AC", "Hive" → "HI").
export function initials(name: string): string {
  const cleaned = name.replace(/^@/, "").replace(/[-_]/g, " ");
  const parts = cleaned.split(/\s+/).filter(Boolean);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return cleaned.slice(0, 2).toUpperCase();
}
