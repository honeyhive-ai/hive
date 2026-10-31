//! Host liveness over the relay's ephemeral presence channel.
//!
//! A host (a member's desktop, an always-on box) used to prove it was online by
//! re-appending a durable `WorkspaceHostUpserted` with a fresh `last_seen` every
//! minute or so: thousands of signed events a day per host, synced to every
//! member forever. Liveness is transient, so it now rides the relay's presence
//! slots (`/v1/workspaces/{room}/presence`, overwritten in place, never part of
//! the event log). The durable host record is only written when something real
//! changes (label, kind, owner).
//!
//! A beat is sealed under the workspace key, so the relay sees only an opaque
//! slot name and ciphertext. Readers merge beats with the durable `last_seen`
//! (whichever is newer), so a host still running an older build — which keeps
//! heartbeating durably — shows its true state; an older reader sees the new
//! hosts' durable `last_seen` stop moving and reports them offline/stale.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use hive_core::{open_symmetric, seal_symmetric, SealedEnvelope, Timestamp, WorkspaceHost};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::relay_client::{RelayClient, RelayError};

/// A host counts as online when it beat within this many seconds.
pub const HOST_ONLINE_WINDOW_SECS: i64 = 90;
/// How often a running host publishes its beat (well inside the window, so one
/// lost beat doesn't flip it offline).
pub const HOST_BEAT_EVERY_SECS: i64 = 30;

/// The presence slot a host's beat lives in. Prefixed so a reader of human
/// presence (`actorId` + typing) skips it.
pub fn host_presence_slot(host_id: &str) -> String {
    format!("host:{host_id}")
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Beat {
    host_id: String,
    /// Unix seconds.
    at: i64,
}

/// Seal a beat for `host_id` at `at` (unix seconds) under the workspace key.
pub fn seal_host_beat(key: &[u8; 32], host_id: &str, at: i64) -> Option<Value> {
    let plain = serde_json::to_vec(&Beat { host_id: host_id.to_string(), at }).ok()?;
    let sealed = seal_symmetric(key, 0, &plain).ok()?;
    Some(serde_json::json!({ "hostBeat": sealed }))
}

/// Open every host beat in a presence listing (`slot` → blob). Skips human
/// presence, beats sealed under another key, and a beat whose slot doesn't
/// match the host it names. A beat from the future is clamped to `now`.
pub fn open_host_beats(blobs: &HashMap<String, Value>, key: &[u8; 32], now: i64) -> HashMap<String, i64> {
    let mut out = HashMap::new();
    for (slot, blob) in blobs {
        let Some(sealed) = blob.get("hostBeat") else { continue };
        let Ok(sealed) = serde_json::from_value::<SealedEnvelope>(sealed.clone()) else { continue };
        let Ok(plain) = open_symmetric(key, &sealed) else { continue };
        let Ok(beat) = serde_json::from_slice::<Beat>(&plain) else { continue };
        if *slot != host_presence_slot(&beat.host_id) {
            continue;
        }
        let at = beat.at.min(now);
        let e = out.entry(beat.host_id).or_insert(at);
        *e = (*e).max(at);
    }
    out
}

/// Publish this host's beat to the room's presence channel.
pub async fn publish_host_beat(relay: &RelayClient, room: &str, key: &[u8; 32], host_id: &str) -> Result<(), RelayError> {
    let now = Timestamp::now().inner().unix_timestamp();
    let Some(blob) = seal_host_beat(key, host_id, now) else { return Ok(()) };
    relay.publish_presence(room, &host_presence_slot(host_id), &blob).await
}

/// Read every host's latest beat in the room (host id → unix seconds).
pub async fn fetch_host_beats(relay: &RelayClient, room: &str, key: &[u8; 32]) -> Result<HashMap<String, i64>, RelayError> {
    let blobs = relay.list_presence(room).await?;
    Ok(open_host_beats(&blobs, key, Timestamp::now().inner().unix_timestamp()))
}

/// The latest beat seen per host, shared between the task that polls the relay
/// and everything that asks "is this host online?".
#[derive(Clone, Default)]
pub struct HostPresence {
    beats: Arc<Mutex<HashMap<String, i64>>>,
}

impl HostPresence {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a beat for `host_id` (keeps the newest).
    pub fn record(&self, host_id: &str, at: i64) {
        let mut b = self.beats.lock().unwrap_or_else(|p| p.into_inner());
        let e = b.entry(host_id.to_string()).or_insert(at);
        *e = (*e).max(at);
    }

    /// Merge a fetched listing.
    pub fn merge(&self, beats: HashMap<String, i64>) {
        for (id, at) in beats {
            self.record(&id, at);
        }
    }

    /// The newest beat seen for `host_id`.
    pub fn beat_of(&self, host_id: &str) -> Option<i64> {
        self.beats.lock().unwrap_or_else(|p| p.into_inner()).get(host_id).copied()
    }

    /// When `host` was last seen: its newest presence beat or its durable
    /// `last_seen` (an older build still heartbeats durably), whichever is later.
    pub fn last_seen(&self, host: &WorkspaceHost) -> Timestamp {
        let durable = host.last_seen.inner().unix_timestamp();
        match self.beat_of(&host.id) {
            Some(at) if at > durable => Timestamp::from_unix_seconds(at),
            _ => host.last_seen,
        }
    }

    /// Whether `host` was seen within [`HOST_ONLINE_WINDOW_SECS`] of `now`.
    pub fn is_online(&self, host: &WorkspaceHost, now: &Timestamp) -> bool {
        now.inner().unix_timestamp() - self.last_seen(host).inner().unix_timestamp() <= HOST_ONLINE_WINDOW_SECS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::HostKind;

    fn key(s: &str) -> [u8; 32] {
        hive_core::derive_workspace_key(s)
    }

    #[test]
    fn beats_round_trip_and_ignore_everything_else() {
        let k = key("room secret");
        let mut blobs = HashMap::new();
        blobs.insert(host_presence_slot("box"), seal_host_beat(&k, "box", 1_000).unwrap());
        // Human presence (plaintext typing blob) is not a beat.
        blobs.insert("actor-1".into(), serde_json::json!({ "actorId": "actor-1", "ts": 5 }));
        // A beat sealed under another workspace's key doesn't open.
        blobs.insert(host_presence_slot("stranger"), seal_host_beat(&key("other"), "stranger", 1_000).unwrap());
        // A beat that claims another host's slot is ignored.
        blobs.insert(host_presence_slot("desk"), seal_host_beat(&k, "box", 2_000).unwrap());
        // A beat from the future is clamped to now.
        blobs.insert(host_presence_slot("fast"), seal_host_beat(&k, "fast", 9_999).unwrap());

        let beats = open_host_beats(&blobs, &k, 5_000);
        assert_eq!(beats.get("box"), Some(&1_000));
        assert_eq!(beats.get("fast"), Some(&5_000));
        assert_eq!(beats.len(), 2);
        // The relay only ever sees ciphertext.
        let raw = serde_json::to_string(&blobs[&host_presence_slot("box")]).unwrap();
        assert!(!raw.contains("hostId"));
    }

    #[test]
    fn online_uses_the_newer_of_beat_and_durable_last_seen() {
        let now = Timestamp::now();
        let t = now.inner().unix_timestamp();
        let mut host = WorkspaceHost::new("box", HostKind::Worker, "Box");
        host.last_seen = Timestamp::from_unix_seconds(t - 10_000);
        let p = HostPresence::new();
        assert!(!p.is_online(&host, &now), "stale durable record and no beat: offline");
        p.record("box", t - 20);
        assert!(p.is_online(&host, &now), "a fresh beat: online");
        assert_eq!(p.last_seen(&host).inner().unix_timestamp(), t - 20);

        // An older build heartbeating durably, no beats: still online.
        let mut legacy = WorkspaceHost::new("old", HostKind::Device, "Old");
        legacy.last_seen = Timestamp::from_unix_seconds(t - 5);
        assert!(p.is_online(&legacy, &now));
    }

    #[tokio::test]
    async fn publish_and_fetch_through_the_relay() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, crate::test_relay::router()).await.unwrap() });
        let relay = RelayClient::new(format!("http://{addr}"));
        let k = key("room secret");
        publish_host_beat(&relay, "room-a", &k, "box").await.unwrap();
        let beats = fetch_host_beats(&relay, "room-a", &k).await.unwrap();
        let now = Timestamp::now().inner().unix_timestamp();
        let at = *beats.get("box").expect("beat published");
        assert!(now - at < 5);
        assert!(fetch_host_beats(&relay, "room-b", &k).await.unwrap().is_empty());
    }
}
