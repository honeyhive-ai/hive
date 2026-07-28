//! Option C identity mode — "survive a hostile relay" (security plan S1).
//!
//! Option **A** (the live default, [`IdentityMode::Relay`]) trusts the roster's
//! account keys as-is: the relay's GitHub-gated directory vouched them. This
//! module adds Option **C** ([`IdentityMode::GithubSigningKeys`]), where a
//! member's account signing key is verified against **GitHub's native
//! `ssh_signing_keys` directory** instead of trusting the relay. A malicious
//! relay can no longer substitute a key: verifiers fetch
//! `https://api.github.com/users/<login>/ssh_signing_keys` straight from GitHub
//! and confirm the account's registered signing key is listed there.
//!
//! ## Convergence — the load-bearing constraint
//!
//! Two devices that are at *different* GitHub-verification states must never
//! diverge in *applied* state. The rule (mirroring the phase-7 "don't drop
//! undecodable body" / `NotYetOpenable` retry path):
//!
//! - **Verified** (GitHub fetched, account key IS listed) → **accept**.
//! - **NotListed** (GitHub fetched, account key is NOT listed) → **quarantine**
//!   — provably bad, and *stable* (GitHub is authoritative & shared, so every
//!   device eventually fetches the same answer).
//! - **Unknown** (GitHub not fetched yet / unreachable / rate-limited / the
//!   login couldn't be resolved) → **HOLD and retry**, *never* drop. A peer
//!   without network yet would otherwise resolve this event differently into
//!   applied-vs-dropped state — that is the divergence we must not create.
//!
//! Because GitHub is a shared authoritative source, all devices *eventually*
//! reach the identical verified set → convergence in the limit. The hold is
//! implemented exactly like a missing epoch key: the sync cursor is held before
//! the event so it is re-fetched and re-classified once verification completes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use uuid::Uuid;

use crate::envelope_verifier::{QuarantineReason, Verdict};

const USER_AGENT: &str = "hive-app";
/// How long a fetched GitHub verdict is trusted before a re-fetch. Keeps the
/// hot ingest path off the network while still noticing key changes eventually.
pub const DEFAULT_TTL: Duration = Duration::from_secs(10 * 60);

/// How a member's account signing key is bound to their real identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IdentityMode {
    /// Option A (default): trust the roster's account keys as-is — the relay's
    /// GitHub-gated directory vouched them. **Current, unchanged behavior.**
    #[default]
    Relay,
    /// Option C: verify each account key against the login's GitHub
    /// `ssh_signing_keys`. Survives a hostile relay.
    GithubSigningKeys,
}

impl IdentityMode {
    /// Read the mode from `HIVE_IDENTITY_MODE` (`relay` | `github-signing-keys`).
    /// Anything unset/unrecognized falls back to [`IdentityMode::Relay`] so the
    /// default deployment is byte-for-byte unchanged.
    pub fn from_env() -> Self {
        match std::env::var("HIVE_IDENTITY_MODE").ok().as_deref() {
            Some("github-signing-keys") | Some("github") | Some("option-c") => {
                IdentityMode::GithubSigningKeys
            }
            _ => IdentityMode::Relay,
        }
    }
}

/// The GitHub-identity verdict for a single account key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityVerdict {
    /// GitHub fetched; the account key IS one of the login's signing keys.
    Verified,
    /// GitHub fetched; the account key is NOT listed — provably bad.
    NotListed,
    /// Not fetched yet, unreachable, rate-limited, or the login is unknown.
    /// Never a reason to drop — hold and retry.
    Unknown,
}

/// What the ingest path should do with an event, once identity is folded in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestAction {
    /// Signature + identity check out — project it.
    Accept,
    /// Provably bad — drop it (advance the cursor past it; re-fetching won't help).
    Quarantine(QuarantineReason),
    /// Can't decide yet — hold the cursor before it and retry (never drop). The
    /// `&'static str` is a log reason.
    Hold(&'static str),
}

// ---------------------------------------------------------------------------
// Verdict cache (per account, TTL) — shared between the async populator (which
// hits GitHub) and the sync engine (which reads it synchronously on ingest).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Entry {
    /// The exact account key this verdict was computed for. If the pinned key in
    /// the log later differs (shouldn't — keys are first-registration-pinned) the
    /// entry is treated as a miss, so a stale verdict can't bless a new key.
    key: Vec<u8>,
    verdict: IdentityVerdict,
    expires_at: Instant,
}

/// A TTL cache of GitHub-identity verdicts, keyed by account id.
#[derive(Debug, Default)]
pub struct IdentityCache {
    entries: HashMap<Uuid, Entry>,
}

impl IdentityCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current verdict for `account`'s `key`, or `Unknown` if we have no
    /// fresh entry for exactly this key. `Unknown` means *hold* — never drop.
    pub fn verdict(&self, account: Uuid, key: &[u8]) -> IdentityVerdict {
        self.verdict_at(account, key, Instant::now())
    }

    fn verdict_at(&self, account: Uuid, key: &[u8], now: Instant) -> IdentityVerdict {
        match self.entries.get(&account) {
            Some(e) if e.key == key && now < e.expires_at => e.verdict,
            _ => IdentityVerdict::Unknown,
        }
    }

    /// Record a fetched verdict for `account`'s `key` with the given TTL.
    pub fn put(&mut self, account: Uuid, key: Vec<u8>, verdict: IdentityVerdict, ttl: Duration) {
        self.put_at(account, key, verdict, ttl, Instant::now());
    }

    fn put_at(
        &mut self,
        account: Uuid,
        key: Vec<u8>,
        verdict: IdentityVerdict,
        ttl: Duration,
        now: Instant,
    ) {
        self.entries.insert(
            account,
            Entry {
                key,
                verdict,
                expires_at: now + ttl,
            },
        );
    }
}

/// A cache shared between the async GitHub populator and the sync engine.
pub type SharedIdentityCache = Arc<Mutex<IdentityCache>>;

/// A fresh empty shared cache.
pub fn shared_cache() -> SharedIdentityCache {
    Arc::new(Mutex::new(IdentityCache::new()))
}

// ---------------------------------------------------------------------------
// The gate: fold the GitHub-identity verdict into the roster verdict. Pure and
// order-free, so it is trivially convergent given the same (roster, cache).
// ---------------------------------------------------------------------------

/// Combine the roster [`Verdict`] with the identity `mode` + cache into an
/// [`IngestAction`]. **Option A (`Relay`) is byte-for-byte the current policy:**
/// accept `Valid`/`Unverifiable`, quarantine `Quarantine`, never hold on
/// identity. Option C additionally gates a roster-`Valid` event on the signer's
/// account being GitHub-verified.
pub fn gate_ingest(
    mode: IdentityMode,
    base: Verdict,
    signer_account: Option<Uuid>,
    account_key: Option<&[u8]>,
    cache: &IdentityCache,
) -> IngestAction {
    match mode {
        // Option A: exactly the pre-existing policy (drop only provably-bad).
        IdentityMode::Relay => match base {
            Verdict::Valid | Verdict::Unverifiable(_) => IngestAction::Accept,
            Verdict::Quarantine(r) => IngestAction::Quarantine(r),
        },
        IdentityMode::GithubSigningKeys => match base {
            // Provably bad by signature/roster — drop regardless of identity.
            Verdict::Quarantine(r) => IngestAction::Quarantine(r),
            // Device isn't even in the roster yet (or unsigned): we can't tell
            // whose account it is. Hold for retry — its cert (and then GitHub
            // verification) may still arrive. Never drop.
            Verdict::Unverifiable(_) => {
                IngestAction::Hold("device/cert not yet known (option-c hold)")
            }
            // Signature + cert chain are good; now require GitHub identity.
            Verdict::Valid => {
                let (Some(account), Some(key)) = (signer_account, account_key) else {
                    // Valid but we somehow lack the account's pinned key — hold.
                    return IngestAction::Hold("account key unknown (option-c hold)");
                };
                match cache.verdict(account, key) {
                    IdentityVerdict::Verified => IngestAction::Accept,
                    IdentityVerdict::NotListed => {
                        IngestAction::Quarantine(QuarantineReason::GithubKeyNotListed)
                    }
                    // Not fetched yet / unreachable / rate-limited / unknown login.
                    IdentityVerdict::Unknown => {
                        IngestAction::Hold("github identity not yet verified (option-c hold)")
                    }
                }
            }
        },
    }
}

// ---------------------------------------------------------------------------
// GitHub fetch — reuse github.rs's reqwest/user-agent pattern. Mockable in
// tests by injecting a `SigningKeyFetcher` (never hits the network in tests).
// ---------------------------------------------------------------------------

/// The result of trying to fetch a login's GitHub signing keys — the four
/// outcomes the plan asks us to handle distinctly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    /// 200: the login's ed25519 signing keys (raw 32-byte pubkeys).
    Keys(Vec<[u8; 32]>),
    /// 404: no such login (possibly a relay-supplied wrong mapping) — treat as
    /// Unknown/hold, not a quarantine, so a bad login can't drop good events.
    NotFound,
    /// 403/429: rate-limited — hold and retry after the TTL.
    RateLimited,
    /// Network/transport error or an unexpected status — hold and retry.
    Unreachable(String),
}

/// Classify an account key against a fetch outcome. Pure — the decision core.
pub fn classify(account_key: &[u8], outcome: &FetchOutcome) -> IdentityVerdict {
    match outcome {
        FetchOutcome::Keys(keys) => {
            if account_key.len() == 32 && keys.iter().any(|k| k.as_slice() == account_key) {
                IdentityVerdict::Verified
            } else {
                IdentityVerdict::NotListed
            }
        }
        // A definitively-empty listing (200 → Keys([])) is NotListed above; a
        // 404 / rate-limit / transport error is inconclusive → hold.
        FetchOutcome::NotFound | FetchOutcome::RateLimited | FetchOutcome::Unreachable(_) => {
            IdentityVerdict::Unknown
        }
    }
}

/// Fetch + classify + cache a single account's GitHub identity. Generic over the
/// fetcher so tests inject a stub; production passes [`fetch_ssh_signing_keys`].
/// Only definitive verdicts (`Verified`/`NotListed`) are cached; an inconclusive
/// (`Unknown`) fetch leaves the cache untouched so the account stays held and is
/// retried on the next pass (no negative caching of transient failures).
pub async fn refresh_account<F, Fut>(
    login: &str,
    account: Uuid,
    account_key: &[u8],
    cache: &SharedIdentityCache,
    ttl: Duration,
    fetch: F,
) -> IdentityVerdict
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = FetchOutcome>,
{
    let outcome = fetch(login.to_string()).await;
    let verdict = classify(account_key, &outcome);
    if verdict != IdentityVerdict::Unknown {
        if let Ok(mut c) = cache.lock() {
            c.put(account, account_key.to_vec(), verdict, ttl);
        }
    }
    verdict
}

/// Live fetch of `https://api.github.com/users/<login>/ssh_signing_keys`. No
/// token needed (public endpoint). Parses each listed key to a raw ed25519
/// pubkey; non-ed25519 keys are ignored (Hive account keys are ed25519).
pub async fn fetch_ssh_signing_keys(login: String) -> FetchOutcome {
    let client = match reqwest::Client::builder().user_agent(USER_AGENT).build() {
        Ok(c) => c,
        Err(e) => return FetchOutcome::Unreachable(e.to_string()),
    };
    let url = format!("https://api.github.com/users/{login}/ssh_signing_keys");
    let resp = match client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return FetchOutcome::Unreachable(e.to_string()),
    };
    let status = resp.status();
    if status.as_u16() == 404 {
        return FetchOutcome::NotFound;
    }
    if status.as_u16() == 403 || status.as_u16() == 429 {
        return FetchOutcome::RateLimited;
    }
    if !status.is_success() {
        return FetchOutcome::Unreachable(format!("github returned {status}"));
    }
    let rows: Vec<serde_json::Value> = match resp.json().await {
        Ok(v) => v,
        Err(e) => return FetchOutcome::Unreachable(e.to_string()),
    };
    let keys = rows
        .iter()
        .filter_map(|row| row.get("key").and_then(|k| k.as_str()))
        .filter_map(parse_openssh_ed25519)
        .collect();
    FetchOutcome::Keys(keys)
}

/// Parse an OpenSSH authorized-keys line (`ssh-ed25519 AAAA... [comment]`) into
/// its raw 32-byte ed25519 public key. Returns `None` for non-ed25519 keys or
/// malformed blobs. Public for tests.
pub fn parse_openssh_ed25519(line: &str) -> Option<[u8; 32]> {
    // Find the base64 blob: the token right after an "ssh-ed25519" token, or the
    // first token that decodes to a valid ssh-ed25519 wire blob.
    let mut tokens = line.split_whitespace();
    let blob_b64 = match tokens.next() {
        Some("ssh-ed25519") => tokens.next()?,
        // Some callers hand just the blob; try the first token as the blob.
        Some(t) => t,
        None => return None,
    };
    let blob = STANDARD.decode(blob_b64).ok()?;
    parse_ed25519_wire(&blob)
}

/// Parse the SSH wire format of an ed25519 public key blob:
/// `string "ssh-ed25519"` then `string <32-byte key>`, each length-prefixed by a
/// big-endian u32.
fn parse_ed25519_wire(blob: &[u8]) -> Option<[u8; 32]> {
    let mut pos = 0usize;
    let read_str = |blob: &[u8], pos: &mut usize| -> Option<Vec<u8>> {
        if *pos + 4 > blob.len() {
            return None;
        }
        let len =
            u32::from_be_bytes([blob[*pos], blob[*pos + 1], blob[*pos + 2], blob[*pos + 3]]) as usize;
        *pos += 4;
        if *pos + len > blob.len() {
            return None;
        }
        let s = blob[*pos..*pos + len].to_vec();
        *pos += len;
        Some(s)
    };
    let algo = read_str(blob, &mut pos)?;
    if algo != b"ssh-ed25519" {
        return None;
    }
    let key = read_str(blob, &mut pos)?;
    key.try_into().ok()
}

/// Encode a raw 32-byte ed25519 public key as an OpenSSH `ssh-ed25519 AAAA...`
/// line — the inverse of [`parse_openssh_ed25519`]. Handy for tests and for
/// showing a user which line to add to their GitHub signing keys.
pub fn to_openssh_ed25519(key: &[u8; 32]) -> String {
    let mut blob = Vec::with_capacity(4 + 11 + 4 + 32);
    blob.extend_from_slice(&(11u32).to_be_bytes());
    blob.extend_from_slice(b"ssh-ed25519");
    blob.extend_from_slice(&(32u32).to_be_bytes());
    blob.extend_from_slice(key);
    format!("ssh-ed25519 {}", STANDARD.encode(&blob))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    #[test]
    fn openssh_roundtrip_and_reject_non_ed25519() {
        let k = key(7);
        let line = to_openssh_ed25519(&k);
        assert!(line.starts_with("ssh-ed25519 "));
        assert_eq!(parse_openssh_ed25519(&line), Some(k));
        // With a trailing comment (GitHub sometimes includes a title).
        assert_eq!(parse_openssh_ed25519(&format!("{line} mona@example")), Some(k));
        // An RSA line parses to None (not ed25519).
        let rsa = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAB comment";
        assert_eq!(parse_openssh_ed25519(rsa), None);
        assert_eq!(parse_openssh_ed25519("garbage"), None);
    }

    #[test]
    fn classify_listed_unlisted_and_inconclusive() {
        let mine = key(1);
        let other = key(2);
        assert_eq!(
            classify(&mine, &FetchOutcome::Keys(vec![key(9), mine])),
            IdentityVerdict::Verified
        );
        assert_eq!(
            classify(&mine, &FetchOutcome::Keys(vec![other])),
            IdentityVerdict::NotListed
        );
        // Empty listing is a definitive "not listed".
        assert_eq!(classify(&mine, &FetchOutcome::Keys(vec![])), IdentityVerdict::NotListed);
        // Inconclusive fetches → Unknown (hold).
        assert_eq!(classify(&mine, &FetchOutcome::NotFound), IdentityVerdict::Unknown);
        assert_eq!(classify(&mine, &FetchOutcome::RateLimited), IdentityVerdict::Unknown);
        assert_eq!(
            classify(&mine, &FetchOutcome::Unreachable("dns".into())),
            IdentityVerdict::Unknown
        );
    }

    #[test]
    fn cache_ttl_and_key_binding() {
        let mut c = IdentityCache::new();
        let acct = Uuid::new_v4();
        let k = key(3).to_vec();
        let now = Instant::now();
        // Miss before any entry.
        assert_eq!(c.verdict_at(acct, &k, now), IdentityVerdict::Unknown);
        c.put_at(acct, k.clone(), IdentityVerdict::Verified, Duration::from_secs(60), now);
        // Fresh hit.
        assert_eq!(c.verdict_at(acct, &k, now), IdentityVerdict::Verified);
        // Expired → miss.
        assert_eq!(
            c.verdict_at(acct, &k, now + Duration::from_secs(61)),
            IdentityVerdict::Unknown
        );
        // A different key for the same account is NOT covered by the entry.
        assert_eq!(c.verdict_at(acct, &key(4), now), IdentityVerdict::Unknown);
    }

    #[test]
    fn env_default_is_relay() {
        // Default (var handling) resolves to Relay for anything unrecognized.
        assert_eq!(IdentityMode::default(), IdentityMode::Relay);
    }

    #[test]
    fn gate_option_a_is_unchanged_policy() {
        let cache = IdentityCache::new();
        let acct = Uuid::new_v4();
        let k = key(1);
        // Relay mode: accept valid + unverifiable, drop only provably-bad —
        // identical to the pre-Option-C policy, cache irrelevant.
        assert_eq!(
            gate_ingest(IdentityMode::Relay, Verdict::Valid, Some(acct), Some(&k), &cache),
            IngestAction::Accept
        );
        assert_eq!(
            gate_ingest(
                IdentityMode::Relay,
                Verdict::Unverifiable(QuarantineReason::UnknownDevice),
                None,
                None,
                &cache
            ),
            IngestAction::Accept
        );
        assert_eq!(
            gate_ingest(
                IdentityMode::Relay,
                Verdict::Quarantine(QuarantineReason::BadSignature),
                Some(acct),
                Some(&k),
                &cache
            ),
            IngestAction::Quarantine(QuarantineReason::BadSignature)
        );
    }

    #[test]
    fn gate_option_c_holds_unverified_accepts_verified_quarantines_notlisted() {
        let acct = Uuid::new_v4();
        let k = key(5);

        // Not-yet-verified account (cache empty) → HOLD, never drop.
        let empty = IdentityCache::new();
        assert!(matches!(
            gate_ingest(
                IdentityMode::GithubSigningKeys,
                Verdict::Valid,
                Some(acct),
                Some(&k),
                &empty
            ),
            IngestAction::Hold(_)
        ));

        // Verified → accept.
        let mut verified = IdentityCache::new();
        verified.put(acct, k.to_vec(), IdentityVerdict::Verified, Duration::from_secs(60));
        assert_eq!(
            gate_ingest(
                IdentityMode::GithubSigningKeys,
                Verdict::Valid,
                Some(acct),
                Some(&k),
                &verified
            ),
            IngestAction::Accept
        );

        // NotListed (GitHub fetched, key absent) → quarantine (provably bad).
        let mut notlisted = IdentityCache::new();
        notlisted.put(acct, k.to_vec(), IdentityVerdict::NotListed, Duration::from_secs(60));
        assert_eq!(
            gate_ingest(
                IdentityMode::GithubSigningKeys,
                Verdict::Valid,
                Some(acct),
                Some(&k),
                &notlisted
            ),
            IngestAction::Quarantine(QuarantineReason::GithubKeyNotListed)
        );

        // A roster-Unverifiable (unknown device) event holds in Option C, not accept.
        assert!(matches!(
            gate_ingest(
                IdentityMode::GithubSigningKeys,
                Verdict::Unverifiable(QuarantineReason::UnknownDevice),
                None,
                None,
                &empty
            ),
            IngestAction::Hold(_)
        ));

        // Provably-bad signature is dropped regardless of identity mode.
        assert_eq!(
            gate_ingest(
                IdentityMode::GithubSigningKeys,
                Verdict::Quarantine(QuarantineReason::BadSignature),
                Some(acct),
                Some(&k),
                &empty
            ),
            IngestAction::Quarantine(QuarantineReason::BadSignature)
        );
    }

    #[tokio::test]
    async fn refresh_account_caches_definitive_not_transient() {
        let acct = Uuid::new_v4();
        let k = key(8);
        let cache = shared_cache();

        // A transient failure must NOT be cached (stays held, retried).
        let v = refresh_account("mona", acct, &k, &cache, DEFAULT_TTL, |_login| async {
            FetchOutcome::Unreachable("dns".into())
        })
        .await;
        assert_eq!(v, IdentityVerdict::Unknown);
        assert_eq!(cache.lock().unwrap().verdict(acct, &k), IdentityVerdict::Unknown);

        // A definitive Verified is cached.
        let listed = FetchOutcome::Keys(vec![k]);
        let v = refresh_account("mona", acct, &k, &cache, DEFAULT_TTL, move |_login| async move {
            listed
        })
        .await;
        assert_eq!(v, IdentityVerdict::Verified);
        assert_eq!(cache.lock().unwrap().verdict(acct, &k), IdentityVerdict::Verified);
    }
}
