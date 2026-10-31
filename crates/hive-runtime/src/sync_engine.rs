//! Workspace sync engine — the app-level loop that makes the relay actually
//! converge two clients (follow-up #125). Pushes new local envelopes to the
//! relay and ingests remote ones into the local event store, deduped by
//! `event_id`. Ported from the sync coordination in `WorkspaceSyncServices.swift`.
//!
//! This is the relay-forwarding sync path; direct P2P (STUN/hole-punch) and a
//! true CRDT are still tracked follow-ups. Ordering is by the relay's server
//! sequence on fetch + local ingestion order, which the commutative projector
//! (upsert-by-id, idempotent reactions) tolerates.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use hive_core::{open_symmetric, seal_symmetric, SealedEnvelope, SessionEventEnvelope};
use serde_json::Value;
use uuid::Uuid;

use crate::event_store::EventStore;
use crate::envelope_verifier::{build_roster, verdict_for};
use crate::identity_verifier::{gate_ingest, shared_cache, IdentityMode, IngestAction, SharedIdentityCache};
use crate::relay_client::{RelayClient, RelayError, ENVELOPE_PAGE_LIMIT};

/// Most relay pages one [`SyncEngine::pull_round`] fetches and applies. A room
/// with more history than this catches up over several rounds
/// ([`SyncEngine::is_behind`] says another round is due), so one round never
/// holds the store or memory for an unbounded download.
pub const PULL_PAGES_PER_ROUND: usize = 8;

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
    /// A relay push was attempted with no workspace key. E2EE is mandatory for
    /// relay sync — we refuse to send plaintext to the relay rather than
    /// silently degrading (B4). The caller must configure a key or stay
    /// local-only. No plaintext value is ever produced in this case.
    #[error("refusing to push plaintext to the relay: no workspace key configured (E2EE is mandatory for relay sync)")]
    RelayRequiresEncryption,
}

/// Drives one workspace's relay sync. Holds cursors so repeated rounds only
/// move new data.
pub struct SyncEngine {
    relay: RelayClient,
    workspace: String,
    /// The workspace ids whose events belong in this room — by default just
    /// [`hive_core::room_workspace_id`] of the room. The store is shared by every
    /// workspace on the device (its local workspace, each joined room), so the
    /// push scan only sends events stamped with one of these ids, and a fetched
    /// event stamped with any other id is dropped rather than ingested.
    scope: HashSet<Uuid>,
    /// Test-only: admit every workspace (fixtures that predate room scoping).
    #[cfg(test)]
    any_workspace: bool,
    /// event_ids already pushed (or ingested from the relay) — never re-push.
    /// This is a **fast-path cache** layered over the durable `pushed` table in
    /// the event store: on first use it is seeded from that table (so a restart
    /// resumes dedup), and it grows with this session's activity. The durable
    /// store is the source of truth; this set is bounded by the number of events
    /// in the workspace (same order as the durable set).
    seen: HashSet<Uuid>,
    /// Highest store row the push scan has looked at; 0 = scan everything.
    /// Reset on a failed push so the rolled-back envelopes are found again.
    scanned_row: i64,
    /// highest relay server sequence fetched so far. Loaded from the durable
    /// `sync_cursor` on first use and persisted whenever it advances, so a
    /// restart resumes from the durable cursor instead of 0.
    last_fetched_seq: u64,
    /// Where the next page starts while catching up across pages. It runs ahead
    /// of `last_fetched_seq` when a not-yet-openable body holds the durable
    /// cursor back, so paging still reaches the head; once caught up it resets
    /// and the next round re-fetches from the durable cursor (the retry point).
    page_after: Option<u64>,
    /// The relay said more envelopes remain past the last page fetched.
    behind: bool,
    /// Lowest held (not-yet-openable / identity-pending) seq seen on an earlier
    /// page of the current multi-page walk. `apply_fetched` caps the cursor
    /// below it, so a later page that holds nothing can't advance the durable
    /// cursor past a held body on an earlier page (which would skip it).
    walk_hold: Option<u64>,
    /// Envelopes requested per page.
    page_limit: u32,
    /// Whether durable sync state (cursor + pushed set) has been loaded from the
    /// store yet. Guards a one-time seed on the first store-bearing call.
    loaded: bool,
    /// event_ids confirmed on the relay (pushed this session, or ingested from
    /// it) that still need to be written to the durable `pushed` table. Flushed
    /// by any store-bearing call. A **failed** push never lands here, preserving
    /// the partial-push rollback invariant.
    pending: Vec<Uuid>,
    /// Key epoch → workspace key. New events are sealed under the **highest**
    /// epoch; a fetched body is opened with the key for **its** epoch (stamped on
    /// the `SealedEnvelope`). Retaining older epochs is what keeps history
    /// readable across rotations. Empty ⇒ no key: relay pushes are **refused**
    /// (E2EE is mandatory on the wire — see [`SyncEngine::encode`]).
    keyring: BTreeMap<u32, [u8; 32]>,
    /// Identity-verification mode (security plan S1). `Relay` (Option A, the
    /// default) trusts the roster's account keys as-is — **current behavior**.
    /// `GithubSigningKeys` (Option C) additionally requires each account key to
    /// be verified against GitHub `ssh_signing_keys` before its events project.
    identity_mode: IdentityMode,
    /// GitHub-identity verdict cache (Option C). Populated out-of-band by an async
    /// task that hits GitHub; read synchronously on ingest. Unused in `Relay` mode.
    identity_cache: SharedIdentityCache,
}

/// Outcome of decoding one fetched relay body. Separating the two failure
/// classes is what keeps a single bad body from halting the stream:
/// - `Malformed` bodies are **provably** un-openable (bad JSON, or an AEAD tag
///   that fails under a key we DO hold = tampered/corrupt/wrong-key). Re-fetching
///   can't help, so the cursor advances past them.
/// - `NotYetOpenable` bodies are well-formed but sealed under an epoch key we
///   don't hold *yet* (a lagging rotation, or a foreign-room post). A later key
///   might open them, so the cursor is held just before them to retry — while
///   later openable events are still applied this pass.
enum Decoded {
    Ready(Box<SessionEventEnvelope>),
    Malformed(String),
    NotYetOpenable(String),
}

impl SyncEngine {
    pub fn new(relay: RelayClient, workspace: impl Into<String>) -> Self {
        let workspace = workspace.into();
        let scope = HashSet::from([hive_core::room_workspace_id(&workspace)]);
        Self {
            relay,
            workspace,
            scope,
            #[cfg(test)]
            any_workspace: false,
            seen: HashSet::new(),
            scanned_row: 0,
            last_fetched_seq: 0,
            page_after: None,
            behind: false,
            walk_hold: None,
            page_limit: ENVELOPE_PAGE_LIMIT,
            loaded: false,
            pending: Vec::new(),
            keyring: BTreeMap::new(),
            identity_mode: IdentityMode::from_env(),
            identity_cache: shared_cache(),
        }
    }

    /// Scope this room to `workspace_id` instead of the id derived from the room
    /// string. For a room whose events carry an id that isn't
    /// `room_workspace_id(room)` (tests, older fixtures). Exactly one workspace:
    /// a room never mixes workspaces.
    pub fn with_room_workspace(mut self, workspace_id: Uuid) -> Self {
        self.scope = HashSet::from([workspace_id]);
        self
    }

    /// Whether an event stamped `workspace_id` belongs in this room.
    pub fn in_scope(&self, workspace_id: Uuid) -> bool {
        #[cfg(test)]
        if self.any_workspace {
            return true;
        }
        self.scope.contains(&workspace_id)
    }

    /// Test-only: an engine that admits every workspace, for fixtures that
    /// stamp events with arbitrary workspace ids.
    #[cfg(test)]
    pub(crate) fn unscoped(relay: RelayClient, workspace: impl Into<String>) -> Self {
        let mut e = Self::new(relay, workspace);
        e.any_workspace = true;
        e
    }

    /// Override the identity-verification mode (default: [`IdentityMode::from_env`]).
    /// Mainly for tests and explicit opt-in wiring.
    pub fn with_identity_mode(mut self, mode: IdentityMode) -> Self {
        self.identity_mode = mode;
        self
    }

    /// Share a specific GitHub-identity verdict cache — so the app's async
    /// GitHub-verification task and this engine read/write the same cache.
    pub fn with_identity_cache(mut self, cache: SharedIdentityCache) -> Self {
        self.identity_cache = cache;
        self
    }

    /// The shared GitHub-identity verdict cache, for an out-of-band async task to
    /// populate (Option C). In `Relay` mode it is simply never consulted.
    pub fn identity_cache(&self) -> SharedIdentityCache {
        Arc::clone(&self.identity_cache)
    }

    /// Enable E2EE with a single base-epoch (0) key — the passphrase-derived path.
    pub fn with_key(mut self, key: [u8; 32]) -> Self {
        self.keyring.insert(0, key);
        self
    }

    /// Enable E2EE with a full epoch→key ring (base key + opened rotations).
    pub fn with_keyring(mut self, keyring: BTreeMap<u32, [u8; 32]>) -> Self {
        self.keyring = keyring;
        self
    }

    /// Encode an envelope for the relay: sealed ciphertext under the current
    /// (highest-epoch) key. With **no** key this **refuses** with
    /// [`SyncError::RelayRequiresEncryption`] instead of emitting plaintext —
    /// E2EE is mandatory for relay sync, so the relay never sees cleartext
    /// bodies, the member roster, or inline credentials (B4).
    fn encode(&self, env: &SessionEventEnvelope) -> Result<Value, SyncError> {
        // Seal under the highest epoch we hold — the current key.
        match self.keyring.iter().next_back() {
            Some((&version, k)) => {
                let plain = serde_json::to_vec(env)?;
                Ok(serde_json::to_value(seal_symmetric(k, version, &plain)?)?)
            }
            // No key: never originate plaintext to a relay. Refuse the push.
            None => Err(SyncError::RelayRequiresEncryption),
        }
    }

    /// Decode a relay body back into an envelope, classifying failures so a bad
    /// body can never halt the stream (see [`Decoded`]). A sealed body is opened
    /// with the key for **its** epoch.
    fn decode(&self, body: &Value) -> Decoded {
        if body.get("ciphertext").is_some() {
            // Claims to be sealed. If it doesn't even parse as a SealedEnvelope
            // it's provably garbage (e.g. a hostile `{"ciphertext":1}`) — advance.
            let sealed: SealedEnvelope = match serde_json::from_value(body.clone()) {
                Ok(s) => s,
                Err(e) => return Decoded::Malformed(format!("not a sealed envelope: {e}")),
            };
            // Well-formed, but we don't hold this epoch's key yet (lagging
            // rotation, or a foreign-room post): a later key might open it → retry.
            let Some(k) = self.keyring.get(&sealed.version) else {
                return Decoded::NotYetOpenable(format!("no key for epoch {}", sealed.version));
            };
            match open_symmetric(k, &sealed) {
                Ok(plain) => match serde_json::from_slice(&plain) {
                    Ok(env) => Decoded::Ready(Box::new(env)),
                    Err(e) => Decoded::Malformed(format!("opened but not an envelope: {e}")),
                },
                // We hold this epoch's key yet the AEAD tag fails to verify: the
                // body is tampered/corrupt (or foreign ciphertext colliding on
                // this epoch, incl. a wrong workspace key). Re-fetching can't
                // help → advance past it.
                Err(_) => Decoded::Malformed(format!(
                    "AEAD open failed at epoch {} (tampered/corrupt or wrong key)",
                    sealed.version
                )),
            }
        } else {
            match serde_json::from_value(body.clone()) {
                Ok(env) => Decoded::Ready(Box::new(env)),
                Err(e) => Decoded::Malformed(format!("not a session envelope: {e}")),
            }
        }
    }

    /// Push every local envelope (across all sessions) not yet pushed. Returns
    /// how many were sent.
    pub async fn push_new(&mut self, store: &EventStore) -> Result<usize, SyncError> {
        let to_push = self.take_unpushed(store)?;
        let res = self.push_envelopes(&to_push).await;
        // Persist whatever actually reached the relay — even on a partial
        // failure the delivered prefix is recorded in `pending`, so flushing
        // here durably dedups it. The failed tail was rolled back out of both
        // `seen` and `pending`, so it is never marked pushed.
        self.persist_pending(store)?;
        res?;
        Ok(to_push.len())
    }

    /// Fetch remote envelopes past the cursor and ingest the unseen ones, one
    /// bounded round (see [`SyncEngine::pull_round`]). Returns how many were
    /// newly applied.
    pub async fn pull(&mut self, store: &mut EventStore) -> Result<usize, SyncError> {
        self.pull_round(store).await
    }

    /// Fetch and apply up to [`PULL_PAGES_PER_ROUND`] pages, each applied (and
    /// its cursor persisted) before the next is fetched, so progress survives an
    /// interruption and only one page is in memory at a time. Stops early once
    /// caught up; otherwise [`SyncEngine::is_behind`] stays true and the caller
    /// should run another round soon.
    pub async fn pull_round(&mut self, store: &mut EventStore) -> Result<usize, SyncError> {
        // Load the durable cursor before fetching so a fresh engine resumes from
        // it rather than re-fetching the whole room from 0.
        self.ensure_loaded(store)?;
        let mut applied = 0;
        for _ in 0..PULL_PAGES_PER_ROUND {
            let fetched = self.fetch_new().await?;
            applied += self.apply_fetched(store, &fetched)?;
            if !self.behind {
                break;
            }
        }
        Ok(applied)
    }

    /// Pull rounds until caught up, or `max_rounds` rounds have run. For
    /// one-shot callers (a CLI sync, a daemon's first sync) that need the whole
    /// room before they act. Returns how many events were newly applied.
    pub async fn catch_up(&mut self, store: &mut EventStore, max_rounds: usize) -> Result<usize, SyncError> {
        let mut applied = self.pull_round(store).await?;
        for _ in 1..max_rounds {
            if !self.behind {
                break;
            }
            applied += self.pull_round(store).await?;
        }
        Ok(applied)
    }

    /// The relay holds envelopes past what this engine has fetched: another
    /// pull round is due.
    pub fn is_behind(&self) -> bool {
        self.behind
    }

    /// Envelopes to request per page (tests use a small value).
    pub fn with_page_limit(mut self, limit: u32) -> Self {
        self.page_limit = limit.max(1);
        self
    }

    /// One-time seed of durable sync state (cursor + pushed set) from the store,
    /// so a freshly-constructed engine resumes rather than restarts. Idempotent;
    /// the cursor is merged with `max` so an in-flight advance is never lost.
    fn ensure_loaded(&mut self, store: &EventStore) -> Result<(), SyncError> {
        if self.loaded {
            return Ok(());
        }
        self.last_fetched_seq = self.last_fetched_seq.max(store.sync_cursor(&self.workspace)?);
        for id in store.pushed_ids(&self.workspace)? {
            self.seen.insert(id);
        }
        self.loaded = true;
        Ok(())
    }

    /// Flush `pending` (event_ids confirmed on the relay) to the durable `pushed`
    /// table. Called by every store-bearing entry point.
    fn persist_pending(&mut self, store: &EventStore) -> Result<(), SyncError> {
        for id in self.pending.drain(..) {
            store.mark_pushed(&self.workspace, id)?;
        }
        Ok(())
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
        // Resume dedup from the durable `pushed` set, and flush any pending
        // durable writes from a prior round (e.g. a push whose cycle bailed
        // before persisting).
        self.ensure_loaded(store)?;
        self.persist_pending(store)?;
        let mut out = Vec::new();
        if self.scanned_row > 0 {
            // Incremental: only rows stored since the last scan. Everything at
            // or below `scanned_row` was already collected (or seen), and a
            // failed push resets the mark, so nothing is skipped.
            let (last, envs) = store.envelopes_after_row(self.scanned_row)?;
            self.scanned_row = last;
            for env in envs {
                if self.in_scope(env.workspace_id) && self.seen.insert(env.event_id) {
                    out.push(env);
                }
            }
            return Ok(out);
        }
        // Full scan (first round, or after a failed push). Read the high-water
        // mark first: a row committed during the scan has a larger id, so the
        // next incremental round finds it (and `seen` dedups any overlap).
        let mark = store.max_row_id()?;
        for session_id in store.list_session_ids()? {
            for env in store.list(session_id)? {
                // Another workspace's event (the device-local workspace, a
                // different room) never leaves for this room.
                if !self.in_scope(env.workspace_id) {
                    continue;
                }
                // `seen` is seeded from the durable `pushed` table, so this skips
                // events already on the relay even across a restart. Newly
                // collected events are marked seen optimistically; a failed push
                // rolls them back (see `push_envelopes`).
                if self.seen.insert(env.event_id) {
                    out.push(env);
                }
            }
        }
        self.scanned_row = mark;
        Ok(out)
    }

    /// Push pre-collected envelopes to the relay, sealing them if a key is set
    /// (async; no store). `take_unpushed` optimistically marked these seen; if a
    /// push fails mid-batch we **roll back** `seen` for the failed envelope and
    /// every one after it, so the next round re-pushes them instead of silently
    /// dropping the tail.
    pub async fn push_envelopes(&mut self, envelopes: &[SessionEventEnvelope]) -> Result<(), SyncError> {
        for (i, env) in envelopes.iter().enumerate() {
            // `encode` refuses when there's no key (E2EE mandatory); roll back
            // the optimistic `seen` marks just like a network failure so the
            // envelopes are re-attempted once a key is configured.
            let body = match self.encode(env) {
                Ok(body) => body,
                Err(e) => {
                    for unsent in &envelopes[i..] {
                        self.seen.remove(&unsent.event_id);
                    }
                    self.scanned_row = 0;
                    return Err(e);
                }
            };
            if let Err(e) = self.relay.push_value(&self.workspace, &body).await {
                for unsent in &envelopes[i..] {
                    self.seen.remove(&unsent.event_id);
                }
                self.scanned_row = 0;
                return Err(e.into());
            }
            // Delivered: queue it for the durable `pushed` table so a restart
            // won't re-push it. Only reached AFTER a successful relay write, so a
            // failed push (handled above) never marks its events pushed.
            self.pending.push(env.event_id);
        }
        Ok(())
    }

    /// Whether relay pushes will be sealed (a key is configured). `false` means
    /// every push would be refused — callers should stay local-only or surface
    /// an actionable "no workspace key" error rather than attempting to sync.
    pub fn is_encrypted(&self) -> bool {
        !self.keyring.is_empty()
    }

    /// Fetch the next page of raw remote bodies (async; no store). The durable
    /// cursor is **not** advanced here — it only moves in `apply_fetched`, so a
    /// body fetched before its decryption key is available can be re-fetched and
    /// isn't lost. One page only: [`SyncEngine::is_behind`] says whether more
    /// remain, and the next call continues after this page.
    ///
    /// Callers that construct a fresh engine must load the durable cursor first
    /// (`take_unpushed` / `pull_round` do), or the first page starts at 0.
    pub async fn fetch_new(&mut self) -> Result<Vec<(u64, Value)>, SyncError> {
        let after = match self.page_after {
            Some(p) if self.behind => p.max(self.last_fetched_seq),
            _ => {
                // A fresh walk from the durable cursor: it re-meets any held body.
                self.walk_hold = None;
                self.last_fetched_seq
            }
        };
        let page = self.relay.fetch_page(&self.workspace, after, self.page_limit).await?;
        self.behind = page.more;
        self.page_after = page.more.then_some(page.next_after);
        Ok(page.rows)
    }

    /// Decode + ingest fetched bodies into the store (sync; no IO awaits), then
    /// advance the cursor. Returns how many were newly applied.
    ///
    /// **Cursor / retry semantics.** A single bad body must never halt the stream
    /// (the old code either aborted the pull on a decode error or `break`ed the
    /// batch, leaving the cursor before the offender so every later pull re-fetched
    /// it forever and applied nothing after it). Instead we classify each body and
    /// keep the cursor **monotonic**, advancing it to the highest CONTIGUOUS
    /// resolved sequence that sits *below the lowest not-yet-openable body*:
    /// - `Malformed`/quarantined bodies are provably bad and stable — we skip them
    ///   and let the cursor advance past them (re-fetching can't help).
    /// - A `NotYetOpenable` body (well-formed sealed body under an epoch key we
    ///   lack) becomes the **retry point**: the cursor is held just before the
    ///   lowest such sequence so a later pull re-fetches it once its key arrives —
    ///   but every openable event *after* it is still applied this pass (deduped,
    ///   idempotent), so a missing-key body can't strand readable events behind it.
    ///
    /// A genuinely un-openable-forever body under a *held* epoch (foreign/tampered
    /// ciphertext) is `Malformed`, so it advances and cannot permanently wedge.
    pub fn apply_fetched(
        &mut self,
        store: &mut EventStore,
        fetched: &[(u64, Value)],
    ) -> Result<usize, SyncError> {
        // Resume the durable cursor + pushed set (so a fresh engine that only
        // pulls still resumes), and flush any pending durable writes.
        self.ensure_loaded(store)?;
        self.persist_pending(store)?;
        let cursor_before = self.last_fetched_seq;
        // Classify every body; never stop early. `resolved` = sequences the cursor
        // may pass (openable or provably-bad); `retry_at` = lowest sequence we must
        // re-fetch later (a not-yet-openable body), which caps how far the cursor
        // may advance this pass.
        let mut decoded: Vec<(u64, SessionEventEnvelope)> = Vec::new();
        let mut resolved: Vec<u64> = Vec::new();
        let mut retry_at: Option<u64> = None;
        let mut foreign = 0usize;
        for (seq, body) in fetched {
            match self.decode(body) {
                // An event of another workspace has no business in this room
                // (an older client pushed everything in its store). Never
                // ingest it, and move past it: it is stable, so re-fetching
                // can't change the verdict.
                Decoded::Ready(env) if !self.in_scope(env.workspace_id) => {
                    foreign += 1;
                    resolved.push(*seq);
                }
                Decoded::Ready(env) => decoded.push((*seq, *env)),
                Decoded::Malformed(reason) => {
                    tracing::warn!(seq = *seq, reason, "skipping a malformed relay body; advancing the cursor past it");
                    resolved.push(*seq);
                }
                Decoded::NotYetOpenable(reason) => {
                    // Decrypt-failure log: previously silent, which made a wrong
                    // workspace key an invisible onboarding failure.
                    tracing::warn!(seq = *seq, reason, "cannot open a fetched body yet (missing epoch key); holding the cursor before it to retry");
                    retry_at = Some(retry_at.map_or(*seq, |r| r.min(*seq)));
                }
            }
        }

        if foreign > 0 {
            tracing::warn!(room = %self.workspace, foreign, "dropped fetched events stamped with another workspace");
        }
        // A body held on an earlier page of this walk still caps the cursor.
        if let Some(h) = self.walk_hold {
            retry_at = Some(retry_at.map_or(h, |r| r.min(h)));
        }

        // Verify-on-ingest (S1): build the trust roster from what we already hold
        // plus the trust events in this batch, then classify each envelope. The
        // policy is non-bricking: reject only the *provably bad* (bad signature,
        // revoked device, impersonation); merely-unverifiable events (unsigned,
        // or from a device whose cert we haven't seen yet) are accepted and
        // re-checked as the roster grows — never dropped.
        //
        // Under `IdentityMode::GithubSigningKeys` (Option C) the identity gate
        // folds in a GitHub `ssh_signing_keys` check: a roster-valid event whose
        // account isn't GitHub-verified **yet** is *held* exactly like a
        // not-yet-openable body (the cursor is held before it, so it's re-fetched
        // and re-classified once verification completes) — never dropped, so two
        // devices at different verification states can't diverge in applied state.
        // Only a *provably-bad* identity (GitHub fetched, key not listed) is
        // quarantined. `Relay` mode (Option A, default) never holds on identity —
        // it is byte-for-byte the pre-existing policy.
        let mut roster_src = store.roster_envelopes()?;
        roster_src.extend(decoded.iter().map(|(_, e)| e.clone()));
        let roster = build_roster(&roster_src);
        // Recover from a poisoned lock instead of panicking the sync task: the
        // identity cache holds only best-effort verification results (rebuilt/
        // re-fetched), so a panic in a prior holder shouldn't crash every future
        // sync pass. This path is network-input-adjacent — fail soft, not hard.
        let identity = self
            .identity_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut applied = 0;
        for (seq, env) in &decoded {
            let base = verdict_for(&roster, env);
            // The account this event is authored as — `verdict_for` has already
            // confirmed the signing device is cert-verified for it (else it's
            // quarantined as impersonation), so it's the right account to run the
            // Option-C GitHub identity gate against. Fall back to the device's
            // account for events that carry no stamp (e.g. some trust events).
            let signer_account = env
                .actor_stamp
                .as_ref()
                .and_then(|s| s.actor.account_id)
                .or_else(|| env.signer_device_id.and_then(|d| roster.account_of(d)));
            let account_key = signer_account.and_then(|a| roster.account_signing_key(a));
            match gate_ingest(self.identity_mode, base, signer_account, account_key, &identity) {
                IngestAction::Quarantine(reason) => {
                    tracing::warn!(?reason, event_id = %env.event_id, "quarantined a fetched event");
                    // Provably bad and stable — advance past it (re-fetching won't help).
                    resolved.push(*seq);
                }
                IngestAction::Hold(reason) => {
                    // Can't verify yet (Option C: GitHub identity pending/unreachable).
                    // Hold the cursor before it and retry, mirroring NotYetOpenable —
                    // NEVER drop, so a peer that later verifies converges to the same
                    // applied state. Not ingested this pass.
                    tracing::debug!(reason, seq = *seq, event_id = %env.event_id, "holding a fetched event for identity re-verification");
                    retry_at = Some(retry_at.map_or(*seq, |r| r.min(*seq)));
                }
                IngestAction::Accept => {
                    self.seen.insert(env.event_id);
                    // This event is now on the relay (we received it from there): queue
                    // it for the durable `pushed` set so a restart never bounces it back.
                    self.pending.push(env.event_id);
                    if store.ingest(env)? {
                        applied += 1;
                    }
                    resolved.push(*seq);
                }
            }
        }
        drop(identity);

        // Advance the cursor to the highest resolved sequence, but never to or
        // past the lowest not-yet-openable sequence (so it's re-fetched), and
        // never backwards. Later openable events above `retry_at` were still
        // applied above; the cursor just waits for the gap to fill.
        for seq in resolved {
            if retry_at.is_none_or(|r| seq < r) {
                self.last_fetched_seq = self.last_fetched_seq.max(seq);
            }
        }
        // Carry the hold to the next page of this walk; a walk that reached the
        // head (or a direct call) drops it — the next walk starts at the cursor.
        self.walk_hold = if self.behind { retry_at } else { None };

        // Persist the durable dedup markers, and the cursor iff it advanced —
        // this is what lets a restart resume from here instead of seq 0. The
        // retry semantics above are unchanged: the cursor still sits before the
        // lowest not-yet-openable body, so that value is what gets persisted.
        self.persist_pending(store)?;
        if self.last_fetched_seq != cursor_before {
            store.set_sync_cursor(&self.workspace, self.last_fetched_seq)?;
        }
        Ok(applied)
    }

    /// The current fetch cursor (highest relay sequence durably applied).
    pub fn cursor(&self) -> u64 {
        self.last_fetched_seq
    }

    /// Replace the base-epoch (0) key. Lets previously undecodable base-epoch
    /// events decode on the next pull.
    pub fn set_key(&mut self, key: [u8; 32]) {
        self.keyring.insert(0, key);
    }

    /// Add (or replace) the key for a specific epoch — e.g. a rotation reached
    /// us. New events then seal under the new highest epoch, while older-epoch
    /// events already fetched remain openable.
    pub fn add_key(&mut self, version: u32, key: [u8; 32]) {
        self.keyring.insert(version, key);
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
            axum::serve(listener, crate::test_relay::router()).await.unwrap();
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

    /// A chat (snapshot + one message from Alex) stamped with `wid`.
    fn seed_chat_in(store: &mut EventStore, wid: Uuid, body: &str) -> Uuid {
        let session = ChatSession::new("Chat", wid, "anthropic");
        let sid = session.id;
        store
            .ingest(&SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(session) }))
            .unwrap();
        store
            .ingest(&SessionEventEnvelope::new(
                sid,
                wid,
                2,
                SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "Alex", body) },
            ))
            .unwrap();
        sid
    }

    /// Regression: a device's store holds every workspace it has (its local
    /// workspace, each room it joined), but a room's sync only ever sends that
    /// room's events. The old push scan sent the whole store to whichever room
    /// was connected.
    #[tokio::test]
    async fn push_sends_only_the_rooms_own_workspace() {
        let base = spawn_relay().await;
        let room = "room-alpha";
        let room_ws = hive_core::room_workspace_id(room);
        let key = hive_core::derive_workspace_key("alpha secret");

        let mut store = EventStore::open_in_memory().unwrap();
        seed_chat_in(&mut store, room_ws, "team message");
        seed_chat_in(&mut store, Uuid::new_v4(), "PRIVATE local-only note");
        seed_chat_in(&mut store, hive_core::room_workspace_id("room-beta"), "PRIVATE other-room message");

        let mut eng = SyncEngine::new(RelayClient::new(&base), room).with_key(key);
        assert_eq!(eng.push_new(&store).await.unwrap(), 2, "only the room's snapshot + message");
        // A later round, incremental or full, still never picks the others up.
        assert_eq!(eng.push_new(&store).await.unwrap(), 0);
        let mut fresh = SyncEngine::new(RelayClient::new(&base), room).with_key(key);
        assert_eq!(fresh.push_new(&store).await.unwrap(), 0);

        let raw = RelayClient::new(&base).fetch_values(room, 0).await.unwrap();
        assert_eq!(raw.len(), 2);
        for (_, body) in &raw {
            let sealed: SealedEnvelope = serde_json::from_value(body.clone()).unwrap();
            let env: SessionEventEnvelope = serde_json::from_slice(&open_symmetric(&key, &sealed).unwrap()).unwrap();
            assert_eq!(env.workspace_id, room_ws);
            assert!(!serde_json::to_string(&env).unwrap().contains("PRIVATE"));
        }
    }

    /// Defense in depth: events of another workspace that an older client
    /// pushed into the room are never ingested, and don't wedge the cursor.
    #[tokio::test]
    async fn pull_drops_events_of_another_workspace() {
        let base = spawn_relay().await;
        let room = "room-alpha";
        let room_ws = hive_core::room_workspace_id(room);
        let key = hive_core::derive_workspace_key("alpha secret");

        // The pre-fix behavior: an unscoped engine pushes its whole store.
        let mut leaky_store = EventStore::open_in_memory().unwrap();
        let foreign = seed_chat_in(&mut leaky_store, Uuid::new_v4(), "PRIVATE local-only note");
        let ours = seed_chat_in(&mut leaky_store, room_ws, "team message");
        let mut leaky = SyncEngine::unscoped(RelayClient::new(&base), room).with_key(key);
        assert_eq!(leaky.push_new(&leaky_store).await.unwrap(), 4);

        let mut store = EventStore::open_in_memory().unwrap();
        let mut eng = SyncEngine::new(RelayClient::new(&base), room).with_key(key);
        assert_eq!(eng.pull(&mut store).await.unwrap(), 2);
        assert!(store.load_session(foreign).unwrap().is_none(), "foreign chat never ingested");
        assert!(store.load_session(ours).unwrap().is_some());
        assert_eq!(eng.cursor(), 4, "cursor moves past the dropped events");
        assert_eq!(store.list_session_ids().unwrap(), vec![ours]);
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

        // A pushes; B pulls into an empty store. E2EE is mandatory for relay
        // sync, so both carry the shared workspace key.
        let key = hive_core::derive_workspace_key("converge room secret");
        let mut sync_a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(sync_a.push_new(&store_a).await.unwrap(), 2);

        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut sync_b = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
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

        let mut sync_a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
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
        let mut sync_b = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(sync_b.pull(&mut store_b).await.unwrap(), 2);
        let session_b = store_b.load_session(sid).unwrap().unwrap();
        assert_eq!(session_b.messages[0].body, "TOP SECRET PLAN");

        // B with the WRONG key can't open anything.
        let mut store_c = EventStore::open_in_memory().unwrap();
        let mut sync_c = SyncEngine::unscoped(RelayClient::new(&base), &workspace)
            .with_key(hive_core::derive_workspace_key("wrong"));
        assert_eq!(sync_c.pull(&mut store_c).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn failed_push_rolls_back_so_events_retry() {
        // Relay pointed at a closed port → every push errors mid-batch.
        let mut store = EventStore::open_in_memory().unwrap();
        seed_chat(&mut store); // one event (the snapshot)
        let mut eng = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:9"), Uuid::new_v4().to_string())
            .with_key(hive_core::derive_workspace_key("rollback room"));

        let batch = eng.take_unpushed(&store).unwrap();
        assert_eq!(batch.len(), 1);
        // The push fails — the optimistically-marked events must roll back to
        // unseen so the next round re-pushes them instead of dropping the tail.
        assert!(eng.push_envelopes(&batch).await.is_err());
        let retry = eng.take_unpushed(&store).unwrap();
        assert_eq!(retry.len(), 1, "unsent events must be retryable after a failed push");
    }

    /// After the first full scan, rounds scan only new rows — and still find
    /// every new event, while a failed push makes the next round rescan.
    #[tokio::test]
    async fn push_scan_is_incremental_and_rescans_after_a_failure() {
        let base = spawn_relay().await;
        let mut store = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store);
        let mut eng = SyncEngine::unscoped(RelayClient::new(&base), Uuid::new_v4().to_string())
            .with_key(hive_core::derive_workspace_key("incremental room"));
        let first = eng.take_unpushed(&store).unwrap();
        assert_eq!(first.len(), 1);
        eng.push_envelopes(&first).await.unwrap();
        assert!(eng.take_unpushed(&store).unwrap().is_empty());

        let msg = |body: &str| SessionEvent::MessageAppended { message: ChatMessage::new(MessageRole::User, "Mara", body) };
        let e = store.append(sid, wid, msg("hi")).unwrap();
        let batch = eng.take_unpushed(&store).unwrap();
        assert_eq!(batch.iter().map(|e| e.event_id).collect::<Vec<_>>(), [e.event_id]);

        // That push fails (relay gone): the next round finds it again.
        let mut offline = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:9"), Uuid::new_v4().to_string())
            .with_key(hive_core::derive_workspace_key("incremental room"));
        std::mem::swap(&mut eng.relay, &mut offline.relay);
        assert!(eng.push_envelopes(&batch).await.is_err());
        let retry = eng.take_unpushed(&store).unwrap();
        assert_eq!(retry.iter().map(|e| e.event_id).collect::<Vec<_>>(), [e.event_id]);
    }

    #[tokio::test]
    async fn keyless_relay_push_is_refused_no_plaintext() {
        // B4: a relay-targeted engine with NO key must REFUSE to push rather
        // than send plaintext. Pointed at a LIVE, reachable relay to prove the
        // refusal isn't merely a network failure: the distinct
        // `RelayRequiresEncryption` error is produced and NOTHING is written to
        // the relay. The events roll back to unpushed so a later keyed round
        // re-sends them.
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();
        let mut store = EventStore::open_in_memory().unwrap();
        seed_chat(&mut store); // one event (the snapshot)

        let mut eng = SyncEngine::unscoped(RelayClient::new(&base), &workspace);
        assert!(!eng.is_encrypted(), "no key configured");

        let batch = eng.take_unpushed(&store).unwrap();
        assert_eq!(batch.len(), 1);
        let err = eng.push_envelopes(&batch).await.unwrap_err();
        assert!(
            matches!(err, SyncError::RelayRequiresEncryption),
            "keyless push must refuse with RelayRequiresEncryption, got {err:?}"
        );
        // No plaintext value was produced or sent: the relay holds nothing.
        assert!(
            RelayClient::new(&base).fetch_values(&workspace, 0).await.unwrap().is_empty(),
            "a refused keyless push must not write anything to the relay"
        );
        // Rolled back → retryable once a key is configured (not silently dropped).
        let retry = eng.take_unpushed(&store).unwrap();
        assert_eq!(
            retry.len(),
            1,
            "refused envelopes must remain unpushed for a later keyed round"
        );

        // With a key, those same events now seal and land on the relay as
        // ciphertext (a keyed workspace still syncs).
        eng.set_key(hive_core::derive_workspace_key("now keyed"));
        assert!(eng.is_encrypted());
        eng.push_envelopes(&retry).await.unwrap();
        let raw = RelayClient::new(&base).fetch_values(&workspace, 0).await.unwrap();
        assert_eq!(raw.len(), 1);
        assert!(
            raw[0].1.get("ciphertext").is_some(),
            "a keyed push must seal — the relay must only ever see ciphertext"
        );
    }

    #[tokio::test]
    async fn not_yet_openable_events_do_not_advance_the_cursor() {
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();
        // A seals under an epoch (1) that B does not hold yet.
        let k1 = hive_core::e2ee::generate_workspace_key().unwrap();

        // A publishes two sealed events under epoch 1.
        let mut store_a = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store_a);
        store_a
            .ingest(&SessionEventEnvelope::new(
                sid,
                wid,
                2,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "A", "hi"),
                },
            ))
            .unwrap();
        let mut sync_a = SyncEngine::unscoped(RelayClient::new(&base), &workspace);
        sync_a.add_key(1, k1);
        sync_a.push_new(&store_a).await.unwrap();

        // B holds NO key for epoch 1: the bodies are well-formed but not-yet-
        // openable, so it applies nothing AND does NOT advance its cursor past
        // them (they must be retried once the key arrives).
        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut sync_b = SyncEngine::unscoped(RelayClient::new(&base), &workspace);
        assert_eq!(sync_b.pull(&mut store_b).await.unwrap(), 0);
        assert_eq!(sync_b.cursor(), 0, "not-yet-openable events must not be skipped");

        // The epoch-1 key reaches B (e.g. a rotation catches up). The retained
        // events are re-fetched and now converge — not lost.
        sync_b.add_key(1, k1);
        assert_eq!(sync_b.pull(&mut store_b).await.unwrap(), 2);
        assert_eq!(store_b.load_session(sid).unwrap().unwrap().messages.len(), 1);
    }

    #[test]
    fn malformed_body_is_skipped_and_cursor_advances_past_it() {
        // A garbage `{"ciphertext":1}` (hostile or corrupt) sitting between two
        // good plaintext events must not halt the stream: the good events apply
        // and the cursor advances past the bad one, so it's never re-fetched.
        let session = ChatSession::new("Shared", Uuid::new_v4(), "anthropic");
        let sid = session.id;
        let wid = session.workspace_id;
        let snap = SessionEventEnvelope::new(
            sid,
            wid,
            1,
            SessionEvent::SessionSnapshot { session: Box::new(session) },
        );
        let msg = SessionEventEnvelope::new(
            sid,
            wid,
            2,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "A", "still delivered"),
            },
        );
        let batch: Vec<(u64, Value)> = vec![
            (1, serde_json::to_value(&snap).unwrap()),
            (2, serde_json::json!({ "ciphertext": 1 })), // provably malformed
            (3, serde_json::to_value(&msg).unwrap()),
        ];

        let mut store = EventStore::open_in_memory().unwrap();
        let mut eng =
            SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), Uuid::new_v4().to_string());
        assert_eq!(eng.apply_fetched(&mut store, &batch).unwrap(), 2);
        assert!(store.has_event(snap.event_id).unwrap());
        assert!(store.has_event(msg.event_id).unwrap(), "event after the bad body applies");
        assert_eq!(eng.cursor(), 3, "cursor advances past the malformed body");
        // No infinite re-fetch: re-applying the same batch does nothing new.
        assert_eq!(eng.apply_fetched(&mut store, &batch).unwrap(), 0);
        assert_eq!(eng.cursor(), 3);
    }

    #[test]
    fn missing_epoch_key_does_not_strand_later_openable_events() {
        // A body sealed under an epoch the engine LACKS, followed by an event it
        // CAN open: the openable event must still apply (not be stranded), the
        // cursor must stay before the not-yet-openable body (retry point), and
        // the behavior must be deterministic across repeated passes.
        let held = hive_core::derive_workspace_key("held epoch 0");
        let missing = hive_core::e2ee::generate_workspace_key().unwrap(); // epoch 7, unheld
        let mut eng = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), Uuid::new_v4().to_string())
            .with_key(held);

        let session = ChatSession::new("Shared", Uuid::new_v4(), "anthropic");
        let sid = session.id;
        let wid = session.workspace_id;
        let snap = SessionEventEnvelope::new(
            sid,
            wid,
            1,
            SessionEvent::SessionSnapshot { session: Box::new(session) },
        );
        let msg = SessionEventEnvelope::new(
            sid,
            wid,
            2,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "A", "reachable"),
            },
        );
        let seal = |k: &[u8; 32], ver: u32, e: &SessionEventEnvelope| -> Value {
            let plain = serde_json::to_vec(e).unwrap();
            serde_json::to_value(seal_symmetric(k, ver, &plain).unwrap()).unwrap()
        };
        let batch: Vec<(u64, Value)> = vec![
            (1, seal(&missing, 7, &snap)), // not-yet-openable (epoch 7 unheld)
            (2, seal(&held, 0, &msg)),     // openable
        ];

        let mut store = EventStore::open_in_memory().unwrap();
        assert_eq!(eng.apply_fetched(&mut store, &batch).unwrap(), 1);
        assert!(store.has_event(msg.event_id).unwrap(), "later openable event is not stranded");
        assert!(!store.has_event(snap.event_id).unwrap(), "not-yet-openable body is not applied");
        assert_eq!(eng.cursor(), 0, "cursor stays before the not-yet-openable seq (retry point)");

        // Deterministic: another pass over the same batch changes nothing.
        assert_eq!(eng.apply_fetched(&mut store, &batch).unwrap(), 0);
        assert_eq!(eng.cursor(), 0);

        // Once the missing epoch key arrives, the retained body converges too and
        // the cursor jumps forward.
        eng.add_key(7, missing);
        assert_eq!(eng.apply_fetched(&mut store, &batch).unwrap(), 1);
        assert!(store.has_event(snap.event_id).unwrap());
        assert_eq!(eng.cursor(), 2);
    }

    #[test]
    fn apply_fetched_rejects_provably_bad_keeps_good() {
        use hive_core::crypto::{DeviceCertificate, SigningKeypair};
        use hive_core::identity::{ActorIdentity, ActorKind, ActorStamp, WorkspaceMember, WorkspaceRole};
        use hive_core::Timestamp;

        let account = SigningKeypair::generate().unwrap();
        let account_id = Uuid::new_v4();
        let device = SigningKeypair::generate().unwrap();
        let device_id = Uuid::new_v4();
        let cert = DeviceCertificate::issue(
            &account,
            account_id,
            device_id,
            &device.public_key_bytes(),
            Timestamp::epoch(),
        );
        let member = WorkspaceMember {
            id: account_id.to_string(),
            actor: ActorIdentity {
                id: account_id.to_string(),
                display_name: "A".into(),
                kind: ActorKind::Human,
                account_id: Some(account_id),
                device_id: Some(device_id),
                git_email: None,
                key_agreement_public: None,
                avatar_url: None,
            },
            role: WorkspaceRole::Owner,
            title: String::new(),
            index: 1,
            joined_at: Timestamp::epoch(),
        };

        // A content event signed by `device`, stamping `claim` as the author.
        let content = |lamport: i64, claim: Uuid| -> SessionEventEnvelope {
            let mut e = SessionEventEnvelope::new(
                Uuid::nil(),
                Uuid::nil(),
                lamport,
                SessionEvent::SessionTitleChanged { title: format!("t{lamport}") },
            );
            e.actor_stamp = Some(ActorStamp {
                actor: ActorIdentity {
                    id: claim.to_string(),
                    display_name: "A".into(),
                    kind: ActorKind::Human,
                    account_id: Some(claim),
                    device_id: Some(device_id),
                    git_email: None,
                    key_agreement_public: None,
                    avatar_url: None,
                },
                recorded_at: Timestamp::epoch(),
            });
            hive_core::sign_envelope(&mut e, device_id, &device);
            e
        };

        let good = content(100, account_id);
        let mut tampered = content(101, account_id);
        tampered.sequence = 9999; // breaks the signature
        let spoof = content(102, Uuid::new_v4()); // stamps a different account

        let plain = |lamport: i64, payload: SessionEvent| {
            SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), lamport, payload)
        };
        let all: Vec<SessionEventEnvelope> = vec![
            plain(1, SessionEvent::MemberAdded { member }),
            plain(2, SessionEvent::AccountKeyRegistered {
                account_id,
                signing_public_key: account.public_key_bytes().to_vec(),
            }),
            plain(3, SessionEvent::DeviceCertificateAdded { certificate: cert }),
            good.clone(),
            tampered.clone(),
            spoof.clone(),
        ];
        let batch: Vec<(u64, Value)> = all
            .iter()
            .enumerate()
            .map(|(i, e)| (i as u64 + 1, serde_json::to_value(e).unwrap()))
            .collect();

        let mut store = EventStore::open_in_memory().unwrap();
        let mut eng = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), Uuid::new_v4().to_string());
        eng.apply_fetched(&mut store, &batch).unwrap();

        assert!(store.has_event(good.event_id).unwrap(), "valid signed event ingested");
        assert!(!store.has_event(tampered.event_id).unwrap(), "tampered event rejected");
        assert!(!store.has_event(spoof.event_id).unwrap(), "impersonating event rejected");
    }

    #[tokio::test]
    async fn rotated_epochs_do_not_strand_history() {
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();
        let k0 = hive_core::derive_workspace_key("epoch 0 base");
        let k1 = hive_core::e2ee::generate_workspace_key().unwrap();

        // A seals a snapshot under epoch 0, rotates to epoch 1, seals a message.
        let mut store_a = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store_a);
        let mut sync_a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(k0);
        assert_eq!(sync_a.push_new(&store_a).await.unwrap(), 1); // snapshot @ epoch 0

        store_a
            .ingest(&SessionEventEnvelope::new(
                sid,
                wid,
                2,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "A", "after rotation"),
                },
            ))
            .unwrap();
        sync_a.add_key(1, k1); // epoch 1 is now the highest → new events seal under it
        assert_eq!(sync_a.push_new(&store_a).await.unwrap(), 1); // message @ epoch 1

        // B holds only epoch 0: reads the epoch-0 snapshot; the epoch-1 message is
        // undecodable and retained (not permanently skipped — phase 7).
        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut sync_b = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(k0);
        sync_b.pull(&mut store_b).await.unwrap();
        let sb = store_b.load_session(sid).unwrap().expect("snapshot synced");
        assert_eq!(sb.title, "Shared", "epoch-0 content is readable");
        assert_eq!(sb.messages.len(), 0, "epoch-1 message not yet readable");

        // Epoch 1 reaches B. It reads the message AND still reads epoch-0 history —
        // both keys are retained, so the rotation stranded nothing.
        sync_b.add_key(1, k1);
        sync_b.pull(&mut store_b).await.unwrap();
        let sb = store_b.load_session(sid).unwrap().unwrap();
        assert_eq!(sb.title, "Shared", "epoch-0 history still readable after rotation");
        assert_eq!(sb.messages.len(), 1, "epoch-1 message now readable");
        assert_eq!(sb.messages[0].body, "after rotation");
    }

    #[tokio::test]
    async fn new_engine_resumes_from_persisted_cursor() {
        // #2: durable cursor. After a pull persists the cursor, a NEW engine on
        // the SAME store must resume from it and fetch nothing new — not
        // re-fetch the whole room from seq 0.
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();
        let key = hive_core::derive_workspace_key("resume room");

        let mut store_a = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store_a);
        store_a
            .ingest(&SessionEventEnvelope::new(
                sid,
                wid,
                2,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "A", "hi"),
                },
            ))
            .unwrap();
        let mut sync_a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(sync_a.push_new(&store_a).await.unwrap(), 2);

        // First engine on B pulls both, persisting the cursor into store_b.
        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut sync_b1 = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(sync_b1.pull(&mut store_b).await.unwrap(), 2);
        let cursor = sync_b1.cursor();
        assert!(cursor > 0, "cursor advanced");
        assert_eq!(store_b.sync_cursor(&workspace).unwrap(), cursor, "cursor is durable");
        drop(sync_b1);

        // A brand-new engine (simulating a restart) resumes from the durable
        // cursor: it fetches nothing new and starts at the persisted cursor.
        let mut sync_b2 = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(sync_b2.pull(&mut store_b).await.unwrap(), 0, "resumes; nothing re-fetched");
        assert_eq!(sync_b2.cursor(), cursor, "cursor restored from the durable store");
    }

    #[tokio::test]
    async fn new_engine_does_not_repush_already_pushed() {
        // #2: durable push-dedup. After pushing, a NEW engine on the same store
        // must NOT re-collect or re-push the already-pushed events.
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();
        let key = hive_core::derive_workspace_key("no-repush room");

        let mut store = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store);
        store
            .ingest(&SessionEventEnvelope::new(
                sid,
                wid,
                2,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "A", "hi"),
                },
            ))
            .unwrap();

        let mut sync1 = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(sync1.push_new(&store).await.unwrap(), 2);
        drop(sync1);
        assert_eq!(store.pushed_ids(&workspace).unwrap().len(), 2, "pushes are durable");

        // Fresh engine, same store: the durable `pushed` set makes take_unpushed
        // collect nothing, so a restart re-push is a no-op and the relay keeps
        // exactly two bodies (no duplicates).
        let mut sync2 = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert!(sync2.take_unpushed(&store).unwrap().is_empty(), "already-pushed not re-collected");
        assert_eq!(sync2.push_new(&store).await.unwrap(), 0, "restart re-push is a no-op");
        let raw = RelayClient::new(&base).fetch_values(&workspace, 0).await.unwrap();
        assert_eq!(raw.len(), 2, "no duplicate bodies on the relay after restart");
    }

    #[test]
    fn durable_cursor_preserves_not_yet_openable_retry() {
        // #2 + resilience: the not-yet-openable retry point must survive a
        // restart. A body sealed under an epoch we lack sits before an openable
        // one; the openable event applies, the cursor is held before the missing
        // body, and BOTH the applied event and the held cursor are durable — so a
        // fresh engine on the same store still retries the missing body (doesn't
        // strand it or skip past it).
        let workspace = Uuid::new_v4().to_string();
        let held = hive_core::derive_workspace_key("held epoch 0");
        let missing = hive_core::e2ee::generate_workspace_key().unwrap(); // epoch 7, unheld

        let session = ChatSession::new("Shared", Uuid::new_v4(), "anthropic");
        let sid = session.id;
        let wid = session.workspace_id;
        let snap = SessionEventEnvelope::new(
            sid,
            wid,
            1,
            SessionEvent::SessionSnapshot { session: Box::new(session) },
        );
        let msg = SessionEventEnvelope::new(
            sid,
            wid,
            2,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "A", "reachable"),
            },
        );
        let seal = |k: &[u8; 32], ver: u32, e: &SessionEventEnvelope| -> Value {
            let plain = serde_json::to_vec(e).unwrap();
            serde_json::to_value(seal_symmetric(k, ver, &plain).unwrap()).unwrap()
        };
        let batch: Vec<(u64, Value)> = vec![
            (1, seal(&missing, 7, &snap)), // not-yet-openable (epoch 7 unheld)
            (2, seal(&held, 0, &msg)),     // openable
        ];

        let mut store = EventStore::open_in_memory().unwrap();
        let mut eng1 = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), &workspace)
            .with_key(held);
        assert_eq!(eng1.apply_fetched(&mut store, &batch).unwrap(), 1);
        assert_eq!(eng1.cursor(), 0, "cursor held before the not-yet-openable seq");
        assert_eq!(store.sync_cursor(&workspace).unwrap(), 0, "durable cursor is the retry point");
        drop(eng1);

        // Fresh engine (restart), STILL without epoch 7: resumes cursor 0, the
        // openable event is already ingested so nothing new applies, and the
        // cursor stays before the missing body.
        let mut eng2 = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), &workspace)
            .with_key(held);
        assert_eq!(eng2.apply_fetched(&mut store, &batch).unwrap(), 0, "openable already applied");
        assert_eq!(eng2.cursor(), 0, "resumed cursor still before the retry point");

        // Epoch 7 arrives: the retained body converges and the cursor jumps,
        // durably.
        eng2.add_key(7, missing);
        assert_eq!(eng2.apply_fetched(&mut store, &batch).unwrap(), 1);
        assert!(store.has_event(snap.event_id).unwrap());
        assert_eq!(eng2.cursor(), 2);
        assert_eq!(store.sync_cursor(&workspace).unwrap(), 2, "durable cursor advanced past the gap");
    }

    #[tokio::test]
    async fn bidirectional_round_trip() {
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();

        let key = hive_core::derive_workspace_key("round trip room");
        let mut store_a = EventStore::open_in_memory().unwrap();
        let (sid, wid) = seed_chat(&mut store_a);
        let mut sync_a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        sync_a.sync_once(&mut store_a).await.unwrap();

        // B joins, syncs, then replies.
        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut sync_b = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
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

    // --- Option C (github-signing-keys identity mode) ------------------------

    use crate::identity_verifier::{shared_cache, IdentityMode, IdentityVerdict, SharedIdentityCache};

    /// A member principal with its trust events (`MemberAdded`, `AccountKeyRegistered`,
    /// `DeviceCertificateAdded`) + an account signing key we can mark verified.
    struct OcPrincipal {
        account_id: Uuid,
        account_key: Vec<u8>,
        device_kp: hive_core::crypto::SigningKeypair,
        device_id: Uuid,
        trust: Vec<SessionEventEnvelope>,
    }

    fn oc_principal() -> OcPrincipal {
        use hive_core::crypto::{DeviceCertificate, SigningKeypair};
        use hive_core::identity::{ActorIdentity, ActorKind, WorkspaceMember, WorkspaceRole};
        use hive_core::Timestamp;

        let account = SigningKeypair::generate().unwrap();
        let account_id = Uuid::new_v4();
        let device = SigningKeypair::generate().unwrap();
        let device_id = Uuid::new_v4();
        let cert = DeviceCertificate::issue(
            &account,
            account_id,
            device_id,
            &device.public_key_bytes(),
            Timestamp::epoch(),
        );
        let member = WorkspaceMember {
            id: account_id.to_string(),
            actor: ActorIdentity {
                id: account_id.to_string(),
                display_name: "A".into(),
                kind: ActorKind::Human,
                account_id: Some(account_id),
                device_id: Some(device_id),
                git_email: None,
                key_agreement_public: None,
                avatar_url: None,
            },
            role: WorkspaceRole::Owner,
            title: String::new(),
            index: 1,
            joined_at: Timestamp::epoch(),
        };
        // Trust events are signed by the device (as they are in production), so
        // under Option C they follow the same verified/held path as content.
        let signed = |lamport: i64, payload: SessionEvent| {
            let mut e = SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), lamport, payload);
            hive_core::sign_envelope(&mut e, device_id, &device);
            e
        };
        let trust = vec![
            signed(1, SessionEvent::MemberAdded { member }),
            signed(
                2,
                SessionEvent::AccountKeyRegistered {
                    account_id,
                    signing_public_key: account.public_key_bytes().to_vec(),
                },
            ),
            signed(3, SessionEvent::DeviceCertificateAdded { certificate: cert }),
        ];
        OcPrincipal {
            account_id,
            account_key: account.public_key_bytes().to_vec(),
            device_kp: device,
            device_id,
            trust,
        }
    }

    fn oc_content(p: &OcPrincipal, lamport: i64) -> SessionEventEnvelope {
        use hive_core::identity::{ActorIdentity, ActorKind, ActorStamp};
        use hive_core::Timestamp;
        let mut e = SessionEventEnvelope::new(
            Uuid::nil(),
            Uuid::nil(),
            lamport,
            SessionEvent::SessionTitleChanged { title: format!("t{lamport}") },
        );
        e.actor_stamp = Some(ActorStamp {
            actor: ActorIdentity {
                id: p.account_id.to_string(),
                display_name: "A".into(),
                kind: ActorKind::Human,
                account_id: Some(p.account_id),
                device_id: Some(p.device_id),
                git_email: None,
                key_agreement_public: None,
                avatar_url: None,
            },
            recorded_at: Timestamp::epoch(),
        });
        hive_core::sign_envelope(&mut e, p.device_id, &p.device_kp);
        e
    }

    fn oc_engine(cache: SharedIdentityCache) -> SyncEngine {
        SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), Uuid::new_v4().to_string())
            .with_identity_mode(IdentityMode::GithubSigningKeys)
            .with_identity_cache(cache)
    }

    fn oc_batch(p: &OcPrincipal, content: &SessionEventEnvelope) -> Vec<(u64, Value)> {
        p.trust
            .iter()
            .chain(std::iter::once(content))
            .enumerate()
            .map(|(i, e)| (i as u64 + 1, serde_json::to_value(e).unwrap()))
            .collect()
    }

    #[test]
    fn option_c_unverified_is_held_not_dropped_then_promoted() {
        let p = oc_principal();
        let content = oc_content(&p, 100);
        let batch = oc_batch(&p, &content);

        let cache = shared_cache();
        let mut eng = oc_engine(Arc::clone(&cache));
        let mut store = EventStore::open_in_memory().unwrap();

        // GitHub not fetched yet: the roster-valid content event is HELD, not
        // dropped. Trust events themselves are unsigned → also held under Option C.
        assert_eq!(eng.apply_fetched(&mut store, &batch).unwrap(), 0);
        assert!(!store.has_event(content.event_id).unwrap(), "unverified content is held, not applied");
        assert_eq!(eng.cursor(), 0, "cursor held at the front for retry (nothing resolved)");

        // Deterministic: replaying the same batch changes nothing while unverified.
        assert_eq!(eng.apply_fetched(&mut store, &batch).unwrap(), 0);
        assert!(!store.has_event(content.event_id).unwrap());

        // GitHub verification completes → the held event is promoted on re-fetch.
        cache.lock().unwrap().put(
            p.account_id,
            p.account_key.clone(),
            IdentityVerdict::Verified,
            std::time::Duration::from_secs(600),
        );
        eng.apply_fetched(&mut store, &batch).unwrap();
        assert!(store.has_event(content.event_id).unwrap(), "promoted once verified");
    }

    #[test]
    fn option_c_key_not_listed_is_quarantined() {
        let p = oc_principal();
        let content = oc_content(&p, 100);
        let batch = oc_batch(&p, &content);

        let cache = shared_cache();
        // GitHub fetched and the account key is NOT among the login's signing keys.
        cache.lock().unwrap().put(
            p.account_id,
            p.account_key.clone(),
            IdentityVerdict::NotListed,
            std::time::Duration::from_secs(600),
        );
        let mut eng = oc_engine(Arc::clone(&cache));
        let mut store = EventStore::open_in_memory().unwrap();

        eng.apply_fetched(&mut store, &batch).unwrap();
        assert!(!store.has_event(content.event_id).unwrap(), "provably-bad identity is quarantined");
        // Quarantine is stable → the cursor advances past it (re-fetching won't help).
        assert_eq!(eng.cursor() as usize, batch.len(), "cursor advances past a quarantined event");
    }

    #[test]
    fn option_a_default_ignores_github_state_unchanged() {
        // Same batch, same (NotListed) cache — but Relay mode (the default) never
        // consults it: behavior is byte-for-byte the pre-Option-C policy, so the
        // valid signed content event is ingested.
        let p = oc_principal();
        let content = oc_content(&p, 100);
        let batch = oc_batch(&p, &content);

        let cache = shared_cache();
        cache.lock().unwrap().put(
            p.account_id,
            p.account_key.clone(),
            IdentityVerdict::NotListed,
            std::time::Duration::from_secs(600),
        );
        let mut eng = SyncEngine::unscoped(RelayClient::new("http://127.0.0.1:0"), Uuid::new_v4().to_string())
            .with_identity_mode(IdentityMode::Relay)
            .with_identity_cache(cache);
        let mut store = EventStore::open_in_memory().unwrap();

        eng.apply_fetched(&mut store, &batch).unwrap();
        assert!(store.has_event(content.event_id).unwrap(), "Option A ingests regardless of GitHub state");
    }

    #[test]
    fn option_c_devices_at_different_verification_states_converge() {
        // The core convergence property: two devices ingest the SAME batch under
        // Option C but at different GitHub-verification states. The one without
        // verification yet HOLDS the event (doesn't drop it); once it verifies, it
        // reaches the identical applied state. No divergence into applied-vs-dropped.
        let p = oc_principal();
        let content = oc_content(&p, 100);
        let batch = oc_batch(&p, &content);

        // Device A: already GitHub-verified.
        let cache_a = shared_cache();
        cache_a.lock().unwrap().put(
            p.account_id,
            p.account_key.clone(),
            IdentityVerdict::Verified,
            std::time::Duration::from_secs(600),
        );
        let mut eng_a = oc_engine(Arc::clone(&cache_a));
        let mut store_a = EventStore::open_in_memory().unwrap();
        eng_a.apply_fetched(&mut store_a, &batch).unwrap();

        // Device B: no network yet (cache empty) → holds, applies nothing.
        let cache_b = shared_cache();
        let mut eng_b = oc_engine(Arc::clone(&cache_b));
        let mut store_b = EventStore::open_in_memory().unwrap();
        eng_b.apply_fetched(&mut store_b, &batch).unwrap();

        assert!(store_a.has_event(content.event_id).unwrap(), "A applied (verified)");
        assert!(!store_b.has_event(content.event_id).unwrap(), "B held (not yet verified) — NOT dropped");

        // B's GitHub verification catches up (shared authority → same verdict) and
        // it re-fetches the held event → converges to A's applied state.
        cache_b.lock().unwrap().put(
            p.account_id,
            p.account_key.clone(),
            IdentityVerdict::Verified,
            std::time::Duration::from_secs(600),
        );
        eng_b.apply_fetched(&mut store_b, &batch).unwrap();
        assert!(store_b.has_event(content.event_id).unwrap(), "B converges once verified");

        // Both devices now hold the identical event.
        assert_eq!(
            store_a.has_event(content.event_id).unwrap(),
            store_b.has_event(content.event_id).unwrap(),
        );
    }

    #[test]
    fn option_c_content_before_trust_across_pulls_converges_not_wedged() {
        // P2-21 adversarial fetch-split: content arrives in one pull BEFORE its
        // trust events (a hostile/fragmented relay ordering, or trust straddling a
        // cursor boundary). Under Option C the content must be HELD (device
        // unknown) — never dropped — and once a later pull carries the trust it
        // must converge (apply), not wedge behind a permanently-held cursor.
        let p = oc_principal();
        let content = oc_content(&p, 1);
        let cache = shared_cache();
        cache.lock().unwrap().put(
            p.account_id,
            p.account_key.clone(),
            IdentityVerdict::Verified,
            std::time::Duration::from_secs(600),
        );
        let mut eng = oc_engine(Arc::clone(&cache));
        let mut store = EventStore::open_in_memory().unwrap();

        // Pull 1: content alone at seq 1, no trust yet → held (device unknown).
        let pull1 = vec![(1u64, serde_json::to_value(&content).unwrap())];
        eng.apply_fetched(&mut store, &pull1).unwrap();
        assert!(
            !store.has_event(content.event_id).unwrap(),
            "content with no trust must be HELD, not applied and not dropped"
        );

        // Pull 2: re-fetch from the held cursor now carries content + its trust.
        let mut pull2 = pull1.clone();
        for (i, e) in p.trust.iter().enumerate() {
            pull2.push((i as u64 + 2, serde_json::to_value(e).unwrap()));
        }
        eng.apply_fetched(&mut store, &pull2).unwrap();
        assert!(
            store.has_event(content.event_id).unwrap(),
            "content converges once its trust arrives in a later pull — no permanent wedge"
        );
    }

    // --- Paged pull (a room larger than one relay page) ---

    /// A mock relay whose pages hold at most `cap` envelopes.
    async fn spawn_paged_relay(cap: usize) -> (String, crate::test_relay::EnvelopeGets) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (router, gets) = crate::test_relay::router_with_stats(cap);
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (format!("http://{addr}"), gets)
    }

    /// One chat: a snapshot + `n` messages = `n + 1` envelopes.
    fn seed_long_chat(store: &mut EventStore, n: usize) -> Uuid {
        let (sid, wid) = seed_chat(store);
        for i in 0..n {
            store
                .ingest(&SessionEventEnvelope::new(
                    sid,
                    wid,
                    2 + i as i64,
                    SessionEvent::MessageAppended {
                        message: ChatMessage::new(MessageRole::User, "A", format!("msg {i}")),
                    },
                ))
                .unwrap();
        }
        sid
    }

    /// A new device joining a room bigger than one relay page gets all of it:
    /// a round is bounded to PULL_PAGES_PER_ROUND pages and says it is behind,
    /// the next round continues, and the durable cursor ends at the head.
    #[tokio::test]
    async fn pull_rounds_page_through_a_room_larger_than_one_page() {
        let (base, gets) = spawn_paged_relay(3).await;
        let workspace = Uuid::new_v4().to_string();
        let key = hive_core::derive_workspace_key("paged room");

        let mut store_a = EventStore::open_in_memory().unwrap();
        let sid = seed_long_chat(&mut store_a, 40); // 41 envelopes = 14 pages of 3
        let mut a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(a.push_new(&store_a).await.unwrap(), 41);

        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut b = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        let before = gets.count();
        let first = b.pull_round(&mut store_b).await.unwrap();
        assert_eq!(first, 3 * PULL_PAGES_PER_ROUND, "one round = a bounded number of pages");
        assert_eq!(gets.count() - before, PULL_PAGES_PER_ROUND);
        assert!(b.is_behind(), "more pages remain");
        assert_eq!(store_b.sync_cursor(&workspace).unwrap(), first as u64, "progress is durable per page");

        let second = b.pull_round(&mut store_b).await.unwrap();
        assert_eq!(first + second, 41);
        assert!(!b.is_behind(), "caught up");
        assert_eq!(b.cursor(), 41);
        let session = store_b.load_session(sid).unwrap().expect("chat synced");
        assert_eq!(session.messages.len(), 40);

        // Caught up: the next round is a single empty page.
        let before = gets.count();
        assert_eq!(b.pull_round(&mut store_b).await.unwrap(), 0);
        assert_eq!(gets.count() - before, 1);

        // `catch_up` (one-shot callers) gets the whole room in one call.
        let mut store_c = EventStore::open_in_memory().unwrap();
        let mut c = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(c.catch_up(&mut store_c, 100).await.unwrap(), 41);
        assert!(!c.is_behind());
        assert_eq!(store_c.load_session(sid).unwrap().unwrap().messages.len(), 40);

        // A restarted engine resumes from the durable cursor, not from 0.
        let mut c2 = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        assert_eq!(c2.pull_round(&mut store_c).await.unwrap(), 0);
        assert_eq!(c2.cursor(), 41);
    }

    /// The client's own page size is honored too (smaller than the relay cap).
    #[tokio::test]
    async fn pull_honors_the_client_page_limit() {
        let (base, gets) = spawn_paged_relay(1000).await;
        let workspace = Uuid::new_v4().to_string();
        let key = hive_core::derive_workspace_key("client limit");
        let mut store_a = EventStore::open_in_memory().unwrap();
        seed_long_chat(&mut store_a, 9); // 10 envelopes
        let mut a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        a.push_new(&store_a).await.unwrap();

        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut b = SyncEngine::unscoped(RelayClient::new(&base), &workspace)
            .with_key(key)
            .with_page_limit(4);
        let before = gets.count();
        assert_eq!(b.pull_round(&mut store_b).await.unwrap(), 10);
        assert_eq!(gets.count() - before, 3, "4 + 4 + 2");
        assert!(!b.is_behind());
    }

    /// A body that can't be opened yet holds the durable cursor (the retry
    /// point) — paging must still walk past it to the head in the same catch-up,
    /// instead of re-fetching the same first page forever. Once caught up, the
    /// next round re-fetches from the retry point, and the body converges when
    /// its key arrives.
    #[tokio::test]
    async fn paging_walks_past_a_held_body_and_retries_it_later() {
        let (base, _gets) = spawn_paged_relay(2).await;
        let workspace = Uuid::new_v4().to_string();
        let held = hive_core::derive_workspace_key("held epoch 0");
        let missing = hive_core::e2ee::generate_workspace_key().unwrap();

        // seq 1: a snapshot sealed under epoch 7 (not held yet).
        let session = ChatSession::new("Shared", Uuid::new_v4(), "anthropic");
        let (sid, wid) = (session.id, session.workspace_id);
        let snap = SessionEventEnvelope::new(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(session) });
        let relay = RelayClient::new(&base);
        let sealed = seal_symmetric(&missing, 7, &serde_json::to_vec(&snap).unwrap()).unwrap();
        relay.push_value(&workspace, &serde_json::to_value(sealed).unwrap()).await.unwrap();
        // seqs 2..=7: openable messages in another chat.
        let mut store_a = EventStore::open_in_memory().unwrap();
        let other = seed_long_chat(&mut store_a, 5);
        let mut a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(held);
        assert_eq!(a.push_new(&store_a).await.unwrap(), 6);

        let mut store_b = EventStore::open_in_memory().unwrap();
        let mut b = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(held);
        assert_eq!(b.catch_up(&mut store_b, 10).await.unwrap(), 6, "every openable event applied");
        assert!(!b.is_behind(), "reached the head despite the held body");
        assert_eq!(b.cursor(), 0, "durable cursor still at the retry point");
        assert_eq!(store_b.load_session(other).unwrap().unwrap().messages.len(), 5);

        // The key arrives: the next round starts at the retry point again.
        b.add_key(7, missing);
        b.catch_up(&mut store_b, 10).await.unwrap();
        assert!(store_b.has_event(snap.event_id).unwrap(), "held body converges");
        assert_eq!(b.cursor(), 7);
        assert_eq!(store_b.sync_cursor(&workspace).unwrap(), 7);
    }

    /// The probe asks for one envelope, never the room's history.
    #[tokio::test]
    async fn probe_fetches_one_envelope() {
        let (base, _gets) = spawn_paged_relay(1000).await;
        let workspace = Uuid::new_v4().to_string();
        let key = hive_core::derive_workspace_key("probe");
        let mut store_a = EventStore::open_in_memory().unwrap();
        seed_long_chat(&mut store_a, 4);
        let mut a = SyncEngine::unscoped(RelayClient::new(&base), &workspace).with_key(key);
        a.push_new(&store_a).await.unwrap();
        let page = RelayClient::new(&base).fetch_page(&workspace, 0, 1).await.unwrap();
        assert_eq!(page.rows.len(), 1);
        assert!(page.more);
        assert_eq!(page.next_after, 1);
        assert_eq!(RelayClient::new(&base).probe(&workspace).await, crate::relay_client::RelayProbe::Ok);
    }
}
