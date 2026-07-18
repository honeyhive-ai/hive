//! Workspace sync engine — the app-level loop that makes the relay actually
//! converge two clients (follow-up #125). Pushes new local envelopes to the
//! relay and ingests remote ones into the local event store, deduped by
//! `event_id`. Ported from the sync coordination in `WorkspaceSyncServices.swift`.
//!
//! This is the relay-forwarding sync path; direct P2P (STUN/hole-punch) and a
//! true CRDT are still tracked follow-ups. Ordering is by the relay's server
//! sequence on fetch + local ingestion order, which the commutative projector
//! (upsert-by-id, idempotent reactions) tolerates.

use std::collections::HashSet;

use hive_core::{open_symmetric, seal_symmetric, SealedEnvelope, SessionEventEnvelope};
use serde_json::Value;
use uuid::Uuid;

use crate::event_store::EventStore;
use crate::relay_client::{RelayClient, RelayError};

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Relay(#[from] RelayError),
    #[error(transparent)]
    Store(#[from] crate::event_store::EventStoreError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("crypto error: {0}")]
    Crypto(#[from] hive_core::CryptoError),
}

/// Drives one workspace's relay sync. Holds cursors so repeated rounds only
/// move new data.
pub struct SyncEngine {
    relay: RelayClient,
    workspace: String,
    /// event_ids already pushed (or ingested from the relay) — never re-push.
    seen: HashSet<Uuid>,
    /// highest relay server sequence fetched so far.
    last_fetched_seq: u64,
    /// When set, envelopes are sealed (ChaCha20-Poly1305) before the relay and
    /// opened on fetch — the relay only ever sees ciphertext.
    key: Option<[u8; 32]>,
}

impl SyncEngine {
    pub fn new(relay: RelayClient, workspace: impl Into<String>) -> Self {
        Self {
            relay,
            workspace: workspace.into(),
            seen: HashSet::new(),
            last_fetched_seq: 0,
            key: None,
        }
    }

    /// Enable E2EE on the wire with a shared workspace key.
    pub fn with_key(mut self, key: [u8; 32]) -> Self {
        self.key = Some(key);
        self
    }

    /// Encode an envelope for the relay: sealed ciphertext if a key is set,
    /// else plaintext JSON.
    fn encode(&self, env: &SessionEventEnvelope) -> Result<Value, SyncError> {
        match &self.key {
            Some(k) => {
                let plain = serde_json::to_vec(env)?;
                Ok(serde_json::to_value(seal_symmetric(k, &plain)?)?)
            }
            None => Ok(serde_json::to_value(env)?),
        }
    }

    /// Decode a relay body back into an envelope. Returns `None` for a sealed
    /// body we can't open (no/incorrect key) so a foreign room doesn't poison us.
    fn decode(&self, body: &Value) -> Result<Option<SessionEventEnvelope>, SyncError> {
        if body.get("ciphertext").is_some() {
            let Some(k) = &self.key else { return Ok(None) };
            let sealed: SealedEnvelope = serde_json::from_value(body.clone())?;
            match open_symmetric(k, &sealed) {
                Ok(plain) => Ok(Some(serde_json::from_slice(&plain)?)),
                Err(_) => Ok(None),
            }
        } else {
            Ok(Some(serde_json::from_value(body.clone())?))
        }
    }

    /// Push every local envelope (across all sessions) not yet pushed. Returns
    /// how many were sent.
    pub async fn push_new(&mut self, store: &EventStore) -> Result<usize, SyncError> {
        let to_push = self.take_unpushed(store)?;
        self.push_envelopes(&to_push).await?;
        Ok(to_push.len())
    }

    /// Fetch remote envelopes past the cursor and ingest the unseen ones.
    /// Returns how many were newly applied.
    pub async fn pull(&mut self, store: &mut EventStore) -> Result<usize, SyncError> {
        let fetched = self.fetch_new().await?;
        self.apply_fetched(store, &fetched)
    }

    /// One full round: push local, then pull remote.
    pub async fn sync_once(&mut self, store: &mut EventStore) -> Result<(usize, usize), SyncError> {
        let pushed = self.push_new(store).await?;
        let pulled = self.pull(store).await?;
        Ok((pushed, pulled))
    }

    // --- Split storage/network steps, so callers (the background task) can run
    // without holding a (non-Send) store reference across an await. ---

    /// Collect local envelopes not yet pushed, marking them seen (sync; no IO).
    pub fn take_unpushed(
        &mut self,
        store: &EventStore,
    ) -> Result<Vec<SessionEventEnvelope>, SyncError> {
        let mut out = Vec::new();
        for session_id in store.list_session_ids()? {
            for env in store.list(session_id)? {
                if self.seen.insert(env.event_id) {
                    out.push(env);
                }
            }
        }
        Ok(out)
    }

    /// Push pre-collected envelopes to the relay, sealing them if a key is set
    /// (async; no store).
    pub async fn push_envelopes(&self, envelopes: &[SessionEventEnvelope]) -> Result<(), SyncError> {
        for env in envelopes {
            let body = self.encode(env)?;
            self.relay.push_value(&self.workspace, &body).await?;
        }
        Ok(())
    }

    /// Fetch raw remote bodies past the cursor, advancing it (async; no store).
    pub async fn fetch_new(&mut self) -> Result<Vec<(u64, Value)>, SyncError> {
        let fetched = self.relay.fetch_values(&self.workspace, self.last_fetched_seq).await?;
        for (seq, _) in &fetched {
            self.last_fetched_seq = self.last_fetched_seq.max(*seq);
        }
        Ok(fetched)
    }

    /// Decode + ingest fetched bodies into the store (sync; no IO awaits).
    /// Sealed bodies we can't open are skipped. Returns how many were applied.
    pub fn apply_fetched(
        &mut self,
        store: &mut EventStore,
        fetched: &[(u64, Value)],
    ) -> Result<usize, SyncError> {
        let mut applied = 0;
        for (_seq, body) in fetched {
            if let Some(env) = self.decode(body)? {
                self.seen.insert(env.event_id);
                if store.ingest(&env)? {
                    applied += 1;
                }
            }
        }
        Ok(applied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::{ChatMessage, ChatSession, MessageRole, SessionEvent, SessionEventEnvelope};

    async fn spawn_relay() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, hive_relay::router()).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn seed_chat(store: &mut EventStore) -> (Uuid, Uuid) {
        let session = ChatSession::new("Shared", Uuid::new_v4(), "anthropic");
        let sid = session.id;
        let wid = session.workspace_id;
        let snap = SessionEventEnvelope::new(
            sid,
            wid,
            1,
            SessionEvent::SessionSnapshot { session: Box::new(session) },
        );
        store.ingest(&snap).unwrap();
        (sid, wid)
    }

    #[tokio::test]
    async fn two_devices_converge_through_the_relay() {
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();

        // Device A: a chat with a snapshot + a message.
        let mut store_a = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store_a);
        let msg = SessionEventEnvelope::new(
            sid,
            wid,
            2,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "A", "hello from A"),
            },
        );
        store_a.ingest(&msg).unwrap();

        // A pushes; B pulls into an empty store.
        let mut sync_a = SyncEngine::new(RelayClient::new(&base), &workspace);
        assert_eq!(sync_a.push_new(&store_a).await.unwrap(), 2);

        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut sync_b = SyncEngine::new(RelayClient::new(&base), &workspace);
        let applied = sync_b.pull(&mut store_b).await.unwrap();
        assert_eq!(applied, 2);

        // B now projects the same conversation.
        let session_b = store_b.load_session(sid).unwrap().expect("session synced to B");
        assert_eq!(session_b.title, "Shared");
        assert_eq!(session_b.messages.len(), 1);
        assert_eq!(session_b.messages[0].body, "hello from A");

        // Idempotent: a second pull applies nothing new.
        assert_eq!(sync_b.pull(&mut store_b).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn e2ee_sync_keeps_ciphertext_on_the_relay() {
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();
        let key = hive_core::derive_workspace_key("shared room secret");

        let mut store_a = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store_a);
        store_a
            .ingest(&SessionEventEnvelope::new(
                sid,
                wid,
                2,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "A", "TOP SECRET PLAN"),
                },
            ))
            .unwrap();

        let mut sync_a = SyncEngine::new(RelayClient::new(&base), &workspace).with_key(key);
        sync_a.push_new(&store_a).await.unwrap();

        // The relay's stored bodies must be ciphertext — no plaintext leak.
        let raw = RelayClient::new(&base)
            .fetch_values(&workspace, 0)
            .await
            .unwrap();
        assert_eq!(raw.len(), 2);
        for (_seq, body) in &raw {
            assert!(body.get("ciphertext").is_some(), "body must be sealed");
            let s = serde_json::to_string(body).unwrap();
            assert!(!s.contains("TOP SECRET PLAN"));
            assert!(!s.contains("messageAppended"));
        }

        // B with the same key converges; the message is readable again.
        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut sync_b = SyncEngine::new(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(sync_b.pull(&mut store_b).await.unwrap(), 2);
        let session_b = store_b.load_session(sid).unwrap().unwrap();
        assert_eq!(session_b.messages[0].body, "TOP SECRET PLAN");

        // B with the WRONG key can't open anything.
        let mut store_c = EventStore::open_in_memory().unwrap();
        let mut sync_c = SyncEngine::new(RelayClient::new(&base), &workspace)
            .with_key(hive_core::derive_workspace_key("wrong"));
        assert_eq!(sync_c.pull(&mut store_c).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn bidirectional_round_trip() {
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();

        let mut store_a = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store_a);
        let mut sync_a = SyncEngine::new(RelayClient::new(&base), &workspace);
        sync_a.sync_once(&mut store_a).await.unwrap();

        // B joins, syncs, then replies.
        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut sync_b = SyncEngine::new(RelayClient::new(&base), &workspace);
        sync_b.sync_once(&mut store_b).await.unwrap();
        let reply = SessionEventEnvelope::new(
            sid,
            wid,
            99,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::Assistant, "B", "reply from B"),
            },
        );
        store_b.ingest(&reply).unwrap();
        sync_b.sync_once(&mut store_b).await.unwrap();

        // A pulls B's reply.
        sync_a.sync_once(&mut store_a).await.unwrap();
        let session_a = store_a.load_session(sid).unwrap().unwrap();
        assert!(session_a.messages.iter().any(|m| m.body == "reply from B"));
    }
}
