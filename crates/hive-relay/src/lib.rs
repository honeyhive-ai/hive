//! `hive-relay` — the rendezvous + envelope-forwarding server, ported from the
//! Swift `RelayServer` (SwiftNIO) onto axum + tokio.
//!
//! NOTE: The **production relay is now the Go implementation** in `/relay`
//! (same `/v1` contract + `hrt1` token format). This Rust crate is retained
//! only as the behavioral oracle and as an in-process loopback fixture for the
//! Rust client's sync tests (`hive-runtime` dev-dependency) — it is not built
//! into the app and is not deployed. Don't add features here; change `/relay`.
//!
//! The relay is a thin, **content-blind** forwarding mailbox: clients post
//! opaque (end-to-end encrypted) event envelopes keyed by workspace, and peers
//! fetch everything after a cursor. It also offers a rendezvous board (publish/
//! lookup STUN candidates for future direct P2P) and ephemeral presence. It
//! never sees plaintext — see the E2EE workspace key in `hive-core::e2ee`.
//!
//! State is in-memory (a reference relay good for tests and small deployments);
//! durable storage is a later concern.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod token;
mod social;
mod store;

/// Short pairing codes: confusion-free alphabet (no I/L/O/U, no 1/0 ambiguity
/// beyond that), 6 chars → ~1e9 combos, ample for short-lived codes.
const CODE_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const CODE_LEN: usize = 6;
const DEFAULT_PAIR_TTL_SECS: u64 = 600;
const MAX_PAIR_TTL_SECS: u64 = 3600;
const MAX_PAIR_PAYLOAD: usize = 8192;

/// One stored envelope: the relay assigns a monotonic server sequence and keeps
/// the opaque body verbatim.
#[derive(Clone, Serialize, Deserialize)]
struct Stored {
    seq: u64,
    body: Value,
}

#[derive(Default, Clone, Serialize, Deserialize)]
struct Workspace {
    envelopes: Vec<Stored>,
    next_seq: u64,
    /// device id → published STUN candidates (rendezvous board).
    candidates: HashMap<String, Value>,
    /// device id → latest presence blob.
    presence: HashMap<String, Value>,
    /// Append-only log of opaque workspace-key rotation blobs (ciphertext only).
    /// Members fetch all and adopt the highest version sealed to their device.
    keyring: Vec<Value>,
}

/// A short-lived pairing entry: a short code maps to an opaque payload (a peer
/// friend code or workspace invite) until it expires. The relay only brokers
/// the handoff; it never sees plaintext traffic.
struct Pairing {
    payload: String,
    expires_at: Instant,
}

/// One account's directory entry: its GitHub identity + the X25519 public keys
/// of each of its devices. Lets a teammate be invited by `@handle` and the
/// workspace key sealed to *all* their devices.
#[derive(Clone, Default, Serialize, Deserialize)]
struct DirAccount {
    github_id: u64,
    login: String,
    name: Option<String>,
    /// device id → X25519 key-agreement public key (hex/base64, opaque here).
    devices: HashMap<String, String>,
}

/// Who may use this relay. **Open** = anyone with the URL (the self-hosted
/// default — the URL is not a secret). **Tokens** = gated: every `/v1/*` request
/// (except health) must present `Authorization: Bearer <token>` from the
/// allowed set — the model for a hosted/paid relay, where your billing backend
/// issues the token to entitled users. Hiding the URL is never the gate.
#[derive(Clone, Default)]
pub enum EntitlementPolicy {
    /// Self-host default: anyone may connect.
    #[default]
    Open,
    /// A static allowlist of opaque bearer tokens (coarse on/off gate).
    Tokens(std::collections::HashSet<String>),
    /// Signed entitlement tokens verified against the issuer's Ed25519 public
    /// key. Carries per-plan limits + RBAC capabilities in the token claims.
    SignedKey(Box<ed25519_dalek::VerifyingKey>),
}

/// Outcome of an entitlement check. `Allowed` may carry verified [`TokenClaims`]
/// (only for `SignedKey`) so downstream handlers can enforce per-plan limits.
pub enum Entitlement {
    Denied,
    Allowed(Option<token::TokenClaims>),
}

impl EntitlementPolicy {
    /// Resolve from the environment, most specific first:
    /// 1. `HIVE_RELAY_TOKEN_PUBKEY` (hex/base64 Ed25519) → verify signed tokens;
    /// 2. `HIVE_RELAY_ACCESS_TOKENS` (comma-separated) → static allowlist;
    /// 3. otherwise → open (self-host default).
    pub fn from_env() -> Self {
        if let Ok(pk) = std::env::var("HIVE_RELAY_TOKEN_PUBKEY") {
            if let Some(vk) = token::parse_pubkey(&pk) {
                return EntitlementPolicy::SignedKey(Box::new(vk));
            }
            eprintln!("HIVE_RELAY_TOKEN_PUBKEY set but unparseable; ignoring");
        }
        match std::env::var("HIVE_RELAY_ACCESS_TOKENS") {
            Ok(v) if !v.trim().is_empty() => EntitlementPolicy::Tokens(
                v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            ),
            _ => EntitlementPolicy::Open,
        }
    }

    /// Evaluate a presented bearer token against the policy at time `now_unix`.
    fn evaluate(&self, token: Option<&str>, now_unix: u64) -> Entitlement {
        match self {
            EntitlementPolicy::Open => Entitlement::Allowed(None),
            EntitlementPolicy::Tokens(set) => match token {
                Some(t) if set.contains(t) => Entitlement::Allowed(None),
                _ => Entitlement::Denied,
            },
            EntitlementPolicy::SignedKey(vk) => match token.and_then(|t| token::verify(t, vk)) {
                Some(claims) if !claims.is_expired(now_unix) => Entitlement::Allowed(Some(claims)),
                _ => Entitlement::Denied,
            },
        }
    }

    /// Convenience boolean (claims discarded). Used by tests; the middleware
    /// uses [`EntitlementPolicy::evaluate`] so it can read verified claims.
    #[cfg(test)]
    fn allows(&self, token: Option<&str>) -> bool {
        matches!(self.evaluate(token, now_unix()), Entitlement::Allowed(_))
    }
}

/// Current unix time in seconds (for token expiry checks).
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A pre-write authorization hook. The open / reference relay leaves this unset
/// (pure content-blind forwarding). A **paid build** — the private
/// `hive-relay-enterprise` crate — sets a guard that enforces workspace
/// membership / roles before any write. This is a minimal extension seam: no
/// membership logic lives in this (open) crate.
#[async_trait::async_trait]
pub trait WriteGuard: Send + Sync {
    /// Called before any workspace write (`envelopes` / `keyring` / `presence` /
    /// `candidates`). Return `Err(response)` to reject the write.
    async fn check(&self, workspace: &str, headers: &HeaderMap) -> Result<(), Response>;
}

#[derive(Clone, Default)]
pub struct RelayState {
    workspaces: Arc<RwLock<HashMap<String, Workspace>>>,
    pairings: Arc<RwLock<HashMap<String, Pairing>>>,
    /// GitHub login (lowercased) → account directory entry.
    directory: Arc<RwLock<HashMap<String, DirAccount>>>,
    /// Social graph: account key (`github:<id>`) → device registry + inbox.
    accounts: Arc<RwLock<HashMap<String, social::AccountState>>>,
    /// GitHub login (lowercased) → account key, so a friend can be targeted by
    /// `@username` (resolved against accounts that have registered a device).
    login_index: Arc<RwLock<HashMap<String, String>>>,
    /// Friend requests keyed by request id (pending + closed history).
    friend_requests: Arc<RwLock<HashMap<String, social::FriendRequest>>>,
    /// Accepted friendships as canonical-ordered account-key pairs.
    friend_edges: Arc<RwLock<std::collections::HashSet<(String, String)>>>,
    /// Optional cap on accepted friends per account (the hosted free tier sets
    /// this to 5 — see `docs/hive-social-graph-plan.md` §9). `None` = unlimited
    /// (the self-host default).
    friend_cap: Option<usize>,
    /// Where durable state is snapshotted (a file on a persistent volume).
    /// `None` = in-memory only (tests, ephemeral relays). See [`store`].
    persist_path: Option<std::path::PathBuf>,
    /// Who may use this relay (open self-host vs token-gated hosted).
    entitlement: Arc<EntitlementPolicy>,
    /// Optional pre-write authorization hook. `None` on the open relay; a paid
    /// build sets one to enforce membership. See [`WriteGuard`].
    write_guard: Option<Arc<dyn WriteGuard>>,
}

impl RelayState {
    pub fn with_entitlement(policy: EntitlementPolicy) -> Self {
        Self { entitlement: Arc::new(policy), ..Self::default() }
    }

    /// Attach a pre-write authorization hook (used by the enterprise build to
    /// enforce workspace membership). The open relay never sets one.
    pub fn with_write_guard(mut self, guard: Arc<dyn WriteGuard>) -> Self {
        self.write_guard = Some(guard);
        self
    }

    /// Expose the configured write guard (for `enforce_write`).
    fn guard(&self) -> Option<&Arc<dyn WriteGuard>> {
        self.write_guard.as_ref()
    }

    /// Cap accepted friends per account (the hosted free tier sets 5). The
    /// self-host default leaves it unlimited.
    pub fn with_friend_cap(mut self, cap: Option<usize>) -> Self {
        self.friend_cap = cap;
        self
    }
}

/// Reject `/v1/*` (except health) unless the caller is entitled. No-op when the
/// policy is Open.
async fn require_entitlement(State(state): State<RelayState>, mut req: Request, next: Next) -> Response {
    let token = bearer_token(req.headers());
    match state.entitlement.evaluate(token.as_deref(), now_unix()) {
        Entitlement::Allowed(claims) => {
            // Stash verified claims so handlers can enforce per-plan limits
            // (member cap, retention, TURN) without re-verifying.
            if let Some(c) = claims {
                req.extensions_mut().insert(c);
            }
            next.run(req).await
        }
        Entitlement::Denied => {
            (StatusCode::UNAUTHORIZED, "relay requires a valid access token").into_response()
        }
    }
}

/// Extract a `Bearer <token>` from the Authorization header (used for the relay
/// entitlement/access token).
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The caller's GitHub token for directory identity — sent in a dedicated header
/// so it doesn't collide with the entitlement `Authorization: Bearer`. Falls
/// back to the bearer for older clients / open relays.
fn github_token_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-hive-github-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| bearer_token(headers))
}

#[derive(Deserialize)]
struct GithubUser {
    id: u64,
    login: String,
    #[serde(default)]
    name: Option<String>,
}

/// Authenticate a GitHub token by fetching the user it belongs to. This is how
/// the directory binds device keys to a *verified* GitHub identity (no spoofing).
async fn verify_github_token(token: &str) -> Option<GithubUser> {
    let client = reqwest::Client::builder().user_agent("hive-relay").build().ok()?;
    let resp = client
        .get("https://api.github.com/user")
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<GithubUser>().await.ok()
}

/// Generate a random confusion-free pairing code. Not a secret — it's short and
/// short-lived, and collisions are checked against the live store.
fn random_pair_code() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let mut h = DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut h);
    CTR.fetch_add(1, Ordering::Relaxed).hash(&mut h);
    let mut x = h.finish();
    (0..CODE_LEN)
        .map(|_| {
            let c = CODE_ALPHABET[(x & 31) as usize] as char;
            x >>= 5;
            c
        })
        .collect()
}

/// Normalize user-entered codes: strip separators/spaces, uppercase.
fn normalize_pair_code(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_uppercase()
}

fn prune_pairings(map: &mut HashMap<String, Pairing>) {
    let now = Instant::now();
    map.retain(|_, p| p.expires_at > now);
}

/// Build the relay router. `main.rs` serves it; tests mount it in-process.
/// Entitlement policy comes from the environment (`HIVE_RELAY_ACCESS_TOKENS`).
pub fn router() -> Router {
    router_with_state(RelayState::with_entitlement(EntitlementPolicy::from_env()))
}

pub fn router_with_state(state: RelayState) -> Router {
    // Everything except /v1/health is behind the entitlement gate (a no-op when
    // the policy is Open, i.e. a self-hosted relay).
    let gated = Router::new()
        .route(
            "/v1/workspaces/:id/envelopes",
            post(post_envelope).get(list_envelopes),
        )
        .route(
            "/v1/workspaces/:id/candidates",
            post(publish_candidates).get(list_candidates),
        )
        .route(
            "/v1/workspaces/:id/presence",
            post(publish_presence).get(list_presence),
        )
        .route("/v1/pair", post(create_pairing))
        .route("/v1/pair/:code", get(resolve_pairing))
        .route(
            "/v1/workspaces/:id/keyring",
            post(publish_keyring).get(list_keyring),
        )
        .route("/v1/directory/register", post(directory_register))
        .route("/v1/directory/:handle", get(directory_lookup))
        .route("/v1/account/register", post(account_register))
        .route("/v1/account/heartbeat", post(account_heartbeat))
        .route("/v1/account/inbox", get(account_inbox))
        .route("/v1/account/devices", get(account_devices))
        .route("/v1/friends", get(friends_list))
        .route("/v1/friends/presence", get(friends_presence))
        .route("/v1/account/visibility", post(account_visibility))
        .route("/v1/friends/:account/devices", get(friend_devices_list))
        .route("/v1/friends/:account", delete(friend_remove))
        .route(
            "/v1/friends/requests",
            post(friend_request_create).get(friend_requests_list),
        )
        .route("/v1/friends/requests/:id/accept", post(friend_request_accept))
        .route("/v1/friends/requests/:id/reject", post(friend_request_reject))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_entitlement));

    Router::new()
        .route("/v1/health", get(|| async { "ok" }))
        .merge(gated)
        .with_state(state)
}

#[derive(Deserialize)]
struct DirectoryRegister {
    device_id: String,
    /// This device's X25519 key-agreement public key (opaque to the relay).
    ka_public: String,
}

/// Register (or refresh) one of the caller's devices under their GitHub account.
/// Auth: `Authorization: Bearer <github token>` — verified against GitHub.
async fn directory_register(
    State(state): State<RelayState>,
    headers: HeaderMap,
    Json(req): Json<DirectoryRegister>,
) -> Result<Json<Value>, StatusCode> {
    let token = github_token_header(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = verify_github_token(&token).await.ok_or(StatusCode::UNAUTHORIZED)?;
    let mut dir = state.directory.write().unwrap();
    let entry = dir.entry(user.login.to_lowercase()).or_insert_with(DirAccount::default);
    entry.github_id = user.id;
    entry.login = user.login.clone();
    entry.name = user.name.clone();
    if !req.ka_public.trim().is_empty() && !req.device_id.trim().is_empty() {
        entry.devices.insert(req.device_id, req.ka_public);
    }
    Ok(Json(serde_json::json!({ "githubId": user.id, "login": user.login })))
}

/// Resolve a GitHub handle to its account + every device's key-agreement public
/// key (so an inviter can seal the workspace key to all of them). Auth-gated to
/// signed-in GitHub users.
async fn directory_lookup(
    State(state): State<RelayState>,
    headers: HeaderMap,
    Path(handle): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let token = github_token_header(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    verify_github_token(&token).await.ok_or(StatusCode::UNAUTHORIZED)?;
    let dir = state.directory.read().unwrap();
    let entry = dir.get(&handle.trim().trim_start_matches('@').to_lowercase()).ok_or(StatusCode::NOT_FOUND)?;
    let devices: Vec<Value> = entry
        .devices
        .iter()
        .map(|(id, pk)| serde_json::json!({ "deviceId": id, "kaPublic": pk }))
        .collect();
    Ok(Json(serde_json::json!({
        "githubId": entry.github_id,
        "login": entry.login,
        "name": entry.name,
        "devices": devices,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountRegister {
    device_id: String,
    /// This device's direct-P2P node id, for the peer-link bootstrap (optional).
    #[serde(default)]
    node_id: Option<String>,
    /// Friendly device label (e.g. "MacBook"), optional.
    #[serde(default)]
    label: Option<String>,
}

/// Register (or refresh) one of the caller's devices in the social registry and
/// index its GitHub login. Auth: GitHub token (verified). Returns the account
/// key every device of this account shares.
async fn account_register(
    State(state): State<RelayState>,
    headers: HeaderMap,
    Json(req): Json<AccountRegister>,
) -> Result<Json<Value>, StatusCode> {
    if req.device_id.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let token = github_token_header(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = verify_github_token(&token).await.ok_or(StatusCode::UNAUTHORIZED)?;
    let key = state.register_account_device(
        user.id,
        &user.login,
        req.device_id.trim(),
        req.node_id.filter(|s| !s.trim().is_empty()),
        req.label.filter(|s| !s.trim().is_empty()),
        now_unix(),
    );
    Ok(Json(serde_json::json!({
        "accountId": key,
        "githubId": user.id,
        "login": user.login,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountHeartbeat {
    device_id: String,
}

/// Refresh this device's `last_seen` (drives presence). Auth: GitHub token.
async fn account_heartbeat(
    State(state): State<RelayState>,
    headers: HeaderMap,
    Json(req): Json<AccountHeartbeat>,
) -> Result<Json<Value>, StatusCode> {
    let token = github_token_header(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = verify_github_token(&token).await.ok_or(StatusCode::UNAUTHORIZED)?;
    let known = state.heartbeat_device(user.id, req.device_id.trim(), now_unix());
    Ok(Json(serde_json::json!({ "ok": known })))
}

/// Poll this account's inbox (the account channel): events with `seq > after`,
/// fanned out to every device of the account. Auth: GitHub token.
async fn account_inbox(
    State(state): State<RelayState>,
    headers: HeaderMap,
    Query(q): Query<AfterQuery>,
) -> Result<Json<Vec<social::InboxRow>>, StatusCode> {
    let token = github_token_header(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = verify_github_token(&token).await.ok_or(StatusCode::UNAUTHORIZED)?;
    let key = RelayState::account_key(user.id);
    Ok(Json(state.account_inbox_after(&key, q.after)))
}

/// List this account's registered devices (for presence + P2P bootstrap).
/// Auth: GitHub token.
async fn account_devices(
    State(state): State<RelayState>,
    headers: HeaderMap,
) -> Result<Json<Vec<social::DeviceRow>>, StatusCode> {
    let token = github_token_header(&headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = verify_github_token(&token).await.ok_or(StatusCode::UNAUTHORIZED)?;
    let key = RelayState::account_key(user.id);
    Ok(Json(state.account_devices(&key)))
}

/// Map a friend-graph refusal to an HTTP status. `CapReached` → 402 so the
/// client can surface an upgrade prompt (the free-tier collaborator cap).
fn friend_err_status(e: social::FriendError) -> StatusCode {
    use social::FriendError::*;
    match e {
        SelfRequest => StatusCode::BAD_REQUEST,
        AlreadyFriends | NotPending => StatusCode::CONFLICT,
        CapReached => StatusCode::PAYMENT_REQUIRED,
        NotFound => StatusCode::NOT_FOUND,
        NotYours => StatusCode::FORBIDDEN,
        TooManyPending => StatusCode::TOO_MANY_REQUESTS,
    }
}

/// Resolve the authenticated caller's GitHub identity → account key.
async fn caller_account(headers: &HeaderMap) -> Result<(GithubUser, String), StatusCode> {
    let token = github_token_header(headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let user = verify_github_token(&token).await.ok_or(StatusCode::UNAUTHORIZED)?;
    let key = RelayState::account_key(user.id);
    Ok((user, key))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FriendRequestBody {
    to_login: String,
}

/// Send a friend request to `@toLogin`. The relay stamps the sender from the
/// verified GitHub token (no spoofing) and delivers the request to *every*
/// device of the target via its inbox.
async fn friend_request_create(
    State(state): State<RelayState>,
    headers: HeaderMap,
    Json(body): Json<FriendRequestBody>,
) -> Result<Json<Value>, StatusCode> {
    let (user, from_account) = caller_account(&headers).await?;
    let to_login = body.to_login.trim().trim_start_matches('@').to_string();
    // Username-only discovery: the target must have signed in (registered) for us
    // to resolve them — otherwise 404 "user hasn't joined Hive".
    let to_account = state.account_key_for_login(&to_login).ok_or(StatusCode::NOT_FOUND)?;
    let req = state
        .create_friend_request(&from_account, &user.login, &to_account, &to_login, now_unix())
        .map_err(friend_err_status)?;
    state.push_account_event(&to_account, req.request_event());
    Ok(Json(serde_json::json!({ "requestId": req.id, "state": "pending" })))
}

/// Accept a pending request (recipient only). Notifies both accounts so the
/// pending UI dismisses on every device of each.
async fn friend_request_accept(
    State(state): State<RelayState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let (_, account) = caller_account(&headers).await?;
    let req = state.accept_friend_request(&id, &account).map_err(friend_err_status)?;
    let ev = req.resolved_event();
    state.push_account_event(&req.from_account, ev.clone());
    state.push_account_event(&req.to_account, ev);
    Ok(Json(serde_json::json!({ "ok": true, "state": "accepted" })))
}

/// Reject (recipient) or cancel (sender) a pending request; dismiss it on both
/// accounts' devices.
async fn friend_request_reject(
    State(state): State<RelayState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let (_, account) = caller_account(&headers).await?;
    let req = state.close_friend_request(&id, &account).map_err(friend_err_status)?;
    let ev = req.resolved_event();
    state.push_account_event(&req.from_account, ev.clone());
    state.push_account_event(&req.to_account, ev);
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// List the caller's accepted friends.
async fn friends_list(
    State(state): State<RelayState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Value>>, StatusCode> {
    let (_, account) = caller_account(&headers).await?;
    let rows = state
        .list_friends(&account)
        .into_iter()
        .map(|(account_id, login)| serde_json::json!({ "accountId": account_id, "login": login }))
        .collect();
    Ok(Json(rows))
}

/// List the caller's incoming (pending) friend requests.
async fn friend_requests_list(
    State(state): State<RelayState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Value>>, StatusCode> {
    let (_, account) = caller_account(&headers).await?;
    let rows = state
        .incoming_requests(&account, now_unix())
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "requestId": r.id,
                "fromAccount": r.from_account,
                "fromLogin": r.from_login,
                "createdAt": r.created_at,
            })
        })
        .collect();
    Ok(Json(rows))
}

/// Each accepted friend with their presence (`online`/`away`/`offline`).
async fn friends_presence(
    State(state): State<RelayState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Value>>, StatusCode> {
    let (_, account) = caller_account(&headers).await?;
    let rows = state
        .friend_presence(&account, now_unix())
        .into_iter()
        .map(|(account_id, login, presence)| {
            serde_json::json!({ "accountId": account_id, "login": login, "presence": presence })
        })
        .collect();
    Ok(Json(rows))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VisibilityBody {
    appear_offline: bool,
}

/// Toggle the caller's "appear offline" visibility.
async fn account_visibility(
    State(state): State<RelayState>,
    headers: HeaderMap,
    Json(body): Json<VisibilityBody>,
) -> Result<Json<Value>, StatusCode> {
    let (user, _) = caller_account(&headers).await?;
    state.set_visibility(user.id, body.appear_offline);
    Ok(Json(serde_json::json!({ "appearOffline": body.appear_offline })))
}

/// A friend's registered devices (dialable node ids) for the P2P peer-link
/// bootstrap. 403 unless the caller and `:account` are accepted friends.
async fn friend_devices_list(
    State(state): State<RelayState>,
    headers: HeaderMap,
    Path(other): Path<String>,
) -> Result<Json<Vec<social::DeviceRow>>, StatusCode> {
    let (_, account) = caller_account(&headers).await?;
    state.friend_devices(&account, &other).map(Json).ok_or(StatusCode::FORBIDDEN)
}

/// Remove an accepted friend (the `:account` is the friend's account key).
async fn friend_remove(
    State(state): State<RelayState>,
    headers: HeaderMap,
    Path(other): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let (_, account) = caller_account(&headers).await?;
    let removed = state.remove_friend(&account, &other);
    Ok(Json(serde_json::json!({ "ok": removed })))
}

/// Run the configured [`WriteGuard`] before a workspace write, if any. The open
/// relay has no guard → always `Ok` (content-blind forwarding). A paid build
/// sets a guard (see the private `hive-relay-enterprise` crate) that enforces
/// workspace membership / roles here.
async fn enforce_write(state: &RelayState, workspace: &str, headers: &HeaderMap) -> Result<(), Response> {
    match state.guard() {
        Some(g) => g.check(workspace, headers).await,
        None => Ok(()),
    }
}

/// Append an opaque workspace-key rotation blob (ciphertext only — the relay
/// can't read it). Members poll `list_keyring` and adopt the newest version
/// sealed to their device.
async fn publish_keyring(
    State(state): State<RelayState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, Response> {
    enforce_write(&state, &id, &headers).await?;
    let mut ws = state.workspaces.write().unwrap();
    ws.entry(id).or_default().keyring.push(body);
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn list_keyring(State(state): State<RelayState>, Path(id): Path<String>) -> Json<Vec<Value>> {
    let ws = state.workspaces.read().unwrap();
    Json(ws.get(&id).map(|w| w.keyring.clone()).unwrap_or_default())
}

#[derive(Deserialize)]
struct PairRequest {
    payload: String,
    #[serde(default)]
    ttl_secs: Option<u64>,
}

#[derive(Serialize)]
struct PairResponse {
    code: String,
    expires_in: u64,
}

/// Store a payload behind a fresh short code; returns the code + its TTL.
async fn create_pairing(
    State(state): State<RelayState>,
    Json(req): Json<PairRequest>,
) -> Result<Json<PairResponse>, (StatusCode, String)> {
    if req.payload.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "empty payload".into()));
    }
    if req.payload.len() > MAX_PAIR_PAYLOAD {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "payload too large".into()));
    }
    let ttl = req
        .ttl_secs
        .unwrap_or(DEFAULT_PAIR_TTL_SECS)
        .clamp(30, MAX_PAIR_TTL_SECS);
    let mut map = state.pairings.write().unwrap();
    prune_pairings(&mut map);
    let code = loop {
        let c = random_pair_code();
        if !map.contains_key(&c) {
            break c;
        }
    };
    map.insert(
        code.clone(),
        Pairing {
            payload: req.payload,
            expires_at: Instant::now() + Duration::from_secs(ttl),
        },
    );
    Ok(Json(PairResponse {
        code,
        expires_in: ttl,
    }))
}

/// Resolve a short code back to its payload (404 if unknown/expired).
async fn resolve_pairing(
    State(state): State<RelayState>,
    Path(code): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let code = normalize_pair_code(&code);
    let mut map = state.pairings.write().unwrap();
    prune_pairings(&mut map);
    match map.get(&code) {
        Some(p) => Ok(Json(serde_json::json!({ "payload": p.payload }))),
        None => Err(StatusCode::NOT_FOUND),
    }
}

#[derive(Serialize)]
struct PostEnvelopeResponse {
    seq: u64,
}

async fn post_envelope(
    State(state): State<RelayState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<PostEnvelopeResponse>, Response> {
    enforce_write(&state, &id, &headers).await?;
    let mut ws = state.workspaces.write().unwrap();
    let workspace = ws.entry(id).or_default();
    workspace.next_seq += 1;
    let seq = workspace.next_seq;
    workspace.envelopes.push(Stored { seq, body });
    Ok(Json(PostEnvelopeResponse { seq }))
}

#[derive(Deserialize)]
struct AfterQuery {
    #[serde(default)]
    after: u64,
}

#[derive(Serialize)]
struct EnvelopeRow {
    seq: u64,
    body: Value,
}

async fn list_envelopes(
    State(state): State<RelayState>,
    Path(id): Path<String>,
    Query(q): Query<AfterQuery>,
) -> Json<Vec<EnvelopeRow>> {
    let ws = state.workspaces.read().unwrap();
    let rows = ws
        .get(&id)
        .map(|w| {
            w.envelopes
                .iter()
                .filter(|e| e.seq > q.after)
                .map(|e| EnvelopeRow {
                    seq: e.seq,
                    body: e.body.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    Json(rows)
}

#[derive(Deserialize)]
struct DeviceBlob {
    device_id: String,
    #[serde(default)]
    data: Value,
}

async fn publish_candidates(
    State(state): State<RelayState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(blob): Json<DeviceBlob>,
) -> Result<Json<Value>, Response> {
    enforce_write(&state, &id, &headers).await?;
    let mut ws = state.workspaces.write().unwrap();
    ws.entry(id).or_default().candidates.insert(blob.device_id, blob.data);
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn list_candidates(
    State(state): State<RelayState>,
    Path(id): Path<String>,
) -> Json<HashMap<String, Value>> {
    let ws = state.workspaces.read().unwrap();
    Json(ws.get(&id).map(|w| w.candidates.clone()).unwrap_or_default())
}

async fn publish_presence(
    State(state): State<RelayState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(blob): Json<DeviceBlob>,
) -> Result<Json<Value>, Response> {
    enforce_write(&state, &id, &headers).await?;
    let mut ws = state.workspaces.write().unwrap();
    ws.entry(id).or_default().presence.insert(blob.device_id, blob.data);
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn list_presence(
    State(state): State<RelayState>,
    Path(id): Path<String>,
) -> Json<HashMap<String, Value>> {
    let ws = state.workspaces.read().unwrap();
    Json(ws.get(&id).map(|w| w.presence.clone()).unwrap_or_default())
}

#[cfg(test)]
mod pairing_tests {
    use super::*;

    #[test]
    fn codes_are_confusion_free_and_sized() {
        for _ in 0..200 {
            let c = random_pair_code();
            assert_eq!(c.len(), CODE_LEN);
            assert!(c.bytes().all(|b| CODE_ALPHABET.contains(&b)), "bad char in {c}");
            // no ambiguous letters
            assert!(!c.contains(['I', 'L', 'O', 'U']));
        }
    }

    #[test]
    fn normalize_strips_separators_and_cases() {
        assert_eq!(normalize_pair_code("  k7p-2qx "), "K7P2QX");
        assert_eq!(normalize_pair_code("K7P 2QX"), "K7P2QX");
    }

    #[test]
    fn prune_drops_expired() {
        let mut m = HashMap::new();
        m.insert("OLD".into(), Pairing { payload: "x".into(), expires_at: Instant::now() - Duration::from_secs(1) });
        m.insert("NEW".into(), Pairing { payload: "y".into(), expires_at: Instant::now() + Duration::from_secs(60) });
        prune_pairings(&mut m);
        assert!(!m.contains_key("OLD"));
        assert!(m.contains_key("NEW"));
    }
}

#[cfg(test)]
mod directory_tests {
    use super::*;

    #[test]
    fn bearer_token_parsing() {
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer gho_abc123".parse().unwrap());
        assert_eq!(bearer_token(&h).as_deref(), Some("gho_abc123"));
        let empty = HeaderMap::new();
        assert_eq!(bearer_token(&empty), None);
        let mut basic = HeaderMap::new();
        basic.insert("authorization", "Basic xyz".parse().unwrap());
        assert_eq!(bearer_token(&basic), None);
    }

    #[test]
    fn directory_upsert_and_lookup() {
        let mut dir: HashMap<String, DirAccount> = HashMap::new();
        let e = dir.entry("octocat".into()).or_insert_with(DirAccount::default);
        e.github_id = 42;
        e.login = "octocat".into();
        e.devices.insert("dev-mac".into(), "kapub-mac".into());
        e.devices.insert("dev-win".into(), "kapub-win".into());
        // A second register from another device adds, doesn't replace.
        let again = dir.get("octocat").unwrap();
        assert_eq!(again.devices.len(), 2);
        assert_eq!(again.devices.get("dev-win").map(String::as_str), Some("kapub-win"));
        assert!(dir.get("nobody").is_none());
    }
}

#[cfg(test)]
mod entitlement_tests {
    use super::*;

    #[test]
    fn open_allows_everyone_gated_checks_token() {
        let open = EntitlementPolicy::Open;
        assert!(open.allows(None));
        assert!(open.allows(Some("anything")));

        let gated = EntitlementPolicy::Tokens(["paid-tok".to_string()].into_iter().collect());
        assert!(!gated.allows(None));
        assert!(!gated.allows(Some("free-guess")));
        assert!(gated.allows(Some("paid-tok")));
    }
}

#[cfg(test)]
mod write_guard_tests {
    use super::*;

    struct DenyAll;
    #[async_trait::async_trait]
    impl WriteGuard for DenyAll {
        async fn check(&self, _ws: &str, _h: &HeaderMap) -> Result<(), Response> {
            Err((StatusCode::FORBIDDEN, "denied").into_response())
        }
    }

    #[tokio::test]
    async fn no_guard_allows_writes() {
        let state = RelayState::default();
        assert!(enforce_write(&state, "ws1", &HeaderMap::new()).await.is_ok());
    }

    #[tokio::test]
    async fn guard_can_reject_writes() {
        let state = RelayState::default().with_write_guard(Arc::new(DenyAll));
        let resp = enforce_write(&state, "ws1", &HeaderMap::new()).await.unwrap_err();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}
