/// Composer attachment helpers. Attachments live in the message body as
/// `[Attached: <abs path>]` markers (matching the Rust side), so the transcript
/// can split them back out for display and agents read the path directly.

export function attachmentMarker(path: string): string {
  return `[Attached: ${path}]`;
}

/// A blob attachment reference embedded in a message body (shared over the relay).
/// Mirrors the Rust `BlobRef` marker in crates/hive-runtime/src/attachment_blob.rs.
export interface BlobAttachmentRef {
  id: string;
  name: string;
  size: number;
  contentType: string;
}

function parseBlobInner(inner: string): BlobAttachmentRef | null {
  // "id=<id> size=<n> type=<mime> name=<name...>" — name is last so it may have spaces.
  const nameAt = inner.indexOf("name=");
  if (nameAt < 0) return null;
  const name = inner.slice(nameAt + "name=".length).trim();
  let id = "";
  let size = 0;
  let contentType = "application/octet-stream";
  for (const tok of inner.slice(0, nameAt).trim().split(/\s+/)) {
    if (tok.startsWith("id=")) id = tok.slice(3);
    else if (tok.startsWith("size=")) size = parseInt(tok.slice(5), 10) || 0;
    else if (tok.startsWith("type=")) contentType = tok.slice(5);
  }
  return id ? { id, name, size, contentType } : null;
}

/// Split a body into display text, local path attachments, and relay blob
/// references. Blob markers are stripped first (more specific than the path
/// marker), then plain `[Attached: <path>]` markers.
export function splitAttachments(body: string): {
  text: string;
  paths: string[];
  blobs: BlobAttachmentRef[];
} {
  const paths: string[] = [];
  const blobs: BlobAttachmentRef[] = [];
  const text = body
    .replace(/\[Attached-blob: ([^\]]+)\]/g, (_m, inner: string) => {
      const r = parseBlobInner(inner);
      if (r) blobs.push(r);
      return "";
    })
    .replace(/\[Attached: ([^\]]+)\]/g, (_m, p: string) => {
      const trimmed = p.trim();
      if (trimmed) paths.push(trimmed);
      return "";
    })
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  return { text, paths, blobs };
}

export function isImageType(contentType: string, name: string): boolean {
  return contentType.startsWith("image/") || isImagePath(name);
}

export function fileBaseName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

export function isImagePath(path: string): boolean {
  return /\.(png|jpe?g|gif|webp|bmp|svg)$/i.test(path);
}
