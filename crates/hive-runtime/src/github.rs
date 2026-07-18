//! GitHub sign-in via the OAuth **device flow** — desktop-friendly (no client
//! secret, no redirect server). The user authorizes a short code at
//! github.com/login/device; we poll for a token, then fetch their profile.
//!
//! The GitHub user is the Hive *account*; [`account_id_for`] derives a stable
//! account id from the GitHub user id, so the same person signing in on multiple
//! devices resolves to one account (each device keeps its own keypairs).

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const USER_URL: &str = "https://api.github.com/user";
const EMAILS_URL: &str = "https://api.github.com/user/emails";
const SCOPE: &str = "read:user user:email";
const USER_AGENT: &str = "hive-app";

/// Namespace for deriving a Hive account id from a GitHub user id (UUID v5).
const ACCOUNT_NS: Uuid = Uuid::from_u128(0x6869_7665_6769_7468_6163_636f_756e_7401);

/// Stable Hive account id for a GitHub numeric user id. Deterministic, so every
/// device signed into the same GitHub account computes the same account id.
pub fn account_id_for(github_user_id: u64) -> Uuid {
    Uuid::new_v5(&ACCOUNT_NS, format!("github:{github_user_id}").as_bytes())
}

#[derive(Debug, Error)]
pub enum GithubError {
    #[error("http error: {0}")]
    Http(String),
    #[error("github error: {0}")]
    Api(String),
    #[error("no GitHub client id configured (set HIVE_GITHUB_CLIENT_ID or Settings)")]
    NoClientId,
}

/// The code + URL the user must visit to authorize this device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

/// A signed-in GitHub account (the Hive account identity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubAccount {
    pub id: u64,
    pub login: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

impl GithubAccount {
    pub fn account_id(&self) -> Uuid {
        account_id_for(self.id)
    }
    /// Best display name: GitHub name, else login.
    pub fn display_name(&self) -> String {
        self.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| self.login.clone())
    }
}

/// Outcome of one token poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    /// Keep polling at the given interval.
    Pending,
    /// Slow down — increase the interval.
    SlowDown,
    /// Authorized: here's the access token.
    Token(String),
    /// User denied, or the code expired — stop.
    Denied,
    Expired,
}

fn client() -> Result<reqwest::Client, GithubError> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| GithubError::Http(e.to_string()))
}

/// Step 1: request a device + user code.
pub async fn start_device_flow(client_id: &str) -> Result<DeviceStart, GithubError> {
    if client_id.trim().is_empty() {
        return Err(GithubError::NoClientId);
    }
    let resp = client()?
        .post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .form(&[("client_id", client_id), ("scope", SCOPE)])
        .send()
        .await
        .map_err(|e| GithubError::Http(e.to_string()))?;
    let v: serde_json::Value = resp.json().await.map_err(|e| GithubError::Http(e.to_string()))?;
    parse_device_start(&v)
}

fn parse_device_start(v: &serde_json::Value) -> Result<DeviceStart, GithubError> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let device_code = s("device_code");
    if device_code.is_empty() {
        return Err(GithubError::Api(
            v.get("error_description")
                .and_then(|x| x.as_str())
                .unwrap_or("malformed device-code response")
                .to_string(),
        ));
    }
    Ok(DeviceStart {
        device_code,
        user_code: s("user_code"),
        verification_uri: s("verification_uri"),
        interval: v.get("interval").and_then(|x| x.as_u64()).unwrap_or(5),
        expires_in: v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(900),
    })
}

/// Step 2: poll once for the access token.
pub async fn poll_token(client_id: &str, device_code: &str) -> Result<PollOutcome, GithubError> {
    let resp = client()?
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .form(&[
            ("client_id", client_id),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .send()
        .await
        .map_err(|e| GithubError::Http(e.to_string()))?;
    let v: serde_json::Value = resp.json().await.map_err(|e| GithubError::Http(e.to_string()))?;
    Ok(parse_poll(&v))
}

fn parse_poll(v: &serde_json::Value) -> PollOutcome {
    if let Some(tok) = v.get("access_token").and_then(|x| x.as_str()) {
        return PollOutcome::Token(tok.to_string());
    }
    match v.get("error").and_then(|x| x.as_str()).unwrap_or("") {
        "authorization_pending" => PollOutcome::Pending,
        "slow_down" => PollOutcome::SlowDown,
        "expired_token" => PollOutcome::Expired,
        "access_denied" => PollOutcome::Denied,
        _ => PollOutcome::Pending,
    }
}

/// Step 3: fetch the signed-in user's profile (fills in a primary verified email
/// if the public profile email is null).
pub async fn fetch_user(token: &str) -> Result<GithubAccount, GithubError> {
    let c = client()?;
    let v: serde_json::Value = c
        .get(USER_URL)
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| GithubError::Http(e.to_string()))?
        .json()
        .await
        .map_err(|e| GithubError::Http(e.to_string()))?;
    let mut account: GithubAccount =
        serde_json::from_value(v).map_err(|e| GithubError::Api(e.to_string()))?;
    if account.email.is_none() {
        account.email = primary_email(&c, token).await;
    }
    Ok(account)
}

async fn primary_email(c: &reqwest::Client, token: &str) -> Option<String> {
    let rows: Vec<serde_json::Value> = c
        .get(EMAILS_URL)
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    rows.iter()
        .find(|e| e.get("primary").and_then(|x| x.as_bool()).unwrap_or(false))
        .or_else(|| rows.first())
        .and_then(|e| e.get("email").and_then(|x| x.as_str()).map(str::to_string))
}

// ---------------------------------------------------------------------------
// Enterprise: GitHub org Teams → workspace roster + roles (#143)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct GithubTeam {
    pub slug: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubTeamMember {
    pub login: String,
    pub id: u64,
}

/// A resolved org member: their GitHub identity + the workspace role implied by
/// their team membership (highest across all their teams).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgMember {
    pub login: String,
    pub github_user_id: u64,
    pub role: hive_core::WorkspaceRole,
}

/// Map a team slug to a workspace role by naming convention. `Owner` is reserved
/// for the workspace creator, so org admin-ish teams map to `Admin`.
pub fn role_for_team_slug(slug: &str) -> hive_core::WorkspaceRole {
    use hive_core::WorkspaceRole::*;
    let s = slug.to_ascii_lowercase();
    if ["admin", "owner", "lead", "maintain"].iter().any(|k| s.contains(k)) {
        Admin
    } else if ["viewer", "read", "guest", "audit"].iter().any(|k| s.contains(k)) {
        Viewer
    } else {
        Contributor
    }
}

/// Fold per-team members into a deduped roster, keeping the highest role each
/// login earns across the teams they belong to. Pure — unit-tested.
pub fn merge_team_members(
    teams: &[(GithubTeam, Vec<GithubTeamMember>)],
) -> Vec<OrgMember> {
    use std::collections::HashMap;
    let mut by_login: HashMap<String, OrgMember> = HashMap::new();
    for (team, members) in teams {
        let role = role_for_team_slug(&team.slug);
        for m in members {
            let entry = by_login.entry(m.login.clone()).or_insert_with(|| OrgMember {
                login: m.login.clone(),
                github_user_id: m.id,
                role,
            });
            if role.rank() > entry.role.rank() {
                entry.role = role;
            }
        }
    }
    let mut out: Vec<OrgMember> = by_login.into_values().collect();
    out.sort_by(|a, b| a.login.cmp(&b.login));
    out
}

async fn gh_get_json(token: &str, url: &str) -> Result<serde_json::Value, GithubError> {
    let resp = client()?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| GithubError::Http(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(GithubError::Api(format!("GitHub returned {}", resp.status())));
    }
    resp.json().await.map_err(|e| GithubError::Http(e.to_string()))
}

/// Fetch an org's teams + each team's members, and resolve them into a roster
/// (highest role wins). Needs a token with `read:org`.
pub async fn resolve_org_roster(token: &str, org: &str) -> Result<Vec<OrgMember>, GithubError> {
    let org = org.trim();
    if org.is_empty() {
        return Err(GithubError::Api("organization is required".into()));
    }
    let teams_json = gh_get_json(token, &format!("https://api.github.com/orgs/{org}/teams?per_page=100")).await?;
    let teams: Vec<GithubTeam> = serde_json::from_value(teams_json).unwrap_or_default();
    let mut collected = Vec::new();
    for team in teams {
        let members_json = gh_get_json(
            token,
            &format!("https://api.github.com/orgs/{org}/teams/{}/members?per_page=100", team.slug),
        )
        .await?;
        let members: Vec<GithubTeamMember> = serde_json::from_value(members_json).unwrap_or_default();
        collected.push((team, members));
    }
    Ok(merge_team_members(&collected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_slug_maps_to_role() {
        use hive_core::WorkspaceRole::*;
        assert_eq!(role_for_team_slug("admins"), Admin);
        assert_eq!(role_for_team_slug("platform-leads"), Admin);
        assert_eq!(role_for_team_slug("maintainers"), Admin);
        assert_eq!(role_for_team_slug("read-only"), Viewer);
        assert_eq!(role_for_team_slug("auditors"), Viewer);
        assert_eq!(role_for_team_slug("engineering"), Contributor);
    }

    #[test]
    fn merge_keeps_highest_role_and_dedups() {
        let teams = vec![
            (
                GithubTeam { slug: "engineering".into(), name: "Engineering".into() },
                vec![
                    GithubTeamMember { login: "mona".into(), id: 1 },
                    GithubTeamMember { login: "nat".into(), id: 2 },
                ],
            ),
            (
                GithubTeam { slug: "admins".into(), name: "Admins".into() },
                vec![GithubTeamMember { login: "mona".into(), id: 1 }],
            ),
        ];
        let roster = merge_team_members(&teams);
        assert_eq!(roster.len(), 2, "mona deduped across two teams");
        let mona = roster.iter().find(|m| m.login == "mona").unwrap();
        assert_eq!(mona.role, hive_core::WorkspaceRole::Admin, "admin beats contributor");
        let nat = roster.iter().find(|m| m.login == "nat").unwrap();
        assert_eq!(nat.role, hive_core::WorkspaceRole::Contributor);
    }

    #[test]
    fn account_id_is_stable_per_github_user() {
        // Same GitHub id → same account id (so multiple devices converge).
        assert_eq!(account_id_for(1234), account_id_for(1234));
        assert_ne!(account_id_for(1234), account_id_for(5678));
    }

    #[test]
    fn parse_device_start_and_errors() {
        let ok = serde_json::json!({
            "device_code": "dc", "user_code": "WXYZ-1234",
            "verification_uri": "https://github.com/login/device",
            "interval": 5, "expires_in": 900
        });
        let s = parse_device_start(&ok).unwrap();
        assert_eq!(s.user_code, "WXYZ-1234");
        assert_eq!(s.interval, 5);
        assert!(parse_device_start(&serde_json::json!({"error":"x"})).is_err());
    }

    #[test]
    fn parse_poll_states() {
        assert_eq!(parse_poll(&serde_json::json!({"access_token":"t"})), PollOutcome::Token("t".into()));
        assert_eq!(parse_poll(&serde_json::json!({"error":"authorization_pending"})), PollOutcome::Pending);
        assert_eq!(parse_poll(&serde_json::json!({"error":"slow_down"})), PollOutcome::SlowDown);
        assert_eq!(parse_poll(&serde_json::json!({"error":"access_denied"})), PollOutcome::Denied);
        assert_eq!(parse_poll(&serde_json::json!({"error":"expired_token"})), PollOutcome::Expired);
    }

    #[test]
    fn account_display_name_and_id() {
        let a = GithubAccount { id: 42, login: "octocat".into(), name: Some("The Octocat".into()), email: None, avatar_url: None };
        assert_eq!(a.display_name(), "The Octocat");
        assert_eq!(a.account_id(), account_id_for(42));
        let b = GithubAccount { id: 7, login: "mona".into(), name: None, email: None, avatar_url: None };
        assert_eq!(b.display_name(), "mona");
    }
}
