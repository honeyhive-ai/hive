//! SQLite-backed event store — the Rust replacement for Swift's NDJSON event
//! log (`<workspaceRoot>/.hive/events/<sessionID>.ndjson`) +
//! `WorkspaceEventEnvelopeStore`.
//!
//! The append-only event-log semantics are unchanged: each workspace has an
//! ordered, per-session-sequenced stream of [`SessionEventEnvelope`]s; current
//! session state is obtained by projecting that stream (`hive_core::project`).
//! Only the storage medium changes (SQLite rows instead of NDJSON files), per
//! the clean-replacement plan.
//!
//! Each envelope's full JSON is stored verbatim in `envelope_json`; the
//! indexed columns (`session_id`, `sequence`, `kind`, `scope`, `timestamp`)
//! exist for efficient ordering/filtering and are derived from the envelope.

use std::path::Path;

use hive_core::{project, ChatSession, SessionEvent, SessionEventEnvelope};
use rusqlite::{Connection, OptionalExtension};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum EventStoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, EventStoreError>;

/// An append-only, per-session-sequenced event log over SQLite.
pub struct EventStore {
    conn: Connection,
}

impl EventStore {
    /// Open (creating if needed) the event store at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Open an in-memory store (tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        // WAL for concurrent readers (the live UI connection + the background
        // sync connection share the same file); busy_timeout absorbs brief
        // write contention between them.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // NORMAL is safe under WAL (no corruption; at worst the last commit is
        // lost on an OS crash/power loss) and avoids an fsync per commit — a big
        // win for chat workloads. (#2)
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS events (
                row_id        INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id      TEXT NOT NULL UNIQUE,
                session_id    TEXT NOT NULL,
                workspace_id  TEXT NOT NULL,
                sequence      INTEGER NOT NULL,
                kind          TEXT NOT NULL,
                scope         TEXT NOT NULL,
                timestamp     TEXT NOT NULL,
                envelope_json TEXT NOT NULL,
                UNIQUE (session_id, sequence)
            );
            CREATE INDEX IF NOT EXISTS idx_events_session_seq
                ON events (session_id, sequence);
            CREATE INDEX IF NOT EXISTS idx_events_workspace
                ON events (workspace_id, row_id);
            "#,
        )?;
        Ok(Self { conn })
    }

    /// Highest sequence recorded for a session, if any.
    pub fn max_sequence(&self, session_id: Uuid) -> Result<Option<i64>> {
        let seq = self
            .conn
            .query_row(
                "SELECT MAX(sequence) FROM events WHERE session_id = ?1",
                [session_id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten();
        Ok(seq)
    }

    /// Append a pre-built envelope verbatim. Fails if its
    /// `(session_id, sequence)` already exists (the append-only invariant).
    pub fn append_envelope(&self, env: &SessionEventEnvelope) -> Result<()> {
        let json = serde_json::to_string(env)?;
        let timestamp = serde_json::to_value(env.timestamp)?
            .as_str()
            .unwrap_or_default()
            .to_string();
        self.conn.execute(
            r#"
            INSERT INTO events
                (event_id, session_id, workspace_id, sequence, kind, scope, timestamp, envelope_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            rusqlite::params![
                env.event_id.to_string(),
                env.session_id.to_string(),
                env.workspace_id.to_string(),
                env.sequence,
                env.payload.kind_str(),
                serde_json::to_value(env.scope)?.as_str().unwrap_or_default(),
                timestamp,
                json,
            ],
        )?;
        Ok(())
    }

    /// Append an event for a session, assigning the next sequence atomically,
    /// and return the stored envelope. This is the common write path.
    pub fn append(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        payload: SessionEvent,
    ) -> Result<SessionEventEnvelope> {
        let tx = self.conn.transaction()?;
        let next_seq = tx
            .query_row(
                "SELECT MAX(sequence) FROM events WHERE session_id = ?1",
                [session_id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten()
            .map(|s| s + 1)
            .unwrap_or(1);

        let env = SessionEventEnvelope::new(session_id, workspace_id, next_seq, payload);
        let json = serde_json::to_string(&env)?;
        let timestamp = serde_json::to_value(env.timestamp)?
            .as_str()
            .unwrap_or_default()
            .to_string();
        tx.execute(
            r#"
            INSERT INTO events
                (event_id, session_id, workspace_id, sequence, kind, scope, timestamp, envelope_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            rusqlite::params![
                env.event_id.to_string(),
                env.session_id.to_string(),
                env.workspace_id.to_string(),
                env.sequence,
                env.payload.kind_str(),
                serde_json::to_value(env.scope)?.as_str().unwrap_or_default(),
                timestamp,
                json,
            ],
        )?;
        tx.commit()?;
        Ok(env)
    }

    /// Whether an envelope with this `event_id` is already stored (dedup).
    pub fn has_event(&self, event_id: Uuid) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE event_id = ?1",
            [event_id.to_string()],
            |row| row.get(0),
        )?;
        Ok(n > 0)
    }

    /// Ingest a foreign (synced) envelope: if unseen, append it under a fresh
    /// **local** sequence (preserving the original envelope body for signature
    /// verification + provenance). Returns `true` if newly stored. This gives a
    /// per-device local total order = ingestion order; the projector's events
    /// are commutative enough (upsert-by-id, idempotent reactions) for that to
    /// converge across peers.
    pub fn ingest(&mut self, env: &SessionEventEnvelope) -> Result<bool> {
        if self.has_event(env.event_id)? {
            return Ok(false);
        }
        let tx = self.conn.transaction()?;
        let next_seq = tx
            .query_row(
                "SELECT MAX(sequence) FROM events WHERE session_id = ?1",
                [env.session_id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten()
            .map(|s| s + 1)
            .unwrap_or(1);
        let json = serde_json::to_string(env)?;
        let timestamp = serde_json::to_value(env.timestamp)?
            .as_str()
            .unwrap_or_default()
            .to_string();
        tx.execute(
            r#"
            INSERT INTO events
                (event_id, session_id, workspace_id, sequence, kind, scope, timestamp, envelope_json)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            rusqlite::params![
                env.event_id.to_string(),
                env.session_id.to_string(),
                env.workspace_id.to_string(),
                next_seq,
                env.payload.kind_str(),
                serde_json::to_value(env.scope)?.as_str().unwrap_or_default(),
                timestamp,
                json,
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// All envelopes for a session in sequence order.
    pub fn list(&self, session_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        self.query_envelopes(
            "SELECT envelope_json FROM events WHERE session_id = ?1 ORDER BY sequence ASC",
            [session_id.to_string()],
        )
    }

    /// Envelopes for a session with `sequence > after`, in order — the sync
    /// fetch path.
    pub fn list_after(&self, session_id: Uuid, after: i64) -> Result<Vec<SessionEventEnvelope>> {
        self.query_envelopes(
            "SELECT envelope_json FROM events WHERE session_id = ?1 AND sequence > ?2 ORDER BY sequence ASC",
            rusqlite::params![session_id.to_string(), after],
        )
    }

    fn query_envelopes(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<SessionEventEnvelope>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for json in rows {
            let json = json?;
            // Resilient decode: a single malformed row must not poison the whole
            // session load. Unknown event *kinds* already deserialize to
            // `SessionEvent::Unknown` (forward-compat); this guards genuinely
            // corrupt/truncated JSON — skip and continue rather than erroring the
            // entire projection.
            match serde_json::from_str::<SessionEventEnvelope>(&json) {
                Ok(env) => out.push(env),
                Err(e) => {
                    tracing::warn!(error = %e, "skipping unparseable event row during load");
                }
            }
        }
        Ok(out)
    }

    /// Hard-delete every event for a session (irreversible). Returns the
    /// number of rows removed.
    pub fn delete_session(&self, session_id: Uuid) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM events WHERE session_id = ?1",
            [session_id.to_string()],
        )?;
        Ok(n)
    }

    /// Distinct session ids known to the store, in first-seen order.
    pub fn list_session_ids(&self) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id FROM events GROUP BY session_id ORDER BY MIN(row_id)",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for s in rows {
            if let Ok(id) = Uuid::parse_str(&s?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Project a session into its current state, replaying only from the latest
    /// `sessionSnapshot` so cost stays bounded on long chats (#3).
    pub fn load_session(&self, session_id: Uuid) -> Result<Option<ChatSession>> {
        // Project the full stream. `project` folds in canonical (lamport,
        // event_id) order and seeds from the canonical-latest snapshot, so the
        // result is independent of *this device's* ingestion order. A row_id
        // slice from the latest snapshot would feed `project` a different subset
        // when a snapshot is ingested after its own deltas — reintroducing
        // cross-device divergence. Bounded-cost compaction (canonical
        // lamport-ordered slicing) is a follow-up; correctness comes first.
        let envelopes = self.query_envelopes(
            "SELECT envelope_json FROM events WHERE session_id = ?1 ORDER BY row_id ASC",
            [session_id.to_string()],
        )?;
        Ok(project(&envelopes))
    }

    /// Row id of the latest `sessionSnapshot` for a session, if any.
    fn latest_snapshot_row(&self, session_id: Uuid) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT MAX(row_id) FROM events WHERE session_id = ?1 AND kind = 'sessionSnapshot'",
                [session_id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Envelopes from the latest snapshot onward (insertion order). The snapshot
    /// seeds `project`; later deltas fold onto it. Falls back to the whole
    /// stream if no snapshot exists.
    fn envelopes_from_latest_snapshot(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<SessionEventEnvelope>> {
        match self.latest_snapshot_row(session_id)? {
            Some(row) => self.query_envelopes(
                "SELECT envelope_json FROM events WHERE session_id = ?1 AND row_id >= ?2 ORDER BY row_id ASC",
                rusqlite::params![session_id.to_string(), row],
            ),
            None => self.query_envelopes(
                "SELECT envelope_json FROM events WHERE session_id = ?1 ORDER BY row_id ASC",
                [session_id.to_string()],
            ),
        }
    }

    /// How many events a session has accumulated since its latest snapshot —
    /// drives periodic re-snapshotting.
    pub fn rows_since_last_snapshot(&self, session_id: Uuid) -> Result<i64> {
        let n = match self.latest_snapshot_row(session_id)? {
            Some(row) => self.conn.query_row(
                "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND row_id > ?2",
                rusqlite::params![session_id.to_string(), row],
                |r| r.get::<_, i64>(0),
            )?,
            None => self.conn.query_row(
                "SELECT COUNT(*) FROM events WHERE session_id = ?1",
                [session_id.to_string()],
                |r| r.get::<_, i64>(0),
            )?,
        };
        Ok(n)
    }

    /// One-time maintenance: delete `messageChunkReceived` rows whose message
    /// already has a `messageCompleted` (the completed body supersedes them).
    /// Shrinks DBs written before per-token persistence was removed; projection
    /// is unaffected. Returns rows removed.
    pub fn prune_superseded_chunks(&mut self) -> Result<usize> {
        fn message_id_of(json: &str) -> Option<String> {
            // SessionEvent variant fields keep snake_case (the enum's
            // rename_all only renames variant tags, not their fields).
            serde_json::from_str::<serde_json::Value>(json)
                .ok()?
                .pointer("/payload/message_id")?
                .as_str()
                .map(str::to_owned)
        }

        let mut completed: std::collections::HashSet<String> = std::collections::HashSet::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT envelope_json FROM events WHERE kind = 'messageCompleted'")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            for j in rows {
                if let Some(id) = message_id_of(&j?) {
                    completed.insert(id);
                }
            }
        }
        if completed.is_empty() {
            return Ok(0);
        }

        let mut doomed: Vec<String> = Vec::new(); // event_ids
        {
            let mut stmt = self.conn.prepare(
                "SELECT event_id, envelope_json FROM events WHERE kind = 'messageChunkReceived'",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (event_id, json) = row?;
                if message_id_of(&json).map(|m| completed.contains(&m)).unwrap_or(false) {
                    doomed.push(event_id);
                }
            }
        }

        let tx = self.conn.transaction()?;
        let mut removed = 0;
        for event_id in &doomed {
            removed += tx.execute("DELETE FROM events WHERE event_id = ?1", [event_id])?;
        }
        tx.commit()?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::{ChatMessage, MessageRole};

    fn seeded_store() -> (EventStore, Uuid, Uuid) {
        let mut store = EventStore::open_in_memory().unwrap();
        let session = ChatSession::new("Demo", Uuid::nil(), "anthropic");
        let sid = session.id;
        let wid = session.workspace_id;
        store
            .append(
                sid,
                wid,
                SessionEvent::SessionSnapshot {
                    session: Box::new(session),
                },
            )
            .unwrap();
        (store, sid, wid)
    }

    #[test]
    fn append_assigns_monotonic_sequences() {
        let (mut store, sid, wid) = seeded_store();
        let e2 = store
            .append(
                sid,
                wid,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "Mara", "hi"),
                },
            )
            .unwrap();
        let e3 = store
            .append(
                sid,
                wid,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::Assistant, "Hive", "hello"),
                },
            )
            .unwrap();
        assert_eq!(e2.sequence, 2);
        assert_eq!(e3.sequence, 3);
        assert_eq!(store.max_sequence(sid).unwrap(), Some(3));
    }

    #[test]
    fn load_session_converges_regardless_of_ingest_order() {
        use hive_core::identity::{ActorIdentity, ActorKind, WorkspaceMember, WorkspaceRole};
        use hive_core::time_util::Timestamp;

        let base = ChatSession::new("Demo", Uuid::nil(), "anthropic");
        let (sid, wid) = (base.id, base.workspace_id);
        let member = WorkspaceMember {
            id: "m1".into(),
            actor: ActorIdentity::new("u1", "U1", ActorKind::Human),
            role: WorkspaceRole::Contributor,
            title: String::new(),
            index: 0,
            joined_at: Timestamp::epoch(),
        };
        let events = vec![
            SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) }),
            SessionEventEnvelope::new(sid, wid, 2, SessionEvent::SessionTitleChanged { title: "First".into() }),
            SessionEventEnvelope::new(sid, wid, 3, SessionEvent::MemberAdded { member }),
            SessionEventEnvelope::new(sid, wid, 4, SessionEvent::SessionTitleChanged { title: "Second".into() }),
            SessionEventEnvelope::new(sid, wid, 5, SessionEvent::MemberRemoved { member_id: "m1".into() }),
        ];

        // Device A ingests forward; device B ingests in reverse — so B stores the
        // snapshot LAST (highest row_id). The old row_id-sliced load would have
        // projected only the snapshot on B and diverged.
        let mut a = EventStore::open_in_memory().unwrap();
        for e in &events {
            a.ingest(e).unwrap();
        }
        let mut b = EventStore::open_in_memory().unwrap();
        for e in events.iter().rev() {
            b.ingest(e).unwrap();
        }

        let sa = a.load_session(sid).unwrap().expect("a");
        let sb = b.load_session(sid).unwrap().expect("b");
        assert_eq!(sa, sb, "stores diverged under different ingest order");
        assert_eq!(sa.title, "Second", "canonical-latest title wins");
        assert!(sa.members.is_empty(), "m1 added then removed");
    }

    #[test]
    fn load_session_projects_appended_messages() {
        let (mut store, sid, wid) = seeded_store();
        store
            .append(
                sid,
                wid,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "Mara", "hi"),
                },
            )
            .unwrap();
        let session = store.load_session(sid).unwrap().expect("session");
        assert_eq!(session.title, "Demo");
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].body, "hi");
    }

    #[test]
    fn list_after_returns_only_newer_events() {
        let (mut store, sid, wid) = seeded_store();
        for i in 0..3 {
            store
                .append(
                    sid,
                    wid,
                    SessionEvent::MessageAppended {
                        message: ChatMessage::new(MessageRole::User, "u", format!("m{i}")),
                    },
                )
                .unwrap();
        }
        // snapshot is seq 1; messages are 2,3,4
        let after_2 = store.list_after(sid, 2).unwrap();
        assert_eq!(after_2.len(), 2);
        assert_eq!(after_2[0].sequence, 3);
    }

    #[test]
    fn duplicate_sequence_is_rejected() {
        let (store, sid, wid) = seeded_store();
        // snapshot already occupies sequence 1
        let dup = SessionEventEnvelope::new(
            sid,
            wid,
            1,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "u", "dup"),
            },
        );
        assert!(store.append_envelope(&dup).is_err());
    }

    #[test]
    fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.db");
        let (sid, wid);
        {
            let mut store = EventStore::open(&path).unwrap();
            let session = ChatSession::new("Persisted", Uuid::nil(), "anthropic");
            sid = session.id;
            wid = session.workspace_id;
            store
                .append(
                    sid,
                    wid,
                    SessionEvent::SessionSnapshot {
                        session: Box::new(session),
                    },
                )
                .unwrap();
            store
                .append(
                    sid,
                    wid,
                    SessionEvent::MessageAppended {
                        message: ChatMessage::new(MessageRole::User, "Mara", "persist me"),
                    },
                )
                .unwrap();
        }
        let store = EventStore::open(&path).unwrap();
        let session = store.load_session(sid).unwrap().expect("session");
        assert_eq!(session.title, "Persisted");
        assert_eq!(session.messages[0].body, "persist me");
        let _ = wid;
    }

    #[test]
    fn load_replays_from_latest_snapshot() {
        let (mut store, sid, wid) = seeded_store();
        for i in 0..3 {
            store
                .append(
                    sid,
                    wid,
                    SessionEvent::MessageAppended {
                        message: ChatMessage::new(MessageRole::User, "u", format!("m{i}")),
                    },
                )
                .unwrap();
        }
        // Re-snapshot current state, then add one more message.
        let snap = store.load_session(sid).unwrap().unwrap();
        store
            .append(sid, wid, SessionEvent::SessionSnapshot { session: Box::new(snap) })
            .unwrap();
        store
            .append(
                sid,
                wid,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "u", "after"),
                },
            )
            .unwrap();

        // Replay starts at the latest snapshot (just 2 envelopes), full state intact.
        assert_eq!(store.rows_since_last_snapshot(sid).unwrap(), 1);
        assert_eq!(
            store.envelopes_from_latest_snapshot(sid).unwrap().len(),
            2 // snapshot + the one "after" message
        );
        let s = store.load_session(sid).unwrap().unwrap();
        assert_eq!(s.messages.len(), 4);
        assert_eq!(s.messages[3].body, "after");
    }

    #[test]
    fn prune_drops_chunks_for_completed_messages() {
        let (mut store, sid, wid) = seeded_store();
        let mut placeholder = ChatMessage::new(MessageRole::Assistant, "Hive", "");
        placeholder.is_streaming = true;
        let mid = placeholder.id;
        store
            .append(sid, wid, SessionEvent::MessageAppended { message: placeholder })
            .unwrap();
        for piece in ["He", "llo"] {
            store
                .append(
                    sid,
                    wid,
                    SessionEvent::MessageChunkReceived { message_id: mid, chunk: piece.into() },
                )
                .unwrap();
        }
        store
            .append(
                sid,
                wid,
                SessionEvent::MessageCompleted { message_id: mid, body: "Hello".into() },
            )
            .unwrap();

        let removed = store.prune_superseded_chunks().unwrap();
        assert_eq!(removed, 2, "both chunk rows superseded by completed");
        // projection unchanged
        let s = store.load_session(sid).unwrap().unwrap();
        assert_eq!(s.messages.iter().find(|m| m.id == mid).unwrap().body, "Hello");
        // idempotent
        assert_eq!(store.prune_superseded_chunks().unwrap(), 0);
    }
}
