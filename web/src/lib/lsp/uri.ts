// File-URI helpers. The editor tracks files by repo-relative path and its Monaco
// models carry auto-generated (inmemory:) uris, so the adapter derives its own
// `file://` document uris from the workspace root for the LSP wire, and keeps a
// normalized index to map server-returned uris back to open models.

/** Turn an absolute OS path into a `file://` uri (handles Windows drives). */
export function pathToFileUri(absPath: string): string {
  let p = absPath.replace(/\\/g, "/");
  // Windows drive path "C:/foo" → "/C:/foo".
  if (/^[a-zA-Z]:/.test(p)) p = "/" + p;
  if (!p.startsWith("/")) p = "/" + p;
  // Encode each segment but keep the separators.
  const encoded = p
    .split("/")
    .map((seg) => encodeURIComponent(seg))
    .join("/");
  return "file://" + encoded;
}

/** Join a root `file://` uri with a repo-relative path. */
export function joinFileUri(rootUri: string, relPath: string): string {
  const root = rootUri.replace(/\/+$/, "");
  const rel = relPath.replace(/\\/g, "/").replace(/^\/+/, "");
  const encoded = rel
    .split("/")
    .map((seg) => encodeURIComponent(seg))
    .join("/");
  return root + "/" + encoded;
}

/** Canonical key for comparing uris that may differ only in percent-encoding. */
export function normalizeUri(uri: string): string {
  try {
    const u = new URL(uri);
    if (u.protocol === "file:") {
      return "file://" + decodeURIComponent(u.pathname);
    }
    return uri;
  } catch {
    try {
      return decodeURIComponent(uri);
    } catch {
      return uri;
    }
  }
}
