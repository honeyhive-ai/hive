//! Remote access through a relay: the host side of `hive-tunnel/1`
//! (spec: `docs/relay-tunnel.md`).
//!
//! The phone reaches this box through a relay without a VPN or an inbound
//! port. `hive serve` keeps one outbound control WebSocket to the relay
//! ([`control`]); for each admitted phone it dials an accept WebSocket, runs
//! the Noise responder ([`noise`], [`session`]) and carries the existing
//! HTTP and SSE API over a small stream mux ([`frame`]), dispatching every request
//! into the axum router in process. The relay only forwards ciphertext.
//!
//! State lives in `<HIVE_DATA_DIR>/tunnel/` (directory 0700, files 0600):
//! `keys.json` (host signing + Noise static keys), `phones.json` (paired
//! phones), `relay.json` (`hive remote enable`), `status.json` (live status for
//! `hive remote status`) and `pending/` (enrollment windows and revocations
//! handed over by `hive pair` / `hive phones revoke`, picked up within 2 s).

// Protocol-complete helpers (credential verification, phone-side frames) are
// used by the tests' fake relay and phone, not by the host binary itself.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, mpsc};

pub mod cli;
pub mod control;
pub mod cred;
pub mod frame;
pub mod keys;
pub mod noise;
pub mod phones;
pub mod push;
pub mod session;

#[cfg(test)]
pub mod testkit;
#[cfg(test)]
mod tests;

pub use keys::HostKeys;
pub use phones::{PhoneRecord, Registry};

/// The protocol label used in every `ctx` string and as the HKDF salt.
pub const LABEL: &str = "hive-tunnel/1";
/// The WebSocket subprotocol every tunnel endpoint requires.
pub const SUBPROTOCOL: &str = "hive-tunnel.1";
/// Credential lifetime the host issues (§5).
pub const CRED_LIFETIME_SECS: u64 = 90 * 86_400;
/// Default lifetime of a pairing secret (§4).
pub const PAIR_WINDOW_SECS: u64 = 600;
/// How often `hive serve` looks at `pending/` (§16: "within 2 s").
pub const PENDING_POLL: Duration = Duration::from_secs(2);
/// Allowed clock skew wherever a time is compared (§2).
pub const SKEW_SECS: u64 = 300;

// ── Encoding + derivations (§2) ──────────────────────────────────────────────

/// base64url without padding.
pub fn b64u(x: impl AsRef<[u8]>) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(x)
}

/// Strict base64url decode: no padding, no non-alphabet characters.
pub fn b64u_decode(s: &str) -> Result<Vec<u8>> {
    if s.contains('=') {
        bail!("base64url must not be padded");
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).map_err(|e| anyhow!("bad base64url: {e}"))
}

/// Decode a base64url value that must be exactly `N` bytes.
pub fn b64u_fixed<const N: usize>(s: &str) -> Result<[u8; N]> {
    let v = b64u_decode(s)?;
    v.try_into().map_err(|v: Vec<u8>| anyhow!("expected {N} bytes, got {}", v.len()))
}

/// `ctx(label, p1, p2, …)` = `"hive-tunnel/1 " + label ‖ 0x00 ‖ p1 ‖ p2 ‖ …`.
pub fn ctx(label: &str, parts: &[&[u8]]) -> Vec<u8> {
    let mut out = format!("{LABEL} {label}").into_bytes();
    out.push(0);
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

/// HKDF-SHA256 with salt `"hive-tunnel/1"`.
pub fn hkdf(ikm: &[u8], info: &str, n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n];
    hkdf::Hkdf::<Sha256>::new(Some(LABEL.as_bytes()), ikm)
        .expand(info.as_bytes(), &mut out)
        .expect("HKDF output length is small");
    out
}

pub fn sha256(x: &[u8]) -> [u8; 32] {
    Sha256::digest(x).into()
}

/// 16 random bytes as base64url (22 chars): `pid`s, request ids.
pub fn random_id() -> String {
    b64u(random_bytes::<16>())
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("OS randomness");
    b
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ── Files ────────────────────────────────────────────────────────────────────

pub fn tunnel_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("tunnel")
}

pub fn pending_dir(dir: &Path) -> PathBuf {
    dir.join("pending")
}

/// Create `dir` (and parents) with mode 0700.
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Atomically write `bytes` to `path` with mode 0600 (temp file + rename, so a
/// reader in another process never sees a torn file).
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or_else(|| anyhow!("{} has no parent", path.display()))?;
    ensure_private_dir(parent)?;
    let tmp = parent.join(format!(".{}.{}.tmp", path.file_name().and_then(|n| n.to_str()).unwrap_or("f"), random_id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn write_json(path: &Path, v: &impl Serialize) -> Result<()> {
    write_private(path, &serde_json::to_vec_pretty(v)?)
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b).with_context(|| format!("parsing {}", path.display()))?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

// ── Settings ─────────────────────────────────────────────────────────────────

/// Where and how to reach the relay. Off unless `hive remote enable` wrote
/// `relay.json` or `--remote-relay` / `HIVE_REMOTE_RELAY` is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelaySettings {
    /// Relay base URL, e.g. `https://relay.example.com` (no trailing `/`).
    pub relay: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reg_token: Option<String>,
    /// Allow a plain `http` relay (tests, a relay on loopback).
    #[serde(default)]
    pub insecure_relay: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

impl RelaySettings {
    pub fn new(relay: &str, reg_token: Option<String>, insecure_relay: bool) -> Result<Self> {
        let s = Self { relay: relay.trim().trim_end_matches('/').to_string(), reg_token, insecure_relay, enabled: true };
        s.validate()?;
        Ok(s)
    }

    /// `https` only, unless `insecure_relay` (§4: the host refuses to print an
    /// `http` relay URL without `--insecure-relay`).
    pub fn validate(&self) -> Result<()> {
        let lower = self.relay.to_ascii_lowercase();
        if lower.starts_with("https://") {
            return Ok(());
        }
        if lower.starts_with("http://") {
            if self.insecure_relay {
                return Ok(());
            }
            bail!("relay URL {} is not https (pass --insecure-relay / HIVE_REMOTE_INSECURE_RELAY=1 to allow it)", self.relay);
        }
        bail!("relay URL must start with https:// (got '{}')", self.relay)
    }

    /// The WebSocket URL for `path` (`/v1/tunnel/...`) on this relay.
    pub fn ws_url(&self, path_and_query: &str) -> String {
        let base = if let Some(rest) = self.relay.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = self.relay.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            self.relay.clone()
        };
        format!("{base}{path_and_query}")
    }

    pub fn file(dir: &Path) -> PathBuf {
        dir.join("relay.json")
    }

    /// `relay.json`, when present and enabled.
    pub fn load(dir: &Path) -> Result<Option<Self>> {
        Ok(read_json::<Self>(&Self::file(dir))?.filter(|s| s.enabled))
    }

    /// Flags/env first (`--remote-relay` / `HIVE_REMOTE_RELAY`,
    /// `--reg-token` / `HIVE_REMOTE_REG_TOKEN`, `--insecure-relay` /
    /// `HIVE_REMOTE_INSECURE_RELAY`), then `relay.json`. `HIVE_REMOTE_RELAY=off`
    /// turns remote access off even when `relay.json` enables it.
    pub fn resolve(data_dir: &Path, relay: Option<String>, reg_token: Option<String>, insecure: bool) -> Result<Option<Self>> {
        let insecure = insecure || crate::reply::env_flag("HIVE_REMOTE_INSECURE_RELAY", false);
        let reg_token = reg_token.or_else(|| crate::env_opt("HIVE_REMOTE_REG_TOKEN"));
        match relay.or_else(|| crate::env_opt("HIVE_REMOTE_RELAY")) {
            Some(r) if r.eq_ignore_ascii_case("off") => Ok(None),
            Some(r) => Ok(Some(Self::new(&r, reg_token, insecure)?)),
            None => match Self::load(&tunnel_dir(data_dir))? {
                Some(mut s) => {
                    s.insecure_relay |= insecure;
                    if reg_token.is_some() {
                        s.reg_token = reg_token;
                    }
                    s.validate()?;
                    Ok(Some(s))
                }
                None => Ok(None),
            },
        }
    }
}

/// Live status `hive serve` writes for `hive remote status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveStatus {
    pub connected: bool,
    /// `active` / `pending` from the relay's `welcome` / `status`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub push: bool,
    #[serde(default)]
    pub last_error: Option<String>,
    pub at: u64,
}

// ── Enrollment windows + pending hand-off ────────────────────────────────────

/// `pending/enroll-<eid>.json`, written by `hive pair`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowFile {
    /// b64u(pairing secret).
    pub ps: String,
    pub exp: u64,
}

/// `pending/revoke-<pid>.json`, written by `hive phones revoke`; deleted once
/// the relay acks the revocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeFile {
    pub pid: String,
    pub exp: u64,
}

/// The values derived from a pairing secret (§4).
pub struct PairingSecret {
    pub ps: [u8; 32],
    pub eid: String,
    pub tag: [u8; 32],
    pub psk: [u8; 32],
}

impl PairingSecret {
    pub fn from_bytes(ps: [u8; 32]) -> Self {
        let eid = b64u(hkdf(&ps, "enroll-id", 16));
        let tag: [u8; 32] = hkdf(&ps, "relay-tag", 32).try_into().expect("32");
        let psk: [u8; 32] = hkdf(&ps, "noise-psk", 32).try_into().expect("32");
        Self { ps, eid, tag, psk }
    }

    pub fn generate() -> Self {
        Self::from_bytes(random_bytes())
    }

    pub fn tag_hash(&self) -> String {
        b64u(sha256(&self.tag))
    }
}

/// An open enrollment window in `hive serve`.
pub struct Window {
    pub secret: PairingSecret,
    pub exp: u64,
    /// Failed or rejected attempts (at most 5, §6.4.2).
    pub attempts: u32,
    /// E2: the first phone static that completed msg1.
    pub bound_static: Option<[u8; 32]>,
    /// Whether the relay acked the `enroll` for this window on this connection.
    pub announced: bool,
}

pub const MAX_WINDOW_ATTEMPTS: u32 = 5;

/// Write a new enrollment window for `hive serve` to pick up and return it.
pub fn create_window(dir: &Path, lifetime: u64) -> Result<(PairingSecret, u64)> {
    let secret = PairingSecret::generate();
    let exp = now() + lifetime.min(900);
    write_json(&pending_dir(dir).join(format!("enroll-{}.json", secret.eid)), &WindowFile { ps: b64u(secret.ps), exp })?;
    Ok((secret, exp))
}

/// The v2 pairing link (§4). `direct` = `(server URL, bearer token)` when a
/// direct path exists; `None` for a relay-only link.
pub fn pairing_link_v2(direct: Option<(&str, &str)>, keys: &HostKeys, relay: &str, ps: &[u8; 32], exp: u64) -> String {
    let mut link = match direct {
        Some((url, token)) => format!("{}&", super::pair::pairing_url(url, token)),
        None => "hive://pair?".to_string(),
    };
    link.push_str(&format!(
        "v=2&hid={}&hs={}&relay={}&ps={}&exp={exp}",
        keys.host_id(),
        b64u(keys.static_pub()),
        b64u(relay.trim_end_matches('/')),
        b64u(ps),
    ));
    link
}

// ── The running tunnel host ──────────────────────────────────────────────────

/// Marks a request that arrived through the tunnel (§9). The auth middleware
/// treats it as authorized after re-checking that `pid` is still active.
#[derive(Debug, Clone)]
pub struct TunnelPeer {
    pub pid: String,
}

/// A host request waiting for the relay's `ack`.
enum Pending {
    Enroll(String),
    Revoke(String),
    /// A push to this pid.
    Push(String),
    Other,
}

/// Everything the control connection, the sessions and the API share.
pub struct TunnelHost {
    pub dir: PathBuf,
    pub keys: HostKeys,
    pub settings: RelaySettings,
    pub registry: Registry,
    windows: Mutex<HashMap<String, Window>>,
    /// pid → (exp, last sent): revocations the relay hasn't acked yet.
    revocations: Mutex<HashMap<String, (u64, Option<Instant>)>>,
    /// The live control connection's outgoing queue.
    control: Mutex<Option<mpsc::UnboundedSender<String>>>,
    acks: Mutex<HashMap<String, Pending>>,
    /// Revoked pids, for live sessions to end themselves (§13.2 step 2).
    pub revoked: broadcast::Sender<String>,
    pub sessions: AtomicUsize,
    pub max_sessions: AtomicUsize,
    pub push_available: AtomicBool,
    /// Host-side push limits, foreground suppression and relay refusals.
    push_gate: Mutex<push::PushGate>,
    status: Mutex<LiveStatus>,
}

impl TunnelHost {
    /// Load (or create) the host keys and the phone registry under `data_dir`.
    pub fn open(data_dir: &Path, settings: RelaySettings) -> Result<Arc<Self>> {
        settings.validate()?;
        let dir = tunnel_dir(data_dir);
        ensure_private_dir(&dir)?;
        ensure_private_dir(&pending_dir(&dir))?;
        let keys = HostKeys::load_or_create(&dir)?;
        let registry = Registry::load(&dir)?;
        let (revoked, _) = broadcast::channel(64);
        Ok(Arc::new(Self {
            dir,
            keys,
            settings,
            registry,
            windows: Mutex::new(HashMap::new()),
            revocations: Mutex::new(HashMap::new()),
            control: Mutex::new(None),
            acks: Mutex::new(HashMap::new()),
            revoked,
            sessions: AtomicUsize::new(0),
            max_sessions: AtomicUsize::new(8),
            push_available: AtomicBool::new(false),
            push_gate: Mutex::new(push::PushGate::default()),
            status: Mutex::new(LiveStatus::default()),
        }))
    }

    pub fn host_id(&self) -> &str {
        self.keys.host_id()
    }

    fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queue a host request on the control connection; returns `false` when
    /// it isn't connected (the caller's state machine retries on reconnect).
    fn send_request(&self, mut msg: Value, pending: Pending) -> bool {
        let id = random_id();
        msg["id"] = json!(id);
        let sent = Self::lock(&self.control).as_ref().is_some_and(|tx| tx.send(msg.to_string()).is_ok());
        if sent {
            Self::lock(&self.acks).insert(id, pending);
        }
        sent
    }

    fn send_plain(&self, msg: Value) -> bool {
        Self::lock(&self.control).as_ref().is_some_and(|tx| tx.send(msg.to_string()).is_ok())
    }

    pub(crate) fn set_control(&self, tx: Option<mpsc::UnboundedSender<String>>) {
        let connected = tx.is_some();
        *Self::lock(&self.control) = tx;
        Self::lock(&self.acks).clear();
        if !connected {
            for w in Self::lock(&self.windows).values_mut() {
                w.announced = false;
            }
            for r in Self::lock(&self.revocations).values_mut() {
                r.1 = None;
            }
        }
        self.update_status(|s| s.connected = connected);
    }

    pub(crate) fn update_status(&self, f: impl FnOnce(&mut LiveStatus)) {
        let snapshot = {
            let mut s = Self::lock(&self.status);
            f(&mut s);
            s.at = now();
            s.clone()
        };
        let _ = write_json(&self.dir.join("status.json"), &snapshot);
    }

    pub fn live_status(&self) -> LiveStatus {
        Self::lock(&self.status).clone()
    }

    /// The relay answered a host request.
    pub(crate) fn on_ack(&self, id: &str, ok: bool, code: Option<&str>) {
        let Some(p) = Self::lock(&self.acks).remove(id) else { return };
        match p {
            Pending::Revoke(pid) if ok => {
                Self::lock(&self.revocations).remove(&pid);
                let _ = std::fs::remove_file(pending_dir(&self.dir).join(format!("revoke-{pid}.json")));
            }
            Pending::Revoke(pid) => {
                eprintln!("remote: relay refused revoking {pid} ({}); retrying", code.unwrap_or("?"));
                if let Some(r) = Self::lock(&self.revocations).get_mut(&pid) {
                    r.1 = None;
                }
            }
            Pending::Enroll(eid) if !ok => {
                eprintln!("remote: relay refused enrollment window {eid} ({})", code.unwrap_or("?"));
            }
            Pending::Push(pid) if !ok => {
                let code = code.unwrap_or("?");
                if code == "push_unavailable" {
                    // Off until the next `welcome` says otherwise; no retry.
                    if self.push_available.swap(false, Ordering::SeqCst) {
                        eprintln!("remote: the relay can't deliver pushes (push_unavailable); pushes are off until it reconnects");
                        self.update_status(|s| s.push = false);
                    }
                } else {
                    tracing::debug!(pid = %pid, code, "relay refused a push");
                    Self::lock(&self.push_gate).refused(&pid, code, Instant::now());
                }
            }
            _ => {}
        }
    }

    // ── Enrollment windows ──

    /// Open an in-memory window (normally loaded from `pending/`).
    pub fn add_window(&self, secret: PairingSecret, exp: u64) {
        let eid = secret.eid.clone();
        Self::lock(&self.windows)
            .entry(eid)
            .or_insert(Window { secret, exp, attempts: 0, bound_static: None, announced: false });
        self.announce_windows();
    }

    /// Send `enroll` for windows the relay hasn't heard of on this connection.
    pub(crate) fn announce_windows(&self) {
        let mut todo = Vec::new();
        for (eid, w) in Self::lock(&self.windows).iter_mut() {
            if !w.announced && w.exp > now() {
                w.announced = true;
                todo.push(json!({ "t": "enroll", "eid": eid, "tagHash": w.secret.tag_hash(), "exp": w.exp }));
            }
        }
        for msg in todo {
            let eid = msg["eid"].as_str().unwrap_or_default().to_string();
            if !self.send_request(msg, Pending::Enroll(eid.clone())) {
                if let Some(w) = Self::lock(&self.windows).get_mut(&eid) {
                    w.announced = false;
                }
            }
        }
    }

    /// Close a window: used (E4), cancelled, exhausted or expired.
    pub fn close_window(&self, eid: &str) {
        let removed = Self::lock(&self.windows).remove(eid).is_some();
        let _ = std::fs::remove_file(pending_dir(&self.dir).join(format!("enroll-{eid}.json")));
        if removed {
            self.send_request(json!({ "t": "enrollClose", "eid": eid }), Pending::Other);
        }
    }

    /// The psk for an open, unexpired window.
    pub(crate) fn window_psk(&self, eid: &str) -> Option<[u8; 32]> {
        let w = Self::lock(&self.windows);
        w.get(eid).filter(|w| w.exp > now() && w.attempts < MAX_WINDOW_ATTEMPTS).map(|w| w.secret.psk)
    }

    /// Count a failed attempt; closes the window after the fifth.
    pub(crate) fn window_failed(&self, eid: &str) {
        let exhausted = {
            let mut ws = Self::lock(&self.windows);
            match ws.get_mut(eid) {
                Some(w) => {
                    w.attempts += 1;
                    w.attempts >= MAX_WINDOW_ATTEMPTS
                }
                None => false,
            }
        };
        if exhausted {
            eprintln!("remote: pairing window {eid} closed after {MAX_WINDOW_ATTEMPTS} failed attempts");
            self.close_window(eid);
        }
    }

    /// E2: bind the window to the first static that completes msg1; a
    /// different static later is refused.
    pub(crate) fn window_bind(&self, eid: &str, static_pub: [u8; 32]) -> bool {
        let mut ws = Self::lock(&self.windows);
        let Some(w) = ws.get_mut(eid) else { return false };
        match w.bound_static {
            Some(s) => s == static_pub,
            None => {
                w.bound_static = Some(static_pub);
                true
            }
        }
    }

    // ── Revocation ──

    /// §13.2 steps 1–3 for `pid`: mark it revoked, end its live sessions, and
    /// queue `revoke{pid, exp}` until the relay acks it (persisted in
    /// `pending/`, so it survives a restart).
    pub fn revoke(&self, pid: &str) -> Result<bool> {
        let rec_exp = self.registry.revoke(pid)?;
        let file = pending_dir(&self.dir).join(format!("revoke-{pid}.json"));
        let file_exp = read_json::<RevokeFile>(&file).ok().flatten().map(|f| f.exp);
        let exp = match (rec_exp, file_exp) {
            (None, None) => return Ok(false),
            (a, b) => a.unwrap_or(0).max(b.unwrap_or(0)),
        };
        write_json(&file, &RevokeFile { pid: pid.to_string(), exp })?;
        let _ = self.revoked.send(pid.to_string());
        Self::lock(&self.revocations).entry(pid.to_string()).or_insert((exp, None)).0 = exp;
        self.send_revocations();
        Ok(true)
    }

    /// (Re)send unacked revocations: on connect, and every 30 s until acked.
    pub(crate) fn send_revocations(&self) {
        let due: Vec<(String, u64)> = Self::lock(&self.revocations)
            .iter()
            .filter(|(_, (_, sent))| sent.is_none_or(|t| t.elapsed() > Duration::from_secs(30)))
            .map(|(p, (e, _))| (p.clone(), *e))
            .collect();
        for (pid, exp) in due {
            if self.send_request(json!({ "t": "revoke", "pid": pid, "exp": exp }), Pending::Revoke(pid.clone())) {
                if let Some(r) = Self::lock(&self.revocations).get_mut(&pid) {
                    r.1 = Some(Instant::now());
                }
            }
        }
    }

    pub fn pending_revocations(&self) -> Vec<String> {
        Self::lock(&self.revocations).keys().cloned().collect()
    }

    /// Pick up what `hive pair` / `hive phones revoke` left in `pending/`, and
    /// expire stale windows. Called every [`PENDING_POLL`].
    pub fn poll_pending(&self) {
        let t = now();
        let mut expired = Vec::new();
        for (eid, w) in Self::lock(&self.windows).iter() {
            if w.exp <= t {
                expired.push(eid.clone());
            }
        }
        for eid in expired {
            self.close_window(&eid);
        }
        let Ok(entries) = std::fs::read_dir(pending_dir(&self.dir)) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(eid) = name.strip_prefix("enroll-").and_then(|n| n.strip_suffix(".json")) {
                if Self::lock(&self.windows).contains_key(eid) {
                    continue;
                }
                match read_json::<WindowFile>(&path) {
                    Ok(Some(f)) if f.exp > t => match b64u_fixed::<32>(&f.ps) {
                        Ok(ps) => {
                            let secret = PairingSecret::from_bytes(ps);
                            if secret.eid == eid {
                                self.add_window(secret, f.exp.min(t + 900));
                            } else {
                                let _ = std::fs::remove_file(&path);
                            }
                        }
                        Err(_) => {
                            let _ = std::fs::remove_file(&path);
                        }
                    },
                    Ok(_) => {
                        let _ = std::fs::remove_file(&path);
                    }
                    Err(e) => eprintln!("remote: {e:#}"),
                }
            } else if let Some(pid) = name.strip_prefix("revoke-").and_then(|n| n.strip_suffix(".json")) {
                let known = Self::lock(&self.revocations).contains_key(pid);
                if !known {
                    if let Err(e) = self.revoke(pid) {
                        eprintln!("remote: revoking {pid}: {e:#}");
                    }
                }
            }
        }
        self.announce_windows();
        self.send_revocations();
    }

    // ── Push (§14) ──

    /// Encrypt and send a push to every active phone that wants pushes. Each
    /// phone gets the preview or not according to its `previews` preference.
    /// A phone with the app open (a live `/v1/events` stream through the
    /// tunnel) is skipped, and the host-side limits apply ([`push`]).
    /// Returns how many phones it was sent to.
    pub fn push_all(&self, content: &PushContent) -> usize {
        self.push_all_at(content, Instant::now())
    }

    pub fn push_all_at(&self, content: &PushContent, at: Instant) -> usize {
        if !self.push_available.load(Ordering::SeqCst) {
            return 0;
        }
        let mut sent = 0;
        for rec in self.registry.list().into_iter().filter(|r| r.is_active() && r.push_enabled) {
            let verdict = Self::lock(&self.push_gate).check(&rec.pid, &content.kind, content.chat_id.as_deref(), at);
            if verdict != push::Verdict::Send {
                tracing::debug!(pid = %rec.pid, kind = %content.kind, ?verdict, "push not sent");
                continue;
            }
            let Some((seq, key)) = self.registry.next_push_seq(&rec.pid) else { continue };
            let ct = seal_push(&key, self.host_id(), &rec.pid, content, rec.push_previews, seq, now());
            let mut msg = json!({ "t": "push", "pid": rec.pid, "ct": b64u(ct), "prio": "high" });
            if let Some(c) = &content.chat_id {
                msg["collapse"] = json!(push_collapse(&key, c));
            }
            if self.send_request(msg, Pending::Push(rec.pid.clone())) {
                sent += 1;
            }
        }
        sent
    }

    /// Count an open `/v1/events` stream from `pid` (the app is in the
    /// foreground there) until the guard drops.
    pub fn foreground(self: &Arc<Self>, pid: &str) -> ForegroundGuard {
        Self::lock(&self.push_gate).foreground_open(pid);
        ForegroundGuard { host: self.clone(), pid: pid.to_string() }
    }

    /// Open `/v1/events` streams from `pid` right now.
    pub fn foreground_streams(&self, pid: &str) -> usize {
        Self::lock(&self.push_gate).foreground_streams(pid)
    }
}

/// Held by an events stream that came through the tunnel; see
/// [`TunnelHost::foreground`]. Dropping it (stream finished, cancelled with
/// RESET, or its session ended) uncounts the stream.
pub struct ForegroundGuard {
    host: Arc<TunnelHost>,
    pid: String,
}

impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        TunnelHost::lock(&self.host.push_gate).foreground_close(&self.pid);
    }
}

/// The spec's cap on the raw encrypted push (nonce ‖ ciphertext ‖ tag).
pub const PUSH_CT_MAX: usize = 2048;
/// Preview length in characters (Unicode scalar values), before the `…`.
pub const PREVIEW_CHARS: usize = 120;

/// What a push is about (§14). `title` and `sender` never carry message
/// text; `preview` is the content shown only to phones with previews on.
/// Kinds: `message.done`, `mention`, `proposal.created`, `workflow.approval`,
/// `phone.paired`, `test` (`docs/push-notifications.md`).
#[derive(Debug, Clone, Default)]
pub struct PushContent {
    pub kind: String,
    pub chat_id: Option<String>,
    /// "coder replied", "Change ready for review", …
    pub title: String,
    /// The agent or author name alone.
    pub sender: Option<String>,
    /// The proposal to review (`proposal.created`, `workflow.approval`).
    pub proposal_id: Option<String>,
    /// The workflow run waiting on an approval (`workflow.approval`).
    pub workflow_run_id: Option<String>,
    /// That run's workflow definition (`workflow.approval`).
    pub workflow_id: Option<String>,
    /// The raw content to preview (message text, proposal title). Shortened
    /// with [`preview_text`] when sealed.
    pub preview: Option<String>,
    /// A body that isn't content (for example a phone's name on
    /// `phone.paired`): shown regardless of the preference, when there is
    /// no preview.
    pub fixed_body: String,
}

/// The first [`PREVIEW_CHARS`] characters of `s`: whitespace runs collapsed
/// to one space, control characters dropped, cut on a character boundary,
/// `…` appended when cut.
pub fn preview_text(s: &str) -> String {
    let mut out = String::new();
    let mut n = 0;
    let mut pending_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if c.is_control() {
            continue;
        }
        let extra = usize::from(pending_space) + 1;
        if n + extra > PREVIEW_CHARS {
            out.push('…');
            return out;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
        n += extra;
    }
    out
}

/// The push plaintext for one phone (§14).
pub fn push_plaintext(content: &PushContent, previews: bool, seq: u64, ts: u64) -> Value {
    let preview = if previews { content.preview.as_deref().map(preview_text).filter(|p| !p.is_empty()) } else { None };
    let body = match &preview {
        Some(p) => p.clone(),
        None if content.preview.is_some() => String::new(), // sender only
        None => clip(&content.fixed_body, 512),
    };
    let mut pt = json!({ "v": 1, "seq": seq, "ts": ts, "kind": content.kind, "title": clip(&content.title, 64), "body": clip(&body, 512) });
    if let Some(c) = &content.chat_id {
        pt["chatId"] = json!(c);
    }
    for (key, value) in [("proposalId", &content.proposal_id), ("workflowRunId", &content.workflow_run_id), ("workflowId", &content.workflow_id)] {
        if let Some(v) = value {
            pt[key] = json!(clip(v, 64));
        }
    }
    if let Some(s) = &content.sender {
        pt["sender"] = json!(clip(s, 64));
    }
    if preview.is_some() {
        pt["preview"] = json!(true);
    }
    pt
}

/// Encrypt the push for one phone; drops the preview if the result would
/// exceed [`PUSH_CT_MAX`].
pub fn seal_push(key: &[u8; 32], host_id: &str, pid: &str, content: &PushContent, previews: bool, seq: u64, ts: u64) -> Vec<u8> {
    let pt = push_plaintext(content, previews, seq, ts);
    let ct = push_encrypt(key, host_id, pid, &random_bytes::<12>(), pt.to_string().as_bytes());
    if ct.len() <= PUSH_CT_MAX || !previews {
        return ct;
    }
    let pt = push_plaintext(content, false, seq, ts);
    push_encrypt(key, host_id, pid, &random_bytes::<12>(), pt.to_string().as_bytes())
}

fn clip(s: &str, max_bytes: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if out.len() + c.len_utf8() > max_bytes {
            break;
        }
        out.push(c);
    }
    out
}

/// `nonce ‖ ChaCha20-Poly1305(push_key, nonce, plaintext, ctx("push", hid, pid))`.
pub fn push_encrypt(key: &[u8; 32], host_id: &str, pid: &str, nonce: &[u8; 12], plaintext: &[u8]) -> Vec<u8> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    let ad = ctx("push", &[host_id.as_bytes(), pid.as_bytes()]);
    let cipher = chacha20poly1305::ChaCha20Poly1305::new(key.into());
    let ct = cipher.encrypt(nonce.into(), Payload { msg: plaintext, aad: &ad }).expect("encrypt");
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    out
}

/// `b64u(HMAC-SHA256(push_key, chatId)[0..12])`.
pub fn push_collapse(key: &[u8; 32], chat_id: &str) -> String {
    use hmac::Mac;
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(key).expect("any key length");
    mac.update(chat_id.as_bytes());
    b64u(&mac.finalize().into_bytes()[..12])
}

/// Resolves once `stop` is set. (Awaiting `wait_for` directly in a
/// `select!` keeps its non-`Send` guard in the output.)
pub async fn stopped(stop: &mut tokio::sync::watch::Receiver<bool>) {
    let _ = stop.wait_for(|s| *s).await;
}

/// Background loop: poll `pending/` while `hive serve` runs.
pub async fn pending_loop(host: Arc<TunnelHost>, mut stop: tokio::sync::watch::Receiver<bool>) {
    loop {
        host.poll_pending();
        tokio::select! {
            _ = tokio::time::sleep(PENDING_POLL) => {}
            _ = stopped(&mut stop) => return,
        }
    }
}
