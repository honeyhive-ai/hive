//! Durable persistence for the relay's state.
//!
//! The relay holds all its state in memory and only needs it to **survive
//! restarts** (a redeploy shouldn't wipe the social graph or message history).
//! Rather than a full SQL store + a query rewrite of every handler, we snapshot
//! the durable maps to a JSON file on a persistent volume and reload it on boot.
//! This matches the single-instance deployment model (one relay, one volume)
//! and keeps the dependency surface to the `serde_json` already in use.
//!
//! Enable it by pointing `HIVE_RELAY_DATA_DIR` at a writable directory (e.g. a
//! Fly volume mount); see `docs/relay-deploy.md`. Unset ⇒ in-memory only.
//!
//! Ephemeral state is intentionally **not** persisted: pairing codes (short TTL),
//! STUN candidates, and presence blobs (rebuilt from heartbeats). Account
//! `last_seen` is persisted but goes stale across downtime — the first heartbeat
//! after restart refreshes it, so friends simply read offline until then.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::social::{AccountState, FriendRequest};
use crate::{DirAccount, RelayState, Workspace};

const SNAPSHOT_FILE: &str = "relay-state.json";

/// The durable slice of [`RelayState`], serialized to disk.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct StateSnapshot {
    workspaces: HashMap<String, Workspace>,
    directory: HashMap<String, DirAccount>,
    accounts: HashMap<String, AccountState>,
    login_index: HashMap<String, String>,
    friend_requests: HashMap<String, FriendRequest>,
    friend_edges: HashSet<(String, String)>,
}

fn snapshot_path(dir: &Path) -> PathBuf {
    dir.join(SNAPSHOT_FILE)
}

impl RelayState {
    /// Enable durable persistence at `data_dir`: load any existing snapshot into
    /// this state, then remember the path so [`flush`](Self::flush) can write to
    /// it. A missing/corrupt snapshot is treated as empty (logged), so a fresh
    /// volume just starts blank rather than failing to boot.
    pub fn with_persistence(self, data_dir: impl Into<PathBuf>) -> Self {
        let dir = data_dir.into();
        let path = snapshot_path(&dir);
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<StateSnapshot>(&bytes) {
                Ok(snap) => {
                    self.restore(snap);
                    eprintln!("hive-relay: loaded state snapshot from {}", path.display());
                }
                Err(e) => eprintln!(
                    "hive-relay: snapshot at {} is unreadable ({e}); starting empty",
                    path.display()
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("hive-relay: no snapshot at {} yet; starting empty", path.display());
            }
            Err(e) => eprintln!("hive-relay: could not read {} ({e}); starting empty", path.display()),
        }
        Self { persist_path: Some(path), ..self }
    }

    /// True when durable persistence is configured.
    pub fn persistence_enabled(&self) -> bool {
        self.persist_path.is_some()
    }

    /// Build a snapshot of the durable state (clones under read locks).
    fn snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            workspaces: clone_map(&self.workspaces),
            directory: clone_map(&self.directory),
            accounts: clone_map(&self.accounts),
            login_index: clone_map(&self.login_index),
            friend_requests: clone_map(&self.friend_requests),
            friend_edges: self.friend_edges.read().unwrap().clone(),
        }
    }

    /// Overwrite in-memory state from a loaded snapshot.
    fn restore(&self, snap: StateSnapshot) {
        *self.workspaces.write().unwrap() = snap.workspaces;
        *self.directory.write().unwrap() = snap.directory;
        *self.accounts.write().unwrap() = snap.accounts;
        *self.login_index.write().unwrap() = snap.login_index;
        *self.friend_requests.write().unwrap() = snap.friend_requests;
        *self.friend_edges.write().unwrap() = snap.friend_edges;
    }

    /// Atomically write the current state to the configured snapshot file
    /// (write-to-temp + rename, so a crash mid-write can't corrupt the file).
    /// No-op when persistence isn't enabled.
    pub fn flush(&self) -> std::io::Result<()> {
        let Some(path) = self.persist_path.clone() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec(&self.snapshot())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

fn clone_map<K: Clone, V: Clone>(m: &std::sync::RwLock<HashMap<K, V>>) -> HashMap<K, V> {
    m.read().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_through_a_snapshot_file() {
        let dir = std::env::temp_dir().join(format!("hive-relay-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // Populate a state with social + envelope data, then flush.
        let s = RelayState::default().with_persistence(&dir);
        s.register_account_device(1, "alice", "d1", Some("nodeA".into()), None, 100);
        s.register_account_device(2, "bob", "d2", None, None, 100);
        let a = RelayState::account_key(1);
        let b = RelayState::account_key(2);
        let req = s.create_friend_request(&a, "alice", &b, "bob", 1).unwrap();
        s.accept_friend_request(&req.id, &b).unwrap();
        s.push_account_event(&a, serde_json::json!({ "kind": "ping" }));
        s.flush().unwrap();

        // A fresh state loading the same dir sees the persisted graph.
        let restored = RelayState::default().with_persistence(&dir);
        assert!(restored.are_friends(&a, &b));
        assert_eq!(restored.friend_count(&a), 1);
        assert_eq!(restored.account_key_for_login("bob"), Some(b.clone()));
        assert_eq!(restored.account_devices(&a).len(), 1);
        assert_eq!(restored.account_inbox_after(&a, 0).len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_snapshot_starts_empty_and_flush_is_noop_without_path() {
        let dir = std::env::temp_dir().join(format!("hive-relay-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = RelayState::default().with_persistence(&dir);
        assert!(s.persistence_enabled());
        assert!(s.account_devices("github:1").is_empty());

        // Without a path, flush quietly does nothing.
        assert!(RelayState::default().flush().is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
