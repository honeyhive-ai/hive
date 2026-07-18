//! Relay transport client — ported from `RelayTransportClient.swift`. Pushes
//! signed (and, in production, E2EE-sealed) event envelopes to the relay and
//! fetches everything after a cursor, so two clients sharing a workspace
//! converge. The relay is content-blind; see `hive-core::e2ee`.
//!
//! This is the relay-forwarding path. Direct P2P (STUN/hole-punch via the
//! rendezvous board) is a follow-up; relay forwarding already provides working
//! multiuser sync.

use hive_core::SessionEventEnvelope;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Outcome of a cheap connectivity/auth probe. Every failure maps to a variant
/// so the UI can tell "wrong URL / relay down" apart from "bad token".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayProbe {
    /// Reachable and the token (if any) was accepted.
    Ok,
    /// Reached the relay but it rejected the access token (401/403).
    Unauthorized,
    /// Reached an HTTP server but it returned an unexpected status.
    HttpStatus(u16),
    /// Could not reach the relay at all (DNS/connect/timeout/TLS).
    Unreachable(String),
}

/// Accept either a bare relay token or the issued `name:token` form and return
/// just the value the relay expects as the Bearer. The `name:` prefix is only a
/// server-side lookup key (e.g. `HIVE_RELAY_USER_TOKENS`); sending it verbatim
/// 401s. Relay tokens are colon-free (hex, or `hrt1.<b64>.<b64>`), so a leading
/// `identifier:` can be dropped safely; anything that doesn't look like that is
/// returned unchanged.
fn normalize_access_token(raw: &str) -> String {
    let t = raw.trim();
    if let Some((prefix, rest)) = t.split_once(':') {
        let prefix_ok = !prefix.is_empty()
            && prefix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
        let rest_ok = !rest.is_empty() && !rest.contains(':') && !rest.contains('/');
        if prefix_ok && rest_ok {
            return rest.to_string();
        }
    }
    t.to_string()
}

/// A device's key-agreement public key as listed in the directory.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryDevice {
    pub device_id: String,
    /// Hex-encoded X25519 public key.
    pub ka_public: String,
}

/// A directory entry: a GitHub account + its devices' key-agreement keys.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryEntry {
    pub github_id: u64,
    pub login: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub devices: Vec<DirectoryDevice>,
}

/// A workspace member as returned by a membership-enforcing relay.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberEntry {
    pub account: String,
    pub login: String,
    /// `owner | admin | contributor | viewer`.
    pub role: String,
    #[serde(default)]
    pub added_by: String,
    #[serde(default)]
    pub added_at: u64,
}

/// One issued access token's metadata, as returned by the relay admin API
/// (never the raw value or hash).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayTokenEntry {
    pub id: String,
    pub user_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub last_used: u64,
    #[serde(default)]
    pub revoked_at: Option<u64>,
}

/// A relay access user plus their tokens, as returned by `GET /v1/admin/users`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayUserEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub login: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub tokens: Vec<RelayTokenEntry>,
}

/// A freshly issued token — the `raw` value is shown ONCE and never retrievable
/// again.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssuedRelayToken {
    pub user: RelayUserEntry,
    pub token: RelayTokenEntry,
    pub raw: String,
}

/// This account's identity as confirmed by the relay on device registration.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountIdentity {
    /// The account key every device of this account shares (`github:<id>`).
    pub account_id: String,
    pub github_id: u64,
    pub login: String,
}

/// One of an account's registered devices (social registry).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountDevice {
    pub device_id: String,
    #[serde(default)]
    pub node_id: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub last_seen: u64,
}

/// One event from the account inbox (the account channel). `body` is an opaque
/// JSON payload whose `kind` later phases interpret (friend request, etc.).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct InboxEvent {
    pub seq: u64,
    pub body: serde_json::Value,
}

/// An accepted friend.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Friend {
    pub account_id: String,
    pub login: String,
}

/// An incoming (pending) friend request addressed to this account.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingFriendRequest {
    pub request_id: String,
    pub from_account: String,
    pub from_login: String,
    #[serde(default)]
    pub created_at: u64,
}

/// A friend with their current presence (`online` | `away` | `offline`).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendPresence {
    pub account_id: String,
    pub login: String,
    pub presence: String,
}

/// Result of attempting to send a friend request — distinguishes the cases the
/// UI surfaces differently (cap reached → upgrade prompt, unknown user, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FriendRequestOutcome {
    /// Delivered; carries the request id.
    Sent(String),
    /// You're already friends.
    AlreadyFriends,
    /// The free-tier collaborator cap is reached (HTTP 402).
    CapReached,
    /// No Hive account for that username (they haven't signed in).
    UserNotFound,
    /// Bad request (e.g. befriending yourself).
    Invalid,
}

/// One envelope as returned by the relay, with its server sequence.
#[derive(Debug, Clone)]
pub struct FetchedEnvelope {
    pub seq: u64,
    pub envelope: SessionEventEnvelope,
}

/// HTTP client for a single relay endpoint (e.g. `https://relay.example`).
#[derive(Debug, Clone)]
pub struct RelayClient {
    http: reqwest::Client,
    base: String,
    /// Relay access token (entitlement). Attached as `Authorization: Bearer` to
    /// every request; `None` for open/self-hosted relays.
    auth: Option<String>,
    /// GitHub identity token. Attached as `X-Hive-Github-Token` to every request
    /// so a membership-enforcing relay can authenticate the caller; `None` when
    /// not signed in (open relays ignore it).
    github_token: Option<String>,
}

impl RelayClient {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.into().trim_end_matches('/').to_string(),
            auth: None,
            github_token: None,
        }
    }

    /// Set the relay access token (for a gated/paid hosted relay). Empty = none.
    pub fn with_auth(mut self, token: Option<String>) -> Self {
        self.auth = token
            .map(|t| normalize_access_token(&t))
            .filter(|t| !t.is_empty());
        self
    }

    /// Set the caller's GitHub identity token (for membership-enforcing relays).
    /// Empty = none. Attached as `X-Hive-Github-Token` on every request.
    pub fn with_github_token(mut self, token: Option<String>) -> Self {
        self.github_token = token.filter(|t| !t.trim().is_empty());
        self
    }

    /// Attach the entitlement bearer + GitHub identity (if any) to a request.
    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let rb = match &self.auth {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        };
        match &self.github_token {
            Some(t) => rb.header("X-Hive-Github-Token", t),
            None => rb,
        }
    }

    /// Push an envelope to a workspace; returns the relay-assigned sequence.
    pub async fn push(
        &self,
        workspace: &str,
        envelope: &SessionEventEnvelope,
    ) -> Result<u64, RelayError> {
        let body = serde_json::to_value(envelope)?;
        self.push_value(workspace, &body).await
    }

    /// Push an arbitrary opaque body (e.g. a sealed envelope). The relay is
    /// content-blind; with E2EE this is ciphertext.
    pub async fn push_value(
        &self,
        workspace: &str,
        body: &serde_json::Value,
    ) -> Result<u64, RelayError> {
        let url = format!("{}/v1/workspaces/{}/envelopes", self.base, workspace);
        let resp = self.authed(self.http.post(url)).json(body).send().await?;
        let value: serde_json::Value = resp.json().await?;
        Ok(value.get("seq").and_then(serde_json::Value::as_u64).unwrap_or(0))
    }

    /// Cheap connectivity + auth check: an authed GET of the envelope list.
    /// Never errors — maps every failure to a `RelayProbe` the UI can render.
    pub async fn probe(&self, workspace: &str) -> RelayProbe {
        let url = format!("{}/v1/workspaces/{}/envelopes?after=0", self.base, workspace);
        match self.authed(self.http.get(url)).send().await {
            Ok(resp) => {
                let code = resp.status().as_u16();
                if resp.status().is_success() {
                    RelayProbe::Ok
                } else if code == 401 || code == 403 {
                    RelayProbe::Unauthorized
                } else {
                    RelayProbe::HttpStatus(code)
                }
            }
            Err(e) => RelayProbe::Unreachable(e.to_string()),
        }
    }

    /// Fetch raw opaque bodies past `after`, with their server sequences.
    pub async fn fetch_values(
        &self,
        workspace: &str,
        after: u64,
    ) -> Result<Vec<(u64, serde_json::Value)>, RelayError> {
        let url = format!(
            "{}/v1/workspaces/{}/envelopes?after={}",
            self.base, workspace, after
        );
        let rows: Vec<serde_json::Value> = self.authed(self.http.get(url)).send().await?.json().await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let seq = row.get("seq").and_then(serde_json::Value::as_u64).unwrap_or(0);
                let body = row.get("body").cloned().unwrap_or(serde_json::Value::Null);
                (seq, body)
            })
            .collect())
    }

    /// Fetch every envelope on a workspace with server sequence > `after`.
    pub async fn fetch(
        &self,
        workspace: &str,
        after: u64,
    ) -> Result<Vec<FetchedEnvelope>, RelayError> {
        let url = format!(
            "{}/v1/workspaces/{}/envelopes?after={}",
            self.base, workspace, after
        );
        let rows: Vec<serde_json::Value> = self.authed(self.http.get(url)).send().await?.json().await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let seq = row.get("seq").and_then(serde_json::Value::as_u64).unwrap_or(0);
            if let Some(body) = row.get("body") {
                let envelope: SessionEventEnvelope = serde_json::from_value(body.clone())?;
                out.push(FetchedEnvelope { seq, envelope });
            }
        }
        Ok(out)
    }

    /// Upsert this device's ephemeral presence (online + typing) for a
    /// workspace. Presence is not part of the event log — it's transient
    /// metadata keyed by `device_id` and overwritten on each ping.
    pub async fn publish_presence(
        &self,
        workspace: &str,
        device_id: &str,
        data: &serde_json::Value,
    ) -> Result<(), RelayError> {
        let url = format!("{}/v1/workspaces/{}/presence", self.base, workspace);
        let body = serde_json::json!({ "device_id": device_id, "data": data });
        self.authed(self.http.post(url)).json(&body).send().await?;
        Ok(())
    }

    /// Store an opaque payload (a peer friend code or workspace invite) behind a
    /// short, human-typeable pairing code; returns `(code, expires_in_secs)`.
    /// The relay only brokers the handoff — it never sees plaintext traffic.
    pub async fn publish_pairing(
        &self,
        payload: &str,
        ttl_secs: Option<u64>,
    ) -> Result<(String, u64), RelayError> {
        let url = format!("{}/v1/pair", self.base);
        let body = serde_json::json!({ "payload": payload, "ttl_secs": ttl_secs });
        let resp = self.authed(self.http.post(url)).json(&body).send().await?;
        let value: serde_json::Value = resp.json().await?;
        let code = value
            .get("code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let expires_in = value.get("expires_in").and_then(serde_json::Value::as_u64).unwrap_or(0);
        Ok((code, expires_in))
    }

    /// Resolve a short pairing code back to its payload. `Ok(None)` if the code
    /// is unknown or expired.
    pub async fn resolve_pairing(&self, code: &str) -> Result<Option<String>, RelayError> {
        let url = format!("{}/v1/pair/{}", self.base, code.trim());
        let resp = self.authed(self.http.get(url)).send().await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let value: serde_json::Value = resp.json().await?;
        Ok(value
            .get("payload")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string))
    }

    /// Publish a workspace-key rotation (sealed per-device) to the keyring. The
    /// relay stores it opaquely; only recipients can open their sealed blob.
    pub async fn publish_key_rotation(
        &self,
        workspace: &str,
        rotation: &hive_core::e2ee::WorkspaceKeyRotation,
    ) -> Result<(), RelayError> {
        let url = format!("{}/v1/workspaces/{}/keyring", self.base, workspace);
        let body = serde_json::to_value(rotation)?;
        self.authed(self.http.post(url)).json(&body).send().await?;
        Ok(())
    }

    /// Fetch all key rotations published to a workspace's keyring.
    pub async fn fetch_key_rotations(
        &self,
        workspace: &str,
    ) -> Result<Vec<hive_core::e2ee::WorkspaceKeyRotation>, RelayError> {
        let url = format!("{}/v1/workspaces/{}/keyring", self.base, workspace);
        let rows: Vec<serde_json::Value> = self.authed(self.http.get(url)).send().await?.json().await?;
        Ok(rows
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect())
    }

    /// Register (or refresh) one of this account's devices in the directory.
    /// Identity comes from the client's GitHub token (see [`with_github_token`]).
    pub async fn directory_register(
        &self,
        device_id: &str,
        ka_public_hex: &str,
    ) -> Result<(), RelayError> {
        let url = format!("{}/v1/directory/register", self.base);
        let body = serde_json::json!({ "device_id": device_id, "ka_public": ka_public_hex });
        self.authed(self.http.post(url)).json(&body).send().await?;
        Ok(())
    }

    /// Resolve a GitHub handle to its account + every device's key-agreement
    /// public key. `Ok(None)` if unknown. Identity: the client's GitHub token.
    pub async fn directory_lookup(&self, handle: &str) -> Result<Option<DirectoryEntry>, RelayError> {
        let url = format!("{}/v1/directory/{}", self.base, handle.trim().trim_start_matches('@'));
        let resp = self.authed(self.http.get(url)).send().await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        Ok(Some(resp.json().await?))
    }

    /// Claim an unmanaged workspace, making the caller `Owner` and turning on
    /// server-side membership enforcement. `Ok(false)` if the relay doesn't
    /// support membership (open/self-host → 404) or it's already claimed.
    pub async fn claim_membership(&self, workspace: &str) -> Result<bool, RelayError> {
        let url = format!("{}/v1/workspaces/{}/members/claim", self.base, workspace);
        let resp = self.authed(self.http.post(url)).send().await?;
        Ok(resp.status().is_success())
    }

    /// List a workspace's members (membership-enforcing relays only). `Ok(None)`
    /// if the workspace is unmanaged or the relay doesn't support membership.
    pub async fn list_members(&self, workspace: &str) -> Result<Option<Vec<MemberEntry>>, RelayError> {
        let url = format!("{}/v1/workspaces/{}/members", self.base, workspace);
        let resp = self.authed(self.http.get(url)).send().await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        Ok(Some(resp.json().await?))
    }

    /// Add a member or change their role (caller must be `Admin`+).
    pub async fn upsert_member(
        &self,
        workspace: &str,
        account: &str,
        login: &str,
        role: &str,
    ) -> Result<(), RelayError> {
        let url = format!("{}/v1/workspaces/{}/members", self.base, workspace);
        let body = serde_json::json!({ "account": account, "login": login, "role": role });
        self.authed(self.http.post(url)).json(&body).send().await?;
        Ok(())
    }

    /// Remove a member (caller must be `Admin`+). Pair with a key rotation so the
    /// ejected account also loses read access to new traffic.
    pub async fn remove_member(&self, workspace: &str, account: &str) -> Result<(), RelayError> {
        let url = format!("{}/v1/workspaces/{}/members/{}", self.base, workspace, account);
        self.authed(self.http.delete(url)).send().await?;
        Ok(())
    }

    // ── Relay access user/token administration (enterprise relay) ──────────
    // Gated server-side by the relay's admin authorizer (a GitHub-admin
    // allowlist), authenticated by this client's GitHub token. `Ok(None)` when
    // the relay has no admin API (open/OSS) or the caller isn't an admin.

    /// List relay access users + their tokens. `Ok(None)` if the admin API is
    /// unavailable or the caller isn't authorized.
    pub async fn admin_list_users(&self) -> Result<Option<Vec<RelayUserEntry>>, RelayError> {
        let url = format!("{}/v1/admin/users", self.base);
        let resp = self.authed(self.http.get(url)).send().await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        Ok(Some(resp.json().await?))
    }

    /// Create a user and issue their first token. The returned `raw` token is
    /// shown once. Returns the HTTP status text on failure.
    pub async fn admin_create_user(
        &self,
        name: &str,
        login: &str,
        label: &str,
    ) -> Result<IssuedRelayToken, RelayError> {
        let url = format!("{}/v1/admin/users", self.base);
        let body = serde_json::json!({ "name": name, "login": login, "label": label });
        let resp = self.authed(self.http.post(url)).json(&body).send().await?;
        Ok(resp.error_for_status()?.json().await?)
    }

    /// Issue an additional token for an existing user (raw shown once).
    pub async fn admin_issue_token(
        &self,
        user_id: &str,
        label: &str,
    ) -> Result<IssuedRelayToken, RelayError> {
        let url = format!("{}/v1/admin/users/{}/tokens", self.base, user_id);
        let body = serde_json::json!({ "label": label });
        let resp = self.authed(self.http.post(url)).json(&body).send().await?;
        Ok(resp.error_for_status()?.json().await?)
    }

    /// Revoke a single token immediately.
    pub async fn admin_revoke_token(&self, token_id: &str) -> Result<(), RelayError> {
        let url = format!("{}/v1/admin/tokens/{}", self.base, token_id);
        self.authed(self.http.delete(url)).send().await?.error_for_status()?;
        Ok(())
    }

    /// Enable or disable a user (disabling kills all their tokens at once).
    pub async fn admin_set_user_disabled(
        &self,
        user_id: &str,
        disabled: bool,
    ) -> Result<(), RelayError> {
        let url = format!("{}/v1/admin/users/{}/disabled", self.base, user_id);
        let body = serde_json::json!({ "disabled": disabled });
        self.authed(self.http.post(url)).json(&body).send().await?.error_for_status()?;
        Ok(())
    }

    /// Read everyone's presence blobs for a workspace (`device_id` → data).
    pub async fn list_presence(
        &self,
        workspace: &str,
    ) -> Result<std::collections::HashMap<String, serde_json::Value>, RelayError> {
        let url = format!("{}/v1/workspaces/{}/presence", self.base, workspace);
        let map = self.authed(self.http.get(url)).send().await?.json().await?;
        Ok(map)
    }

    // ── Social graph (account registry + account channel) ──────────────────

    /// Register (or refresh) this device under the signed-in GitHub account.
    /// Identity comes from the GitHub token (see [`with_github_token`]).
    /// `Ok(None)` if the relay rejected the call (e.g. not signed in).
    pub async fn account_register(
        &self,
        device_id: &str,
        node_id: Option<&str>,
        label: Option<&str>,
    ) -> Result<Option<AccountIdentity>, RelayError> {
        let url = format!("{}/v1/account/register", self.base);
        let body = serde_json::json!({
            "deviceId": device_id,
            "nodeId": node_id,
            "label": label,
        });
        let resp = self.authed(self.http.post(url)).json(&body).send().await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        Ok(Some(resp.json().await?))
    }

    /// Refresh this device's `last_seen` (presence). `Ok(true)` if the relay
    /// recognised the device; `Ok(false)` if it should re-register first.
    pub async fn account_heartbeat(&self, device_id: &str) -> Result<bool, RelayError> {
        let url = format!("{}/v1/account/heartbeat", self.base);
        let body = serde_json::json!({ "deviceId": device_id });
        let resp = self.authed(self.http.post(url)).json(&body).send().await?;
        if !resp.status().is_success() {
            return Ok(false);
        }
        let v: serde_json::Value = resp.json().await?;
        Ok(v.get("ok").and_then(serde_json::Value::as_bool).unwrap_or(false))
    }

    /// Poll this account's inbox (the account channel) for events with
    /// `seq > after`. The same stream is delivered to every device of the
    /// account, so accepting/dismissing on one device propagates to the rest.
    pub async fn account_inbox(&self, after: u64) -> Result<Vec<InboxEvent>, RelayError> {
        let url = format!("{}/v1/account/inbox?after={}", self.base, after);
        let resp = self.authed(self.http.get(url)).send().await?;
        if !resp.status().is_success() {
            return Ok(Vec::new());
        }
        Ok(resp.json().await?)
    }

    /// List this account's registered devices (presence + P2P bootstrap).
    pub async fn account_devices(&self) -> Result<Vec<AccountDevice>, RelayError> {
        let url = format!("{}/v1/account/devices", self.base);
        let resp = self.authed(self.http.get(url)).send().await?;
        if !resp.status().is_success() {
            return Ok(Vec::new());
        }
        Ok(resp.json().await?)
    }

    // ── Friend graph (P2) ──────────────────────────────────────────────────

    /// Send a friend request to a GitHub `@username`. The relay delivers it to
    /// every device of the target. The outcome distinguishes cap-reached /
    /// unknown-user / already-friends so the UI can respond appropriately.
    pub async fn friend_request(&self, to_login: &str) -> Result<FriendRequestOutcome, RelayError> {
        let url = format!("{}/v1/friends/requests", self.base);
        let body = serde_json::json!({ "toLogin": to_login });
        let resp = self.authed(self.http.post(url)).json(&body).send().await?;
        use reqwest::StatusCode;
        Ok(match resp.status() {
            s if s.is_success() => {
                let v: serde_json::Value = resp.json().await?;
                FriendRequestOutcome::Sent(
                    v.get("requestId").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                )
            }
            StatusCode::PAYMENT_REQUIRED => FriendRequestOutcome::CapReached,
            StatusCode::NOT_FOUND => FriendRequestOutcome::UserNotFound,
            StatusCode::CONFLICT => FriendRequestOutcome::AlreadyFriends,
            _ => FriendRequestOutcome::Invalid,
        })
    }

    /// Accept a pending friend request by id. `Ok(true)` on success.
    pub async fn friend_accept(&self, request_id: &str) -> Result<bool, RelayError> {
        let url = format!("{}/v1/friends/requests/{}/accept", self.base, request_id);
        Ok(self.authed(self.http.post(url)).send().await?.status().is_success())
    }

    /// Reject (recipient) or cancel (sender) a pending request by id.
    pub async fn friend_reject(&self, request_id: &str) -> Result<bool, RelayError> {
        let url = format!("{}/v1/friends/requests/{}/reject", self.base, request_id);
        Ok(self.authed(self.http.post(url)).send().await?.status().is_success())
    }

    /// List the caller's accepted friends.
    pub async fn friends(&self) -> Result<Vec<Friend>, RelayError> {
        let url = format!("{}/v1/friends", self.base);
        let resp = self.authed(self.http.get(url)).send().await?;
        if !resp.status().is_success() {
            return Ok(Vec::new());
        }
        Ok(resp.json().await?)
    }

    /// List the caller's incoming (pending) friend requests.
    pub async fn incoming_friend_requests(&self) -> Result<Vec<IncomingFriendRequest>, RelayError> {
        let url = format!("{}/v1/friends/requests", self.base);
        let resp = self.authed(self.http.get(url)).send().await?;
        if !resp.status().is_success() {
            return Ok(Vec::new());
        }
        Ok(resp.json().await?)
    }

    /// Each accepted friend with their current presence.
    pub async fn friend_presence(&self) -> Result<Vec<FriendPresence>, RelayError> {
        let url = format!("{}/v1/friends/presence", self.base);
        let resp = self.authed(self.http.get(url)).send().await?;
        if !resp.status().is_success() {
            return Ok(Vec::new());
        }
        Ok(resp.json().await?)
    }

    /// A friend's registered devices (dialable node ids) for the P2P peer-link
    /// bootstrap. Empty if not friends (the relay returns 403) or none have a
    /// node id yet.
    pub async fn friend_devices(&self, account_id: &str) -> Result<Vec<AccountDevice>, RelayError> {
        let url = format!("{}/v1/friends/{}/devices", self.base, account_id);
        let resp = self.authed(self.http.get(url)).send().await?;
        if !resp.status().is_success() {
            return Ok(Vec::new());
        }
        Ok(resp.json().await?)
    }

    /// Set this account's "appear offline" visibility. `Ok(true)` on success.
    pub async fn set_visibility(&self, appear_offline: bool) -> Result<bool, RelayError> {
        let url = format!("{}/v1/account/visibility", self.base);
        let body = serde_json::json!({ "appearOffline": appear_offline });
        Ok(self.authed(self.http.post(url)).json(&body).send().await?.status().is_success())
    }

    /// Remove an accepted friend by their account key. `Ok(true)` if removed.
    pub async fn friend_remove(&self, account_id: &str) -> Result<bool, RelayError> {
        let url = format!("{}/v1/friends/{}", self.base, account_id);
        let resp = self.authed(self.http.delete(url)).send().await?;
        if !resp.status().is_success() {
            return Ok(false);
        }
        let v: serde_json::Value = resp.json().await?;
        Ok(v.get("ok").and_then(serde_json::Value::as_bool).unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::{ChatMessage, MessageRole, SessionEvent, SessionEventEnvelope};
    use uuid::Uuid;

    #[test]
    fn normalizes_name_prefixed_tokens() {
        // The issued `name:token` form → bare token.
        assert_eq!(
            normalize_access_token("alice:0123456789abcdef0123456789abcdef01234567"),
            "0123456789abcdef0123456789abcdef01234567"
        );
        // A bare token is untouched.
        assert_eq!(normalize_access_token("abcdef123456"), "abcdef123456");
        // hrt1 tokens contain dots but no colons → untouched.
        assert_eq!(normalize_access_token("hrt1.abc.def"), "hrt1.abc.def");
        // Leading/trailing whitespace trimmed.
        assert_eq!(normalize_access_token("  bob:tok  "), "tok");
        // A mispasted URL is not treated as name:token (rest has '/').
        assert_eq!(normalize_access_token("https://relay.example"), "https://relay.example");
    }

    async fn spawn_relay() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, hive_relay::router()).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn two_clients_sync_through_the_relay() {
        let base = spawn_relay().await;
        let workspace = Uuid::new_v4().to_string();

        // Client A authors and pushes two envelopes.
        let client_a = RelayClient::new(&base);
        let sid = Uuid::new_v4();
        let wid = Uuid::new_v4();
        for (i, body) in ["hello", "from A"].iter().enumerate() {
            let env = SessionEventEnvelope::new(
                sid,
                wid,
                (i + 1) as i64,
                SessionEvent::MessageAppended {
                    message: ChatMessage::new(MessageRole::User, "A", *body),
                },
            );
            client_a.push(&workspace, &env).await.unwrap();
        }

        // Client B fetches from the start and sees both, in order.
        let client_b = RelayClient::new(&base);
        let fetched = client_b.fetch(&workspace, 0).await.unwrap();
        assert_eq!(fetched.len(), 2);
        assert_eq!(fetched[0].seq, 1);
        assert_eq!(fetched[1].seq, 2);
        match &fetched[1].envelope.payload {
            SessionEvent::MessageAppended { message } => assert_eq!(message.body, "from A"),
            _ => panic!("unexpected payload"),
        }

        // Incremental fetch after the cursor returns nothing new.
        assert!(client_b.fetch(&workspace, 2).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn pairing_code_round_trips_through_relay() {
        let base = spawn_relay().await;
        let sharer = RelayClient::new(&base);
        let joiner = RelayClient::new(&base);

        let payload = "hive_AbC123example";
        let (code, ttl) = sharer.publish_pairing(payload, Some(600)).await.unwrap();
        assert!(!code.is_empty());
        assert_eq!(ttl, 600);

        // Joiner resolves it (case/separator tolerant on the relay side).
        let got = joiner.resolve_pairing(&code.to_lowercase()).await.unwrap();
        assert_eq!(got.as_deref(), Some(payload));

        // Unknown code → None, not an error.
        assert_eq!(joiner.resolve_pairing("ZZZZZZ").await.unwrap(), None);
    }

    #[tokio::test]
    async fn key_rotations_publish_and_fetch() {
        use hive_core::e2ee::{KeyAgreementKeypair, WorkspaceKeyRotation};
        let base = spawn_relay().await;
        let owner = RelayClient::new(&base);
        let member = RelayClient::new(&base);
        let workspace = Uuid::new_v4().to_string();

        // Owner seals a v2 key to one remaining device and publishes it.
        let alice = KeyAgreementKeypair::generate().unwrap();
        let key_v2 = [7u8; 32];
        let rot = WorkspaceKeyRotation::seal_for_devices(
            2,
            &key_v2,
            &[("alice".into(), alice.public_key_bytes().to_vec())],
        )
        .unwrap();
        owner.publish_key_rotation(&workspace, &rot).await.unwrap();

        // Member fetches it and opens its sealed blob back to the new key.
        let got = member.fetch_key_rotations(&workspace).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].version, 2);
        let opened = hive_core::e2ee::open(&alice, &got[0].sealed["alice"]).unwrap();
        assert_eq!(opened, key_v2);
    }
}
