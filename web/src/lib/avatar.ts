// Shared avatar helpers. A user/agent avatar is either an uploaded image
// (`data:` URL, synced) or, when absent, deterministic initials on a stable
// color. Kept in one place so every render site (transcript, people list,
// sidebar) agrees.

const AV_COLORS = ["#3f72a8", "#b5673a", "#5a8f6b", "#8a5a9e", "#c08438", "#4c8aa6", "#a85a6a"];

/// Deterministic color from a name/handle — stable across sessions and devices.
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

/// Resize/crop an image File to a small square `data:` URL fit to ride the
/// synced event log. Center-crops to a square, scales to `size` px, and
/// re-encodes as JPEG under the size budget (the backend hard-caps at ~96 KB).
export async function fileToAvatarDataUrl(file: File, size = 128): Promise<string> {
  const bitmap = await createImageBitmap(file);
  const side = Math.min(bitmap.width, bitmap.height);
  const sx = (bitmap.width - side) / 2;
  const sy = (bitmap.height - side) / 2;
  const canvas = document.createElement("canvas");
  canvas.width = size;
  canvas.height = size;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("canvas unsupported");
  ctx.drawImage(bitmap, sx, sy, side, side, 0, 0, size, size);
  bitmap.close?.();
  // Step quality down until it fits comfortably under the backend cap.
  for (const q of [0.85, 0.7, 0.55, 0.4]) {
    const url = canvas.toDataURL("image/jpeg", q);
    if (url.length <= 90 * 1024) return url;
  }
  // Last resort: a smaller square.
  return fileToAvatarDataUrl(file, 96);
}
