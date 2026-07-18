/// Composer attachment helpers. Attachments live in the message body as
/// `[Attached: <abs path>]` markers (matching the Rust side), so the transcript
/// can split them back out for display and agents read the path directly.

export function attachmentMarker(path: string): string {
  return `[Attached: ${path}]`;
}

/// Split a body into display text + the attached paths it references.
export function splitAttachments(body: string): { text: string; paths: string[] } {
  const paths: string[] = [];
  const text = body
    .replace(/\[Attached: ([^\]]+)\]/g, (_m, p: string) => {
      const trimmed = p.trim();
      if (trimmed) paths.push(trimmed);
      return "";
    })
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  return { text, paths };
}

export function fileBaseName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

export function isImagePath(path: string): boolean {
  return /\.(png|jpe?g|gif|webp|bmp|svg)$/i.test(path);
}
