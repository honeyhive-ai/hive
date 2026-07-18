//! MCP OAuth 2.1 client (for remote, authenticated MCP servers like Linear's
//! hosted server).
//!
//! Implements the MCP authorization flow: discover the authorization server
//! from the protected resource, obtain a client id (dynamic registration per
//! RFC 7591, or a pre-configured one), run authorization-code + **PKCE** via a
//! transient **loopback redirect**, and exchange the code for tokens.
//!
//! The pure building blocks (PKCE, well-known discovery URLs, authorize-URL
//! construction, token/metadata parsing) are unit-tested. The live orchestration
//! ([`authorize`]) needs a real browser + server, so it ships build-complete and
//! is validated against an actual login (see task #147).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Fixed loopback port for the OAuth redirect. Must match exactly what's
/// registered in a manually-created OAuth app, so it can't be ephemeral. (For
/// dynamic client registration we send the same URI at registration time.)
pub const REDIRECT_PORT: u16 = 51736;
/// The full redirect URI to register in an OAuth app.
pub const REDIRECT_URI: &str = "http://127.0.0.1:51736/callback";

#[derive(Debug, Error)]
pub enum OAuthError {
    #[error("oauth transport: {0}")]
    Transport(String),
    #[error("oauth protocol: {0}")]
    Protocol(String),
    #[error("oauth io: {0}")]
    Io(String),
    #[error("this server needs a registered client id (no dynamic registration available)")]
    NeedsClientId,
}

// ---------------------------------------------------------------------------
// Pure building blocks (unit-tested)
// ---------------------------------------------------------------------------

/// A PKCE verifier/challenge pair (RFC 7636, S256).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// Generate a fresh PKCE pair: a 43-char base64url verifier and its S256
/// challenge `base64url(sha256(verifier))`.
pub fn generate_pkce() -> Pkce {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("os rng");
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    Pkce {
        challenge: pkce_challenge(&verifier),
        verifier,
    }
}

/// The S256 challenge for a given verifier (split out so it's testable).
pub fn pkce_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// A random URL-safe token (for the `state` CSRF parameter).
pub fn random_state() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("os rng");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The `scheme://host[:port]` origin of a URL (no path).
pub fn origin(url: &str) -> String {
    if let Some(i) = url.find("://") {
        let rest = &url[i + 3..];
        let end = rest.find('/').unwrap_or(rest.len());
        format!("{}{}", &url[..i + 3], &rest[..end])
    } else {
        url.trim_end_matches('/').to_string()
    }
}

/// RFC 9728 protected-resource metadata URL for a resource server.
pub fn well_known_protected_resource(server_url: &str) -> String {
    format!("{}/.well-known/oauth-protected-resource", origin(server_url))
}

/// RFC 8414 authorization-server metadata URL for an issuer.
pub fn well_known_authorization_server(issuer: &str) -> String {
    format!("{}/.well-known/oauth-authorization-server", origin(issuer))
}

/// Percent-encode a query-parameter value (RFC 3986 unreserved set kept raw).
fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Minimal percent-decoding for redirect query values.
fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Build the authorization-request URL (authorization-code + PKCE, with the
/// MCP `resource` indicator per RFC 8707).
#[allow(clippy::too_many_arguments)]
pub fn authorize_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
    scope: &str,
    resource: &str,
) -> String {
    let sep = if authorization_endpoint.contains('?') { '&' } else { '?' };
    format!(
        "{authorization_endpoint}{sep}response_type=code&client_id={}&redirect_uri={}\
         &code_challenge={}&code_challenge_method=S256&state={}&scope={}&resource={}",
        pct(client_id),
        pct(redirect_uri),
        pct(challenge),
        pct(state),
        pct(scope),
        pct(resource),
    )
}

/// RFC 8414 authorization-server metadata (the fields we use).
#[derive(Debug, Clone, Deserialize)]
pub struct AsMetadata {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
}

/// RFC 9728 protected-resource metadata (the fields we use).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProtectedResourceMetadata {
    #[serde(default)]
    pub authorization_servers: Vec<String>,
}

/// A token endpoint response.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub token_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Live orchestration (needs a browser + real server — validated via login)
// ---------------------------------------------------------------------------

/// Inputs for an authorization run.
pub struct OAuthConfig {
    /// The remote MCP server URL (the protected resource).
    pub server_url: String,
    /// A pre-registered client id, or `None` to attempt dynamic registration.
    pub configured_client_id: Option<String>,
    /// Client secret for confidential providers (e.g. Linear, whose token
    /// endpoint requires it). `None` for public/PKCE-only clients.
    pub client_secret: Option<String>,
    /// Requested scope (space-separated).
    pub scope: String,
}

/// A completed authorization: the tokens plus the bits needed to refresh later
/// (the token endpoint + the client id actually used).
pub struct Authorized {
    pub token: TokenResponse,
    pub token_endpoint: String,
    pub client_id: String,
}

/// Run the full authorization-code + PKCE flow and return the tokens. Opens the
/// system browser via `open_url` and captures the redirect on a loopback port.
pub async fn authorize(
    cfg: &OAuthConfig,
    open_url: impl Fn(&str),
) -> Result<Authorized, OAuthError> {
    let http = reqwest::Client::new();
    let meta = discover(&http, &cfg.server_url).await?;

    // Bind the *fixed* loopback redirect: a manually-registered OAuth app must
    // know the exact redirect URI ahead of time, so the port can't be random.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", REDIRECT_PORT))
        .await
        .map_err(|e| {
            OAuthError::Io(format!(
                "loopback port {REDIRECT_PORT} is unavailable ({e}) — close whatever is using it and retry"
            ))
        })?;
    let redirect_uri = REDIRECT_URI.to_string();

    let client_id = match &cfg.configured_client_id {
        Some(id) if !id.is_empty() => id.clone(),
        _ => register_client(&http, &meta, &redirect_uri, &cfg.scope).await?,
    };

    let pkce = generate_pkce();
    let state = random_state();
    let url = authorize_url(
        &meta.authorization_endpoint,
        &client_id,
        &redirect_uri,
        &pkce.challenge,
        &state,
        &cfg.scope,
        &cfg.server_url,
    );
    open_url(&url);

    let code = await_redirect(listener, &state).await?;
    let token = exchange_code(
        &http,
        &meta,
        &client_id,
        cfg.client_secret.as_deref(),
        &redirect_uri,
        &code,
        &pkce.verifier,
    )
    .await?;
    Ok(Authorized {
        token,
        token_endpoint: meta.token_endpoint,
        client_id,
    })
}

/// Exchange a refresh token for a fresh access token (RFC 6749 §6, public
/// client). Used to renew an expired token without another browser round-trip.
pub async fn refresh(
    token_endpoint: &str,
    client_id: &str,
    client_secret: Option<&str>,
    refresh_token: &str,
) -> Result<TokenResponse, OAuthError> {
    let http = reqwest::Client::new();
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    if let Some(secret) = client_secret.filter(|s| !s.is_empty()) {
        form.push(("client_secret", secret));
    }
    let resp = http
        .post(token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|e| OAuthError::Transport(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(OAuthError::Protocol(format!("refresh returned {}", resp.status())));
    }
    resp.json::<TokenResponse>()
        .await
        .map_err(|e| OAuthError::Protocol(e.to_string()))
}

/// Discover the authorization server: protected-resource metadata → issuer →
/// AS metadata. Falls back to treating the server's own origin as the issuer.
async fn discover(http: &reqwest::Client, server_url: &str) -> Result<AsMetadata, OAuthError> {
    let issuer = match http.get(well_known_protected_resource(server_url)).send().await {
        Ok(r) if r.status().is_success() => r
            .json::<ProtectedResourceMetadata>()
            .await
            .ok()
            .and_then(|m| m.authorization_servers.into_iter().next()),
        _ => None,
    }
    .unwrap_or_else(|| origin(server_url));

    let meta = http
        .get(well_known_authorization_server(&issuer))
        .send()
        .await
        .map_err(|e| OAuthError::Transport(e.to_string()))?;
    if !meta.status().is_success() {
        return Err(OAuthError::Protocol(format!(
            "authorization-server metadata returned {}",
            meta.status()
        )));
    }
    meta.json::<AsMetadata>()
        .await
        .map_err(|e| OAuthError::Protocol(e.to_string()))
}

/// RFC 7591 dynamic client registration (public client, PKCE, no secret).
async fn register_client(
    http: &reqwest::Client,
    meta: &AsMetadata,
    redirect_uri: &str,
    scope: &str,
) -> Result<String, OAuthError> {
    let endpoint = meta.registration_endpoint.as_ref().ok_or(OAuthError::NeedsClientId)?;
    let body = serde_json::json!({
        "client_name": "Hive",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "scope": scope,
    });
    let resp = http
        .post(endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|e| OAuthError::Transport(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(OAuthError::Protocol(format!("registration returned {}", resp.status())));
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| OAuthError::Protocol(e.to_string()))?;
    v.get("client_id")
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| OAuthError::Protocol("registration response missing client_id".into()))
}

/// Accept one loopback connection, parse `code`/`state` from the redirect, and
/// return the code (after verifying `state`).
async fn await_redirect(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> Result<String, OAuthError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut stream, _) = listener.accept().await.map_err(|e| OAuthError::Io(e.to_string()))?;
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).await.map_err(|e| OAuthError::Io(e.to_string()))?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let query = req
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|path| path.split('?').nth(1))
        .unwrap_or("");
    let (mut code, mut state) = (None, None);
    for kv in query.split('&') {
        let mut it = kv.splitn(2, '=');
        match (it.next(), it.next()) {
            (Some("code"), Some(v)) => code = Some(pct_decode(v)),
            (Some("state"), Some(v)) => state = Some(pct_decode(v)),
            _ => {}
        }
    }
    let page = "<html><body style='font-family:sans-serif;padding:2rem'>\
        <h3>Hive — authorization complete.</h3><p>You can close this tab.</p></body></html>";
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        page.len(),
        page
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;

    if state.as_deref() != Some(expected_state) {
        return Err(OAuthError::Protocol("state mismatch (possible CSRF)".into()));
    }
    code.ok_or_else(|| OAuthError::Protocol("no authorization code in redirect".into()))
}

/// Exchange the authorization code for tokens. Sends PKCE always, plus a
/// `client_secret` when the provider is confidential (e.g. Linear).
async fn exchange_code(
    http: &reqwest::Client,
    meta: &AsMetadata,
    client_id: &str,
    client_secret: Option<&str>,
    redirect_uri: &str,
    code: &str,
    verifier: &str,
) -> Result<TokenResponse, OAuthError> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", verifier),
    ];
    if let Some(secret) = client_secret.filter(|s| !s.is_empty()) {
        form.push(("client_secret", secret));
    }
    let resp = http
        .post(&meta.token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|e| OAuthError::Transport(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(OAuthError::Protocol(format!("token endpoint returned {}", resp.status())));
    }
    resp.json::<TokenResponse>()
        .await
        .map_err(|e| OAuthError::Protocol(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_s256_base64url() {
        // RFC 7636 Appendix B worked example.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(pkce_challenge(verifier), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn generated_pkce_round_trips() {
        let p = generate_pkce();
        assert!(p.verifier.len() >= 43);
        assert_eq!(p.challenge, pkce_challenge(&p.verifier));
        // Two calls differ.
        assert_ne!(generate_pkce().verifier, generate_pkce().verifier);
    }

    #[test]
    fn origin_strips_path() {
        assert_eq!(origin("https://mcp.linear.app/sse"), "https://mcp.linear.app");
        assert_eq!(origin("https://h:8443/a/b?x=1"), "https://h:8443");
        assert_eq!(origin("https://h"), "https://h");
    }

    #[test]
    fn well_known_urls() {
        assert_eq!(
            well_known_protected_resource("https://mcp.linear.app/sse"),
            "https://mcp.linear.app/.well-known/oauth-protected-resource"
        );
        assert_eq!(
            well_known_authorization_server("https://auth.linear.app/path"),
            "https://auth.linear.app/.well-known/oauth-authorization-server"
        );
    }

    #[test]
    fn authorize_url_encodes_params() {
        let u = authorize_url(
            "https://auth.example/authorize",
            "client123",
            "http://127.0.0.1:51736/callback",
            "CHALLENGE",
            "STATE",
            "read write",
            "https://mcp.linear.app/sse",
        );
        assert!(u.starts_with("https://auth.example/authorize?response_type=code"));
        assert!(u.contains("client_id=client123"));
        assert!(u.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A51736%2Fcallback"));
        assert!(u.contains("code_challenge=CHALLENGE&code_challenge_method=S256"));
        assert!(u.contains("scope=read%20write"));
        assert!(u.contains("resource=https%3A%2F%2Fmcp.linear.app%2Fsse"));
        // Appends with & when the endpoint already has a query.
        assert!(authorize_url("https://a/x?foo=1", "c", "r", "ch", "s", "sc", "rs").contains("?foo=1&response_type=code"));
    }

    #[test]
    fn pct_decode_handles_escapes() {
        assert_eq!(pct_decode("a%2Fb%3Ac"), "a/b:c");
        assert_eq!(pct_decode("hello+world"), "hello world");
        assert_eq!(pct_decode("plain"), "plain");
    }

    #[test]
    fn token_and_metadata_parse() {
        let t: TokenResponse = serde_json::from_str(
            r#"{"access_token":"abc","refresh_token":"r","expires_in":3600,"token_type":"Bearer"}"#,
        )
        .unwrap();
        assert_eq!(t.access_token, "abc");
        assert_eq!(t.refresh_token.as_deref(), Some("r"));
        assert_eq!(t.expires_in, Some(3600));
        // Minimal token (no refresh/expiry) still parses.
        let t2: TokenResponse = serde_json::from_str(r#"{"access_token":"x"}"#).unwrap();
        assert!(t2.refresh_token.is_none());
        let m: AsMetadata = serde_json::from_str(
            r#"{"authorization_endpoint":"https://a/au","token_endpoint":"https://a/tok"}"#,
        )
        .unwrap();
        assert_eq!(m.token_endpoint, "https://a/tok");
        assert!(m.registration_endpoint.is_none());
    }
}
