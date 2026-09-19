//! E2EE file-attachment blobs over the relay.
//!
//! An attachment is sealed with the workspace key (`seal_symmetric`) and framed
//! self-describingly, then uploaded to the relay's blob channel (an
//! enterprise-relay capability). The message that syncs carries only a
//! `[Attached-blob: …]` reference — never a local path — so every member can
//! fetch + open the file. The relay stores ciphertext; only key-holders decrypt.
//!
//! Frame layout (little-endian): `[epoch: u32][nonce: 12 bytes][ciphertext]`.
//! Self-describing so a downloader needs only the workspace key.

use hive_core::e2ee::{open_symmetric, seal_symmetric, SealedEnvelope};

use crate::relay_client::{RelayClient, RelayError};

const NONCE_LEN: usize = 12;
const HEADER_LEN: usize = 4 + NONCE_LEN;

/// A reference to an uploaded attachment blob, embedded in a message body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRef {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub content_type: String,
}

impl BlobRef {
    /// The message-body marker. `name` is placed LAST so it may contain spaces —
    /// everything after `name=` up to `]` is the filename (id/size/type are
    /// token-safe). Mirrored in `web/src/lib/attachments.ts`.
    pub fn marker(&self) -> String {
        format!(
            "[Attached-blob: id={} size={} type={} name={}]",
            self.id, self.size, self.content_type, self.name
        )
    }
}

/// Parse every `[Attached-blob: …]` reference out of a message body.
pub fn parse_markers(body: &str) -> Vec<BlobRef> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("[Attached-blob:") {
        let after = &rest[start + "[Attached-blob:".len()..];
        let Some(end) = after.find(']') else { break };
        let inner = after[..end].trim();
        if let Some(r) = parse_one(inner) {
            out.push(r);
        }
        rest = &after[end + 1..];
    }
    out
}

fn parse_one(inner: &str) -> Option<BlobRef> {
    // inner = "id=<id> size=<n> type=<mime> name=<name...>"
    let name_at = inner.find("name=")?;
    let name = inner[name_at + "name=".len()..].trim().to_string();
    let head = &inner[..name_at];
    let mut id = String::new();
    let mut size = 0u64;
    let mut content_type = "application/octet-stream".to_string();
    for tok in head.split_whitespace() {
        if let Some(v) = tok.strip_prefix("id=") {
            id = v.to_string();
        } else if let Some(v) = tok.strip_prefix("size=") {
            size = v.parse().unwrap_or(0);
        } else if let Some(v) = tok.strip_prefix("type=") {
            content_type = v.to_string();
        }
    }
    if id.is_empty() {
        return None;
    }
    Some(BlobRef { id, name, size, content_type })
}

/// Seal + frame file bytes for upload. `epoch` is the workspace-key epoch (0 for
/// the base/passphrase key, which every member holds).
pub fn seal_framed(key: &[u8; 32], epoch: u32, plaintext: &[u8]) -> Result<Vec<u8>, RelayError> {
    let sealed = seal_symmetric(key, epoch, plaintext)
        .map_err(|e| RelayError::Message(format!("seal attachment: {e:?}")))?;
    let mut out = Vec::with_capacity(HEADER_LEN + sealed.ciphertext.len());
    out.extend_from_slice(&sealed.version.to_le_bytes());
    out.extend_from_slice(&sealed.nonce);
    out.extend_from_slice(&sealed.ciphertext);
    Ok(out)
}

/// Unframe + open a downloaded blob. Returns None if the frame is malformed or the
/// key can't open it (wrong workspace key / corrupted).
pub fn open_framed(key: &[u8; 32], framed: &[u8]) -> Option<Vec<u8>> {
    if framed.len() < HEADER_LEN {
        return None;
    }
    let version = u32::from_le_bytes(framed[0..4].try_into().ok()?);
    let sealed = SealedEnvelope {
        version,
        nonce: framed[4..HEADER_LEN].to_vec(),
        ciphertext: framed[HEADER_LEN..].to_vec(),
    };
    open_symmetric(key, &sealed).ok()
}

/// Seal `plaintext`, upload it to the workspace's relay blob channel, and return
/// the reference to embed in the message. `id` is caller-chosen (a UUID).
pub async fn upload(
    client: &RelayClient,
    workspace: &str,
    key: &[u8; 32],
    id: &str,
    name: &str,
    content_type: &str,
    plaintext: &[u8],
) -> Result<BlobRef, RelayError> {
    let framed = seal_framed(key, 0, plaintext)?;
    client.put_blob(workspace, id, framed).await?;
    Ok(BlobRef {
        id: id.to_string(),
        name: name.to_string(),
        size: plaintext.len() as u64,
        content_type: content_type.to_string(),
    })
}

/// Fetch + open a blob. `Ok(None)` = the relay has no such blob (never uploaded,
/// aged out, or an open relay without the channel); `Err` = a transport failure.
pub async fn download(
    client: &RelayClient,
    workspace: &str,
    key: &[u8; 32],
    id: &str,
) -> Result<Option<Vec<u8>>, RelayError> {
    match client.get_blob(workspace, id).await? {
        Some(framed) => Ok(open_framed(key, &framed)),
        None => Ok(None),
    }
}

/// A safe, collision-free local cache filename for a blob: `<id>-<sanitized name>`.
pub fn cache_filename(id: &str, name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    let safe = if safe.trim_matches('_').is_empty() { "file".to_string() } else { safe };
    format!("{id}-{safe}")
}

/// Resolve every `[Attached-blob: …]` reference in `body` to a local file —
/// downloading + decrypting + caching under `cache_dir/<id>-<name>` when missing —
/// and rewrite the marker to `[Attached: <path>]` so a subprocess agent reads the
/// file directly. A reference that can't be fetched/opened is left untouched.
pub async fn materialize_body(
    body: &str,
    client: &RelayClient,
    workspace: &str,
    key: &[u8; 32],
    cache_dir: &std::path::Path,
) -> String {
    let refs = parse_markers(body);
    if refs.is_empty() {
        return body.to_string();
    }
    let mut out = body.to_string();
    for r in refs {
        let path = cache_dir.join(cache_filename(&r.id, &r.name));
        if !path.exists() {
            match download(client, workspace, key, &r.id).await {
                Ok(Some(bytes)) => {
                    let _ = std::fs::create_dir_all(cache_dir);
                    if std::fs::write(&path, &bytes).is_err() {
                        continue;
                    }
                }
                _ => continue, // leave the marker when we can't fetch/open it
            }
        }
        out = out.replace(&r.marker(), &format!("[Attached: {}]", path.display()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_round_trip() {
        let key = [7u8; 32];
        let data = b"attachment bytes \x00\xff with NULs";
        let framed = seal_framed(&key, 0, data).unwrap();
        assert_ne!(&framed[HEADER_LEN..], data, "must be ciphertext, not plaintext");
        assert_eq!(open_framed(&key, &framed).as_deref(), Some(&data[..]));
        // Wrong key can't open.
        assert_eq!(open_framed(&[9u8; 32], &framed), None);
        // Malformed frame.
        assert_eq!(open_framed(&key, b"short"), None);
    }

    #[test]
    fn marker_round_trip_with_spaces_in_name() {
        let r = BlobRef {
            id: "abc-123".into(),
            name: "my report v2.png".into(),
            size: 4096,
            content_type: "image/png".into(),
        };
        let body = format!("Here's the file {}\n\nthanks", r.marker());
        let parsed = parse_markers(&body);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0], r);
    }

    #[test]
    fn parses_multiple_and_ignores_plain_attachments() {
        let a = BlobRef { id: "1".into(), name: "a.txt".into(), size: 1, content_type: "text/plain".into() };
        let b = BlobRef { id: "2".into(), name: "b.bin".into(), size: 2, content_type: "application/octet-stream".into() };
        let body = format!("{}\n[Attached: /tmp/local.png]\n{}", a.marker(), b.marker());
        let parsed = parse_markers(&body);
        assert_eq!(parsed, vec![a, b]);
    }
}
