//! Direct peer-to-peer sync foundation (P1).
//!
//! This is the transport-agnostic seam that a real networked implementation
//! (iroh-backed: ed25519 node ids, hole-punching, relay fallback) plugs into
//! later. P1 ships the identity/contact model, a shareable "friend code", a
//! [`PeerLink`] trait, an in-memory [`LoopbackLink`] for tests, and a
//! [`PeerSync`] that exchanges signed envelopes over any link — mirroring the
//! relay [`crate::sync_engine::SyncEngine`] so it slots behind the same seam.
//!
//! No real networking here; that's P2.

use std::collections::HashSet;

use hive_core::SessionEventEnvelope;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::event_store::EventStore;

#[derive(Debug, Error)]
pub enum PeerError {
    #[error("peer link closed")]
    Closed,
    #[error("encode/decode error: {0}")]
    Codec(#[from] serde_json::Error),
    #[error("store error: {0}")]
    Store(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("invalid peer id: {0}")]
    BadPeerId(String),
}

/// A peer's stable identity — the encoded ed25519 public key. Maps directly to
/// an iroh `NodeId` in P2 (iroh node ids are ed25519 public keys too).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerId(pub String);

/// Current code prefix. Codes are `hive_` + base64url(public key) — ~48 chars
/// for a 32-byte key, vs ~74 for the old hex form.
const CODE_PREFIX: &str = "hive_";
/// Legacy prefix: `hivepeer1:` + lowercase hex. Still accepted on input so codes
/// shared before the shortening keep working.
const LEGACY_PREFIX: &str = "hivepeer1:";

impl PeerId {
    /// Build from raw public-key bytes (lowercase hex — the canonical internal
    /// form; iroh node-id parsing and contact storage both expect hex).
    pub fn from_public_key(bytes: &[u8]) -> Self {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        PeerId(s)
    }

    /// Raw public-key bytes (decoded from the hex inner form).
    fn key_bytes(&self) -> Option<Vec<u8>> {
        hex_to_bytes(&self.0)
    }

    /// A shareable "friend code" you hand to someone so they can add you. The
    /// public key is base64url-encoded (no padding) to keep it compact.
    pub fn to_code(&self) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        match self.key_bytes() {
            Some(bytes) => format!("{CODE_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)),
            // Non-hex inner value (shouldn't happen): fall back to legacy form.
            None => format!("{LEGACY_PREFIX}{}", self.0),
        }
    }

    /// Parse a friend code back into a peer id. Accepts the compact `hive_`
    /// (base64url) form and the legacy `hivepeer1:` (hex) form. Tolerates
    /// surrounding whitespace; returns `None` for anything else.
    pub fn from_code(code: &str) -> Option<PeerId> {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let code = code.trim();
        if let Some(b64) = code.strip_prefix(CODE_PREFIX) {
            let bytes = URL_SAFE_NO_PAD.decode(b64.trim()).ok()?;
            return (!bytes.is_empty()).then(|| PeerId::from_public_key(&bytes));
        }
        if let Some(hex) = code.strip_prefix(LEGACY_PREFIX) {
            let hex = hex.trim();
            return (!hex.is_empty()).then(|| PeerId(hex.to_string()));
        }
        None
    }
}

/// Decode a lowercase/uppercase hex string into bytes (even length only).
fn hex_to_bytes(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// A saved peer ("friend") you can connect to directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub peer_id: PeerId,
    #[serde(default)]
    pub label: String,
}

/// An authenticated, ordered, bidirectional byte channel to known peers. The
/// loopback impl is in-process; the iroh impl (P2) does real NAT traversal.
#[allow(async_fn_in_trait)] // used generically, never as `dyn`
pub trait PeerLink {
    /// Send bytes to a connected peer.
    async fn send(&self, to: &PeerId, data: Vec<u8>) -> Result<(), PeerError>;
    /// Next inbound `(sender, bytes)`, or `None` once the link closes. Takes
    /// `&self` (interior mutability) so a node can be shared between a sender
    /// and a receiver task concurrently.
    async fn recv(&self) -> Option<(PeerId, Vec<u8>)>;
    /// This end's own peer id.
    fn local_id(&self) -> &PeerId;
}

/// An in-memory link pair wiring two ends together — for tests and as the
/// reference behavior the networked impl must match.
pub struct LoopbackLink {
    local: PeerId,
    tx: mpsc::UnboundedSender<(PeerId, Vec<u8>)>,
    rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<(PeerId, Vec<u8>)>>,
}

impl LoopbackLink {
    /// Create two ends connected to each other.
    pub fn pair(a: PeerId, b: PeerId) -> (LoopbackLink, LoopbackLink) {
        let (to_a, a_inbox) = mpsc::unbounded_channel();
        let (to_b, b_inbox) = mpsc::unbounded_channel();
        let la = LoopbackLink { local: a.clone(), tx: to_b, rx: tokio::sync::Mutex::new(a_inbox) };
        let lb = LoopbackLink { local: b, tx: to_a, rx: tokio::sync::Mutex::new(b_inbox) };
        (la, lb)
    }
}

impl PeerLink for LoopbackLink {
    async fn send(&self, _to: &PeerId, data: Vec<u8>) -> Result<(), PeerError> {
        // The loopback has a single remote; tag the message with our own id.
        self.tx.send((self.local.clone(), data)).map_err(|_| PeerError::Closed)
    }
    async fn recv(&self) -> Option<(PeerId, Vec<u8>)> {
        self.rx.lock().await.recv().await
    }
    fn local_id(&self) -> &PeerId {
        &self.local
    }
}

/// Exchanges signed envelopes with peers over a [`PeerLink`]. Transport-
/// agnostic: it serializes/deserializes envelopes and tracks which it has
/// already seen, so the same instance can broadcast and ingest without loops.
#[derive(Default)]
pub struct PeerSync {
    seen: HashSet<Uuid>,
}

impl PeerSync {
    pub fn new() -> Self {
        Self::default()
    }

    /// Serialize every local envelope not yet shared, marking them seen.
    pub fn take_unpushed(&mut self, store: &EventStore) -> Result<Vec<Vec<u8>>, PeerError> {
        let mut out = Vec::new();
        let ids = store.list_session_ids().map_err(|e| PeerError::Store(e.to_string()))?;
        for session_id in ids {
            let envs = store.list(session_id).map_err(|e| PeerError::Store(e.to_string()))?;
            for env in envs {
                if self.seen.insert(env.event_id) {
                    out.push(serde_json::to_vec(&env)?);
                }
            }
        }
        Ok(out)
    }

    /// Send all unpushed envelopes to a peer over `link`.
    pub async fn push_to<L: PeerLink>(
        &mut self,
        link: &L,
        to: &PeerId,
        store: &EventStore,
    ) -> Result<usize, PeerError> {
        let batch = self.take_unpushed(store)?;
        let n = batch.len();
        for data in batch {
            link.send(to, data).await?;
        }
        Ok(n)
    }

    /// Apply one received envelope into the store. Returns whether it was new.
    pub fn apply(&mut self, store: &mut EventStore, data: &[u8]) -> Result<bool, PeerError> {
        let env: SessionEventEnvelope = serde_json::from_slice(data)?;
        self.seen.insert(env.event_id);
        store.ingest(&env).map_err(|e| PeerError::Store(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::{ChatMessage, MessageRole, SessionEvent};

    #[test]
    fn friend_code_round_trips() {
        // A realistic 32-byte key.
        let id = PeerId::from_public_key(&[0xab; 32]);
        assert_eq!(id.0.len(), 64); // hex inner form
        let code = id.to_code();
        assert!(code.starts_with("hive_"));
        // Compact: well under the ~74-char legacy hex code.
        assert!(code.len() < 50, "code too long: {} ({code})", code.len());
        assert_eq!(PeerId::from_code(&format!("  {code}  ")), Some(id.clone()));
        assert_eq!(PeerId::from_code("not-a-code"), None);
        assert_eq!(PeerId::from_code("hive_"), None);
    }

    #[test]
    fn legacy_hex_codes_still_parse() {
        let id = PeerId::from_public_key(&[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(id.0, "deadbeef");
        // Old codes shared before the shortening must still resolve.
        assert_eq!(PeerId::from_code("hivepeer1:deadbeef"), Some(id));
        assert_eq!(PeerId::from_code("hivepeer1:"), None);
    }

    fn sample_envelope(seq: i64) -> SessionEventEnvelope {
        SessionEventEnvelope::new(
            Uuid::nil(),
            Uuid::nil(),
            seq,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "u", "hello"),
            },
        )
    }

    #[tokio::test]
    async fn two_peers_converge_over_loopback() {
        let a = PeerId("alice".into());
        let b = PeerId("bob".into());
        let (la, lb) = LoopbackLink::pair(a.clone(), b.clone());

        let store_a = EventStore::open_in_memory().unwrap();
        let mut store_b = EventStore::open_in_memory().unwrap();
        let env = sample_envelope(1);
        store_a.append_envelope(&env).unwrap();

        let mut sync_a = PeerSync::new();
        let mut sync_b = PeerSync::new();

        // A pushes its unpushed envelopes to B.
        let pushed = sync_a.push_to(&la, &b, &store_a).await.unwrap();
        assert_eq!(pushed, 1);

        // B receives and applies them.
        let (_from, data) = lb.recv().await.unwrap();
        let applied = sync_b.apply(&mut store_b, &data).unwrap();
        assert!(applied);

        // B now has A's session; a re-apply is a no-op (dedup).
        assert_eq!(store_b.list(Uuid::nil()).unwrap().len(), 1);
        assert!(!sync_b.apply(&mut store_b, &data).unwrap());
        // A doesn't re-push the same envelope.
        assert_eq!(sync_a.push_to(&la, &b, &store_a).await.unwrap(), 0);
    }
}
