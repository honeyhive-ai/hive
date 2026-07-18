//! Social graph: account device registry + account-scoped inbox ("account
//! channel"). State and the pure mutations live here so they're unit-testable
//! without a live GitHub token; the HTTP handlers in `lib.rs` wrap these with
//! GitHub-verified auth.
//!
//! Phase 1 of `docs/hive-social-graph-plan.md`. The "account channel" is
//! poll-based (an inbox with a monotonic `after` cursor, mirroring the
//! workspace envelope log) rather than a websocket — it's the idiomatic fit for
//! this relay (everything else polls) and delivers the same behavior: every
//! signed-in device of an account reads the same inbox, so a request fans out to
//! all of them and a `resolved` event dismisses it everywhere. A websocket push
//! layer can be added later as a latency optimization over this same inbox.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{RelayState, Stored};

/// One registered device under an account: its direct-P2P node id (for the
/// peer-link bootstrap in a later phase), a human label, and the last time it
/// checked in (drives presence).
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct DeviceReg {
    pub node_id: Option<String>,
    pub label: Option<String>,
    pub last_seen: u64,
}

/// Per-account social state: the append-only inbox (account channel) and the
/// device registry. Keyed in [`RelayState::accounts`] by the account key
/// (`github:<id>`), which every device of the account derives identically.
/// Presence within the last [`ONLINE_WINDOW_SECS`] reads as online, within
/// [`AWAY_WINDOW_SECS`] as away, older/never as offline.
const ONLINE_WINDOW_SECS: u64 = 70;
const AWAY_WINDOW_SECS: u64 = 300;
/// Pending friend requests auto-expire after this (abuse control).
const REQUEST_TTL_SECS: u64 = 14 * 24 * 60 * 60;
/// Cap on simultaneously-pending outbound requests per account (abuse control).
const MAX_PENDING_OUTBOUND: usize = 50;

/// Account-level presence (the §9 decision: per account, not per device).
pub(crate) fn presence_str(last_seen: u64, now: u64) -> &'static str {
    let age = now.saturating_sub(last_seen);
    if last_seen == 0 || age > AWAY_WINDOW_SECS {
        "offline"
    } else if age <= ONLINE_WINDOW_SECS {
        "online"
    } else {
        "away"
    }
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct AccountState {
    /// The account's GitHub login (most recent seen on register), so friends can
    /// be listed by `@handle` without a second lookup.
    pub login: String,
    /// When set, the account reports as offline to friends regardless of
    /// heartbeats ("appear offline").
    pub appear_offline: bool,
    /// Append-only, server-sequenced events fanned out to all the account's
    /// devices (friend requests, request-resolved dismissals, presence nudges).
    pub inbox: Vec<Stored>,
    pub next_seq: u64,
    /// device id → registration.
    pub devices: HashMap<String, DeviceReg>,
}

/// Lifecycle of a friend request.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub(crate) enum RequestState {
    Pending,
    Accepted,
    Rejected,
    Cancelled,
}

impl RequestState {
    fn as_str(self) -> &'static str {
        match self {
            RequestState::Pending => "pending",
            RequestState::Accepted => "accepted",
            RequestState::Rejected => "rejected",
            RequestState::Cancelled => "cancelled",
        }
    }
}

/// A pending/closed friend request. `from_account`/`to_account` are account keys
/// (`github:<id>`); the relay stamps `from` from the verified GitHub token, so a
/// request can't be forged to look like another user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FriendRequest {
    pub id: String,
    pub from_account: String,
    pub from_login: String,
    pub to_account: String,
    pub to_login: String,
    pub created_at: u64,
    pub state: RequestState,
}

/// Why a friend operation was refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FriendError {
    SelfRequest,
    AlreadyFriends,
    CapReached,
    NotFound,
    NotYours,
    NotPending,
    /// Too many simultaneously-pending outbound requests (abuse control).
    TooManyPending,
}

#[derive(Serialize)]
pub(crate) struct InboxRow {
    pub seq: u64,
    pub body: Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeviceRow {
    pub device_id: String,
    pub node_id: Option<String>,
    pub label: Option<String>,
    pub last_seen: u64,
}

impl RelayState {
    /// The canonical account key derived from a GitHub numeric id. Every device
    /// of the account computes the same value, so the inbox/registry are shared.
    pub(crate) fn account_key(github_id: u64) -> String {
        format!("github:{github_id}")
    }

    /// Register (or refresh) one of the account's devices, and index the login →
    /// account key so a friend can be targeted by `@username` in a later phase.
    pub(crate) fn register_account_device(
        &self,
        github_id: u64,
        login: &str,
        device_id: &str,
        node_id: Option<String>,
        label: Option<String>,
        now: u64,
    ) -> String {
        let key = Self::account_key(github_id);
        self.login_index
            .write()
            .unwrap()
            .insert(login.to_lowercase(), key.clone());
        let mut accts = self.accounts.write().unwrap();
        let acct = accts.entry(key.clone()).or_default();
        acct.login = login.to_string();
        acct.devices
            .insert(device_id.to_string(), DeviceReg { node_id, label, last_seen: now });
        key
    }

    /// Refresh a device's `last_seen`. Returns false if the device isn't
    /// registered (the caller should register first).
    pub(crate) fn heartbeat_device(&self, github_id: u64, device_id: &str, now: u64) -> bool {
        let key = Self::account_key(github_id);
        let mut accts = self.accounts.write().unwrap();
        match accts.get_mut(&key).and_then(|a| a.devices.get_mut(device_id)) {
            Some(d) => {
                d.last_seen = now;
                true
            }
            None => false,
        }
    }

    /// Append an event to an account's inbox; returns the assigned sequence.
    /// Used by later phases to deliver friend requests / resolutions to every
    /// device of `account_key`.
    pub(crate) fn push_account_event(&self, account_key: &str, body: Value) -> u64 {
        let mut accts = self.accounts.write().unwrap();
        let acct = accts.entry(account_key.to_string()).or_default();
        acct.next_seq += 1;
        let seq = acct.next_seq;
        acct.inbox.push(Stored { seq, body });
        seq
    }

    /// Inbox events with `seq > after` (the account-channel poll).
    pub(crate) fn account_inbox_after(&self, account_key: &str, after: u64) -> Vec<InboxRow> {
        let accts = self.accounts.read().unwrap();
        accts
            .get(account_key)
            .map(|a| {
                a.inbox
                    .iter()
                    .filter(|e| e.seq > after)
                    .map(|e| InboxRow { seq: e.seq, body: e.body.clone() })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The account's registered devices (for presence + P2P bootstrap).
    pub(crate) fn account_devices(&self, account_key: &str) -> Vec<DeviceRow> {
        let accts = self.accounts.read().unwrap();
        accts
            .get(account_key)
            .map(|a| {
                a.devices
                    .iter()
                    .map(|(id, d)| DeviceRow {
                        device_id: id.clone(),
                        node_id: d.node_id.clone(),
                        label: d.label.clone(),
                        last_seen: d.last_seen,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Resolve a GitHub login (case-insensitive) to its account key, if any
    /// device of that account has ever registered.
    pub(crate) fn account_key_for_login(&self, login: &str) -> Option<String> {
        self.login_index
            .read()
            .unwrap()
            .get(&login.trim().trim_start_matches('@').to_lowercase())
            .cloned()
    }

    /// The login last seen for an account key (for listing friends by handle).
    fn login_for_account(&self, account_key: &str) -> String {
        self.accounts
            .read()
            .unwrap()
            .get(account_key)
            .map(|a| a.login.clone())
            .unwrap_or_default()
    }

    // ── Friend graph (P2) ───────────────────────────────────────────────────

    /// Canonical (sorted) account-key pair, so an edge is stored once regardless
    /// of who sent the request.
    fn canonical_pair(a: &str, b: &str) -> (String, String) {
        if a <= b {
            (a.to_string(), b.to_string())
        } else {
            (b.to_string(), a.to_string())
        }
    }

    pub(crate) fn are_friends(&self, a: &str, b: &str) -> bool {
        self.friend_edges.read().unwrap().contains(&Self::canonical_pair(a, b))
    }

    /// Number of accepted friends an account has (drives the free-tier cap).
    pub(crate) fn friend_count(&self, account: &str) -> usize {
        self.friend_edges
            .read()
            .unwrap()
            .iter()
            .filter(|(a, b)| a == account || b == account)
            .count()
    }

    fn cap_blocks(&self, account: &str) -> bool {
        matches!(self.friend_cap, Some(cap) if self.friend_count(account) >= cap)
    }

    /// Open a friend request from one account to another. Idempotent: a duplicate
    /// pending request returns the existing one. The caller (HTTP handler) is
    /// responsible for pushing the inbox event to the target.
    pub(crate) fn create_friend_request(
        &self,
        from_account: &str,
        from_login: &str,
        to_account: &str,
        to_login: &str,
        now: u64,
    ) -> Result<FriendRequest, FriendError> {
        if from_account == to_account {
            return Err(FriendError::SelfRequest);
        }
        if self.are_friends(from_account, to_account) {
            return Err(FriendError::AlreadyFriends);
        }
        if self.cap_blocks(from_account) {
            return Err(FriendError::CapReached);
        }
        self.expire_pending(now);
        let mut reqs = self.friend_requests.write().unwrap();
        if let Some(existing) = reqs.values().find(|r| {
            r.state == RequestState::Pending
                && r.from_account == from_account
                && r.to_account == to_account
        }) {
            return Ok(existing.clone());
        }
        // Abuse control: bound how many requests one account can have in flight.
        let pending_out = reqs
            .values()
            .filter(|r| r.state == RequestState::Pending && r.from_account == from_account)
            .count();
        if pending_out >= MAX_PENDING_OUTBOUND {
            return Err(FriendError::TooManyPending);
        }
        let req = FriendRequest {
            id: new_request_id(),
            from_account: from_account.to_string(),
            from_login: from_login.to_string(),
            to_account: to_account.to_string(),
            to_login: to_login.to_string(),
            created_at: now,
            state: RequestState::Pending,
        };
        reqs.insert(req.id.clone(), req.clone());
        Ok(req)
    }

    /// Accept a pending request (only the recipient may). Creates the edge and
    /// returns the (now Accepted) request so the handler can notify both sides.
    pub(crate) fn accept_friend_request(
        &self,
        request_id: &str,
        acceptor_account: &str,
    ) -> Result<FriendRequest, FriendError> {
        let (from_account, to_account) = {
            let reqs = self.friend_requests.read().unwrap();
            let r = reqs.get(request_id).ok_or(FriendError::NotFound)?;
            if r.to_account != acceptor_account {
                return Err(FriendError::NotYours);
            }
            if r.state != RequestState::Pending {
                return Err(FriendError::NotPending);
            }
            (r.from_account.clone(), r.to_account.clone())
        };
        if self.cap_blocks(&from_account) || self.cap_blocks(&to_account) {
            return Err(FriendError::CapReached);
        }
        self.friend_edges
            .write()
            .unwrap()
            .insert(Self::canonical_pair(&from_account, &to_account));
        let mut reqs = self.friend_requests.write().unwrap();
        let r = reqs.get_mut(request_id).ok_or(FriendError::NotFound)?;
        r.state = RequestState::Accepted;
        Ok(r.clone())
    }

    /// Reject (recipient) or cancel (sender) a pending request. Returns the
    /// closed request so the handler can dismiss it on both accounts' devices.
    pub(crate) fn close_friend_request(
        &self,
        request_id: &str,
        actor_account: &str,
    ) -> Result<FriendRequest, FriendError> {
        let mut reqs = self.friend_requests.write().unwrap();
        let r = reqs.get_mut(request_id).ok_or(FriendError::NotFound)?;
        if r.state != RequestState::Pending {
            return Err(FriendError::NotPending);
        }
        r.state = if actor_account == r.to_account {
            RequestState::Rejected
        } else if actor_account == r.from_account {
            RequestState::Cancelled
        } else {
            return Err(FriendError::NotYours);
        };
        Ok(r.clone())
    }

    /// Remove an accepted friendship. Returns false if they weren't friends.
    pub(crate) fn remove_friend(&self, account: &str, other: &str) -> bool {
        self.friend_edges
            .write()
            .unwrap()
            .remove(&Self::canonical_pair(account, other))
    }

    /// An account's accepted friends as `(account_key, login)`.
    pub(crate) fn list_friends(&self, account: &str) -> Vec<(String, String)> {
        let others: Vec<String> = {
            let edges = self.friend_edges.read().unwrap();
            edges
                .iter()
                .filter_map(|(a, b)| {
                    if a == account {
                        Some(b.clone())
                    } else if b == account {
                        Some(a.clone())
                    } else {
                        None
                    }
                })
                .collect()
        };
        others
            .into_iter()
            .map(|k| {
                let login = self.login_for_account(&k);
                (k, login)
            })
            .collect()
    }

    /// Pending requests addressed *to* this account (its incoming list).
    pub(crate) fn incoming_requests(&self, account: &str, now: u64) -> Vec<FriendRequest> {
        self.expire_pending(now);
        self.friend_requests
            .read()
            .unwrap()
            .values()
            .filter(|r| r.state == RequestState::Pending && r.to_account == account)
            .cloned()
            .collect()
    }

    /// Mark any pending request older than [`REQUEST_TTL_SECS`] as cancelled.
    fn expire_pending(&self, now: u64) {
        let mut reqs = self.friend_requests.write().unwrap();
        for r in reqs.values_mut() {
            if r.state == RequestState::Pending && now.saturating_sub(r.created_at) > REQUEST_TTL_SECS
            {
                r.state = RequestState::Cancelled;
            }
        }
    }

    // ── Presence (P3) ───────────────────────────────────────────────────────

    /// Set the caller's "appear offline" flag. No-op if never registered.
    pub(crate) fn set_visibility(&self, github_id: u64, appear_offline: bool) {
        let key = Self::account_key(github_id);
        if let Some(acct) = self.accounts.write().unwrap().get_mut(&key) {
            acct.appear_offline = appear_offline;
        }
    }

    /// An account's presence: the freshest device heartbeat mapped to a state,
    /// or `offline` when it has chosen to appear offline.
    pub(crate) fn presence_of(&self, account_key: &str, now: u64) -> &'static str {
        let accts = self.accounts.read().unwrap();
        match accts.get(account_key) {
            Some(a) if a.appear_offline => "offline",
            Some(a) => {
                let latest = a.devices.values().map(|d| d.last_seen).max().unwrap_or(0);
                presence_str(latest, now)
            }
            None => "offline",
        }
    }

    /// A friend's registered devices (for the P2P peer-link bootstrap): only
    /// returned when `caller` and `friend` are accepted friends, so node ids
    /// aren't disclosed to strangers. `None` = not friends (caller gets 403).
    pub(crate) fn friend_devices(&self, caller: &str, friend: &str) -> Option<Vec<DeviceRow>> {
        if !self.are_friends(caller, friend) {
            return None;
        }
        Some(self.account_devices(friend))
    }

    /// Each accepted friend with their `(account_key, login, presence)`.
    pub(crate) fn friend_presence(&self, account: &str, now: u64) -> Vec<(String, String, String)> {
        self.list_friends(account)
            .into_iter()
            .map(|(key, login)| {
                let presence = self.presence_of(&key, now).to_string();
                (key, login, presence)
            })
            .collect()
    }
}

/// A request id: opaque, collision-resistant enough for an in-memory store.
/// (No `uuid`/`rand` dep in this crate; mirrors `random_pair_code`'s approach.)
fn new_request_id() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let mut h = DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut h);
    CTR.fetch_add(1, Ordering::Relaxed).hash(&mut h);
    let a = h.finish();
    // Second hash for more bits.
    let mut h2 = DefaultHasher::new();
    a.hash(&mut h2);
    "fr".chars().chain(format!("{a:016x}{:016x}", h2.finish()).chars()).collect()
}

/// JSON helpers for the inbox event bodies (so handlers + tests agree on shape).
impl FriendRequest {
    pub(crate) fn request_event(&self) -> Value {
        serde_json::json!({
            "kind": "friendRequest",
            "requestId": self.id,
            "fromAccount": self.from_account,
            "fromLogin": self.from_login,
            "createdAt": self.created_at,
        })
    }

    pub(crate) fn resolved_event(&self) -> Value {
        serde_json::json!({
            "kind": "friendResolved",
            "requestId": self.id,
            "state": self.state.as_str(),
            "fromAccount": self.from_account,
            "toAccount": self.to_account,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> RelayState {
        RelayState::default()
    }

    #[test]
    fn two_devices_share_one_account_and_its_inbox() {
        let s = state();
        s.register_account_device(42, "Octocat", "devA", Some("nodeA".into()), None, 100);
        s.register_account_device(42, "octocat", "devB", Some("nodeB".into()), None, 101);

        let key = RelayState::account_key(42);
        // Both devices land under the same account.
        assert_eq!(s.account_devices(&key).len(), 2);
        // Login index is case-insensitive and resolves to that account.
        assert_eq!(s.account_key_for_login("@OCTOCAT"), Some(key.clone()));

        // An event pushed to the account is visible to every device's poll.
        let seq = s.push_account_event(&key, serde_json::json!({ "kind": "ping" }));
        assert_eq!(seq, 1);
        let rows = s.account_inbox_after(&key, 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, 1);
        // After acknowledging seq 1, the poll is empty (cursor semantics).
        assert!(s.account_inbox_after(&key, 1).is_empty());
    }

    #[test]
    fn inbox_sequence_is_monotonic_per_account() {
        let s = state();
        let key = RelayState::account_key(7);
        assert_eq!(s.push_account_event(&key, serde_json::json!({})), 1);
        assert_eq!(s.push_account_event(&key, serde_json::json!({})), 2);
        assert_eq!(s.push_account_event(&key, serde_json::json!({})), 3);
        assert_eq!(s.account_inbox_after(&key, 1).len(), 2);
    }

    #[test]
    fn heartbeat_updates_last_seen_only_for_registered_devices() {
        let s = state();
        s.register_account_device(9, "user", "dev1", None, None, 1000);
        assert!(s.heartbeat_device(9, "dev1", 2000));
        assert!(!s.heartbeat_device(9, "ghost", 2000));
        let dev = s.account_devices(&RelayState::account_key(9));
        assert_eq!(dev.iter().find(|d| d.device_id == "dev1").unwrap().last_seen, 2000);
    }

    #[test]
    fn unknown_account_polls_empty() {
        let s = state();
        assert!(s.account_inbox_after("github:999", 0).is_empty());
        assert!(s.account_devices("github:999").is_empty());
        assert_eq!(s.account_key_for_login("nobody"), None);
    }

    // ── Friend graph (P2) ───────────────────────────────────────────────────

    fn key(id: u64) -> String {
        RelayState::account_key(id)
    }

    #[test]
    fn request_accept_creates_a_symmetric_edge() {
        let s = state();
        // Both accounts have signed in (registered a device), as in production.
        s.register_account_device(1, "alice", "d1", None, None, 0);
        s.register_account_device(2, "bob", "d2", None, None, 0);
        let (a, b) = (key(1), key(2));
        let req = s.create_friend_request(&a, "alice", &b, "bob", 10).unwrap();
        assert!(!s.are_friends(&a, &b));
        // Recipient accepts.
        let accepted = s.accept_friend_request(&req.id, &b).unwrap();
        assert_eq!(accepted.state, RequestState::Accepted);
        // Edge exists from both directions; counted once each side.
        assert!(s.are_friends(&a, &b));
        assert!(s.are_friends(&b, &a));
        assert_eq!(s.friend_count(&a), 1);
        assert_eq!(s.list_friends(&a).first().map(|(_, l)| l.clone()), Some("bob".into()));
    }

    #[test]
    fn only_the_recipient_can_accept_and_request_must_be_pending() {
        let s = state();
        let (a, b, c) = (key(1), key(2), key(3));
        let req = s.create_friend_request(&a, "a", &b, "b", 0).unwrap();
        assert_eq!(s.accept_friend_request(&req.id, &c), Err(FriendError::NotYours));
        assert_eq!(s.accept_friend_request(&req.id, &a), Err(FriendError::NotYours));
        s.accept_friend_request(&req.id, &b).unwrap();
        // Second accept is no longer pending.
        assert_eq!(s.accept_friend_request(&req.id, &b), Err(FriendError::NotPending));
    }

    #[test]
    fn duplicate_request_is_idempotent_and_self_request_rejected() {
        let s = state();
        let (a, b) = (key(1), key(2));
        let r1 = s.create_friend_request(&a, "a", &b, "b", 0).unwrap();
        let r2 = s.create_friend_request(&a, "a", &b, "b", 0).unwrap();
        assert_eq!(r1.id, r2.id);
        assert_eq!(
            s.create_friend_request(&a, "a", &a, "a", 0),
            Err(FriendError::SelfRequest)
        );
    }

    #[test]
    fn reject_closes_request_without_an_edge() {
        let s = state();
        let (a, b) = (key(1), key(2));
        let req = s.create_friend_request(&a, "a", &b, "b", 0).unwrap();
        let closed = s.close_friend_request(&req.id, &b).unwrap();
        assert_eq!(closed.state, RequestState::Rejected);
        assert!(!s.are_friends(&a, &b));
        assert!(s.incoming_requests(&b, 1).is_empty());
    }

    #[test]
    fn cancel_by_sender_uses_cancelled_state() {
        let s = state();
        let (a, b) = (key(1), key(2));
        let req = s.create_friend_request(&a, "a", &b, "b", 0).unwrap();
        assert_eq!(
            s.close_friend_request(&req.id, &a).unwrap().state,
            RequestState::Cancelled
        );
    }

    #[test]
    fn remove_friend_drops_the_edge() {
        let s = state();
        let (a, b) = (key(1), key(2));
        let req = s.create_friend_request(&a, "a", &b, "b", 0).unwrap();
        s.accept_friend_request(&req.id, &b).unwrap();
        assert!(s.remove_friend(&a, &b));
        assert!(!s.are_friends(&a, &b));
        assert!(!s.remove_friend(&a, &b)); // already gone
    }

    #[test]
    fn already_friends_request_is_rejected() {
        let s = state();
        let (a, b) = (key(1), key(2));
        let req = s.create_friend_request(&a, "a", &b, "b", 0).unwrap();
        s.accept_friend_request(&req.id, &b).unwrap();
        assert_eq!(
            s.create_friend_request(&a, "a", &b, "b", 0),
            Err(FriendError::AlreadyFriends)
        );
    }

    #[test]
    fn friend_cap_blocks_request_and_accept() {
        // Cap of 1: alice befriends bob, then can't request carol.
        let s = RelayState::default().with_friend_cap(Some(1));
        let (a, b, c) = (key(1), key(2), key(3));
        let r1 = s.create_friend_request(&a, "a", &b, "b", 0).unwrap();
        s.accept_friend_request(&r1.id, &b).unwrap();
        assert_eq!(
            s.create_friend_request(&a, "a", &c, "c", 0),
            Err(FriendError::CapReached)
        );
        // And if a request predates the cap being hit, accept re-checks it.
        let s2 = RelayState::default().with_friend_cap(Some(1));
        let pending = s2.create_friend_request(&c, "c", &a, "a", 0).unwrap();
        let r = s2.create_friend_request(&b, "b", &a, "a", 0).unwrap();
        s2.accept_friend_request(&r.id, &a).unwrap(); // a now has 1 friend
        assert_eq!(s2.accept_friend_request(&pending.id, &a), Err(FriendError::CapReached));
    }

    // ── Presence + abuse controls (P3) ──────────────────────────────────────

    #[test]
    fn presence_maps_last_seen_to_states() {
        assert_eq!(presence_str(0, 1000), "offline"); // never seen
        assert_eq!(presence_str(1000, 1000), "online"); // just now
        assert_eq!(presence_str(1000, 1000 + 60), "online"); // within window
        assert_eq!(presence_str(1000, 1000 + 200), "away"); // past online window
        assert_eq!(presence_str(1000, 1000 + 1000), "offline"); // past away window
    }

    #[test]
    fn friend_presence_reflects_heartbeats_and_appear_offline() {
        let s = state();
        s.register_account_device(1, "alice", "d1", None, None, 0);
        s.register_account_device(2, "bob", "d2", None, None, 0);
        let (a, b) = (key(1), key(2));
        let req = s.create_friend_request(&a, "alice", &b, "bob", 0).unwrap();
        s.accept_friend_request(&req.id, &b).unwrap();

        // Bob heartbeats at t=1000; alice sees him online shortly after.
        s.heartbeat_device(2, "d2", 1000);
        let pres = s.friend_presence(&a, 1010);
        assert_eq!(pres[0].2, "online");
        // Later, he drifts to away then offline.
        assert_eq!(s.friend_presence(&a, 1000 + 200)[0].2, "away");
        assert_eq!(s.friend_presence(&a, 1000 + 1000)[0].2, "offline");

        // Bob appears offline → always offline regardless of heartbeat.
        s.heartbeat_device(2, "d2", 2000);
        s.set_visibility(2, true);
        assert_eq!(s.friend_presence(&a, 2000)[0].2, "offline");
    }

    #[test]
    fn pending_requests_expire_and_drop_from_incoming() {
        let s = state();
        let (a, b) = (key(1), key(2));
        let req = s.create_friend_request(&a, "a", &b, "b", 0).unwrap();
        assert_eq!(s.incoming_requests(&b, 100).len(), 1);
        // Past the TTL, the request is expired and no longer pending/incoming.
        let later = REQUEST_TTL_SECS + 10;
        assert!(s.incoming_requests(&b, later).is_empty());
        // Re-requesting now succeeds (the stale one no longer blocks).
        let again = s.create_friend_request(&a, "a", &b, "b", later).unwrap();
        assert_ne!(again.id, req.id);
    }

    #[test]
    fn too_many_pending_outbound_is_capped() {
        let s = state();
        let from = key(1);
        for i in 0..MAX_PENDING_OUTBOUND {
            let to = key(100 + i as u64);
            s.create_friend_request(&from, "me", &to, "t", 0).unwrap();
        }
        let over = key(999);
        assert_eq!(
            s.create_friend_request(&from, "me", &over, "t", 0),
            Err(FriendError::TooManyPending)
        );
    }

    // ── P2P bootstrap discovery (P4) ────────────────────────────────────────

    #[test]
    fn friend_devices_are_visible_only_between_friends() {
        let s = state();
        s.register_account_device(1, "alice", "d1", Some("node-a".into()), None, 0);
        s.register_account_device(2, "bob", "d2", Some("node-b".into()), None, 0);
        let (a, b, stranger) = (key(1), key(2), key(3));
        // Strangers can't see each other's devices.
        assert!(s.friend_devices(&a, &b).is_none());
        // Become friends → each can fetch the other's dialable node ids.
        let req = s.create_friend_request(&a, "alice", &b, "bob", 0).unwrap();
        s.accept_friend_request(&req.id, &b).unwrap();
        let bob_devs = s.friend_devices(&a, &b).unwrap();
        assert_eq!(bob_devs.len(), 1);
        assert_eq!(bob_devs[0].node_id.as_deref(), Some("node-b"));
        assert!(s.friend_devices(&b, &a).is_some());
        // A non-friend third party still can't.
        assert!(s.friend_devices(&stranger, &a).is_none());
    }

    #[test]
    fn inbox_events_have_the_expected_shape() {
        let s = state();
        let (a, b) = (key(1), key(2));
        let req = s.create_friend_request(&a, "alice", &b, "bob", 7).unwrap();
        let ev = req.request_event();
        assert_eq!(ev["kind"], "friendRequest");
        assert_eq!(ev["fromLogin"], "alice");
        assert_eq!(ev["requestId"], req.id);
        let accepted = s.accept_friend_request(&req.id, &b).unwrap();
        let rev = accepted.resolved_event();
        assert_eq!(rev["kind"], "friendResolved");
        assert_eq!(rev["state"], "accepted");
    }
}
