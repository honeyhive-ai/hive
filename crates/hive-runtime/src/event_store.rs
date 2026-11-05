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

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use hive_core::{project_with_options, ChatSession, SessionEvent, SessionEventEnvelope};
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

/// A chat responder's compaction state as kept on this device: the first
/// kept message (`boundary`), the ids the `summary` covers (everything before
/// the boundary when it was made) and their
/// [`hive_core::context_budget::messages_fingerprint`], which the reader
/// checks against the live transcript before reusing the summary.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredCompaction {
    pub boundary: Option<Uuid>,
    pub covered: Vec<Uuid>,
    pub fingerprint: String,
    pub summary: String,
}

/// An append-only, per-session-sequenced event log over SQLite.
pub struct EventStore {
    conn: Connection,
    /// Projections reused while their session's rows are unchanged. A busy
    /// workspace-config log (host heartbeats) runs to tens of thousands of
    /// events, and every chat load projects it again, so without this one
    /// dispatch or UI refresh spent seconds re-folding the same log.
    projections: Mutex<HashMap<ProjectionKey, (RowsSig, Option<ChatSession>)>>,
}

/// (session, trusted founder, trusted workspace): what a projection depends on
/// besides the session's rows.
type ProjectionKey = (Uuid, Option<String>, Option<Uuid>);

/// (row count, max row id) of a session. Any append raises the max row id and
/// any delete lowers the count, so an equal signature means the same rows
/// (envelope rows are never updated in place).
type RowsSig = (i64, i64);

/// Cached projections kept at most; past this the cache starts over.
const PROJECTION_CACHE_MAX: usize = 256;

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

            -- Durable sync state (#2), keyed by relay workspace/room. Without
            -- this the sync engine kept its cursor + pushed-set in memory only,
            -- so every process restart re-fetched from seq 0 and re-pushed the
            -- entire local log. `sync_cursor` is the highest relay server
            -- sequence durably applied; `pushed` is the set of event_ids already
            -- on the relay (we pushed them, or ingested them from there) so a
            -- restart never re-sends them.
            CREATE TABLE IF NOT EXISTS sync_cursor (
                workspace        TEXT PRIMARY KEY,
                last_fetched_seq INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS pushed (
                workspace TEXT NOT NULL,
                event_id  TEXT NOT NULL,
                PRIMARY KEY (workspace, event_id)
            );

            -- Device-local compaction state, never synced: per chat and
            -- responder runtime, the batch boundary plus the summary of
            -- everything before it, the ids it covers and their fingerprint
            -- (see StoredCompaction). Lets an app restart reuse a summary
            -- instead of spending tens of seconds re-summarizing.
            CREATE TABLE IF NOT EXISTS compaction_state (
                session_id   TEXT NOT NULL,
                runtime_id   TEXT NOT NULL,
                boundary     TEXT,
                covered_json TEXT NOT NULL,
                fingerprint  TEXT NOT NULL,
                summary      TEXT NOT NULL,
                PRIMARY KEY (session_id, runtime_id)
            );
            "#,
        )?;
        Ok(Self { conn, projections: Mutex::new(HashMap::new()) })
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

    /// The highest Lamport timestamp among a session's stored events (0 if none),
    /// read from the envelope JSON. Used to assign a **causal** clock to newly
    /// authored events: strictly greater than everything this device has seen —
    /// locally authored *or* ingested from a peer — so causal order is preserved
    /// across devices, not just a per-device authoring count.
    pub fn max_lamport(&self, session_id: Uuid) -> Result<u64> {
        let max: Option<i64> = self
            .conn
            .query_row(
                "SELECT MAX(json_extract(envelope_json, '$.lamport')) FROM events WHERE session_id = ?1",
                [session_id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten();
        Ok(max.unwrap_or(0).max(0) as u64)
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

    /// The envelopes that feed the trust roster (membership, account keys and
    /// their rotations, device certificates and revocations, snapshots), across
    /// all sessions. Cheap — filtered by `kind`. Consumed by
    /// `envelope_verifier::build_roster` to verify signatures on ingest without
    /// any network. Revocations and rotations must be here: a `DeviceRevoked`
    /// pulled in an earlier batch is what keeps a revoked device's later events
    /// out.
    pub fn roster_envelopes(&self) -> Result<Vec<SessionEventEnvelope>> {
        self.query_envelopes(
            "SELECT envelope_json FROM events \
             WHERE kind IN ('accountKeyRegistered','accountKeyRotated','deviceCertificateAdded','deviceRevoked',\
             'memberAdded','memberRemoved','sessionSnapshot') \
             ORDER BY row_id ASC",
            [],
        )
    }

    /// A workspace's whole trust history — account keys (registered/rotated),
    /// device certificates and revocations, and membership — for folding its
    /// [`crate::envelope_verifier::WorkspaceRoster`] without the rest of the log.
    pub fn trust_envelopes(&self, workspace_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        self.query_envelopes(
            "SELECT envelope_json FROM events WHERE workspace_id = ?1 \
             AND kind IN ('accountKeyRegistered','accountKeyRotated','deviceCertificateAdded','deviceRevoked',\
             'memberAdded','memberRemoved','sessionSnapshot') \
             ORDER BY row_id ASC",
            [workspace_id.to_string()],
        )
    }

    /// A workspace's account keys and device certificates (registrations,
    /// rotations, certs), for checking a device belongs to an account without
    /// folding the whole trust roster. Cheap: filtered by workspace and kind.
    pub fn identity_envelopes(&self, workspace_id: Uuid) -> Result<Vec<SessionEventEnvelope>> {
        self.query_envelopes(
            "SELECT envelope_json FROM events WHERE workspace_id = ?1 \
             AND kind IN ('accountKeyRegistered','accountKeyRotated','deviceCertificateAdded') \
             ORDER BY row_id ASC",
            [workspace_id.to_string()],
        )
    }

    /// The highest row id stored (0 when empty): a cursor for "what arrived since".
    pub fn max_row_id(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COALESCE(MAX(row_id), 0) FROM events", [], |row| row.get(0))?)
    }

    /// Sessions that gained a message after row `after` (see [`Self::max_row_id`]),
    /// for answering what a sync pull brought in.
    pub fn sessions_with_messages_after(&self, after: i64) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT session_id FROM events WHERE row_id > ?1 AND kind = 'messageAppended'",
        )?;
        let rows = stmt.query_map([after], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for id in rows {
            if let Ok(id) = Uuid::parse_str(&id?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// The messages appended after row `after`, as `(session, message, signing
    /// device)` in arrival order: what one sync pull brought in, message by
    /// message. A pull can carry several user turns for one chat (two posts a
    /// few ms apart); dispatch must consider each of them, not only the
    /// chat's last. The signer lets the caller skip its own device's posts.
    pub fn messages_after(&self, after: i64) -> Result<Vec<(Uuid, Uuid, Option<Uuid>)>> {
        Ok(self
            .query_envelopes(
                "SELECT envelope_json FROM events WHERE row_id > ?1 AND kind = 'messageAppended' ORDER BY row_id ASC",
                [after],
            )?
            .into_iter()
            .filter_map(|e| match e.payload {
                SessionEvent::MessageAppended { message } => Some((e.session_id, message.id, e.signer_device_id)),
                _ => None,
            })
            .collect())
    }

    /// Whether this workspace already carries a device certificate binding
    /// `device_id` to `account_id` — so identity publication stays idempotent
    /// (and re-publishes once when the account id changes, e.g. GitHub sign-in).
    pub fn has_device_certificate(
        &self,
        workspace_id: Uuid,
        device_id: Uuid,
        account_id: Uuid,
    ) -> Result<bool> {
        let envs = self.query_envelopes(
            "SELECT envelope_json FROM events WHERE workspace_id = ?1 AND kind = 'deviceCertificateAdded'",
            [workspace_id.to_string()],
        )?;
        Ok(envs.iter().any(|e| {
            matches!(&e.payload, SessionEvent::DeviceCertificateAdded { certificate }
                if certificate.device_id == device_id && certificate.account_id == account_id)
        }))
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

    /// Every envelope stored after row `after`, in row order, with the highest
    /// row id seen (`after` when there are none) — the incremental push scan.
    /// Row ids only grow (AUTOINCREMENT, one writer at a time), so a later call
    /// from the returned row id never misses a row committed since.
    pub fn envelopes_after_row(&self, after: i64) -> Result<(i64, Vec<SessionEventEnvelope>)> {
        let mut stmt = self
            .conn
            .prepare("SELECT row_id, envelope_json FROM events WHERE row_id > ?1 ORDER BY row_id ASC")?;
        let rows = stmt.query_map([after], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?;
        let mut last = after;
        let mut out = Vec::new();
        for row in rows {
            let (row_id, json) = row?;
            last = last.max(row_id);
            match serde_json::from_str::<SessionEventEnvelope>(&json) {
                Ok(env) => out.push(env),
                Err(e) => tracing::warn!(error = %e, "skipping unparseable event row during push scan"),
            }
        }
        Ok((last, out))
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

    // --- Durable sync state (#2): survives process restarts so the sync
    // engine resumes its fetch cursor + push-dedup instead of restarting from
    // scratch. Keyed by the relay `workspace`/room string. ---

    /// The durable fetch cursor for a workspace — the highest relay server
    /// sequence applied so far (0 if the workspace has never synced).
    pub fn sync_cursor(&self, workspace: &str) -> Result<u64> {
        let seq: Option<i64> = self
            .conn
            .query_row(
                "SELECT last_fetched_seq FROM sync_cursor WHERE workspace = ?1",
                [workspace],
                |row| row.get(0),
            )
            .optional()?;
        Ok(seq.unwrap_or(0).max(0) as u64)
    }

    /// Persist the fetch cursor for a workspace (upsert).
    pub fn set_sync_cursor(&self, workspace: &str, last_fetched_seq: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_cursor (workspace, last_fetched_seq) VALUES (?1, ?2) \
             ON CONFLICT(workspace) DO UPDATE SET last_fetched_seq = excluded.last_fetched_seq",
            rusqlite::params![workspace, last_fetched_seq as i64],
        )?;
        Ok(())
    }

    /// Mark an event as already on the relay for a workspace (idempotent), so a
    /// later `take_unpushed` — even after a restart — never re-pushes it.
    pub fn mark_pushed(&self, workspace: &str, event_id: Uuid) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO pushed (workspace, event_id) VALUES (?1, ?2)",
            rusqlite::params![workspace, event_id.to_string()],
        )?;
        Ok(())
    }

    /// Every event_id already pushed for a workspace — used to seed the sync
    /// engine's in-memory dedup cache on construction.
    pub fn pushed_ids(&self, workspace: &str) -> Result<Vec<Uuid>> {
        let mut stmt = self
            .conn
            .prepare("SELECT event_id FROM pushed WHERE workspace = ?1")?;
        let rows = stmt.query_map([workspace], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for s in rows {
            if let Ok(id) = Uuid::parse_str(&s?) {
                out.push(id);
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
        self.delete_compaction(session_id, None)?;
        Ok(n)
    }

    // --- Device-local compaction state (`compaction_state`). Not part of the
    // event log: nothing here is synced or projected. ---

    /// The stored compaction state for a chat's responder runtime, if any. A
    /// row that no longer parses is treated as absent.
    pub fn compaction(&self, session_id: Uuid, runtime_id: &str) -> Result<Option<StoredCompaction>> {
        let row = self
            .conn
            .query_row(
                "SELECT boundary, covered_json, fingerprint, summary FROM compaction_state \
                 WHERE session_id = ?1 AND runtime_id = ?2",
                rusqlite::params![session_id.to_string(), runtime_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((boundary, covered_json, fingerprint, summary)) = row else {
            return Ok(None);
        };
        let boundary = match boundary.as_deref().map(Uuid::parse_str) {
            None => None,
            Some(Ok(id)) => Some(id),
            Some(Err(_)) => return Ok(None),
        };
        let Ok(covered) = serde_json::from_str::<Vec<Uuid>>(&covered_json) else {
            return Ok(None);
        };
        Ok(Some(StoredCompaction { boundary, covered, fingerprint, summary }))
    }

    /// Save (replace) a chat responder's compaction state.
    pub fn save_compaction(&self, session_id: Uuid, runtime_id: &str, state: &StoredCompaction) -> Result<()> {
        self.conn.execute(
            "INSERT INTO compaction_state (session_id, runtime_id, boundary, covered_json, fingerprint, summary) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(session_id, runtime_id) DO UPDATE SET boundary = excluded.boundary, \
             covered_json = excluded.covered_json, fingerprint = excluded.fingerprint, summary = excluded.summary",
            rusqlite::params![
                session_id.to_string(),
                runtime_id,
                state.boundary.map(|b| b.to_string()),
                serde_json::to_string(&state.covered)?,
                state.fingerprint,
                state.summary,
            ],
        )?;
        Ok(())
    }

    /// Forget a chat's compaction state: one responder runtime's, or every
    /// runtime's when `runtime_id` is `None`.
    pub fn delete_compaction(&self, session_id: Uuid, runtime_id: Option<&str>) -> Result<()> {
        match runtime_id {
            Some(rt) => self.conn.execute(
                "DELETE FROM compaction_state WHERE session_id = ?1 AND runtime_id = ?2",
                rusqlite::params![session_id.to_string(), rt],
            )?,
            None => self
                .conn
                .execute("DELETE FROM compaction_state WHERE session_id = ?1", [session_id.to_string()])?,
        };
        Ok(())
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
        self.load_session_with_founder(session_id, None, None)
    }

    /// [`load_session`], but pinned to a `trusted_founder` — the out-of-band
    /// workspace-founder account id from the `hivews1:` invite (B2 residual fix).
    /// Projection then rejects any base seed not authored by that founder, so a
    /// forged low-lamport `SessionSnapshot` from another keyholder cannot displace
    /// the founder's seed. `None` is exactly [`load_session`] (legacy / local /
    /// no-invite). `ChatService::load` supplies its pinned founder here.
    pub fn load_session_with_founder(
        &self,
        session_id: Uuid,
        trusted_founder: Option<&str>,
        // The device-local workspace id, if this session belongs to it — its events
        // skip ingest-time authz (private, single-device; see project_with_options).
        trusted_workspace: Option<Uuid>,
    ) -> Result<Option<ChatSession>> {
        // Project the full stream. `project_with_founder` folds in canonical
        // (lamport, event_id) order and selects the base by a deterministic rule
        // (+ the fixed founder string), so the result is independent of *this
        // device's* ingestion order. A row_id slice from the latest snapshot would
        // feed a different subset when a snapshot is ingested after its own deltas
        // — reintroducing cross-device divergence. Bounded-cost compaction
        // (canonical lamport-ordered slicing) is a follow-up; correctness first.
        let sig: RowsSig = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(MAX(row_id), 0) FROM events WHERE session_id = ?1",
            [session_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let key: ProjectionKey = (session_id, trusted_founder.map(str::to_string), trusted_workspace);
        if let Some((cached, session)) = self.projections.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            if *cached == sig {
                return Ok(session.clone());
            }
        }
        let envelopes = self.query_envelopes(
            "SELECT envelope_json FROM events WHERE session_id = ?1 ORDER BY row_id ASC",
            [session_id.to_string()],
        )?;
        let session = project_with_options(&envelopes, trusted_founder, trusted_workspace);
        let mut cache = self.projections.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= PROJECTION_CACHE_MAX && !cache.contains_key(&key) {
            cache.clear();
        }
        cache.insert(key, (sig, session.clone()));
        Ok(session)
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

    /// Workspaces that hold events but aren't `keep` and that `own_device`
    /// never signed an event in, with their event counts — what
    /// [`purge_foreign_workspaces`](Self::purge_foreign_workspaces) would drop.
    /// The nil workspace (legacy local events) is never foreign.
    pub fn foreign_workspaces(&self, keep: &HashSet<Uuid>, own_device: Uuid) -> Result<Vec<(Uuid, usize)>> {
        let counts: Vec<(String, i64)> = {
            let mut stmt = self.conn.prepare("SELECT workspace_id, COUNT(*) FROM events GROUP BY workspace_id")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        // The device id as the envelope JSON spells it (`"signerDeviceId":"…"`).
        let signed_here = format!("\"signerDeviceId\":\"{own_device}\"");
        let mut out = Vec::new();
        for (raw, n) in counts {
            let Ok(ws) = Uuid::parse_str(&raw) else { continue };
            if ws.is_nil() || keep.contains(&ws) {
                continue;
            }
            let own: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM events WHERE workspace_id = ?1 AND instr(envelope_json, ?2) > 0",
                rusqlite::params![raw, signed_here],
                |r| r.get(0),
            )?;
            if own == 0 {
                out.push((ws, n as usize));
            }
        }
        Ok(out)
    }

    /// One-time cleanup after the room-scoping fix: delete every event of a
    /// workspace this device doesn't belong to (not in `keep`) and never
    /// authored anything in — events an older client pushed into a shared room
    /// regardless of their workspace. A workspace with any event signed by
    /// `own_device` is kept, so nothing this device wrote is ever dropped.
    ///
    /// With `backup`, the whole database is first copied there (`VACUUM INTO`)
    /// and nothing is deleted unless that copy succeeds. The sync bookkeeping
    /// (cursor, pushed set) of a room whose workspace is dropped goes too, so
    /// re-joining that room later pulls it from the start. Returns the dropped
    /// workspaces with their event counts (empty = nothing to do, no backup).
    pub fn purge_foreign_workspaces(
        &mut self,
        keep: &HashSet<Uuid>,
        own_device: Uuid,
        backup: Option<&Path>,
    ) -> Result<Vec<(Uuid, usize)>> {
        let doomed = self.foreign_workspaces(keep, own_device)?;
        if doomed.is_empty() {
            return Ok(doomed);
        }
        if let Some(path) = backup {
            self.conn.execute("VACUUM INTO ?1", [path.to_string_lossy()])?;
        }
        let rooms: Vec<String> = {
            let mut stmt = self.conn.prepare("SELECT workspace FROM sync_cursor UNION SELECT DISTINCT workspace FROM pushed")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let tx = self.conn.transaction()?;
        for (ws, _) in &doomed {
            let ws_s = ws.to_string();
            tx.execute(
                "DELETE FROM compaction_state WHERE session_id IN (SELECT DISTINCT session_id FROM events WHERE workspace_id = ?1)",
                [&ws_s],
            )?;
            tx.execute(
                "DELETE FROM pushed WHERE event_id IN (SELECT event_id FROM events WHERE workspace_id = ?1)",
                [&ws_s],
            )?;
            tx.execute("DELETE FROM events WHERE workspace_id = ?1", [&ws_s])?;
            for room in rooms.iter().filter(|r| hive_core::room_workspace_id(r) == *ws) {
                tx.execute("DELETE FROM sync_cursor WHERE workspace = ?1", [room])?;
                tx.execute("DELETE FROM pushed WHERE workspace = ?1", [room])?;
            }
        }
        tx.commit()?;
        self.projections.lock().unwrap_or_else(|p| p.into_inner()).clear();
        Ok(doomed)
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
    fn messages_after_lists_every_message_a_pull_added_in_order() {
        let (mut store, sid, wid) = seeded_store();
        let before = store.max_row_id().unwrap();
        let a = ChatMessage::new(MessageRole::User, "Alex", "@hive status?");
        let b = ChatMessage::new(MessageRole::User, "Alex", "@coder run the tests");
        let (a_id, b_id) = (a.id, b.id);
        store.append(sid, wid, SessionEvent::MessageAppended { message: a }).unwrap();
        store.append(sid, wid, SessionEvent::SessionTitleChanged { title: "x".into() }).unwrap();
        store.append(sid, wid, SessionEvent::MessageAppended { message: b }).unwrap();
        assert_eq!(store.messages_after(before).unwrap(), vec![(sid, a_id, None), (sid, b_id, None)]);
        assert!(store.messages_after(store.max_row_id().unwrap()).unwrap().is_empty());
    }

    #[test]
    fn max_lamport_reflects_stored_events_including_ingested() {
        let (mut store, sid, wid) = seeded_store(); // snapshot @ lamport 1
        for l in [5i64, 100, 30] {
            let mut e = SessionEventEnvelope::new(
                sid,
                wid,
                l,
                SessionEvent::SessionTitleChanged { title: format!("t{l}") },
            );
            e.lamport = l as u64;
            store.ingest(&e).unwrap();
        }
        assert_eq!(store.max_lamport(sid).unwrap(), 100, "max over authored + ingested");
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

    /// The projection cache never serves a stale session: an append (also one
    /// made through another connection to the same file) or a delete re-folds.
    #[test]
    fn cached_projection_follows_appends_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hive.db");
        let mut store = EventStore::open(&path).unwrap();
        let session = ChatSession::new("Demo", Uuid::nil(), "anthropic");
        let (sid, wid) = (session.id, session.workspace_id);
        store.append(sid, wid, SessionEvent::SessionSnapshot { session: Box::new(session) }).unwrap();
        let msg = |body: &str| SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "Mara", body) };
        store.append(sid, wid, msg("one")).unwrap();
        assert_eq!(store.load_session(sid).unwrap().unwrap().messages.len(), 1);
        assert_eq!(store.load_session(sid).unwrap().unwrap().messages.len(), 1, "cache hit");

        let mut other = EventStore::open(&path).unwrap();
        other.append(sid, wid, msg("two")).unwrap();
        let bodies: Vec<_> = store.load_session(sid).unwrap().unwrap().messages.into_iter().map(|m| m.body).collect();
        assert_eq!(bodies, ["one", "two"], "a write through another connection is seen");

        store.delete_session(sid).unwrap();
        assert!(store.load_session(sid).unwrap().is_none(), "a delete is seen");
    }

    #[test]
    fn envelopes_after_row_returns_only_newer_rows() {
        let (mut store, sid, wid) = seeded_store();
        let mark = store.max_row_id().unwrap();
        assert_eq!(store.envelopes_after_row(mark).unwrap(), (mark, vec![]));
        let e = store
            .append(sid, wid, SessionEvent::SessionTitleChanged { title: "t".into() })
            .unwrap();
        let (last, envs) = store.envelopes_after_row(mark).unwrap();
        assert_eq!(last, mark + 1);
        assert_eq!(envs.iter().map(|e| e.event_id).collect::<Vec<_>>(), [e.event_id]);
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

        // One event (the "after" message) has landed since the snapshot, and a
        // full projection still reconstructs the complete state.
        assert_eq!(store.rows_since_last_snapshot(sid).unwrap(), 1);
        let s = store.load_session(sid).unwrap().unwrap();
        assert_eq!(s.messages.len(), 4);
        assert_eq!(s.messages[3].body, "after");
    }

    #[test]
    fn sync_cursor_and_pushed_are_durable_per_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.db");
        let (a, b) = ("room-a", "room-b");
        let e1 = Uuid::new_v4();
        let e2 = Uuid::new_v4();
        {
            let store = EventStore::open(&path).unwrap();
            assert_eq!(store.sync_cursor(a).unwrap(), 0, "unknown workspace starts at 0");
            store.set_sync_cursor(a, 42).unwrap();
            store.set_sync_cursor(a, 99).unwrap(); // upsert
            store.set_sync_cursor(b, 7).unwrap();
            store.mark_pushed(a, e1).unwrap();
            store.mark_pushed(a, e1).unwrap(); // idempotent
            store.mark_pushed(a, e2).unwrap();
        }
        // Reopen: state survived the process boundary.
        let store = EventStore::open(&path).unwrap();
        assert_eq!(store.sync_cursor(a).unwrap(), 99, "cursor persisted + upserted");
        assert_eq!(store.sync_cursor(b).unwrap(), 7, "keyed per workspace");
        let mut ids = store.pushed_ids(a).unwrap();
        ids.sort();
        let mut want = vec![e1, e2];
        want.sort();
        assert_eq!(ids, want, "pushed set persisted, deduped");
        assert!(store.pushed_ids(b).unwrap().is_empty(), "pushed set is per workspace");
    }

    #[test]
    fn compaction_state_survives_reopen_is_keyed_per_runtime_and_stays_out_of_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.db");
        let sid = Uuid::new_v4();
        let other = Uuid::new_v4();
        let state = StoredCompaction {
            boundary: Some(Uuid::new_v4()),
            covered: vec![Uuid::new_v4(), Uuid::new_v4()],
            fingerprint: "abc".into(),
            summary: "They agreed on X.".into(),
        };
        {
            let store = EventStore::open(&path).unwrap();
            assert_eq!(store.compaction(sid, "gateway").unwrap(), None);
            store.save_compaction(sid, "gateway", &state).unwrap();
            let mut first = state.clone();
            first.summary = "older".into();
            store.save_compaction(sid, "hosted", &first).unwrap();
            store.save_compaction(other, "gateway", &first).unwrap();
            // Upsert.
            store.save_compaction(sid, "hosted", &StoredCompaction { boundary: None, ..state.clone() }).unwrap();
        }
        let store = EventStore::open(&path).unwrap();
        assert_eq!(store.compaction(sid, "gateway").unwrap().as_ref(), Some(&state), "survived a restart");
        assert_eq!(store.compaction(sid, "hosted").unwrap().unwrap().boundary, None);
        // Device-local: nothing reached the event log.
        assert!(store.list(sid).unwrap().is_empty());
        assert!(store.list_session_ids().unwrap().is_empty());

        store.delete_compaction(sid, Some("hosted")).unwrap();
        assert!(store.compaction(sid, "hosted").unwrap().is_none());
        assert!(store.compaction(sid, "gateway").unwrap().is_some());
        // Deleting the chat drops its state; other chats keep theirs.
        store.delete_session(sid).unwrap();
        assert!(store.compaction(sid, "gateway").unwrap().is_none());
        assert!(store.compaction(other, "gateway").unwrap().is_some());
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

    /// The cleanup for events an older client pushed into a room regardless of
    /// their workspace: another workspace's events go (after a backup), this
    /// device's own work never does.
    #[test]
    fn purge_drops_only_foreign_workspaces_after_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = EventStore::open(dir.path().join("hive.db")).unwrap();
        let me = Uuid::new_v4();
        let room = "room-alpha";
        let room_ws = hive_core::room_workspace_id(room);
        let leaked_room = "room-beta";
        let leaked_ws = hive_core::room_workspace_id(leaked_room);
        let other_local = Uuid::new_v4();
        let mine_elsewhere = Uuid::new_v4();
        let msg = |ws: Uuid, signer: Option<Uuid>| {
            let mut e = SessionEventEnvelope::new(
                Uuid::new_v4(),
                ws,
                1,
                SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "Alex", "hi") },
            );
            e.signer_device_id = signer;
            e
        };
        for e in [
            msg(room_ws, Some(Uuid::new_v4())),
            msg(Uuid::nil(), None),
            msg(leaked_ws, Some(Uuid::new_v4())),
            msg(leaked_ws, None),
            msg(other_local, Some(Uuid::new_v4())),
            msg(mine_elsewhere, Some(me)),
        ] {
            store.ingest(&e).unwrap();
        }
        store.set_sync_cursor(leaked_room, 7).unwrap();
        store.set_sync_cursor(room, 9).unwrap();

        let keep = HashSet::from([room_ws]);
        let mut found = store.foreign_workspaces(&keep, me).unwrap();
        found.sort();
        let mut want = vec![(leaked_ws, 2), (other_local, 1)];
        want.sort();
        assert_eq!(found, want);

        let backup = dir.path().join("hive.db.bak");
        let mut dropped = store.purge_foreign_workspaces(&keep, me, Some(&backup)).unwrap();
        dropped.sort();
        assert_eq!(dropped, want);
        assert_eq!(store.max_row_id().unwrap(), 6);
        let left: i64 = store.conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 3, "the room's event, the legacy nil one, and this device's own");
        assert_eq!(store.sync_cursor(leaked_room).unwrap(), 0, "a dropped room re-pulls from the start");
        assert_eq!(store.sync_cursor(room).unwrap(), 9);

        // The backup holds everything, and a second run is a no-op.
        let copy = EventStore::open(&backup).unwrap();
        let all: i64 = copy.conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(all, 6);
        assert!(store.purge_foreign_workspaces(&keep, me, Some(&dir.path().join("again"))).unwrap().is_empty());
        assert!(!dir.path().join("again").exists());
    }
}
