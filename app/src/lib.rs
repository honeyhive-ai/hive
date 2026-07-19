//! Tauri v2 desktop shell. Owns the `hive-runtime` services, registers IPC
//! commands, and pushes streaming chat updates as Tauri events.
//!
//! Chat path (Phases 3–5 + follow-ups): create a chat, post a user message,
//! and stream a reply from the *responder's resolved runtime* (Anthropic /
//! OpenAI-compatible / subprocess). The system prompt is assembled from the
//! roster, history is windowed + summarized to the model budget, a single
//! `@agent` mention answers in that agent's identity, replies that mention
//! another agent cascade (bounded), and a reply that `@you`-mentions the human
//! fires a native notification.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hive_core::context_budget::{model_context_window, token_estimator};
use hive_core::{
    ActionProposal, ChatMessage, ChatSession, GitContextReader, MessageRole, ModelProviderKind,
    ProposalKind, ProposalStatus, RuntimeTarget, SkillProfile, WorkspaceAgent, WorkspaceRole,
};
use hive_core::{ActorIdentity, ActorKind, VaultSource, WorkspaceMember};
use hive_proto::{
    ApprovalDto, AppInfo, AppSettingsDto, ChatMessageDto, ChatSessionDto, ChatStreamEvent,
    ChatSummaryDto, ContextTelemetryDto, GitFileDiffDto, McpServerDto, ProposalDto,
    IssuedRelayTokenDto, ReactionDto, RelayTokenDto, RelayUserDto, RuntimeSummaryDto, SkillDto,
    VaultSourceDto, WorkspaceAgentDto, WorkspaceInfoDto, WorkspaceMemberDto,
};
use hive_runtime::provider::anthropic::AnthropicResponse;
use hive_runtime::tool_loop::{self, MessagesApi, ToolExecutor};
use hive_runtime::{
    chat_service::ChatService, dispatch, identity_store::FileKeyVault, mcp::McpTransport, mentions::parse_mentions,
    prompt, resolve_manifest_url, turns_from, vault_fetcher, AnthropicClient, ChatTurn, EventStore,
    IdentityStore, McpRegistry, McpServerSpec, ProviderError, ResolvedRuntime,
};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;
use uuid::Uuid;

mod workflows;

#[cfg(target_os = "macos")]
use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{TrayIcon, TrayIconBuilder},
};

const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
const OUTPUT_RESERVE_TOKENS: i64 = 4096;
const SUMMARY_RESERVE_TOKENS: i64 = 700;
/// Max chained agent turns from one user message (loop guard for cascades).
const MAX_CASCADE_DEPTH: usize = 4;

struct AppState {
    service: Mutex<ChatService>,
    identity: IdentityStore<FileKeyVault>,
    data_dir: PathBuf,
    base_runtimes: Mutex<Vec<RuntimeTarget>>,
    managed_runtimes: Mutex<Vec<RuntimeTarget>>,
    default_runtime_id: Mutex<String>,
    /// Model for the synthesized "Primary Runtime" (and the displayed default).
    /// Editable at runtime via `set_default_model`; persisted in settings.
    fallback_model: Mutex<String>,
    device_name: String,
    /// Stable id for the local "My workspace" (solo chats), persisted in settings.
    local_workspace_id: Uuid,
    /// The workspace whose chats are currently shown. Defaults to the local id
    /// each launch; switched to a room id when the user opens a joined room.
    active_workspace: Mutex<Uuid>,
    workspace_root: Mutex<String>,
    /// Per-session incremental summary cache: (covered overflow ids, summary).
    summary_cache: Mutex<HashMap<Uuid, (Vec<Uuid>, String)>>,
    /// Fetched vault content, keyed by raw URL — one fetch per app run so
    /// attaching a vault doesn't add a network round-trip to every message.
    vault_cache: Mutex<HashMap<String, String>>,
    /// MCP servers from `hive.config.toml`; shell-managed installs live in the
    /// workspace catalog and are merged at read time.
    base_mcp: Mutex<Vec<McpServerSpec>>,
    managed_mcp: Mutex<Vec<McpServerSpec>>,
    /// SQLite path — the background sync task opens its own connection here.
    db_path: PathBuf,
    /// Runtime-editable settings (relay, key, API key, permission mode),
    /// persisted to settings.json and shared with the background sync loop.
    settings: Arc<Mutex<LiveSettings>>,
    /// Sessions with an in-flight owner response — guards `maybe_respond`
    /// against double-dispatching the same incoming message.
    responding: Mutex<std::collections::HashSet<Uuid>>,
    /// Message ids we've already raised a local "you were mentioned"
    /// notification for, so syncs don't re-notify.
    notified: Mutex<std::collections::HashSet<Uuid>>,
    /// Live workflow-run drivers (run id → waker). A vote or cancel pokes the
    /// waker so a gate-suspended driver reacts instantly.
    run_wakers: Mutex<HashMap<Uuid, Arc<tokio::sync::Notify>>>,
    /// Gate proposal id → the run suspended on it (for the vote hook).
    gate_runs: Mutex<HashMap<Uuid, Uuid>>,
    /// Runs asked to cancel; their drivers check between transitions.
    canceled_runs: Mutex<std::collections::HashSet<Uuid>>,
}

/// Editable-at-runtime settings, persisted to `<data_dir>/settings.json`.
/// Env vars seed the file on first launch; after that the file wins and the
/// Settings UI mutates it live (the sync loop polls it).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct LiveSettings {
    #[serde(default)]
    relay_url: Option<String>,
    /// Access token for a gated/paid hosted relay (entitlement). Sent as
    /// `Authorization: Bearer` on every relay request. Self-hosted/open relays
    /// leave this blank.
    #[serde(default)]
    relay_access_token: Option<String>,
    #[serde(default = "default_sync_room")]
    sync_room: String,
    /// Passphrase → workspace key (E2EE on the wire). Stored so the UI can show
    /// "configured" and re-derive; never synced/committed.
    #[serde(default)]
    workspace_passphrase: Option<String>,
    /// Optional API key for an Anthropic/OpenAI runtime (claude-code needs none).
    #[serde(default)]
    api_key: Option<String>,
    /// Claude Code permission mode: "default" | "acceptEdits" | "bypassPermissions".
    #[serde(default = "default_permission_mode")]
    claude_permission_mode: String,
    /// Model alias for the local Claude Code CLI (`--model`): e.g. "sonnet",
    /// "opus", "haiku", or a full model id. Empty = the CLI's own default.
    #[serde(default)]
    claude_code_model: String,
    /// Custom instruction for `/summarize` (also used by the automatic
    /// overflow windowing). None/empty = the built-in default.
    #[serde(default)]
    summarize_prompt: Option<String>,
    /// Custom instruction for `/compact`. None/empty = the built-in default.
    #[serde(default)]
    compact_prompt: Option<String>,
    /// Model id for the synthesized "Primary Runtime" (the default when no
    /// runtime is configured). `None` = fall back to env/DEFAULT_MODEL.
    #[serde(default)]
    default_model: Option<String>,
    /// Runtime id new chats default to (set via the UI's "Set default"). `None`
    /// = fall back to the config `default_runtime`. May name a synthesized
    /// runtime (e.g. "claude-code") that isn't a config `[[runtimes]]` entry.
    #[serde(default)]
    default_runtime_id: Option<String>,
    /// Stable id for the local "My workspace" (solo/local-only chats). Generated
    /// once and persisted, so local chats keep a consistent home across launches.
    #[serde(default)]
    local_workspace_id: Option<Uuid>,
    /// Relay rooms this device has joined. Each maps (via [`room_workspace_id`])
    /// to a workspace; chats with those ids are excluded from "My workspace".
    #[serde(default)]
    joined_rooms: Vec<String>,
    /// Git email for commit attribution. Rides on this user's identity so a
    /// host running an agent on their behalf credits them as the commit author.
    #[serde(default)]
    git_email: Option<String>,
    /// Persisted 32-byte direct-P2P transport secret (hex). Generated once; its
    /// public key is this device's shareable peer/"friend" code.
    #[serde(default)]
    p2p_secret: Option<String>,
    /// Persisted 32-byte X25519 key-agreement seed (hex). Its public key is
    /// published in the roster so an owner can seal a rotated workspace key to
    /// this device when revoking a member. Distinct from `p2p_secret` (iroh).
    #[serde(default)]
    ka_secret: Option<String>,
    /// Signed-in GitHub account (the Hive account identity). Same account across
    /// this user's devices; each device keeps its own keypairs.
    #[serde(default)]
    github_account: Option<hive_runtime::github::GithubAccount>,
    /// GitHub access token (device-flow), for authenticated directory calls.
    #[serde(default)]
    github_token: Option<String>,
    /// OAuth App client id for the device flow (env HIVE_GITHUB_CLIENT_ID wins).
    #[serde(default)]
    github_client_id: Option<String>,
    /// Per-provider API keys, keyed by provider name (see `provider_config_name`):
    /// "anthropic", "openAI", "openRouter", "custom". Lets multiple providers
    /// have distinct keys (vs the single legacy `api_key`).
    #[serde(default)]
    provider_keys: std::collections::HashMap<String, String>,
    /// Per-provider base URLs (OpenAI-compatible/Ollama/custom), keyed the same.
    #[serde(default)]
    provider_base_urls: std::collections::HashMap<String, String>,
    /// Team workspaces this device has created or joined (each a relay room +
    /// optional E2EE key). Shown in the rail; switching to one points the live
    /// relay/room/key at it. The single legacy relay config is migrated into
    /// this list on load.
    #[serde(default)]
    workspaces: Vec<WorkspaceConn>,
    /// Reusable agent definitions (a persona = name + model/runtime + role +
    /// instructions) the user can attach to any chat.
    #[serde(default)]
    agent_templates: Vec<AgentTemplate>,
    /// Optional icon for the local "My workspace", as a `data:` URL. Team
    /// workspace icons live on their `WorkspaceConn` instead.
    #[serde(default)]
    local_workspace_icon: Option<String>,
    /// Scheduled/triggered agents: each fires a prompt on a recurring schedule
    /// (interval or daily time) into a fresh chat. Polled by the scheduler loop.
    #[serde(default)]
    schedules: Vec<ScheduledAgentConfig>,
    /// OAuth tokens for remote (authenticated) MCP servers, keyed by server id.
    /// Injected as the bearer when assembling the registry; refreshed on expiry.
    #[serde(default)]
    mcp_oauth: std::collections::HashMap<String, McpOAuthEntry>,
}

/// Persisted OAuth state for one remote MCP server (see [`mcp_oauth`] +
/// task #147). `client_id` + `token_endpoint` are kept so the token can be
/// refreshed without another browser round-trip.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpOAuthEntry {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    /// Absolute expiry as a Unix timestamp (seconds); `None` = unknown/no expiry.
    #[serde(default)]
    expires_at: Option<i64>,
    #[serde(default)]
    client_id: Option<String>,
    /// Client secret for confidential providers (e.g. Linear). Stored locally in
    /// settings.json (the user's own app credential); never synced/committed.
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    token_endpoint: Option<String>,
}

/// A scheduled agent: fire `prompt` at `trigger`'s cadence into a new chat in
/// `workspace_id` (or the local workspace), answered by `runtime_id` (or the
/// workspace default). Persisted in settings.json; managed by the scheduler
/// loop, which stamps `last_run` after each fire.
///
/// `DailyAt` times are interpreted in **UTC** for now (interval triggers are
/// timezone-independent); local-time dailies are a follow-up.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScheduledAgentConfig {
    id: String,
    #[serde(default = "default_true")]
    enabled: bool,
    /// Human label; also titles the chats it spawns.
    #[serde(default)]
    label: String,
    /// Workspace to post into; `None` = the local "My workspace".
    #[serde(default)]
    workspace_id: Option<Uuid>,
    /// Runtime that answers; empty = the workspace's default runtime.
    #[serde(default)]
    runtime_id: String,
    prompt: String,
    trigger: hive_core::ScheduleTrigger,
    /// When it last fired; `None` = never. Stamped by the scheduler.
    #[serde(default)]
    last_run: Option<hive_core::Timestamp>,
}

fn default_true() -> bool {
    true
}

/// A reusable agent definition. Attaching to a chat creates a WorkspaceAgent
/// from (name, runtime, role).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentTemplate {
    id: String,
    name: String,
    runtime_id: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    instructions: String,
}

fn default_sync_room() -> String {
    "default".to_string()
}
fn default_permission_mode() -> String {
    "default".to_string()
}

impl Default for LiveSettings {
    fn default() -> Self {
        Self {
            relay_url: None,
            relay_access_token: None,
            sync_room: default_sync_room(),
            workspace_passphrase: None,
            api_key: None,
            claude_permission_mode: default_permission_mode(),
            claude_code_model: String::new(),
            summarize_prompt: None,
            compact_prompt: None,
            default_model: None,
            default_runtime_id: None,
            local_workspace_id: None,
            joined_rooms: Vec::new(),
            git_email: None,
            p2p_secret: None,
            ka_secret: None,
            github_account: None,
            github_token: None,
            github_client_id: None,
            provider_keys: std::collections::HashMap::new(),
            provider_base_urls: std::collections::HashMap::new(),
            workspaces: Vec::new(),
            agent_templates: Vec::new(),
            local_workspace_icon: None,
            schedules: Vec::new(),
            mcp_oauth: std::collections::HashMap::new(),
        }
    }
}

/// Deterministic workspace id for a relay room — UUID v5 over the room name, so
/// every peer in the same room computes the same id and their chats group
/// together. Used to scope (and exclude from "My workspace") room chats.
fn room_workspace_id(room: &str) -> Uuid {
    // A fixed namespace so the mapping is stable across builds/devices.
    const NS: Uuid = Uuid::from_u128(0x6869_7665_726f_6f6d_776f_726b_7370_6163);
    Uuid::new_v5(&NS, room.trim().as_bytes())
}

/// A joined/created team workspace: a relay room plus the (optional) E2EE key
/// to read it. Stored as a list so the rail can show several at once; switching
/// to one points the live sync fields (relay/room/key) at it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct WorkspaceConn {
    /// Friendly display name (defaults to the room name).
    #[serde(default)]
    name: String,
    relay_url: String,
    room: String,
    /// E2EE passphrase (derives the workspace key); None = plaintext room.
    #[serde(default)]
    key: Option<String>,
    /// Optional workspace icon as a `data:` URL, set by an owner/admin.
    #[serde(default)]
    icon: Option<String>,
    /// When set, this workspace is a 1:1 direct message with that friend's
    /// account key (`github:<id>`). DMs are kept out of the workspace rail and
    /// surfaced in the Friends section instead.
    #[serde(default)]
    dm_account: Option<String>,
}

impl WorkspaceConn {
    fn id(&self) -> Uuid {
        room_workspace_id(&self.room)
    }
    fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            self.room.clone()
        } else {
            self.name.clone()
        }
    }
}

/// Shareable invite encoding for a workspace: `hivews1:<base64url(json)>`. It
/// bundles everything a peer needs to join — relay, room, and the E2EE key —
/// so joining is a single paste.
fn encode_workspace_invite(conn: &WorkspaceConn) -> String {
    use base64::Engine;
    // Icons are local rail decoration; never embed a (potentially large) data
    // URL in an invite code.
    let conn = WorkspaceConn { icon: None, ..conn.clone() };
    let json = serde_json::to_vec(&conn).unwrap_or_default();
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json);
    format!("hivews1:{b64}")
}

fn decode_workspace_invite(invite: &str) -> Result<WorkspaceConn, String> {
    use base64::Engine;
    let body = invite
        .trim()
        .strip_prefix("hivews1:")
        .ok_or_else(|| "not a Hive workspace invite (expected hivews1:…)".to_string())?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body.trim().as_bytes())
        .map_err(|e| format!("invalid invite encoding: {e}"))?;
    let mut conn: WorkspaceConn =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid invite payload: {e}"))?;
    if conn.room.trim().is_empty() {
        return Err("invite is missing a room".to_string());
    }
    conn.name = conn.display_name();
    Ok(conn)
}

#[cfg(test)]
mod workspace_invite_tests {
    use super::*;

    #[test]
    fn invite_round_trips() {
        let conn = WorkspaceConn {
            name: "Team Rocket".into(),
            relay_url: "wss://relay.example/v1".into(),
            room: "team-rocket-9f3a2b".into(),
            key: Some("hunter2".into()),
            icon: None,
            dm_account: None,
        };
        let invite = encode_workspace_invite(&conn);
        assert!(invite.starts_with("hivews1:"));
        assert_eq!(decode_workspace_invite(&invite).unwrap(), conn);
    }

    #[test]
    fn invite_tolerates_whitespace_and_no_key() {
        let conn = WorkspaceConn {
            name: String::new(),
            relay_url: "wss://r/v1".into(),
            room: "plain-room".into(),
            key: None,
            icon: None,
            dm_account: None,
        };
        let invite = format!("  {}\n", encode_workspace_invite(&conn));
        let back = decode_workspace_invite(&invite).unwrap();
        assert_eq!(back.room, "plain-room");
        assert_eq!(back.key, None);
        // empty name normalizes to the room name
        assert_eq!(back.name, "plain-room");
    }

    #[test]
    fn bad_invites_rejected() {
        assert!(decode_workspace_invite("nope").is_err());
        assert!(decode_workspace_invite("hivews1:!!!notbase64").is_err());
    }

    #[test]
    fn same_room_shares_workspace_id() {
        let a = WorkspaceConn { name: "A".into(), relay_url: "x".into(), room: "shared".into(), key: None, icon: None, dm_account: None };
        let b = WorkspaceConn { name: "B".into(), relay_url: "y".into(), room: "shared".into(), key: Some("k".into()), icon: None, dm_account: None };
        assert_eq!(a.id(), b.id());
    }
}

impl LiveSettings {
    /// Derived E2EE key, if a passphrase is set.
    fn workspace_key(&self) -> Option<[u8; 32]> {
        self.workspace_passphrase
            .as_ref()
            .filter(|p| !p.is_empty())
            .map(|p| hive_core::derive_workspace_key(p))
    }
    /// Claude Code extra args for the configured permission mode (empty for
    /// "default" — read-only).
    fn claude_args(&self) -> Vec<String> {
        let mut args = match self.claude_permission_mode.as_str() {
            "acceptEdits" => vec!["--permission-mode".into(), "acceptEdits".into()],
            "bypassPermissions" => vec!["--permission-mode".into(), "bypassPermissions".into()],
            _ => Vec::new(),
        };
        // User-selected Claude Code model (`--model`); empty = the CLI default.
        let m = self.claude_code_model.trim();
        if !m.is_empty() {
            args.push("--model".into());
            args.push(m.to_string());
        }
        args
    }
}

fn settings_file(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("settings.json")
}

fn save_settings(data_dir: &std::path::Path, settings: &LiveSettings) {
    if let Ok(bytes) = serde_json::to_vec_pretty(settings) {
        let _ = std::fs::write(settings_file(data_dir), bytes);
    }
}

/// Load persisted settings, or seed the file from `env_seed` on first launch.
fn load_or_seed_settings(data_dir: &std::path::Path, env_seed: LiveSettings) -> LiveSettings {
    match std::fs::read_to_string(settings_file(data_dir)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or(env_seed),
        Err(_) => {
            save_settings(data_dir, &env_seed);
            env_seed
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ManagedRuntimeCatalog {
    #[serde(default)]
    runtimes: Vec<RuntimeTarget>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ManagedMcpCatalog {
    #[serde(default)]
    servers: Vec<McpServerSpec>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct WorkspaceIndex {
    #[serde(default)]
    roots: Vec<String>,
}

fn merge_runtimes(base: &[RuntimeTarget], managed: &[RuntimeTarget]) -> Vec<RuntimeTarget> {
    let mut merged = base.to_vec();
    for runtime in managed {
        if let Some(existing) = merged.iter_mut().find(|candidate| candidate.id == runtime.id) {
            *existing = runtime.clone();
        } else {
            merged.push(runtime.clone());
        }
    }
    merged
}

fn merge_mcp_servers(base: &[McpServerSpec], managed: &[McpServerSpec]) -> Vec<McpServerSpec> {
    let mut merged = base.to_vec();
    for server in managed {
        if let Some(existing) = merged.iter_mut().find(|candidate| candidate.id == server.id) {
            *existing = server.clone();
        } else {
            merged.push(server.clone());
        }
    }
    merged
}

fn ensure_workspace_hive_dir(workspace_root: &str) -> Result<PathBuf, String> {
    let dir = PathBuf::from(workspace_root).join(".hive");
    std::fs::create_dir_all(&dir).map_err(map_err)?;
    Ok(dir)
}

fn workspace_config_path(workspace_root: &str) -> PathBuf {
    PathBuf::from(workspace_root).join("hive.config.toml")
}

fn workspace_index_path(data_dir: &PathBuf) -> PathBuf {
    data_dir.join("recent-workspaces.json")
}

fn load_workspace_index(data_dir: &PathBuf) -> Vec<String> {
    let path = workspace_index_path(data_dir);
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<WorkspaceIndex>(&text).ok())
        .map(|index| index.roots)
        .unwrap_or_default()
}

fn save_workspace_index(data_dir: &PathBuf, roots: &[String]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&WorkspaceIndex {
        roots: roots.to_vec(),
    })
    .map_err(map_err)?;
    std::fs::write(workspace_index_path(data_dir), text).map_err(map_err)
}

fn normalize_workspace_path(path: &str) -> Result<String, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("workspace path is empty".to_string());
    }
    let candidate = PathBuf::from(trimmed);
    if candidate.exists() {
        // dunce::canonicalize avoids the Windows `\\?\` verbatim prefix for
        // normal drive paths, so the stored/displayed root reads `C:\…`.
        return dunce::canonicalize(&candidate)
            .map(|resolved| resolved.to_string_lossy().to_string())
            .map_err(map_err);
    }
    Ok(trimmed.to_string())
}

fn resolve_workspace_root(path: &str) -> Result<String, String> {
    let normalized = normalize_workspace_path(path)?;
    let candidate = PathBuf::from(&normalized);
    if !candidate.is_dir() {
        return Err(format!("workspace directory does not exist: {normalized}"));
    }
    Ok(normalized)
}

/// Max size of a file referenced into a chat via `@file` (keeps context sane).
const MAX_REF_FILE_BYTES: u64 = 256 * 1024;

/// True if `target` is the workspace root or lives under it (post-canonicalize,
/// so `../` escapes are rejected).
fn is_within(root: &std::path::Path, target: &std::path::Path) -> bool {
    target == root || target.starts_with(root)
}

/// Read a workspace file (relative to the workspace root, or absolute but under
/// it) for inlining into a chat as an `@file` reference. Path-traversal-safe and
/// size-capped.
#[tauri::command]
fn read_workspace_file(state: State<AppState>, path: String) -> Result<String, String> {
    let root = state.workspace_root.lock().unwrap().clone();
    if root.trim().is_empty() {
        return Err("Set a workspace root first (Settings → Workspace).".to_string());
    }
    let root_path = dunce::canonicalize(&root)
        .map_err(|_| "workspace root is unavailable".to_string())?;
    let req = std::path::Path::new(path.trim());
    let joined = if req.is_absolute() { req.to_path_buf() } else { root_path.join(req) };
    let target =
        dunce::canonicalize(&joined).map_err(|_| format!("file not found: {}", path.trim()))?;
    if !is_within(&root_path, &target) {
        return Err("that file is outside the workspace".to_string());
    }
    let len = std::fs::metadata(&target).map_err(map_err)?.len();
    if len > MAX_REF_FILE_BYTES {
        return Err(format!(
            "file too large ({} KB; max {} KB)",
            len / 1024,
            MAX_REF_FILE_BYTES / 1024
        ));
    }
    std::fs::read_to_string(&target).map_err(|_| "file isn't valid UTF-8 text".to_string())
}

fn workspace_paths_match(candidate: &str, target: &str) -> bool {
    candidate == target
        || normalize_workspace_path(candidate)
            .map(|normalized| normalized == target)
            .unwrap_or(false)
}

fn remember_workspace(data_dir: &PathBuf, workspace_root: &str) -> Result<Vec<String>, String> {
    if workspace_root.trim().is_empty() {
        return Ok(load_workspace_index(data_dir));
    }
    let mut roots = load_workspace_index(data_dir);
    roots.retain(|root| !workspace_paths_match(root, workspace_root));
    roots.insert(0, workspace_root.to_string());
    roots.truncate(12);
    save_workspace_index(data_dir, &roots)?;
    Ok(roots)
}

fn forget_workspace(data_dir: &PathBuf, workspace_root: &str) -> Result<Vec<String>, String> {
    let mut roots = load_workspace_index(data_dir);
    roots.retain(|root| !workspace_paths_match(root, workspace_root));
    save_workspace_index(data_dir, &roots)?;
    Ok(roots)
}

fn load_config_document_for_workspace(
    data_dir: &PathBuf,
    workspace_root: &str,
) -> Result<toml::Value, String> {
    let workspace_path = workspace_config_path(workspace_root);
    let source_path = if workspace_path.exists() {
        workspace_path
    } else {
        let fallback = data_dir.join("hive.config.toml");
        if fallback.exists() {
            fallback
        } else {
            return Ok(toml::Value::Table(Default::default()));
        }
    };
    let text = std::fs::read_to_string(source_path).map_err(map_err)?;
    toml::from_str(&text).map_err(map_err)
}

fn save_workspace_config_document(workspace_root: &str, doc: &toml::Value) -> Result<(), String> {
    if workspace_root.trim().is_empty() {
        return Err("workspace root is empty".to_string());
    }
    let text = toml::to_string_pretty(doc).map_err(map_err)?;
    std::fs::write(workspace_config_path(workspace_root), text).map_err(map_err)
}

fn managed_runtime_catalog_path(workspace_root: &str) -> Result<PathBuf, String> {
    Ok(ensure_workspace_hive_dir(workspace_root)?.join("runtimes.json"))
}

fn managed_mcp_catalog_path(workspace_root: &str) -> Result<PathBuf, String> {
    Ok(ensure_workspace_hive_dir(workspace_root)?.join("mcp-servers.json"))
}

fn load_managed_runtimes(workspace_root: &str) -> Result<Vec<RuntimeTarget>, String> {
    let path = managed_runtime_catalog_path(workspace_root)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).map_err(map_err)?;
    let catalog: ManagedRuntimeCatalog = serde_json::from_str(&text).map_err(map_err)?;
    Ok(catalog.runtimes)
}

fn save_managed_runtimes(workspace_root: &str, runtimes: &[RuntimeTarget]) -> Result<(), String> {
    let path = managed_runtime_catalog_path(workspace_root)?;
    let catalog = ManagedRuntimeCatalog {
        runtimes: runtimes.to_vec(),
    };
    let text = serde_json::to_string_pretty(&catalog).map_err(map_err)?;
    std::fs::write(path, text).map_err(map_err)
}

fn load_managed_mcp(workspace_root: &str) -> Result<Vec<McpServerSpec>, String> {
    let path = managed_mcp_catalog_path(workspace_root)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).map_err(map_err)?;
    let catalog: ManagedMcpCatalog = serde_json::from_str(&text).map_err(map_err)?;
    Ok(catalog.servers)
}

fn save_managed_mcp(workspace_root: &str, servers: &[McpServerSpec]) -> Result<(), String> {
    let path = managed_mcp_catalog_path(workspace_root)?;
    let catalog = ManagedMcpCatalog {
        servers: servers.to_vec(),
    };
    let text = serde_json::to_string_pretty(&catalog).map_err(map_err)?;
    std::fs::write(path, text).map_err(map_err)
}

fn load_workspace_catalogs(
    workspace_root: &str,
    data_dir: &PathBuf,
) -> (Vec<RuntimeTarget>, String, Vec<McpServerSpec>, Vec<RuntimeTarget>, Vec<McpServerSpec>) {
    let mut base_runtimes = Vec::new();
    let mut default_runtime_id = "anthropic".to_string();
    let mut base_mcp = Vec::new();
    for candidate in [
        PathBuf::from(workspace_root).join("hive.config.toml"),
        data_dir.join("hive.config.toml"),
    ] {
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            if let Ok(cfg) = hive_core::config::load_from_str(&text) {
                base_runtimes = cfg.runtimes;
                if !cfg.app.default_runtime.is_empty() {
                    default_runtime_id = cfg.app.default_runtime;
                }
                base_mcp = cfg
                    .mcp_servers
                    .into_iter()
                    .map(|m| McpServerSpec {
                        id: m.id,
                        transport: match m.transport {
                            hive_core::config::McpServerTransportKind::Http => McpTransport::Http {
                                url: m.url.unwrap_or_default(),
                            },
                            hive_core::config::McpServerTransportKind::Stdio => {
                                McpTransport::Stdio {
                                    command: m.command.unwrap_or_default(),
                                    args: m.args,
                                }
                            }
                        },
                        enabled: m.enabled,
                        auth: None,
                    })
                    .collect();
                break;
            }
        }
    }
    let managed_runtimes = load_managed_runtimes(workspace_root).unwrap_or_default();
    let managed_mcp = load_managed_mcp(workspace_root).unwrap_or_default();
    (
        base_runtimes,
        default_runtime_id,
        base_mcp,
        managed_runtimes,
        managed_mcp,
    )
}

fn map_err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn rfc3339(ts: hive_core::Timestamp) -> String {
    serde_json::to_value(ts)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn role_str(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Agent => "agent",
    }
}

fn git_kind_str(kind: hive_core::git_context::GitChangeKind) -> &'static str {
    use hive_core::git_context::GitChangeKind::*;
    match kind {
        Modified => "modified",
        Added => "added",
        Deleted => "deleted",
        Renamed => "renamed",
        Untracked => "untracked",
        Conflicted => "conflicted",
    }
}

fn role_name(role: WorkspaceRole) -> &'static str {
    match role {
        WorkspaceRole::Owner => "owner",
        WorkspaceRole::Admin => "admin",
        WorkspaceRole::Contributor => "contributor",
        WorkspaceRole::Viewer => "viewer",
    }
}

fn message_dto(m: &ChatMessage) -> ChatMessageDto {
    ChatMessageDto {
        id: m.id.to_string(),
        role: role_str(m.role).to_string(),
        author: m.author.clone(),
        body: m.body.clone(),
        is_streaming: m.is_streaming,
        created_at: rfc3339(m.created_at),
        reactions: m
            .reactions
            .iter()
            .map(|r| ReactionDto {
                emoji: r.emoji.clone(),
                actor_id: r.actor_id.clone(),
                actor_display_name: r.actor_display_name.clone(),
            })
            .collect(),
        tool_calls: m
            .tool_calls
            .iter()
            .map(|c| hive_proto::ToolCallDto {
                id: c.id.clone(),
                name: c.name.clone(),
                input_json: c.input_json.clone(),
                server_id: c.server_id.clone(),
            })
            .collect(),
        tool_results: m
            .tool_results
            .iter()
            .map(|r| hive_proto::ToolResultDto {
                call_id: r.call_id.clone(),
                content: r.content.clone(),
                is_error: r.is_error,
            })
            .collect(),
    }
}

fn proposal_dto(p: &ActionProposal) -> ProposalDto {
    let kind = match p.kind {
        ProposalKind::FileDiff => "fileDiff",
        ProposalKind::Command => "command",
        ProposalKind::Decision => "decision",
    };
    let status = match p.status {
        ProposalStatus::Open => "open",
        ProposalStatus::Approved => "approved",
        ProposalStatus::Rejected => "rejected",
        ProposalStatus::Applied => "applied",
    };
    ProposalDto {
        id: p.id.to_string(),
        title: p.title.clone(),
        body: p.body.clone(),
        kind: kind.to_string(),
        status: status.to_string(),
        required_approvals: p.required_approvals,
        qualifying_approvals: p.qualifying_approvals() as u32,
        quorum_met: p.is_quorum_met(),
        approvals: p
            .approvals
            .iter()
            .map(|a| ApprovalDto {
                actor_id: a.actor_id.clone(),
                role: role_name(a.role).to_string(),
                approved: a.approved,
            })
            .collect(),
    }
}

fn session_dto(s: &ChatSession) -> ChatSessionDto {
    ChatSessionDto {
        id: s.id.to_string(),
        title: s.title.clone(),
        runtime_id: s.runtime_id.clone(),
        messages: s.messages.iter().map(message_dto).collect(),
    }
}

fn agent_dto(a: &WorkspaceAgent) -> WorkspaceAgentDto {
    WorkspaceAgentDto {
        id: a.id.to_string(),
        name: a.name.clone(),
        runtime_id: a.runtime_id.clone(),
        role: a.role.clone(),
    }
}

fn provider_name(kind: ModelProviderKind) -> &'static str {
    match kind {
        ModelProviderKind::Anthropic => "anthropic",
        ModelProviderKind::OpenAI => "openAI",
        ModelProviderKind::OpenRouter => "openRouter",
        ModelProviderKind::Ollama => "ollama",
        ModelProviderKind::Azure => "azure",
        ModelProviderKind::Custom => "custom",
        ModelProviderKind::HiveDaemon => "hive-daemon",
        ModelProviderKind::Aider => "aider",
        ModelProviderKind::Pi => "pi",
        ModelProviderKind::ClaudeCode => "claude-code",
    }
}

fn parse_provider_kind(input: &str) -> Result<ModelProviderKind, String> {
    match input.to_lowercase().replace(['_', '-'], "").as_str() {
        "anthropic" => Ok(ModelProviderKind::Anthropic),
        "openai" => Ok(ModelProviderKind::OpenAI),
        "openrouter" => Ok(ModelProviderKind::OpenRouter),
        "ollama" => Ok(ModelProviderKind::Ollama),
        "azure" | "azureopenai" => Ok(ModelProviderKind::Azure),
        "custom" => Ok(ModelProviderKind::Custom),
        "hivedaemon" => Ok(ModelProviderKind::HiveDaemon),
        "aider" => Ok(ModelProviderKind::Aider),
        "pi" => Ok(ModelProviderKind::Pi),
        "claudecode" => Ok(ModelProviderKind::ClaudeCode),
        other => Err(format!("unknown provider {other}")),
    }
}

fn parse_runtime_location(input: &str) -> Result<hive_core::RuntimeLocation, String> {
    match input.to_lowercase().as_str() {
        "local" => Ok(hive_core::RuntimeLocation::Local),
        "remote" => Ok(hive_core::RuntimeLocation::Remote),
        other => Err(format!("unknown runtime location {other}")),
    }
}

fn slugify_runtime_id(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "runtime".to_string()
    } else {
        trimmed
    }
}

fn provider_config_name(kind: ModelProviderKind) -> &'static str {
    match kind {
        ModelProviderKind::Anthropic => "anthropic",
        ModelProviderKind::OpenAI => "openAI",
        ModelProviderKind::OpenRouter => "openRouter",
        ModelProviderKind::Ollama => "ollama",
        ModelProviderKind::Azure => "azure",
        ModelProviderKind::Custom => "custom",
        ModelProviderKind::HiveDaemon => "hive-daemon",
        ModelProviderKind::Aider => "aider",
        ModelProviderKind::Pi => "pi",
        ModelProviderKind::ClaudeCode => "claude-code",
    }
}

fn location_config_name(location: hive_core::RuntimeLocation) -> &'static str {
    match location {
        hive_core::RuntimeLocation::Local => "local",
        hive_core::RuntimeLocation::Remote => "remote",
    }
}

fn table_mut<'a>(parent: &'a mut toml::value::Table, key: &str) -> Result<&'a mut toml::value::Table, String> {
    let entry = parent
        .entry(key.to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    entry
        .as_table_mut()
        .ok_or_else(|| format!("{key} must be a TOML table"))
}

fn array_mut<'a>(parent: &'a mut toml::value::Table, key: &str) -> Result<&'a mut Vec<toml::Value>, String> {
    let entry = parent
        .entry(key.to_string())
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    entry
        .as_array_mut()
        .ok_or_else(|| format!("{key} must be a TOML array"))
}

fn runtime_to_toml(runtime: &RuntimeTarget) -> toml::Value {
    let mut table = toml::value::Table::new();
    table.insert("id".into(), toml::Value::String(runtime.id.clone()));
    if !runtime.name.is_empty() {
        table.insert("name".into(), toml::Value::String(runtime.name.clone()));
    }
    table.insert(
        "provider".into(),
        toml::Value::String(provider_config_name(runtime.provider_kind).to_string()),
    );
    table.insert(
        "kind".into(),
        toml::Value::String(location_config_name(runtime.location).to_string()),
    );
    if !runtime.endpoint.is_empty() {
        table.insert("endpoint".into(), toml::Value::String(runtime.endpoint.clone()));
    }
    if let Some(metrics) = &runtime.metrics_endpoint {
        if !metrics.is_empty() {
            table.insert("metrics_endpoint".into(), toml::Value::String(metrics.clone()));
        }
    }
    if !runtime.available_models.is_empty() {
        table.insert(
            "models".into(),
            toml::Value::Array(
                runtime
                    .available_models
                    .iter()
                    .cloned()
                    .map(toml::Value::String)
                    .collect(),
            ),
        );
    } else if !runtime.model_id.is_empty() {
        table.insert(
            "models".into(),
            toml::Value::Array(vec![toml::Value::String(runtime.model_id.clone())]),
        );
    }
    if !runtime.model_id.is_empty() {
        table.insert(
            "preferred_model".into(),
            toml::Value::String(runtime.model_id.clone()),
        );
    }
    if let Some(value) = &runtime.model_provider_id {
        table.insert("model_provider_id".into(), toml::Value::String(value.clone()));
    }
    if let Some(value) = &runtime.model_base_url {
        table.insert("model_base_url".into(), toml::Value::String(value.clone()));
    }
    if let Some(value) = &runtime.request_keep_alive {
        table.insert("keep_alive".into(), toml::Value::String(value.clone()));
    }
    if runtime.capabilities.supports_embeddings {
        table.insert("supports_embeddings".into(), toml::Value::Boolean(true));
    }
    if runtime.capabilities.supports_tools {
        table.insert("supports_tools".into(), toml::Value::Boolean(true));
    }
    if let Some(window) = runtime.capabilities.context_window_tokens {
        table.insert("context_window".into(), toml::Value::Integer(window.into()));
    }
    if runtime.estimated_performance_score != 0.0 {
        table.insert(
            "performance_score".into(),
            toml::Value::Float(runtime.estimated_performance_score),
        );
    }
    if runtime.estimated_cost_per_1m_input_tokens_usd != 0.0 {
        table.insert(
            "cost_per_1m_input_tokens_usd".into(),
            toml::Value::Float(runtime.estimated_cost_per_1m_input_tokens_usd),
        );
    }
    toml::Value::Table(table)
}

fn upsert_runtime_in_config(
    data_dir: &PathBuf,
    workspace_root: &str,
    runtime: &RuntimeTarget,
) -> Result<(), String> {
    let mut doc = load_config_document_for_workspace(data_dir, workspace_root)?;
    let root = doc
        .as_table_mut()
        .ok_or_else(|| "root hive.config.toml must be a TOML table".to_string())?;
    let should_set_default = {
        let app = table_mut(root, "app")?;
        app.get("default_runtime")
            .and_then(toml::Value::as_str)
            .unwrap_or_default()
            .is_empty()
    };
    {
        let runtimes = array_mut(root, "runtimes")?;
        let replacement = runtime_to_toml(runtime);
        if let Some(existing) = runtimes.iter_mut().find(|entry| {
            entry
                .get("id")
                .and_then(toml::Value::as_str)
                .map(|value| value == runtime.id)
                .unwrap_or(false)
        }) {
            *existing = replacement;
        } else {
            runtimes.push(replacement);
        }
    }
    if should_set_default {
        table_mut(root, "app")?.insert(
            "default_runtime".into(),
            toml::Value::String(runtime.id.clone()),
        );
    }
    save_workspace_config_document(workspace_root, &doc)
}

fn remove_runtime_from_config(
    data_dir: &PathBuf,
    workspace_root: &str,
    runtime_id: &str,
) -> Result<bool, String> {
    let mut doc = load_config_document_for_workspace(data_dir, workspace_root)?;
    let root = doc
        .as_table_mut()
        .ok_or_else(|| "root hive.config.toml must be a TOML table".to_string())?;
    let next_default = {
        let Some(runtimes) = root.get_mut("runtimes").and_then(toml::Value::as_array_mut) else {
            return Ok(false);
        };
        let before = runtimes.len();
        runtimes.retain(|entry| {
            entry
                .get("id")
                .and_then(toml::Value::as_str)
                .map(|value| value != runtime_id)
                .unwrap_or(true)
        });
        if before == runtimes.len() {
            return Ok(false);
        }
        runtimes
            .iter()
            .find_map(|entry| entry.get("id").and_then(toml::Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| "anthropic".to_string())
    };
    if let Some(app) = root.get_mut("app").and_then(toml::Value::as_table_mut) {
        if app
            .get("default_runtime")
            .and_then(toml::Value::as_str)
            .map(|value| value == runtime_id)
            .unwrap_or(false)
        {
            app.insert(
                "default_runtime".into(),
                toml::Value::String(next_default),
            );
        }
    }
    save_workspace_config_document(workspace_root, &doc)?;
    Ok(true)
}

fn mcp_spec_to_toml(server: &McpServerSpec) -> toml::Value {
    let mut table = toml::value::Table::new();
    table.insert("id".into(), toml::Value::String(server.id.clone()));
    match &server.transport {
        McpTransport::Http { url } => {
            table.insert("transport".into(), toml::Value::String("http".into()));
            table.insert("url".into(), toml::Value::String(url.clone()));
        }
        McpTransport::Stdio { command, args } => {
            table.insert("transport".into(), toml::Value::String("stdio".into()));
            table.insert("command".into(), toml::Value::String(command.clone()));
            if !args.is_empty() {
                table.insert(
                    "args".into(),
                    toml::Value::Array(args.iter().cloned().map(toml::Value::String).collect()),
                );
            }
        }
    }
    if server.enabled {
        table.insert("enabled".into(), toml::Value::Boolean(true));
    }
    toml::Value::Table(table)
}

fn upsert_mcp_server_in_config(
    data_dir: &PathBuf,
    workspace_root: &str,
    server: &McpServerSpec,
) -> Result<(), String> {
    let mut doc = load_config_document_for_workspace(data_dir, workspace_root)?;
    let root = doc
        .as_table_mut()
        .ok_or_else(|| "root hive.config.toml must be a TOML table".to_string())?;
    let servers = array_mut(root, "mcp_servers")?;
    let replacement = mcp_spec_to_toml(server);
    if let Some(existing) = servers.iter_mut().find(|entry| {
        entry
            .get("id")
            .and_then(toml::Value::as_str)
            .map(|value| value == server.id)
            .unwrap_or(false)
    }) {
        *existing = replacement;
    } else {
        servers.push(replacement);
    }
    save_workspace_config_document(workspace_root, &doc)
}

fn set_mcp_enabled_in_config(
    data_dir: &PathBuf,
    workspace_root: &str,
    server_id: &str,
    enabled: bool,
) -> Result<bool, String> {
    let mut doc = load_config_document_for_workspace(data_dir, workspace_root)?;
    let root = doc
        .as_table_mut()
        .ok_or_else(|| "root hive.config.toml must be a TOML table".to_string())?;
    let Some(servers) = root.get_mut("mcp_servers").and_then(toml::Value::as_array_mut) else {
        return Ok(false);
    };
    let mut changed = false;
    for entry in servers.iter_mut() {
        let matches = entry
            .get("id")
            .and_then(toml::Value::as_str)
            .map(|value| value == server_id)
            .unwrap_or(false);
        if matches {
            if let Some(table) = entry.as_table_mut() {
                table.insert("enabled".into(), toml::Value::Boolean(enabled));
                changed = true;
            }
            break;
        }
    }
    if changed {
        save_workspace_config_document(workspace_root, &doc)?;
    }
    Ok(changed)
}

fn remove_mcp_server_from_config(
    data_dir: &PathBuf,
    workspace_root: &str,
    server_id: &str,
) -> Result<bool, String> {
    let mut doc = load_config_document_for_workspace(data_dir, workspace_root)?;
    let root = doc
        .as_table_mut()
        .ok_or_else(|| "root hive.config.toml must be a TOML table".to_string())?;
    let Some(servers) = root.get_mut("mcp_servers").and_then(toml::Value::as_array_mut) else {
        return Ok(false);
    };
    let before = servers.len();
    servers.retain(|entry| {
        entry
            .get("id")
            .and_then(toml::Value::as_str)
            .map(|value| value != server_id)
            .unwrap_or(true)
    });
    if before == servers.len() {
        return Ok(false);
    }
    save_workspace_config_document(workspace_root, &doc)?;
    Ok(true)
}

fn runtime_dto(rt: &RuntimeTarget, is_default: bool) -> RuntimeSummaryDto {
    RuntimeSummaryDto {
        id: rt.id.clone(),
        name: rt.name.clone(),
        label: rt.display_label(),
        provider: provider_name(rt.provider_kind).to_string(),
        location: match rt.location {
            hive_core::RuntimeLocation::Local => "local".to_string(),
            hive_core::RuntimeLocation::Remote => "remote".to_string(),
        },
        model: rt.model_id.clone(),
        endpoint: rt.endpoint.clone(),
        supports_tools: rt.capabilities.supports_tools,
        supports_embeddings: rt.capabilities.supports_embeddings,
        is_default,
        is_managed: false,
    }
}

fn parse_role(s: &str) -> WorkspaceRole {
    match s {
        "owner" => WorkspaceRole::Owner,
        "admin" => WorkspaceRole::Admin,
        "viewer" => WorkspaceRole::Viewer,
        _ => WorkspaceRole::Contributor,
    }
}

fn member_dto(m: &WorkspaceMember, self_actor_id: &str) -> WorkspaceMemberDto {
    WorkspaceMemberDto {
        id: m.id.clone(),
        actor_id: m.actor.id.clone(),
        display_name: m.actor.display_name.clone(),
        role: role_name(m.role).to_string(),
        title: m.title.clone(),
        index: m.index,
        is_self: m.actor.id == self_actor_id,
    }
}

fn summary_dto(s: &ChatSession) -> ChatSummaryDto {
    ChatSummaryDto {
        id: s.id.to_string(),
        title: s.title.clone(),
        last_activity_at: rfc3339(s.last_activity_at()),
        message_count: s.messages.len() as u32,
        archived: s.archived,
    }
}

// ---------------------------------------------------------------------------
// Runtime resolution (config-driven, with env-resolved API keys)
// ---------------------------------------------------------------------------

fn api_key_for(provider: ModelProviderKind) -> Option<String> {
    let var = match provider {
        ModelProviderKind::Anthropic => "ANTHROPIC_API_KEY",
        ModelProviderKind::OpenAI => "OPENAI_API_KEY",
        ModelProviderKind::OpenRouter => "OPENROUTER_API_KEY",
        _ => return std::env::var("HIVE_PROVIDER_API_KEY").ok(),
    };
    std::env::var(var).ok()
}

/// Normalize a configured endpoint into the OpenAI-compatible
/// chat-completions URL the client expects.
fn openai_endpoint(rt: &RuntimeTarget, provider_base: Option<&str>) -> String {
    let base = rt
        .model_base_url
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| (!rt.endpoint.is_empty()).then(|| rt.endpoint.clone()))
        .or_else(|| provider_base.map(str::to_string).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| dispatch::default_endpoint(rt.provider_kind).to_string());
    if base.contains("/chat/completions") {
        base
    } else {
        format!("{}/v1/chat/completions", base.trim_end_matches('/'))
    }
}

impl AppState {
    fn combined_runtimes(&self) -> Vec<RuntimeTarget> {
        let base = self.base_runtimes.lock().unwrap();
        let managed = self.managed_runtimes.lock().unwrap();
        merge_runtimes(&base, &managed)
    }

    fn current_default_runtime_id(&self) -> String {
        self.default_runtime_id.lock().unwrap().clone()
    }

    fn combined_mcp_servers(&self) -> Vec<McpServerSpec> {
        let base = self.base_mcp.lock().unwrap();
        let managed = self.managed_mcp.lock().unwrap();
        let mut merged = merge_mcp_servers(&base, &managed);
        // Inject stored OAuth bearer tokens (remote authenticated servers) so
        // the registry authenticates. A spec's own `auth` (if any) wins.
        let settings = self.settings.lock().unwrap();
        for spec in &mut merged {
            if spec.auth.is_none() {
                if let Some(entry) = settings.mcp_oauth.get(&spec.id) {
                    spec.auth = Some(entry.access_token.clone());
                }
            }
        }
        merged
    }

    /// Refresh any MCP OAuth tokens at/near expiry (with a refresh token), so the
    /// registry always carries a live bearer — this is how we avoid 401s rather
    /// than reacting to them. Best-effort: a failed refresh leaves the old token
    /// (the server then 401s and the user re-authorizes).
    async fn ensure_fresh_mcp_tokens(&self) {
        let now = hive_core::Timestamp::now().inner().unix_timestamp();
        #[allow(clippy::type_complexity)]
        let due: Vec<(String, String, String, Option<String>, String)> = {
            let s = self.settings.lock().unwrap();
            s.mcp_oauth
                .iter()
                .filter(|(_, e)| e.expires_at.map(|exp| exp <= now + 60).unwrap_or(false))
                .filter_map(|(id, e)| {
                    Some((
                        id.clone(),
                        e.token_endpoint.clone()?,
                        e.client_id.clone()?,
                        e.client_secret.clone(),
                        e.refresh_token.clone()?,
                    ))
                })
                .collect()
        };
        for (id, token_endpoint, client_id, client_secret, refresh_token) in due {
            if let Ok(tok) = hive_runtime::mcp_oauth::refresh(
                &token_endpoint,
                &client_id,
                client_secret.as_deref(),
                &refresh_token,
            )
            .await
            {
                let mut s = self.settings.lock().unwrap();
                if let Some(e) = s.mcp_oauth.get_mut(&id) {
                    e.access_token = tok.access_token;
                    if tok.refresh_token.is_some() {
                        e.refresh_token = tok.refresh_token;
                    }
                    e.expires_at = tok.expires_in.map(|x| now + x as i64);
                }
                save_settings(&self.data_dir, &s);
            }
        }
    }

    fn reload_workspace_catalogs(&self, workspace_root: &str) {
        let (base_runtimes, default_runtime_id, base_mcp, managed_runtimes, managed_mcp) =
            load_workspace_catalogs(workspace_root, &self.data_dir);
        *self.base_runtimes.lock().unwrap() = base_runtimes;
        *self.default_runtime_id.lock().unwrap() = default_runtime_id;
        *self.base_mcp.lock().unwrap() = base_mcp;
        *self.managed_runtimes.lock().unwrap() = managed_runtimes;
        *self.managed_mcp.lock().unwrap() = managed_mcp;
    }

    /// Resolve a runtime id to an executable runtime. Unknown ids fall back to
    /// the default Anthropic runtime.
    fn resolve_runtime(&self, runtime_id: &str) -> ResolvedRuntime {
        // Live settings: API key (overrides env) + Claude Code permission mode.
        let (settings_key, claude_args, provider_keys, provider_base_urls) = {
            let s = self.settings.lock().unwrap();
            (
                s.api_key.clone(),
                s.claude_args(),
                s.provider_keys.clone(),
                s.provider_base_urls.clone(),
            )
        };
        let runtimes = self.combined_runtimes();
        if let Some(rt) = runtimes.iter().find(|r| r.id == runtime_id) {
            let provider = rt.provider_kind;
            let endpoint = if matches!(
                provider,
                ModelProviderKind::Aider | ModelProviderKind::Pi | ModelProviderKind::ClaudeCode
            ) {
                // subprocess: endpoint is the program (config endpoint or the
                // provider name as the command)
                if rt.endpoint.is_empty() {
                    match provider {
                        // the official Claude Code binary is `claude`
                        ModelProviderKind::ClaudeCode => "claude".to_string(),
                        other => format!("{other:?}").to_lowercase(),
                    }
                } else {
                    rt.endpoint.clone()
                }
            } else if provider == ModelProviderKind::Anthropic {
                String::new()
            } else {
                openai_endpoint(rt, provider_base_urls.get(provider_config_name(provider)).map(String::as_str))
            };
            // Per-provider key wins, then the legacy global key, then env.
            let provider_key = provider_keys
                .get(provider_config_name(provider))
                .cloned()
                .filter(|s| !s.is_empty());
            let args = if provider == ModelProviderKind::ClaudeCode {
                claude_args
            } else {
                Vec::new()
            };
            return ResolvedRuntime {
                provider,
                model: rt.model_id.clone(),
                endpoint,
                // Per-provider key → legacy global key → env.
                api_key: provider_key.or(settings_key).or_else(|| api_key_for(provider)),
                args,
                // For `pi` → OpenAI-compatible backends (e.g. local Ollama):
                // carry the provider id + base URL so the bridge can point pi
                // at it. (`endpoint` here is the executable path, not a URL.)
                model_provider_id: rt.model_provider_id.clone(),
                model_base_url: rt.model_base_url.clone().filter(|s| !s.is_empty()),
            };
        }
        // Fallback when no runtime is configured: the bring-your-own Claude Code
        // CLI (`claude`) — streams via stream-json, needs no API key. The
        // permission mode (if any) still applies.
        ResolvedRuntime {
            provider: ModelProviderKind::ClaudeCode,
            model: String::new(),
            endpoint: "claude".to_string(),
            api_key: None,
            args: claude_args,
            model_provider_id: None,
            model_base_url: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Commands — chat lifecycle
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_app_info() -> AppInfo {
    hive_runtime::app_info()
}

#[tauri::command]
fn list_chats(state: State<AppState>) -> Result<Vec<ChatSummaryDto>, String> {
    let active = state.active_workspace_id();
    let me = state.local_actor_id();
    let known_rooms = state.known_room_ids();
    let svc = state.service.lock().unwrap();
    let ids = svc.store().list_session_ids().map_err(map_err)?;
    let mut out = Vec::new();
    for id in ids {
        if let Some(s) = svc.load(id).map_err(map_err)? {
            if session_in_workspace(s.workspace_id, &s.creator_actor_id, active, &me, &known_rooms) {
                out.push(summary_dto(&s));
            }
        }
    }
    Ok(out)
}

fn message_role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "System",
        MessageRole::User => "User",
        MessageRole::Assistant => "Assistant",
        MessageRole::Agent => "Agent",
    }
}

/// Render a chat transcript as a portable Markdown document. Pure (no IO) so
/// it's unit-testable; the `export_chat` command wraps it with a save dialog.
fn chat_markdown(session: &ChatSession) -> String {
    let title = if session.title.trim().is_empty() {
        "Untitled chat"
    } else {
        session.title.trim()
    };
    let mut out = String::new();
    out.push_str(&format!("# {title}\n\n"));
    let shown = session.messages.iter().filter(|m| !m.body.trim().is_empty()).count();
    out.push_str(&format!("_{shown} message(s) · exported from Hive_\n\n"));
    for m in &session.messages {
        if m.body.trim().is_empty() {
            continue;
        }
        let who = if m.author.trim().is_empty() {
            message_role_label(m.role).to_string()
        } else {
            m.author.clone()
        };
        out.push_str(&format!("**{}** · _{}_\n\n", who, message_role_label(m.role)));
        out.push_str(m.body.trim());
        out.push_str("\n\n---\n\n");
    }
    out
}

/// Slugify a chat title into a safe-ish file stem for the export dialog.
fn export_filename(title: &str) -> String {
    let stem: String = title
        .trim()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let stem = stem.trim_matches('-').to_lowercase();
    let stem = if stem.is_empty() { "hive-chat".to_string() } else { stem };
    format!("{stem}.md")
}

/// Export a chat transcript to a Markdown file chosen via a save dialog.
/// Returns the written path, or `None` if the user cancels.
#[tauri::command]
fn export_chat(state: State<AppState>, session_id: String) -> Result<Option<String>, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let session = {
        let svc = state.service.lock().unwrap();
        svc.load(id)
            .map_err(map_err)?
            .ok_or_else(|| format!("unknown session {session_id}"))?
    };
    let markdown = chat_markdown(&session);
    let Some(path) = rfd::FileDialog::new()
        .set_file_name(export_filename(&session.title))
        .add_filter("Markdown", &["md"])
        .save_file()
    else {
        return Ok(None);
    };
    std::fs::write(&path, markdown).map_err(map_err)?;
    Ok(Some(path.to_string_lossy().to_string()))
}

// ---------------------------------------------------------------------------
// Composer attachments
//
// An attachment is saved to `<data_dir>/attachments/` and referenced in the
// message body as a `[Attached: <abs path>]` marker. Subprocess agents
// (claude-code/pi/aider) read the path with their own file tools; the bare
// Anthropic-API runtime inlines image markers as vision blocks at turn time.
// ---------------------------------------------------------------------------

fn attachments_dir(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("attachments")
}

fn sanitize_attachment_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let cleaned = cleaned.trim_matches('-').to_string();
    if cleaned.is_empty() { "file".to_string() } else { cleaned }
}

/// Save a composer attachment (base64 bytes) to disk; returns its absolute path
/// for embedding as a `[Attached: ...]` marker.
#[tauri::command]
fn save_attachment(state: State<AppState>, name: String, data_base64: String) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_base64.as_bytes())
        .map_err(|e| format!("bad attachment encoding: {e}"))?;
    let dir = attachments_dir(&state.data_dir);
    std::fs::create_dir_all(&dir).map_err(map_err)?;
    let fname = format!("{}-{}", Uuid::new_v4(), sanitize_attachment_name(&name));
    let path = dir.join(fname);
    std::fs::write(&path, &bytes).map_err(map_err)?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
fn create_chat(state: State<AppState>, title: String) -> Result<ChatSessionDto, String> {
    let title = if title.trim().is_empty() {
        "New chat".to_string()
    } else {
        title
    };
    let mut svc = state.service.lock().unwrap();
    let session = svc
        .create_chat(title, state.active_workspace_id(), &state.current_default_runtime_id())
        .map_err(map_err)?;
    Ok(session_dto(&session))
}

#[tauri::command]
fn get_chat(state: State<AppState>, session_id: String) -> Result<Option<ChatSessionDto>, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let svc = state.service.lock().unwrap();
    Ok(svc.load(id).map_err(map_err)?.as_ref().map(session_dto))
}

#[tauri::command]
fn get_context_telemetry(
    state: State<AppState>,
    session_id: String,
) -> Result<Option<ContextTelemetryDto>, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let svc = state.service.lock().unwrap();
    let Some(session) = svc.load(id).map_err(map_err)? else {
        return Ok(None);
    };
    let responder = responder_for(&state, &session, None);
    let snapshot = context_snapshot(&state, id, &session, &responder);
    let skill_tokens: u32 = session
        .loaded_skills
        .iter()
        .map(|skill| token_estimator::estimate_text(&skill.instructions) as u32)
        .sum();

    Ok(Some(ContextTelemetryDto {
        session_id: session.id.to_string(),
        runtime_id: responder.runtime_id,
        model: responder.runtime.model,
        context_window_tokens: snapshot.window_tokens as u32,
        reserved_output_tokens: OUTPUT_RESERVE_TOKENS as u32,
        summary_reserve_tokens: SUMMARY_RESERVE_TOKENS as u32,
        system_prompt_tokens: snapshot.system_prompt_tokens as u32,
        history_budget_tokens: snapshot.history_budget_tokens as u32,
        history_tokens: snapshot.history_tokens,
        message_count: session.messages.len() as u32,
        kept_message_count: snapshot.plan.kept.len() as u32,
        kept_tokens: snapshot.kept_tokens,
        overflow_message_count: snapshot.plan.overflow.len() as u32,
        overflow_tokens: snapshot.overflow_tokens,
        skill_count: session.loaded_skills.len() as u32,
        skill_tokens,
        vault_count: session.vault_sources.len() as u32,
        summary_strategy: snapshot.summary_strategy.to_string(),
    }))
}

#[tauri::command]
fn archive_chat(state: State<AppState>, session_id: String, archived: bool) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    svc.set_archived(id, state.active_workspace_id(), archived).map_err(map_err)
}

/// Rename a chat (the ✎ affordance in the chat header).
#[tauri::command]
fn rename_chat(state: State<AppState>, session_id: String, title: String) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("title cannot be empty".into());
    }
    let mut svc = state.service.lock().unwrap();
    svc.set_title(id, state.active_workspace_id(), title).map_err(map_err)
}

#[tauri::command]
fn delete_chat(state: State<AppState>, session_id: String) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    svc.delete_chat(id).map_err(map_err)
}

#[tauri::command]
fn list_agents(state: State<AppState>, session_id: String) -> Result<Vec<WorkspaceAgentDto>, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let svc = state.service.lock().unwrap();
    Ok(svc
        .load(id)
        .map_err(map_err)?
        .map(|s| s.workspace_agents.iter().map(agent_dto).collect())
        .unwrap_or_default())
}

#[tauri::command]
fn add_agent(
    state: State<AppState>,
    session_id: String,
    name: String,
    runtime_id: String,
    role: String,
) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let mut agent = WorkspaceAgent::new(
        name,
        if runtime_id.is_empty() {
            state.current_default_runtime_id()
        } else {
            runtime_id
        },
    );
    agent.role = role;
    let mut svc = state.service.lock().unwrap();
    svc.add_agent(id, state.active_workspace_id(), agent).map_err(map_err)
}

#[tauri::command]
fn remove_agent(state: State<AppState>, session_id: String, agent_id: String) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let aid = Uuid::parse_str(&agent_id).map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    svc.remove_agent(sid, state.active_workspace_id(), aid).map_err(map_err)
}

// ---------------------------------------------------------------------------
// Commands — members / roles (gated by the authorization evaluator)
// ---------------------------------------------------------------------------

#[tauri::command]
fn list_members(state: State<AppState>, session_id: String) -> Result<Vec<WorkspaceMemberDto>, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let svc = state.service.lock().unwrap();
    let me = svc.author().id.clone();
    Ok(svc
        .load(id)
        .map_err(map_err)?
        .map(|s| s.members.iter().map(|m| member_dto(m, &me)).collect())
        .unwrap_or_default())
}

#[tauri::command]
fn add_member(
    state: State<AppState>,
    session_id: String,
    display_name: String,
    role: String,
    title: String,
) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let member_id = Uuid::new_v4().to_string();
    let mut svc = state.service.lock().unwrap();
    let next_index = svc
        .load(id)
        .map_err(map_err)?
        .map(|s| s.members.iter().map(|m| m.index).max().unwrap_or(0) + 1)
        .unwrap_or(1);
    let member = WorkspaceMember {
        id: member_id.clone(),
        actor: ActorIdentity::new(member_id, display_name, ActorKind::Human),
        role: parse_role(&role),
        title,
        index: next_index,
        joined_at: Default::default(),
    };
    svc.add_member(id, state.active_workspace_id(), member).map_err(map_err)
}

/// Enterprise (#143): import a GitHub org's Teams as workspace members, mapping
/// team membership → roles (highest wins). Needs a signed-in GitHub token with
/// `read:org`. Idempotent — existing members (by stable account id) are skipped.
#[tauri::command]
async fn import_github_teams(
    state: State<'_, AppState>,
    session_id: String,
    org: String,
) -> Result<u32, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let token = state
        .settings
        .lock()
        .unwrap()
        .github_token
        .clone()
        .filter(|t| !t.is_empty())
        .ok_or("sign in to GitHub first (Settings → Account)")?;
    let roster = hive_runtime::github::resolve_org_roster(&token, &org)
        .await
        .map_err(|e| e.to_string())?;
    let ws = state.active_workspace_id();
    let mut svc = state.service.lock().unwrap();
    let session = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?;
    let mut next_index = session.members.iter().map(|m| m.index).max().unwrap_or(0);
    let mut added = 0u32;
    for om in roster {
        let account_id = hive_runtime::github::account_id_for(om.github_user_id).to_string();
        if session.members.iter().any(|m| m.id == account_id) {
            continue; // already imported
        }
        next_index += 1;
        let member = WorkspaceMember {
            id: account_id.clone(),
            actor: ActorIdentity::new(account_id, om.login, ActorKind::Human),
            role: om.role,
            title: String::new(),
            index: next_index,
            joined_at: Default::default(),
        };
        svc.add_member(sid, ws, member).map_err(map_err)?;
        added += 1;
    }
    Ok(added)
}

#[tauri::command]
fn remove_member(state: State<AppState>, session_id: String, member_id: String) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    svc.remove_member(id, state.active_workspace_id(), member_id).map_err(map_err)
}

// ── Relay access user management (Settings → Team) ──────────────────────────
// These drive the enterprise relay's /v1/admin/* API, authenticated by the
// signed-in GitHub token; the relay admits only its configured admin logins.

/// The configured relay URL, or a friendly error when sync isn't set up.
fn relay_url_or_err(state: &AppState) -> Result<String, String> {
    state
        .settings
        .lock()
        .unwrap()
        .relay_url
        .clone()
        .filter(|u| !u.trim().is_empty())
        .ok_or_else(|| "set a relay URL in Settings → Team first".into())
}

fn relay_token_dto(t: &hive_runtime::RelayTokenEntry) -> RelayTokenDto {
    RelayTokenDto {
        id: t.id.clone(),
        label: t.label.clone(),
        last_used: if t.last_used == 0 {
            String::new()
        } else {
            rfc3339(hive_core::Timestamp::from_unix_seconds(t.last_used as i64))
        },
        revoked: t.revoked_at.is_some(),
    }
}

#[tauri::command]
async fn list_relay_users(state: State<'_, AppState>) -> Result<Vec<RelayUserDto>, String> {
    let url = relay_url_or_err(&state)?;
    let users = state
        .relay_client(&url)
        .admin_list_users()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("this relay has no user-management API, or you're not an admin")?;
    Ok(users
        .into_iter()
        .map(|u| RelayUserDto {
            id: u.id,
            name: u.name,
            login: u.login,
            disabled: u.disabled,
            tokens: u
                .tokens
                .iter()
                .filter(|t| t.revoked_at.is_none())
                .map(relay_token_dto)
                .collect(),
        })
        .collect())
}

#[tauri::command]
async fn create_relay_user(
    state: State<'_, AppState>,
    name: String,
    login: String,
) -> Result<IssuedRelayTokenDto, String> {
    let url = relay_url_or_err(&state)?;
    let issued = state
        .relay_client(&url)
        .admin_create_user(name.trim(), login.trim(), "")
        .await
        .map_err(|e| e.to_string())?;
    Ok(IssuedRelayTokenDto {
        user_id: issued.user.id,
        user_name: issued.user.name,
        raw: issued.raw,
    })
}

#[tauri::command]
async fn issue_relay_token(
    state: State<'_, AppState>,
    user_id: String,
    label: String,
) -> Result<IssuedRelayTokenDto, String> {
    let url = relay_url_or_err(&state)?;
    let issued = state
        .relay_client(&url)
        .admin_issue_token(&user_id, label.trim())
        .await
        .map_err(|e| e.to_string())?;
    Ok(IssuedRelayTokenDto {
        user_id: issued.user.id,
        user_name: issued.user.name,
        raw: issued.raw,
    })
}

#[tauri::command]
async fn revoke_relay_token(state: State<'_, AppState>, token_id: String) -> Result<(), String> {
    let url = relay_url_or_err(&state)?;
    state
        .relay_client(&url)
        .admin_revoke_token(&token_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_relay_user_disabled(
    state: State<'_, AppState>,
    user_id: String,
    disabled: bool,
) -> Result<(), String> {
    let url = relay_url_or_err(&state)?;
    state
        .relay_client(&url)
        .admin_set_user_disabled(&user_id, disabled)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_member_role(
    state: State<AppState>,
    session_id: String,
    member_id: String,
    role: String,
) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    svc.set_member_role(id, state.active_workspace_id(), member_id, parse_role(&role))
        .map_err(map_err)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RevokeResultDto {
    /// Whether the workspace key was rotated (false if no relay / no recipients).
    rotated: bool,
    /// How many remaining devices the new key was sealed to.
    recipients: u32,
}

/// Remove a member AND rotate the workspace key, sealed only to the remaining
/// members' devices. The removed member keeps the old key but can't open the
/// rotation, so they can't read traffic sent after revocation. Authorization
/// (owner/admin, last-owner protection) is enforced by `remove_member`.
#[tauri::command]
async fn remove_and_revoke(
    state: State<'_, AppState>,
    session_id: String,
    member_id: String,
) -> Result<RevokeResultDto, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let active_ws = state.active_workspace_id();

    // Gather the remaining members' device keys, then remove the member. All
    // synchronous — the service guard must not be held across an await.
    let recipients: Vec<(String, Vec<u8>)> = {
        let mut svc = state.service.lock().unwrap();
        let session = svc
            .load(sid)
            .map_err(map_err)?
            .ok_or_else(|| "Unknown chat.".to_string())?;
        let mut map: std::collections::BTreeMap<String, Vec<u8>> = Default::default();
        for m in &session.members {
            if m.id == member_id {
                continue;
            }
            if let Some(pk) = &m.actor.key_agreement_public {
                map.insert(m.actor.id.clone(), pk.clone());
            }
        }
        // Always seal to the acting owner's own device so they adopt the new key.
        let me = svc.author().clone();
        if let Some(pk) = &me.key_agreement_public {
            map.insert(me.id.clone(), pk.clone());
        }
        svc.remove_member(sid, active_ws, member_id.clone())
            .map_err(map_err)?;
        map.into_iter().collect()
    };

    let (relay_url, room) = {
        let s = state.settings.lock().unwrap();
        (
            s.relay_url.clone().filter(|u| !u.trim().is_empty()),
            s.sync_room.clone(),
        )
    };
    let Some(url) = relay_url else {
        return Ok(RevokeResultDto { rotated: false, recipients: recipients.len() as u32 });
    };
    if recipients.is_empty() {
        return Ok(RevokeResultDto { rotated: false, recipients: 0 });
    }
    let client = state.relay_client(&url);
    let next_version = client
        .fetch_key_rotations(&room)
        .await
        .ok()
        .and_then(|rs| rs.iter().map(|r| r.version).max())
        .unwrap_or(0)
        + 1;
    let new_key = hive_core::e2ee::generate_workspace_key().map_err(|e| format!("{e:?}"))?;
    let rotation = hive_core::e2ee::WorkspaceKeyRotation::seal_for_devices(
        next_version,
        &new_key,
        &recipients,
    )
    .map_err(|e| format!("{e:?}"))?;
    client
        .publish_key_rotation(&room, &rotation)
        .await
        .map_err(|e| e.to_string())?;
    Ok(RevokeResultDto { rotated: true, recipients: recipients.len() as u32 })
}

// ---------------------------------------------------------------------------
// Commands — skills (install from internet, with the manifest resolver)
// ---------------------------------------------------------------------------

fn skill_dto(s: &SkillProfile) -> SkillDto {
    SkillDto {
        id: s.id.to_string(),
        name: s.name.clone(),
        instructions: s.instructions.clone(),
        source_url: s.source_url.clone(),
    }
}

#[tauri::command]
fn list_skills(state: State<AppState>, session_id: String) -> Result<Vec<SkillDto>, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let svc = state.service.lock().unwrap();
    Ok(svc
        .load(id)
        .map_err(map_err)?
        .map(|s| s.loaded_skills.iter().map(skill_dto).collect())
        .unwrap_or_default())
}

#[tauri::command]
fn add_skill_inline(
    state: State<AppState>,
    session_id: String,
    name: String,
    instructions: String,
) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let skill = SkillProfile::new(name, instructions);
    let mut svc = state.service.lock().unwrap();
    svc.add_skill(id, state.active_workspace_id(), skill).map_err(map_err)
}

/// Install a skill from a remote manifest reference (full URL, GitHub blob URL,
/// or `owner/repo/path` shorthand). Resolves → fetches → loads it.
#[tauri::command]
async fn install_skill(
    state: State<'_, AppState>,
    session_id: String,
    name: String,
    source: String,
) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let url = resolve_manifest_url(&source).map_err(map_err)?;
    let instructions = vault_fetcher::fetch_text(&url).await.map_err(map_err)?;
    let mut skill = SkillProfile::new(
        if name.trim().is_empty() { url.clone() } else { name },
        instructions,
    );
    skill.source_url = Some(url);
    let mut svc = state.service.lock().unwrap();
    svc.add_skill(id, state.active_workspace_id(), skill).map_err(map_err)
}

#[tauri::command]
fn remove_skill(state: State<AppState>, session_id: String, skill_id: String) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let skid = Uuid::parse_str(&skill_id).map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    svc.remove_skill(sid, state.active_workspace_id(), skid).map_err(map_err)
}

// ---------------------------------------------------------------------------
// Commands — MCP servers (the inert-until-enabled gate)
// ---------------------------------------------------------------------------

#[tauri::command]
fn list_mcp_servers(state: State<AppState>) -> Result<Vec<McpServerDto>, String> {
    Ok(state
        .combined_mcp_servers()
        .iter()
        .map(|s| {
            let (transport, detail) = match &s.transport {
                McpTransport::Stdio { command, args } => {
                    ("stdio".to_string(), format!("{} {}", command, args.join(" ")))
                }
                McpTransport::Http { url } => ("http".to_string(), url.clone()),
            };
            McpServerDto {
                id: s.id.clone(),
                transport,
                detail: detail.trim().to_string(),
                enabled: s.enabled,
                is_managed: true,
            }
        })
        .collect())
}

/// Toggle a server's enabled flag. Enabling is what would launch/connect it;
/// disabling keeps it inert. (Connection + the tool loop is a follow-up.)
#[tauri::command]
fn set_mcp_enabled(state: State<AppState>, server_id: String, enabled: bool) -> Result<(), String> {
    let managed_snapshot = {
        let mut managed = state.managed_mcp.lock().unwrap();
        if let Some(s) = managed.iter_mut().find(|s| s.id == server_id) {
            s.enabled = enabled;
            Some(managed.clone())
        } else {
            None
        }
    };
    let updated_managed = managed_snapshot.is_some();
    if let Some(servers) = managed_snapshot {
        let root = state.workspace_root.lock().unwrap().clone();
        save_managed_mcp(&root, &servers)?;
    }
    let root = state.workspace_root.lock().unwrap().clone();
    if set_mcp_enabled_in_config(&state.data_dir, &root, &server_id, enabled)? {
        state.reload_workspace_catalogs(&root);
        return Ok(());
    }
    if updated_managed {
        state.reload_workspace_catalogs(&root);
        return Ok(());
    }
    Err(format!("unknown MCP server {server_id}"))
}

fn parse_mcp_transport(id: String, spec: &Value) -> Result<McpServerSpec, String> {
    let transport_hint = spec
        .get("transport")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let url = spec
        .get("url")
        .or_else(|| spec.get("endpoint"))
        .and_then(Value::as_str)
        .map(str::to_string);
    if transport_hint == "http" || url.is_some() {
        return Ok(McpServerSpec {
            id,
            transport: McpTransport::Http {
                url: url.ok_or_else(|| "http MCP manifest requires a url".to_string())?,
            },
            enabled: false,
            auth: None,
        });
    }

    let command = spec
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| "stdio MCP manifest requires a command".to_string())?;
    let args = spec
        .get("args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(McpServerSpec {
        id,
        transport: McpTransport::Stdio {
            command: command.to_string(),
            args,
        },
        enabled: false,
        auth: None,
    })
}

fn parse_mcp_manifest(text: &str, fallback_id: &str) -> Result<Vec<McpServerSpec>, String> {
    let value: Value = serde_json::from_str(text).map_err(map_err)?;
    if let Some(servers) = value.get("mcpServers").and_then(Value::as_object) {
        let mut parsed = Vec::new();
        for (id, spec) in servers {
            parsed.push(parse_mcp_transport(id.clone(), spec)?);
        }
        return Ok(parsed);
    }
    let id = value
        .get("id")
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| slugify_runtime_id(fallback_id));
    Ok(vec![parse_mcp_transport(id, &value)?])
}

#[tauri::command]
async fn install_mcp_server(state: State<'_, AppState>, source: String) -> Result<(), String> {
    let url = resolve_manifest_url(&source).map_err(map_err)?;
    let manifest = vault_fetcher::fetch_text(&url).await.map_err(map_err)?;
    let fallback_id = url.rsplit('/').next().unwrap_or("mcp-server");
    let parsed = parse_mcp_manifest(&manifest, fallback_id)?;
    let root = state.workspace_root.lock().unwrap().clone();
    for server in &parsed {
        upsert_mcp_server_in_config(&state.data_dir, &root, server)?;
    }
    let servers = {
        let mut managed = state.managed_mcp.lock().unwrap();
        let parsed_ids: std::collections::HashSet<String> =
            parsed.iter().map(|server| server.id.clone()).collect();
        managed.retain(|candidate| !parsed_ids.contains(&candidate.id));
        for server in parsed {
            if let Some(existing) = managed.iter_mut().find(|candidate| candidate.id == server.id) {
                *existing = server;
            } else {
                managed.push(server);
            }
        }
        managed.clone()
    };
    save_managed_mcp(&root, &servers)?;
    state.reload_workspace_catalogs(&root);
    Ok(())
}

#[tauri::command]
fn remove_mcp_server(state: State<AppState>, server_id: String) -> Result<(), String> {
    let root = state.workspace_root.lock().unwrap().clone();
    let removed_from_config = remove_mcp_server_from_config(&state.data_dir, &root, &server_id)?;
    let servers = {
        let mut managed = state.managed_mcp.lock().unwrap();
        managed.retain(|server| server.id != server_id);
        managed.clone()
    };
    save_managed_mcp(&root, &servers)?;
    state.reload_workspace_catalogs(&root);
    if !removed_from_config && servers.is_empty() {
        return Err(format!("unknown MCP server {server_id}"));
    }
    Ok(())
}

/// Add (or update) a remote HTTP MCP server by URL — used by presets like
/// Linear's hosted server. Disabled until the user enables it (the gate); a
/// protected server is then connected via [`authorize_mcp_server`].
#[tauri::command]
fn add_remote_mcp_server(state: State<AppState>, id: String, url: String) -> Result<(), String> {
    let id = id.trim().to_string();
    let url = url.trim().to_string();
    if id.is_empty() || url.is_empty() {
        return Err("id and url are required".into());
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("MCP server url must be http(s)".into());
    }
    let server = McpServerSpec {
        id: id.clone(),
        transport: McpTransport::Http { url },
        enabled: false,
        auth: None,
    };
    let root = state.workspace_root.lock().unwrap().clone();
    upsert_mcp_server_in_config(&state.data_dir, &root, &server)?;
    let servers = {
        let mut managed = state.managed_mcp.lock().unwrap();
        if let Some(existing) = managed.iter_mut().find(|s| s.id == id) {
            *existing = server;
        } else {
            managed.push(server);
        }
        managed.clone()
    };
    save_managed_mcp(&root, &servers)?;
    state.reload_workspace_catalogs(&root);
    Ok(())
}

/// Connect a remote MCP server via OAuth 2.1 + PKCE: opens the browser to the
/// server's authorization page, captures the loopback redirect, exchanges the
/// code, and stores the token (refreshable). See `hive_runtime::mcp_oauth`.
#[tauri::command]
async fn authorize_mcp_server(
    state: State<'_, AppState>,
    server_id: String,
    scope: Option<String>,
) -> Result<(), String> {
    let server_url = state
        .combined_mcp_servers()
        .into_iter()
        .find(|s| s.id == server_id)
        .and_then(|s| match s.transport {
            McpTransport::Http { url } => Some(url),
            _ => None,
        })
        .ok_or("no HTTP MCP server with that id")?;
    let (configured_client_id, client_secret) = {
        let s = state.settings.lock().unwrap();
        match s.mcp_oauth.get(&server_id) {
            Some(e) => (e.client_id.clone(), e.client_secret.clone()),
            None => (None, None),
        }
    };

    let cfg = hive_runtime::mcp_oauth::OAuthConfig {
        server_url,
        configured_client_id,
        client_secret: client_secret.clone(),
        scope: scope.unwrap_or_default(),
    };
    let authd = hive_runtime::mcp_oauth::authorize(&cfg, |u| {
        let _ = open_url_in_browser(u);
    })
    .await
    .map_err(|e| e.to_string())?;

    let now = hive_core::Timestamp::now().inner().unix_timestamp();
    let expires_at = authd.token.expires_in.map(|s| now + s as i64);
    {
        let mut settings = state.settings.lock().unwrap();
        settings.mcp_oauth.insert(
            server_id,
            McpOAuthEntry {
                access_token: authd.token.access_token,
                refresh_token: authd.token.refresh_token,
                expires_at,
                client_id: Some(authd.client_id),
                client_secret, // preserve for refresh
                token_endpoint: Some(authd.token_endpoint),
            },
        );
        save_settings(&state.data_dir, &settings);
    }
    Ok(())
}

/// Store the OAuth client credentials for a remote MCP server (e.g. a Linear
/// OAuth app's Client ID + secret), before connecting. Preserves any existing
/// token. The secret stays local (settings.json).
#[tauri::command]
fn set_mcp_oauth_client(
    state: State<AppState>,
    server_id: String,
    client_id: String,
    client_secret: Option<String>,
) -> Result<(), String> {
    let mut settings = state.settings.lock().unwrap();
    let entry = settings.mcp_oauth.entry(server_id).or_default();
    entry.client_id = Some(client_id.trim().to_string()).filter(|s| !s.is_empty());
    entry.client_secret = client_secret.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    save_settings(&state.data_dir, &settings);
    Ok(())
}

/// Linear context source (#145 L3): pull issues from the connected Linear MCP
/// server into a block the caller drops into the composer. The exact tool name
/// is *discovered* from tools/list (so we don't hardcode a guess), preferring an
/// issues-listing tool. Requires the Linear server enabled + Connected.
#[tauri::command]
async fn linear_issues_context(state: State<'_, AppState>) -> Result<String, String> {
    state.ensure_fresh_mcp_tokens().await;
    let registry = McpRegistry::new(state.combined_mcp_servers());
    if !registry.enabled().any(|s| s.id == "linear") {
        return Err("enable + Connect the Linear MCP server first (Settings → Tools)".into());
    }
    let tools = registry.list_tools("linear").await.map_err(|e| e.to_string())?;
    let pick = tools
        .iter()
        .find(|t| {
            let n = t.name.to_lowercase();
            n.contains("issue") && ["list", "my", "assigned", "search", "get"].iter().any(|k| n.contains(k))
        })
        .or_else(|| tools.iter().find(|t| t.name.to_lowercase().contains("issue")))
        .ok_or("the Linear server didn't expose an issues tool")?;
    let result = registry
        .call_tool("linear", &pick.name, &serde_json::json!({}))
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("```linear:{}\n{}\n```", pick.name, result.trim()))
}

// ---------------------------------------------------------------------------
// Commands — vaults (reference material sources)
// ---------------------------------------------------------------------------

fn vault_dto(v: &VaultSource) -> VaultSourceDto {
    let kind = match v {
        VaultSource::GitHub { .. } => "github",
        VaultSource::GitLab { .. } => "gitlab",
        VaultSource::Https { .. } => "https",
    };
    VaultSourceDto {
        kind: kind.to_string(),
        label: v.label(),
        url: v.raw_url(),
    }
}

/// Parse a `kind` + `reference` into a VaultSource. github/gitlab references are
/// `owner/repo/path[@branch]` (gitlab: `group/project/path[@branch]`); https is a
/// full URL.
fn parse_vault_source(kind: &str, reference: &str) -> Result<VaultSource, String> {
    let (path_part, branch) = match reference.split_once('@') {
        Some((p, b)) => (p, b.to_string()),
        None => (reference, "HEAD".to_string()),
    };
    match kind {
        "https" => Ok(VaultSource::Https {
            url: reference.to_string(),
        }),
        "github" => {
            let parts: Vec<&str> = path_part.splitn(3, '/').collect();
            if parts.len() < 3 {
                return Err("github needs owner/repo/path".into());
            }
            Ok(VaultSource::GitHub {
                owner: parts[0].into(),
                repo: parts[1].into(),
                path: parts[2].into(),
                branch,
            })
        }
        "gitlab" => {
            let idx = path_part.rfind('/').ok_or("gitlab needs project/path")?;
            Ok(VaultSource::GitLab {
                project: path_part[..idx].into(),
                path: path_part[idx + 1..].into(),
                branch,
            })
        }
        _ => Err(format!("unknown vault kind {kind}")),
    }
}

#[tauri::command]
fn list_vaults(state: State<AppState>, session_id: String) -> Result<Vec<VaultSourceDto>, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let svc = state.service.lock().unwrap();
    Ok(svc
        .load(id)
        .map_err(map_err)?
        .map(|s| s.vault_sources.iter().map(vault_dto).collect())
        .unwrap_or_default())
}

#[tauri::command]
fn add_vault(
    state: State<AppState>,
    session_id: String,
    kind: String,
    reference: String,
) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let source = parse_vault_source(&kind, reference.trim())?;
    let mut svc = state.service.lock().unwrap();
    svc.add_vault_source(id, state.active_workspace_id(), source).map_err(map_err)
}

#[tauri::command]
fn remove_vault(state: State<AppState>, session_id: String, url: String) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    svc.remove_vault_source(id, state.active_workspace_id(), &url).map_err(map_err)
}

/// Fetch a preview (first ~2 KB) of a vault source's content.
#[tauri::command]
async fn preview_vault(state: State<'_, AppState>, url: String) -> Result<String, String> {
    let text = vault_fetcher::fetch_text(&url).await.map_err(map_err)?;
    // Previewing doubles as refresh: replace the cached copy the context
    // injection uses, so an updated upstream doc takes effect without restart.
    state.vault_cache.lock().unwrap().insert(url, text.clone());
    Ok(text.chars().take(2000).collect())
}

// ---------------------------------------------------------------------------
// Commands — review queue (proposals + quorum) and reactions
// ---------------------------------------------------------------------------

fn local_actor(state: &AppState) -> hive_core::ActorIdentity {
    state
        .identity
        .load()
        .ok()
        .flatten()
        .map(|s| s.account.actor())
        .unwrap_or_else(|| hive_core::ActorIdentity::new("local", "You", hive_core::ActorKind::Human))
}

#[tauri::command]
fn list_proposals(state: State<AppState>, session_id: String) -> Result<Vec<ProposalDto>, String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let svc = state.service.lock().unwrap();
    Ok(svc
        .load(id)
        .map_err(map_err)?
        .map(|s| s.proposals.iter().map(proposal_dto).collect())
        .unwrap_or_default())
}

#[tauri::command]
fn create_proposal(
    state: State<AppState>,
    session_id: String,
    title: String,
    body: String,
    kind: String,
    required_approvals: u32,
) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let pkind = match kind.as_str() {
        "fileDiff" => ProposalKind::FileDiff,
        "command" => ProposalKind::Command,
        _ => ProposalKind::Decision,
    };
    let mut proposal = ActionProposal::new(title, pkind);
    proposal.body = body;
    proposal.required_approvals = required_approvals.max(1);
    let mut svc = state.service.lock().unwrap();
    svc.upsert_proposal(id, state.active_workspace_id(), proposal).map_err(map_err)
}

#[tauri::command]
fn vote_proposal(
    state: State<AppState>,
    session_id: String,
    proposal_id: String,
    approved: bool,
) -> Result<Option<ProposalDto>, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let pid = Uuid::parse_str(&proposal_id).map_err(map_err)?;
    let actor = local_actor(&state);
    let mut svc = state.service.lock().unwrap();
    // The local workspace owner votes (single-user default; multi-user roles
    // resolve from membership once invites land).
    let updated = svc
        .vote_on_proposal(sid, state.active_workspace_id(), pid, actor.id, WorkspaceRole::Owner, approved)
        .map_err(map_err)?;
    drop(svc);
    // A settled vote may be a workflow gate — wake that run's driver so it
    // reacts instantly instead of on its next poll.
    if updated.as_ref().is_some_and(|p| p.status != ProposalStatus::Open) {
        let run = state.gate_runs.lock().unwrap().get(&pid).copied();
        if let Some(rid) = run {
            if let Some(w) = state.run_wakers.lock().unwrap().get(&rid) {
                w.notify_waiters();
            }
        }
    }
    Ok(updated.as_ref().map(proposal_dto))
}

/// Agreement-gated execution: carry out an **approved** proposal. The gate is
/// that this only runs once quorum is met (status Approved) *and* a human
/// explicitly invokes it — agents never auto-execute. The proposal is dispatched
/// to the responding agent as an instruction (so it runs through the normal turn
/// + per-tool consent), then marked Applied.
#[tauri::command]
async fn implement_proposal(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    proposal_id: String,
) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let pid = Uuid::parse_str(&proposal_id).map_err(map_err)?;
    let workspace_id = state.active_workspace_id();

    // Load + gate: must exist, be approved (quorum met), and not already applied.
    let (session, proposal) = {
        let svc = state.service.lock().unwrap();
        let session = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?;
        let proposal = session
            .proposals
            .iter()
            .find(|p| p.id == pid)
            .cloned()
            .ok_or("unknown proposal")?;
        (session, proposal)
    };
    if proposal.status == ProposalStatus::Applied {
        return Err("this proposal was already implemented".into());
    }
    if !proposal.is_quorum_met() {
        return Err("not approved yet — it needs quorum before it can be implemented".into());
    }

    // Dispatch the proposal to the responder as an instruction, so the agent
    // carries it out via its own (consent-gated) tools.
    let kind = match proposal.kind {
        ProposalKind::FileDiff => "file change",
        ProposalKind::Command => "command",
        ProposalKind::Decision => "decision",
    };
    let prompt = format!(
        "An approved proposal ({kind}) is ready to implement. Carry it out now using your tools, \
         then briefly confirm what you did.\n\n## {}\n\n{}",
        proposal.title, proposal.body
    );
    let responder = responder_for(&state, &session, None);
    {
        let mut svc = state.service.lock().unwrap();
        svc.post_user_message(sid, workspace_id, &prompt).map_err(map_err)?;
    }
    let _ = run_turn(&app, &state, sid, workspace_id, &responder).await?;

    // Mark it applied (gate satisfied + dispatched).
    {
        let mut svc = state.service.lock().unwrap();
        let mut applied = proposal;
        applied.status = ProposalStatus::Applied;
        svc.upsert_proposal(sid, workspace_id, applied).map_err(map_err)?;
    }
    let _ = app.emit("workspace://synced", 1);
    Ok(())
}

#[tauri::command]
fn toggle_reaction(
    state: State<AppState>,
    session_id: String,
    message_id: String,
    emoji: String,
) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let mid = Uuid::parse_str(&message_id).map_err(map_err)?;
    let actor = local_actor(&state);
    let mut svc = state.service.lock().unwrap();
    svc.toggle_reaction(sid, state.active_workspace_id(), mid, emoji, &actor)
        .map_err(map_err)
}

// ---------------------------------------------------------------------------
// send_message — the streaming, multi-turn dispatch path
// ---------------------------------------------------------------------------

/// Who is about to respond, and how to execute their turn.
struct Responder {
    system_base: String,
    author: String,
    runtime_id: String,
    runtime: ResolvedRuntime,
    /// Account that owns this responder (agent owner, or the chat creator for
    /// the primary). Only that device dispatches the turn; empty ⇒ local.
    owner_actor_id: String,
}

struct ContextSnapshot {
    window_tokens: i64,
    system_prompt_tokens: i64,
    history_budget_tokens: i64,
    history_tokens: u32,
    kept_tokens: u32,
    overflow_tokens: u32,
    summary_strategy: &'static str,
    plan: hive_core::context_budget::ContextWindowPlan,
}

fn context_snapshot(
    state: &AppState,
    session_id: Uuid,
    session: &ChatSession,
    responder: &Responder,
) -> ContextSnapshot {
    let window_tokens = model_context_window::tokens_for_model(&responder.runtime.model) as i64;
    let system_prompt_tokens = token_estimator::estimate_text(&responder.system_base) as i64;
    let history_budget_tokens =
        (window_tokens - OUTPUT_RESERVE_TOKENS - system_prompt_tokens - SUMMARY_RESERVE_TOKENS)
            .max(1000);
    let plan = hive_core::context_budget::plan(&session.messages, history_budget_tokens);
    let history_tokens = token_estimator::estimate_messages(&session.messages) as u32;
    let kept_tokens = token_estimator::estimate_messages(&plan.kept) as u32;
    let overflow_tokens = token_estimator::estimate_messages(&plan.overflow) as u32;
    let summary_strategy = if plan.overflow.is_empty() {
        "none"
    } else {
        let overflow_ids: Vec<Uuid> = plan.overflow.iter().map(|message| message.id).collect();
        let cached = state.summary_cache.lock().unwrap().get(&session_id).cloned();
        match cached {
            Some((covered, _)) if covered == overflow_ids => "cached",
            Some((covered, _))
                if overflow_ids.starts_with(&covered) && covered.len() < overflow_ids.len() =>
            {
                "incremental"
            }
            _ => "fresh",
        }
    };

    ContextSnapshot {
        window_tokens,
        system_prompt_tokens,
        history_budget_tokens,
        history_tokens,
        kept_tokens,
        overflow_tokens,
        summary_strategy,
        plan,
    }
}

fn responder_for(state: &AppState, session: &ChatSession, agent: Option<&WorkspaceAgent>) -> Responder {
    match agent {
        Some(a) => Responder {
            system_base: prompt::agent_system_prompt(session, a),
            author: a.name.clone(),
            runtime_id: a.runtime_id.clone(),
            runtime: state.resolve_runtime(&a.runtime_id),
            owner_actor_id: a.owner_actor_id.clone(),
        },
        None => Responder {
            system_base: prompt::primary_system_prompt(session),
            author: "Hive".to_string(),
            runtime_id: session.runtime_id.clone(),
            runtime: state.resolve_runtime(&session.runtime_id),
            owner_actor_id: session.creator_actor_id.clone(),
        },
    }
}

impl AppState {
    /// The local account id (used for cross-device dispatch ownership checks).
    fn local_actor_id(&self) -> String {
        self.service.lock().unwrap().author().id.clone()
    }

    /// Relay access token (entitlement) for a gated/paid hosted relay. `None`
    /// for self-hosted/open relays. Attached as a bearer to every relay call.
    fn relay_token(&self) -> Option<String> {
        self.settings.lock().unwrap().relay_access_token.clone()
    }

    /// The caller's GitHub identity token, attached as `X-Hive-Github-Token` so a
    /// membership-enforcing relay can authenticate the caller. `None` if not
    /// signed in (open relays ignore it).
    fn github_token(&self) -> Option<String> {
        self.settings.lock().unwrap().github_token.clone()
    }

    /// A relay client for `url` carrying both the entitlement bearer and the
    /// GitHub identity token from settings.
    fn relay_client(&self, url: &str) -> hive_runtime::RelayClient {
        hive_runtime::RelayClient::new(url)
            .with_auth(self.relay_token())
            .with_github_token(self.github_token())
    }

    /// The workspace new chats are stamped with / the chat list is scoped to.
    fn active_workspace_id(&self) -> Uuid {
        *self.active_workspace.lock().unwrap()
    }

    /// Workspace ids for every relay room this device knows about (joined set +
    /// the currently configured room). Chats with these ids belong to a room,
    /// not "My workspace".
    fn known_room_ids(&self) -> std::collections::HashSet<Uuid> {
        let s = self.settings.lock().unwrap();
        let mut ids: std::collections::HashSet<Uuid> =
            s.joined_rooms.iter().map(|r| room_workspace_id(r)).collect();
        ids.extend(s.workspaces.iter().map(|w| w.id()));
        if s.relay_url.as_ref().map(|u| !u.is_empty()).unwrap_or(false) {
            ids.insert(room_workspace_id(&s.sync_room));
        }
        ids
    }
}

/// Whether `session` belongs in the active workspace's chat list.
///
/// - **A room workspace** shows exactly the chats stamped with that room id
///   (including teammates' chats synced in).
/// - **"My workspace"** (any non-room active id) shows chats that aren't tagged
///   to a known room *and* were authored locally (or predate creator tracking),
///   which re-homes legacy local chats and hides synced room chats that leaked
///   into the flat store before scoping existed.
fn session_in_workspace(
    session_workspace_id: Uuid,
    creator_actor_id: &str,
    active_id: Uuid,
    me: &str,
    known_rooms: &std::collections::HashSet<Uuid>,
) -> bool {
    if known_rooms.contains(&active_id) {
        return session_workspace_id == active_id;
    }
    // Local "My workspace" scope.
    !known_rooms.contains(&session_workspace_id)
        && (creator_actor_id.is_empty() || creator_actor_id == me)
}

/// Whether *this* device should run the turn for `responder`. We dispatch when
/// the responder is unowned (legacy / local-only) or owned by us; otherwise the
/// owner's device answers (and the reply syncs back).
fn owns_responder(local_actor_id: &str, responder: &Responder) -> bool {
    responder.owner_actor_id.is_empty() || responder.owner_actor_id == local_actor_id
}

/// Who, if anyone, a message should be answered by.
#[derive(Clone, Copy)]
enum DispatchTarget {
    Primary,
    Agent(Uuid),
}

fn human_member_count(session: &ChatSession) -> usize {
    session
        .members
        .iter()
        .filter(|m| m.actor.kind == ActorKind::Human)
        .count()
}

/// Decide whether a message triggers a runtime, and which one. An `@agent` or
/// `@primary` mention is always answered. An *un-addressed* message defaults to
/// the primary **only in a solo workspace** (≤1 human) — multi-human chats stay
/// human-to-human until someone explicitly addresses an assistant.
fn dispatch_target(body: &str, session: &ChatSession) -> Option<DispatchTarget> {
    let mentions = parse_mentions(body, session);
    if let Some(id) = mentions.agents.first().copied() {
        return Some(DispatchTarget::Agent(id));
    }
    if mentions.primary {
        return Some(DispatchTarget::Primary);
    }
    (human_member_count(session) <= 1).then_some(DispatchTarget::Primary)
}

/// Window the history to the model budget and, if anything overflows,
/// summarize it (cached incrementally per session) and prepend the summary to
/// the system prompt. Returns (system, turns).
async fn windowed_context(
    state: &State<'_, AppState>,
    session_id: Uuid,
    session: &ChatSession,
    responder: &Responder,
) -> (String, Vec<ChatTurn>) {
    let snapshot = context_snapshot(state, session_id, session, responder);
    let plan = snapshot.plan;
    let turns = turns_from(&plan.kept);
    // Reference vaults ride in the system prompt (capped; cached per app run).
    // Like the summary below, this is appended after the window plan — the
    // caps keep the skew small and the output/summary reserves absorb it.
    let vault_section = vault_context_section(state, session).await;

    if plan.overflow.is_empty() {
        state.summary_cache.lock().unwrap().remove(&session_id);
        return (join_system(&responder.system_base, &vault_section), turns);
    }

    let overflow_ids: Vec<Uuid> = plan.overflow.iter().map(|m| m.id).collect();
    let cached = state.summary_cache.lock().unwrap().get(&session_id).cloned();
    let summary = match cached {
        Some((covered, sum)) if covered == overflow_ids => sum,
        Some((covered, sum)) if overflow_ids.starts_with(&covered) && covered.len() < overflow_ids.len() => {
            let delta = &plan.overflow[covered.len()..];
            summarize(state, &responder.runtime, &summarize_instruction(state), Some(&sum), delta)
                .await
                .unwrap_or(sum)
        }
        _ => summarize(state, &responder.runtime, &summarize_instruction(state), None, &plan.overflow)
            .await
            .unwrap_or_else(|| format!("[Earlier {} messages omitted.]", plan.overflow.len())),
    };
    state
        .summary_cache
        .lock()
        .unwrap()
        .insert(session_id, (overflow_ids, summary.clone()));

    let system = format!(
        "[Earlier conversation summary]\n{summary}\n\n{}",
        join_system(&responder.system_base, &vault_section)
    );
    (system, turns)
}

/// Append an optional extra section to a system prompt.
fn join_system(base: &str, extra: &str) -> String {
    if extra.is_empty() {
        base.to_string()
    } else {
        format!("{base}\n\n{extra}")
    }
}

// Caps for vault injection: enough to carry style guides / API notes without
// blowing small context windows (3 × 6000 chars ≈ 4.5k tokens worst case).
const VAULT_MAX_SOURCES: usize = 3;
const VAULT_MAX_CHARS: usize = 6000;
const VAULT_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Build the `[Reference vaults]` system-prompt section from the session's
/// vault sources. Content is fetched once per app run (cached by raw URL);
/// fetch failures degrade to a labeled "unavailable" line rather than
/// blocking the reply.
async fn vault_context_section(state: &State<'_, AppState>, session: &ChatSession) -> String {
    if session.vault_sources.is_empty() {
        return String::new();
    }
    let mut entries: Vec<(String, Option<String>)> = Vec::new();
    for source in session.vault_sources.iter().take(VAULT_MAX_SOURCES) {
        let url = source.raw_url();
        let cached = state.vault_cache.lock().unwrap().get(&url).cloned();
        let content = match cached {
            Some(text) => Some(text),
            None => {
                match tokio::time::timeout(VAULT_FETCH_TIMEOUT, vault_fetcher::fetch_text(&url))
                    .await
                {
                    Ok(Ok(text)) => {
                        state.vault_cache.lock().unwrap().insert(url.clone(), text.clone());
                        Some(text)
                    }
                    _ => None,
                }
            }
        };
        entries.push((source.label(), content));
    }
    vault_section_text(&entries)
}

/// Pure formatter for the vault section (unit-tested): caps each source and
/// marks truncation/unavailability so the model knows what it's looking at.
fn vault_section_text(entries: &[(String, Option<String>)]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "[Reference vaults]\nWorkspace reference material — treat as background knowledge:\n",
    );
    for (label, content) in entries {
        match content {
            Some(text) => {
                let trimmed = text.trim();
                if trimmed.chars().count() > VAULT_MAX_CHARS {
                    let capped: String = trimmed.chars().take(VAULT_MAX_CHARS).collect();
                    out.push_str(&format!("--- {label} (truncated) ---\n{capped}\n[…truncated]\n"));
                } else {
                    out.push_str(&format!("--- {label} ---\n{trimmed}\n"));
                }
            }
            None => out.push_str(&format!("--- {label} — unavailable (fetch failed) ---\n")),
        }
    }
    out.trim_end().to_string()
}

/// Built-in instruction for conversation summarization — used by /summarize,
/// /compact, and the automatic overflow windowing unless the user overrides it
/// (Settings → Models → Context commands).
const DEFAULT_CONTEXT_SUMMARY_PROMPT: &str =
    "Summarize the conversation for context continuity: decisions, open \
questions, action items, file/code changes, and owners. Be concise (<=200 words).";

/// The active /summarize instruction (also drives auto-windowing summaries).
fn summarize_instruction(state: &AppState) -> String {
    state
        .settings
        .lock()
        .unwrap()
        .summarize_prompt
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CONTEXT_SUMMARY_PROMPT.to_string())
}

/// The active /compact instruction.
fn compact_instruction(state: &AppState) -> String {
    state
        .settings
        .lock()
        .unwrap()
        .compact_prompt
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CONTEXT_SUMMARY_PROMPT.to_string())
}

/// One-shot (non-streaming-collected) summary of overflowed turns.
async fn summarize(
    state: &State<'_, AppState>,
    runtime: &ResolvedRuntime,
    instruction: &str,
    prior: Option<&str>,
    messages: &[ChatMessage],
) -> Option<String> {
    let mut body = String::new();
    if let Some(p) = prior {
        body.push_str("Existing summary so far:\n");
        body.push_str(p);
        body.push_str("\n\nNew messages to fold in:\n");
    }
    for m in messages {
        body.push_str(&format!("{}: {}\n", m.author, m.body));
    }
    let turns = vec![ChatTurn::user(body)];
    let workspace_root = state.workspace_root.lock().unwrap().clone();
    dispatch::stream(runtime, Some(instruction), &turns, Some(&workspace_root), &[], 512, |_| {})
        .await
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// The (possibly customized) /summarize + /compact instructions, plus the
/// built-in default for the UI to show as placeholder/reset target.
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ContextCommandsDto {
    summarize_prompt: String,
    compact_prompt: String,
    default_prompt: String,
}

#[tauri::command]
fn get_context_commands(state: State<AppState>) -> ContextCommandsDto {
    let s = state.settings.lock().unwrap();
    ContextCommandsDto {
        summarize_prompt: s.summarize_prompt.clone().unwrap_or_default(),
        compact_prompt: s.compact_prompt.clone().unwrap_or_default(),
        default_prompt: DEFAULT_CONTEXT_SUMMARY_PROMPT.to_string(),
    }
}

/// Set custom /summarize + /compact instructions. Empty = built-in default.
#[tauri::command]
fn set_context_commands(
    state: State<AppState>,
    summarize_prompt: String,
    compact_prompt: String,
) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    s.summarize_prompt = Some(summarize_prompt.trim().to_string()).filter(|v| !v.is_empty());
    s.compact_prompt = Some(compact_prompt.trim().to_string()).filter(|v| !v.is_empty());
    save_settings(&state.data_dir, &s);
    Ok(())
}

/// `/summarize` — post a model-written summary of the whole conversation as a
/// new assistant message, leaving the transcript intact. A read-only digest.
#[tauri::command]
async fn summarize_chat(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let (session, workspace_id) = {
        let svc = state.service.lock().unwrap();
        let s = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?;
        let wid = s.workspace_id;
        (s, wid)
    };
    if session.messages.is_empty() {
        return Err("nothing to summarize yet".into());
    }
    let responder = responder_for(&state, &session, None);
    let summary = summarize(&state, &responder.runtime, &summarize_instruction(&state), None, &session.messages)
        .await
        .ok_or("the model returned an empty summary")?;
    let body = format!("**Conversation summary**\n\n{summary}");
    let mut svc = state.service.lock().unwrap();
    let id = svc
        .begin_assistant_message(sid, workspace_id, "Summary", &responder.runtime_id)
        .map_err(map_err)?;
    svc.complete_assistant_message(sid, workspace_id, id, body).map_err(map_err)?;
    Ok(())
}

/// `/compact` — collapse the conversation into a single summary checkpoint:
/// summarize every message, then remove them and post the summary as the new
/// head of the chat. Frees context immediately and persists (it's recorded as
/// MessageRemoved + MessageAppended events, so it survives restart). This is
/// the explicit version of the windowing that already auto-compacts on overflow.
#[tauri::command]
async fn compact_chat(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let (session, workspace_id) = {
        let svc = state.service.lock().unwrap();
        let s = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?;
        let wid = s.workspace_id;
        (s, wid)
    };
    if session.messages.len() < 2 {
        return Err("not enough history to compact yet".into());
    }
    let responder = responder_for(&state, &session, None);
    let summary = summarize(&state, &responder.runtime, &compact_instruction(&state), None, &session.messages)
        .await
        .ok_or("the model returned an empty summary")?;
    let ids: Vec<Uuid> = session.messages.iter().map(|m| m.id).collect();
    let body = format!("🗜 **Context compacted** — earlier messages collapsed into this summary.\n\n{summary}");
    let mut svc = state.service.lock().unwrap();
    for id in ids {
        svc.remove_message(sid, workspace_id, id).map_err(map_err)?;
    }
    let id = svc
        .begin_assistant_message(sid, workspace_id, "Summary", &responder.runtime_id)
        .map_err(map_err)?;
    svc.complete_assistant_message(sid, workspace_id, id, body).map_err(map_err)?;
    // In-memory overflow summary cache is now stale (messages changed).
    state.summary_cache.lock().unwrap().remove(&sid);
    Ok(())
}

/// True when a session still has its placeholder title (so auto-titling is safe).
fn is_default_title(title: &str) -> bool {
    let t = title.trim();
    t.is_empty() || t.eq_ignore_ascii_case("new chat") || t.eq_ignore_ascii_case("untitled")
}

/// Clean a model's title response into a short, plain title.
fn sanitize_title(raw: &str) -> String {
    // First non-empty line only.
    let mut line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
    // Drop a leading "Title:" label if the model added one.
    if let Some(idx) = line.find(':') {
        if line[..idx].trim().eq_ignore_ascii_case("title") {
            line = line[idx + 1..].trim().to_string();
        }
    }
    // Strip wrapping quotes / markdown emphasis and trailing sentence punctuation.
    let trimmed = line
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '*' | '#'))
        .trim()
        .trim_end_matches(|c: char| matches!(c, '.' | '!' | '?'))
        .trim();
    // Cap to a handful of words / 60 chars.
    let mut out: String = trimmed.split_whitespace().take(8).collect::<Vec<_>>().join(" ");
    if out.chars().count() > 60 {
        out = out.chars().take(60).collect::<String>().trim_end().to_string();
    }
    out
}

/// Ask the primary runtime for a concise title from the opening exchange.
async fn generate_title(
    state: &State<'_, AppState>,
    runtime: &ResolvedRuntime,
    user_message: &str,
    assistant_reply: &str,
) -> Option<String> {
    let mut body = String::from("User:\n");
    body.push_str(user_message.trim());
    let reply = assistant_reply.trim();
    if !reply.is_empty() {
        body.push_str("\n\nAssistant:\n");
        body.push_str(&reply.chars().take(800).collect::<String>());
    }
    let system = "Write a short, specific title (3 to 6 words) for this conversation. \
Reply with ONLY the title — no quotes, no surrounding punctuation, no trailing period, in \
Title Case.";
    let turns = vec![ChatTurn::user(body)];
    let workspace_root = state.workspace_root.lock().unwrap().clone();
    let raw = dispatch::stream(runtime, Some(system), &turns, Some(&workspace_root), &[], 32, |_| {})
        .await
        .ok()?;
    let title = sanitize_title(&raw);
    (!title.is_empty()).then_some(title)
}

// --- MCP tool-call loop wiring (Anthropic) ---

/// Adapter: one non-streaming Anthropic turn for the tool loop.
struct AnthropicMessages {
    client: AnthropicClient,
    api_key: String,
    model: String,
    system: Option<String>,
}

impl MessagesApi for AnthropicMessages {
    async fn run(&self, messages: &[Value], tools: &[Value]) -> Result<AnthropicResponse, ProviderError> {
        self.client
            .run_messages(&self.api_key, &self.model, self.system.as_deref(), messages, tools, 1024)
            .await
    }
}

/// Executes a tool call by routing the (sanitized) tool name back to its MCP
/// server. Honors the registry's enable gate.
struct McpToolExecutor {
    registry: McpRegistry,
    /// sanitized anthropic tool name → (server id, real tool name)
    names: HashMap<String, (String, String)>,
}

impl ToolExecutor for McpToolExecutor {
    async fn call(&self, name: &str, input: &Value) -> (String, bool) {
        match self.names.get(name) {
            Some((server, tool)) => match self.registry.call_tool(server, tool, input).await {
                Ok(text) => (text, false),
                Err(e) => (format!("tool error: {e}"), true),
            },
            None => (format!("unknown tool {name}"), true),
        }
    }
}

/// Anthropic tool names must match `^[a-zA-Z0-9_-]{1,64}$`.
fn sanitize_tool_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    cleaned.chars().take(64).collect()
}

/// Convert provider turns into the Anthropic message array the tool loop seeds.
fn turns_to_messages(turns: &[ChatTurn]) -> Vec<Value> {
    // `content_value` inlines `[Attached: <image>]` markers as vision blocks.
    turns
        .iter()
        .map(|t| json!({ "role": t.role, "content": hive_runtime::provider::anthropic::content_value(&t.content) }))
        .collect()
}

/// Attempt the MCP tool loop. Returns `Ok(Some(text))` if it ran, `Ok(None)` if
/// there were no enabled tools / no API key (caller falls back to streaming).
async fn try_tool_loop(
    app: &AppHandle,
    state: &State<'_, AppState>,
    session_id: Uuid,
    workspace_id: Uuid,
    responder: &Responder,
    system: &str,
    turns: &[ChatTurn],
) -> Result<Option<TurnOutcome>, String> {
    let Some(api_key) = responder.runtime.api_key.clone() else {
        return Ok(None);
    };
    // Renew any near-expiry remote-server tokens before we build the registry.
    state.ensure_fresh_mcp_tokens().await;
    // Snapshot the registry so we don't hold locks across awaits.
    let registry = McpRegistry::new(state.combined_mcp_servers());
    if registry.enabled().next().is_none() {
        return Ok(None);
    }

    let tagged = registry.list_all_tools().await;
    if tagged.is_empty() {
        return Ok(None);
    }

    let mut tool_defs = Vec::with_capacity(tagged.len());
    let mut names = HashMap::new();
    for (server, tool) in &tagged {
        let tname = sanitize_tool_name(&format!("{server}__{}", tool.name));
        tool_defs.push(json!({
            "name": tname,
            "description": tool.description,
            "input_schema": tool.input_schema,
        }));
        names.insert(tname, (server.clone(), tool.name.clone()));
    }

    let message_id = {
        let mut svc = state.service.lock().unwrap();
        svc.begin_assistant_message(session_id, workspace_id, &responder.author, &responder.runtime_id)
            .map_err(map_err)?
    };

    let model = AnthropicMessages {
        client: AnthropicClient::new(),
        api_key,
        model: responder.runtime.model.clone(),
        system: Some(system.to_string()),
    };
    let executor = McpToolExecutor { registry, names };
    let initial = turns_to_messages(turns);

    let result = tool_loop::run_with_messages(&model, &executor, initial, tool_defs, 6).await;

    let (phase, text) = match result {
        Ok(text) => {
            let mut svc = state.service.lock().unwrap();
            let body =
                finalize_reply(&mut svc, session_id, workspace_id, message_id, &responder.author, &text)?;
            ("completed".to_string(), body)
        }
        Err(e) => {
            let msg = e.to_string();
            let mut svc = state.service.lock().unwrap();
            let _ = svc.complete_assistant_message(
                session_id,
                workspace_id,
                message_id,
                format!("[error] {msg}"),
            );
            ("error".to_string(), msg)
        }
    };
    let _ = app.emit(
        ChatStreamEvent::EVENT,
        ChatStreamEvent {
            session_id: session_id.to_string(),
            message_id: message_id.to_string(),
            phase,
            text: text.clone(),
        },
    );
    Ok(Some(TurnOutcome { message_id, body: text }))
}

/// Finalize an assistant reply: strip `[[react:]]`/`[[vote:]]`/`[[workflow:]]`
/// directives, complete the message with the cleaned body, seed the directive
/// emoji as reactions, and save any agent-authored workflow definitions
/// (authored by the responder). Returns the cleaned body.
fn finalize_reply(
    svc: &mut ChatService,
    session_id: Uuid,
    workspace_id: Uuid,
    message_id: Uuid,
    author: &str,
    full: &str,
) -> Result<String, String> {
    let parsed = hive_runtime::directives::parse_reply_directives(full);
    svc.complete_assistant_message(session_id, workspace_id, message_id, &parsed.cleaned)
        .map_err(map_err)?;
    if !parsed.emojis.is_empty() {
        let actor = hive_core::ActorIdentity::new(author, author, hive_core::ActorKind::Agent);
        for emoji in &parsed.emojis {
            let _ = svc.add_reaction(session_id, workspace_id, message_id, emoji.clone(), &actor);
        }
    }
    // Agent-authored workflows: validate against the same rules as the
    // builder, save on success, and leave a visible system note either way.
    // Definitions are inert — only a human can start a run — so a bad or even
    // malicious definition can't execute anything by itself.
    for json in &parsed.workflows {
        let session = svc.load(session_id).map_err(map_err)?.ok_or("unknown session")?;
        match workflows::definition_from_directive(json, &session) {
            Ok(def) => {
                let note = format!(
                    "🧩 {author} added the workflow “{}” — open the Workflows pane to run it.",
                    def.name
                );
                svc.save_workflow_definition(session_id, workspace_id, def).map_err(map_err)?;
                let _ = svc.post_system_note(session_id, workspace_id, note);
            }
            Err(e) => {
                let _ = svc.post_system_note(
                    session_id,
                    workspace_id,
                    format!("⚠️ {author} proposed a workflow that was rejected: {e}"),
                );
            }
        }
    }
    Ok(parsed.cleaned)
}

/// A finished assistant turn: the transcript message it produced plus the
/// assembled body. Workflow stages key on `message_id` to re-read full stage
/// output from the transcript later.
pub(crate) struct TurnOutcome {
    pub message_id: Uuid,
    pub body: String,
}

/// Run one assistant turn: placeholder → stream deltas (persist + emit) →
/// complete. Returns the produced message id + assembled body.
async fn run_turn(
    app: &AppHandle,
    state: &State<'_, AppState>,
    session_id: Uuid,
    workspace_id: Uuid,
    responder: &Responder,
) -> Result<TurnOutcome, String> {
    let session = {
        let svc = state.service.lock().unwrap();
        svc.load(session_id).map_err(map_err)?.ok_or("unknown session")?
    };
    let (system, turns) = windowed_context(state, session_id, &session, responder).await;
    run_prepared_turn(app, state, session_id, workspace_id, responder, &session, system, turns)
        .await
}

/// The execution half of [`run_turn`], with the context already computed.
/// The workflow engine uses this directly: each parallel stage must snapshot
/// its context *after* its own prompt is posted but *before* a sibling
/// stage's prompt lands, so context assembly and execution are split.
#[allow(clippy::too_many_arguments)]
async fn run_prepared_turn(
    app: &AppHandle,
    state: &State<'_, AppState>,
    session_id: Uuid,
    workspace_id: Uuid,
    responder: &Responder,
    session: &ChatSession,
    system: String,
    turns: Vec<ChatTurn>,
) -> Result<TurnOutcome, String> {
    // If MCP tools are enabled and the responder runs on Anthropic, take the
    // (non-streaming) agentic tool loop instead of plain streaming. On any MCP
    // failure we fall through to streaming.
    if responder.runtime.provider == ModelProviderKind::Anthropic {
        if let Some(outcome) =
            try_tool_loop(app, state, session_id, workspace_id, responder, &system, &turns).await?
        {
            return Ok(outcome);
        }
    }

    // Git commit attribution: credit the human who drove this turn as the
    // commit author (+ other thread participants as co-authors), so commits a
    // subprocess agent makes on this host aren't credited to the host owner.
    let git_subprocess = matches!(
        responder.runtime.provider,
        ModelProviderKind::ClaudeCode | ModelProviderKind::Pi | ModelProviderKind::Aider
    );
    let humans: Vec<hive_core::ActorIdentity> = if git_subprocess {
        session
            .messages
            .iter()
            .filter(|m| matches!(m.role, MessageRole::User))
            .filter_map(|m| m.actor_identity.clone())
            .collect()
    } else {
        Vec::new()
    };
    let git_env = humans
        .last()
        .map(hive_runtime::git_attribution::author_env)
        .unwrap_or_default();
    let system = match humans.last() {
        Some(req) => {
            let note = hive_runtime::git_attribution::commit_attribution_note(req, &humans);
            if note.is_empty() { system } else { format!("{system}\n\n{note}") }
        }
        None => system,
    };

    let message_id = {
        let mut svc = state.service.lock().unwrap();
        svc.begin_assistant_message(session_id, workspace_id, &responder.author, &responder.runtime_id)
            .map_err(map_err)?
    };

    let workspace_root = state.workspace_root.lock().unwrap().clone();
    // Stream deltas to the UI in-memory on every token, but persist to SQLite
    // only as a throttled checkpoint (~1×/750ms) for crash-recovery — not per
    // token (#1). The final body is written once by finalize_reply, so any
    // un-flushed tail is harmless.
    const FLUSH_EVERY: std::time::Duration = std::time::Duration::from_millis(750);
    let mut pending = String::new();
    let mut last_flush = std::time::Instant::now();
    let result = dispatch::stream(
        &responder.runtime,
        Some(&system),
        &turns,
        Some(&workspace_root),
        &git_env,
        1024,
        |text| {
            pending.push_str(&text);
            let _ = app.emit(
                ChatStreamEvent::EVENT,
                ChatStreamEvent {
                    session_id: session_id.to_string(),
                    message_id: message_id.to_string(),
                    phase: "delta".into(),
                    text,
                },
            );
            if last_flush.elapsed() >= FLUSH_EVERY {
                let chunk = std::mem::take(&mut pending);
                let mut svc = state.service.lock().unwrap();
                let _ = svc.append_chunk(session_id, workspace_id, message_id, chunk);
                last_flush = std::time::Instant::now();
            }
        },
    )
    .await;

    match result {
        Ok(full) => {
            let body = {
                let mut svc = state.service.lock().unwrap();
                finalize_reply(&mut svc, session_id, workspace_id, message_id, &responder.author, &full)?
            };
            let _ = app.emit(
                ChatStreamEvent::EVENT,
                ChatStreamEvent {
                    session_id: session_id.to_string(),
                    message_id: message_id.to_string(),
                    phase: "completed".into(),
                    text: body.clone(),
                },
            );
            Ok(TurnOutcome { message_id, body })
        }
        Err(e) => {
            let msg = e.to_string();
            {
                let mut svc = state.service.lock().unwrap();
                let _ = svc.complete_assistant_message(
                    session_id,
                    workspace_id,
                    message_id,
                    format!("[error] {msg}"),
                );
            }
            let _ = app.emit(
                ChatStreamEvent::EVENT,
                ChatStreamEvent {
                    session_id: session_id.to_string(),
                    message_id: message_id.to_string(),
                    phase: "error".into(),
                    text: msg.clone(),
                },
            );
            Err(msg)
        }
    }
}

#[tauri::command]
async fn send_message(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    body: String,
) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let local_actor_id = state.local_actor_id();

    // Record the user turn, then decide who (if anyone) answers it.
    let (workspace_id, mut next_target) = {
        let mut svc = state.service.lock().unwrap();
        let session = svc.load(sid).map_err(map_err)?.ok_or("unknown session")?;
        let workspace_id = session.workspace_id;
        svc.post_user_message(sid, workspace_id, &body).map_err(map_err)?;
        let session = svc.load(sid).map_err(map_err)?.unwrap();
        (workspace_id, dispatch_target(&body, &session))
    };

    // Turn loop: a reply that mentions another (distinct) agent cascades a
    // follow-up turn, bounded by MAX_CASCADE_DEPTH and a visited set. A `None`
    // target means the message was human-to-human — no runtime runs.
    let mut visited: Vec<Uuid> = Vec::new();
    let mut first_reply: Option<String> = None;
    for _ in 0..MAX_CASCADE_DEPTH {
        let Some(target) = next_target else { break };
        let session = {
            let svc = state.service.lock().unwrap();
            svc.load(sid).map_err(map_err)?.ok_or("unknown session")?
        };
        let agent = match target {
            DispatchTarget::Agent(id) => {
                session.workspace_agents.iter().find(|a| a.id == id).cloned()
            }
            DispatchTarget::Primary => None,
        };
        if let Some(a) = &agent {
            visited.push(a.id);
        }
        let responder = responder_for(&state, &session, agent.as_ref());

        // Cross-device dispatch: only the device that owns this responder runs
        // the turn. If it's owned elsewhere, we've already recorded the user
        // message (it syncs); the owner's device picks it up via `maybe_respond`.
        if !owns_responder(&local_actor_id, &responder) {
            break;
        }

        let reply = run_turn(&app, &state, sid, workspace_id, &responder).await?.body;
        if first_reply.is_none() {
            first_reply = Some(reply.clone());
        }

        // Notify humans if the reply pings them.
        let reloaded = {
            let svc = state.service.lock().unwrap();
            svc.load(sid).map_err(map_err)?.unwrap()
        };
        let reply_mentions = parse_mentions(&reply, &reloaded);
        if reply_mentions.human_broadcast || !reply_mentions.humans.is_empty() {
            let _ = app
                .notification()
                .builder()
                .title("Hive")
                .body(format!("{} mentioned you", responder.author))
                .show();
        }

        // Cascade to the next mentioned agent if it's new (agent→agent only;
        // a reply never re-summons the primary).
        next_target = reply_mentions
            .agents
            .into_iter()
            .find(|id| !visited.contains(id))
            .map(DispatchTarget::Agent);
    }

    // Auto-title a still-unnamed chat from its opening exchange, using the
    // session's primary runtime. Best-effort: failures leave the title as-is.
    if let Some(reply) = first_reply {
        let (needs_title, primary_runtime) = {
            let svc = state.service.lock().unwrap();
            match svc.load(sid).map_err(map_err)? {
                Some(s) if is_default_title(&s.title) => {
                    (true, state.resolve_runtime(&s.runtime_id))
                }
                _ => (false, state.resolve_runtime("")),
            }
        };
        if needs_title {
            if let Some(title) = generate_title(&state, &primary_runtime, &body, &reply).await {
                let mut svc = state.service.lock().unwrap();
                let _ = svc.set_title(sid, workspace_id, title);
            }
        }
    }

    Ok(())
}

/// Replace the last assistant/agent turn with a fresh generation: drop it, then
/// re-run the responder over the preceding context.
#[tauri::command]
async fn regenerate(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let workspace_id = state.active_workspace_id();
    // Load (and drop the lock) before resolving the responder, which reads state.
    let session = {
        let svc = state.service.lock().unwrap();
        svc.load(sid).map_err(map_err)?.ok_or("unknown session")?
    };
    let last_id = session
        .messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, MessageRole::Assistant | MessageRole::Agent))
        .map(|m| m.id)
        .ok_or("nothing to regenerate yet")?;
    let responder = responder_for(&state, &session, None);
    {
        let mut svc = state.service.lock().unwrap();
        svc.remove_message(sid, workspace_id, last_id).map_err(map_err)?;
    }
    let _ = run_turn(&app, &state, sid, workspace_id, &responder).await?;
    Ok(())
}

/// Cross-device dispatch: if a chat's trailing message is an unanswered user
/// turn whose responder *this* device owns (the chat's creator for the primary,
/// or an agent owner), generate the reply here. Other devices' messages reach us
/// via sync; the frontend calls this on `workspace://synced` for the open chat.
#[tauri::command]
async fn maybe_respond(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let local_actor_id = state.local_actor_id();

    // Decide whether there's an unanswered user message we own the response to.
    let plan: Option<(Uuid, Responder)> = {
        let svc = state.service.lock().unwrap();
        let Some(session) = svc.load(sid).map_err(map_err)? else {
            return Ok(());
        };
        let last = session
            .messages
            .iter()
            .rev()
            .find(|m| m.role != MessageRole::System);
        match last {
            Some(m) if m.role == MessageRole::User => match dispatch_target(&m.body, &session) {
                Some(target) => {
                    let agent = match target {
                        DispatchTarget::Agent(id) => {
                            session.workspace_agents.iter().find(|a| a.id == id).cloned()
                        }
                        DispatchTarget::Primary => None,
                    };
                    let responder = responder_for(&state, &session, agent.as_ref());
                    owns_responder(&local_actor_id, &responder)
                        .then_some((session.workspace_id, responder))
                }
                None => None,
            },
            _ => None,
        }
    };

    let Some((workspace_id, responder)) = plan else {
        return Ok(());
    };

    // Guard against double-dispatch (two near-simultaneous sync ticks).
    {
        let mut inflight = state.responding.lock().unwrap();
        if !inflight.insert(sid) {
            return Ok(());
        }
    }
    let result = run_turn(&app, &state, sid, workspace_id, &responder).await;
    state.responding.lock().unwrap().remove(&sid);
    result.map(|_| ())
}

/// Add the local user to a chat's roster when they open it, so the People tab
/// shows everyone present. No-op if already a member.
#[tauri::command]
fn ensure_self_member(state: State<AppState>, session_id: String) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let mut svc = state.service.lock().unwrap();
    svc.ensure_self_member(sid, state.active_workspace_id()).map_err(map_err)?;
    Ok(())
}

/// Raise a local notification if the chat's newest message (from someone else)
/// `@`-mentions the local user — directly, via `@you`/`@all`, or by role group.
/// Humans `@`-mentioning humans never triggers a runtime; this is the ping.
/// Called by the frontend on sync; deduped by message id.
#[tauri::command]
fn notify_mentions(
    app: AppHandle,
    state: State<AppState>,
    session_id: String,
) -> Result<(), String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let me = state.local_actor_id();
    let hit: Option<(Uuid, String)> = {
        let svc = state.service.lock().unwrap();
        let Some(session) = svc.load(sid).map_err(map_err)? else {
            return Ok(());
        };
        let Some(last) = session.messages.last() else {
            return Ok(());
        };
        let authored_by_me = last
            .actor_identity
            .as_ref()
            .map(|a| a.id == me)
            .unwrap_or(false);
        if authored_by_me {
            None
        } else {
            let m = parse_mentions(&last.body, &session);
            let my_role = session
                .members
                .iter()
                .find(|mem| mem.actor.id == me)
                .map(|mem| mem.role);
            let mentioned = m.human_broadcast
                || m.humans.iter().any(|h| h == &me)
                || my_role.map(|r| m.roles.contains(&r)).unwrap_or(false);
            mentioned.then(|| (last.id, last.author.clone()))
        }
    };
    if let Some((msg_id, who)) = hit {
        if state.notified.lock().unwrap().insert(msg_id) {
            let _ = app
                .notification()
                .builder()
                .title("Hive")
                .body(format!("{who} mentioned you"))
                .show();
        }
    }
    Ok(())
}

/// Presence entry surfaced to the UI (online + typing). `actorId` is unique;
/// `name` is for display only.
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PresenceDto {
    actor_id: String,
    name: String,
    typing: bool,
    session_id: String,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Presence is considered live for this long after the last ping.
const PRESENCE_TTL_MS: u64 = 8000;

/// Heartbeat the local user's ephemeral presence (online + whether they're
/// typing in `session_id`) to the relay. No-op when relay-less (solo). Best
/// effort — relay hiccups are swallowed.
#[tauri::command]
async fn presence_ping(
    state: State<'_, AppState>,
    session_id: String,
    typing: bool,
) -> Result<(), String> {
    let (relay, room) = {
        let s = state.settings.lock().unwrap();
        (s.relay_url.clone(), s.sync_room.clone())
    };
    let Some(relay) = relay.filter(|r| !r.is_empty()) else {
        return Ok(());
    };
    let (me, name) = {
        let svc = state.service.lock().unwrap();
        let a = svc.author();
        (a.id.clone(), a.display_name.clone())
    };
    let data = serde_json::json!({
        "actorId": me,
        "name": name,
        "sessionId": session_id,
        "typing": typing,
        "ts": now_ms(),
    });
    let _ = state
        .relay_client(&relay)
        .publish_presence(&room, &me, &data)
        .await;
    Ok(())
}

/// Live presence for the workspace (excludes self + stale entries). The UI uses
/// `typing` + `sessionId` for "X is typing…" and the rest for online status.
#[tauri::command]
async fn presence_list(state: State<'_, AppState>) -> Result<Vec<PresenceDto>, String> {
    let (relay, room) = {
        let s = state.settings.lock().unwrap();
        (s.relay_url.clone(), s.sync_room.clone())
    };
    let Some(relay) = relay.filter(|r| !r.is_empty()) else {
        return Ok(Vec::new());
    };
    let me = state.local_actor_id();
    let map = state
        .relay_client(&relay)
        .list_presence(&room)
        .await
        .map_err(map_err)?;
    let now = now_ms();
    let mut out = Vec::new();
    for (_device, data) in map {
        let actor_id = data.get("actorId").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if actor_id.is_empty() || actor_id == me {
            continue;
        }
        let ts = data.get("ts").and_then(serde_json::Value::as_u64).unwrap_or(0);
        if now.saturating_sub(ts) > PRESENCE_TTL_MS {
            continue;
        }
        out.push(PresenceDto {
            actor_id,
            name: data.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            typing: data.get("typing").and_then(serde_json::Value::as_bool).unwrap_or(false),
            session_id: data.get("sessionId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Commands — workspace / settings
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_workspace_diffs(state: State<AppState>) -> Result<Vec<GitFileDiffDto>, String> {
    let root = state.workspace_root.lock().unwrap().clone();
    let diffs = GitContextReader::new()
        .working_tree_diffs(&root)
        .into_iter()
        .map(|d| GitFileDiffDto {
            path: d.path,
            kind: git_kind_str(d.kind).to_string(),
            patch: d.patch,
            added_lines: d.added_lines,
            removed_lines: d.removed_lines,
        })
        .collect();
    Ok(diffs)
}

/// Both sides of one file's diff, for the side-by-side editor. `original` is
/// the HEAD blob (empty for added/untracked files), `modified` the working
/// tree (empty for deleted). Binary files are flagged instead of shipped.
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FileDiffSidesDto {
    original: String,
    modified: String,
    is_binary: bool,
}

#[tauri::command]
fn get_file_diff_sides(state: State<AppState>, path: String) -> Result<FileDiffSidesDto, String> {
    let root = state.workspace_root.lock().unwrap().clone();
    let rel = path.trim();
    // The path comes from our own diff list, but re-validate anyway: it must
    // stay inside the workspace.
    if rel.is_empty() || rel.starts_with('/') || rel.contains("..") {
        return Err("invalid path".into());
    }
    let original = std::process::Command::new("git")
        .args(["show", &format!("HEAD:{rel}")])
        .current_dir(&root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| o.stdout)
        .unwrap_or_default();
    let modified = std::fs::read(std::path::Path::new(&root).join(rel)).unwrap_or_default();
    let is_binary = original.contains(&0) || modified.contains(&0);
    Ok(FileDiffSidesDto {
        original: if is_binary {
            String::new()
        } else {
            String::from_utf8_lossy(&original).to_string()
        },
        modified: if is_binary {
            String::new()
        } else {
            String::from_utf8_lossy(&modified).to_string()
        },
        is_binary,
    })
}

/// GUI editors we know how to detect and launch: (id, label, CLI shim, macOS
/// app-bundle name). CLI shims cover all platforms; the bundle check catches
/// macOS installs where the user never ran "install 'code' in PATH".
const KNOWN_EDITORS: &[(&str, &str, &str, &str)] = &[
    ("vscode", "VS Code", "code", "Visual Studio Code"),
    ("cursor", "Cursor", "cursor", "Cursor"),
    ("windsurf", "Windsurf", "windsurf", "Windsurf"),
    ("zed", "Zed", "zed", "Zed"),
    ("sublime", "Sublime Text", "subl", "Sublime Text"),
];

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct EditorDto {
    id: String,
    label: String,
}

/// Code editors installed on this machine.
#[tauri::command]
fn detect_editors() -> Vec<EditorDto> {
    KNOWN_EDITORS
        .iter()
        .filter(|(_, _, cli, app)| {
            if on_path(cli) {
                return true;
            }
            #[cfg(target_os = "macos")]
            {
                return std::path::Path::new(&format!("/Applications/{app}.app")).exists();
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = app;
                false
            }
        })
        .map(|(id, label, _, _)| EditorDto { id: (*id).into(), label: (*label).into() })
        .collect()
}

/// Open a workspace file (or the workspace root when `path` is empty) in one
/// of the detected editors.
#[tauri::command]
fn open_path_in_editor(
    state: State<AppState>,
    editor_id: String,
    path: String,
) -> Result<(), String> {
    let (_, _, cli, app) = KNOWN_EDITORS
        .iter()
        .find(|(id, _, _, _)| *id == editor_id)
        .ok_or_else(|| format!("unknown editor {editor_id}"))?;
    let root = state.workspace_root.lock().unwrap().clone();
    let rel = path.trim();
    if rel.starts_with('/') || rel.contains("..") {
        return Err("invalid path".into());
    }
    let target = if rel.is_empty() {
        std::path::PathBuf::from(&root)
    } else {
        std::path::Path::new(&root).join(rel)
    };
    if on_path(cli) {
        return std::process::Command::new(cli)
            .arg(&target)
            .spawn()
            .map(|_| ())
            .map_err(map_err);
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-a", app])
            .arg(&target)
            .spawn()
            .map(|_| ())
            .map_err(map_err)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Err(format!("'{cli}' is not on PATH"))
    }
}

#[tauri::command]
fn get_app_settings(state: State<AppState>) -> Result<AppSettingsDto, String> {
    let root = state.workspace_root.lock().unwrap().clone();
    let known_workspaces = load_workspace_index(&state.data_dir);
    let display_name = state
        .identity
        .load()
        .map_err(map_err)?
        .map(|s| s.account.display_name)
        .unwrap_or_else(|| "You".to_string());
    let git_email = state.settings.lock().unwrap().git_email.clone().unwrap_or_default();
    // NB: git status is intentionally NOT computed here — it shells out to `git`
    // (expensive, especially on Windows where process spawn is slow) and was
    // making every settings refetch slow. It moved to `get_git_status`, queried
    // lazily only where the branch/dirty pill is shown. Kept on the DTO (as
    // None/0) for back-compat with the generated bindings.
    Ok(AppSettingsDto {
        display_name,
        git_email,
        device_name: state.device_name.clone(),
        workspace_root: root,
        known_workspaces,
        model: state.fallback_model.lock().unwrap().clone(),
        git_branch: None,
        git_dirty_count: 0,
    })
}

/// Git branch + dirty-count for the active workspace. Separate from
/// `get_app_settings` because it shells out to `git` (slow on Windows), so it's
/// queried lazily only where the git pill is shown.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GitStatusDto {
    branch: Option<String>,
    dirty_count: u32,
}

#[tauri::command]
fn get_git_status(state: State<AppState>) -> GitStatusDto {
    let root = state.workspace_root.lock().unwrap().clone();
    let snap = GitContextReader::new().snapshot(&root);
    let dirty_count = snap.dirty_count();
    GitStatusDto {
        branch: if snap.is_repository { snap.branch } else { None },
        dirty_count,
    }
}

/// Set the user's git email (for commit attribution) and apply it live.
#[tauri::command]
fn set_git_email(state: State<AppState>, email: String) -> Result<(), String> {
    let trimmed = email.trim();
    {
        let mut s = state.settings.lock().unwrap();
        s.git_email = (!trimmed.is_empty()).then(|| trimmed.to_string());
        save_settings(&state.data_dir, &s);
    }
    state
        .service
        .lock()
        .unwrap()
        .set_author_git_email((!trimmed.is_empty()).then(|| trimmed.to_string()));
    Ok(())
}

#[tauri::command]
fn list_runtimes(state: State<AppState>) -> Result<Vec<RuntimeSummaryDto>, String> {
    let default_runtime_id = state.current_default_runtime_id();
    let mut runtimes: Vec<RuntimeSummaryDto> = state
        .combined_runtimes()
        .iter()
        .map(|rt| {
            let mut dto = runtime_dto(rt, rt.id == default_runtime_id);
            dto.is_managed = true;
            dto
        })
        .collect();

    // Claude Code (the local `claude` CLI) is a bring-your-own runtime, not a
    // config `[[runtimes]]` entry, so it wouldn't otherwise appear once any other
    // runtime is added — which made it impossible to switch back to. Always
    // surface it when the CLI is on PATH so it stays selectable (and settable as
    // the default). Its id "claude-code" resolves to the CLI fallback in
    // `resolve_runtime`.
    if !runtimes.iter().any(|r| r.provider == "claude-code") && on_path("claude") {
        let model = state.settings.lock().unwrap().claude_code_model.trim().to_string();
        let label = if model.is_empty() {
            "Claude Code · CLI default".to_string()
        } else {
            format!("Claude Code · {model}")
        };
        runtimes.insert(
            0,
            RuntimeSummaryDto {
                id: "claude-code".to_string(),
                name: "Claude Code".to_string(),
                label,
                provider: "claude-code".to_string(),
                location: "local".to_string(),
                model,
                endpoint: "claude".to_string(),
                supports_tools: true,
                supports_embeddings: false,
                is_default: default_runtime_id == "claude-code",
                is_managed: false,
            },
        );
    }

    if runtimes.is_empty() {
        // No configured runtimes and no Claude Code CLI: synthesize the Anthropic
        // Primary Runtime (needs an API key).
        let fallback_model = state.fallback_model.lock().unwrap().clone();
        runtimes.push(RuntimeSummaryDto {
            id: default_runtime_id.clone(),
            name: "Primary Runtime".to_string(),
            label: format!("Primary Runtime · {fallback_model}"),
            provider: "anthropic".to_string(),
            location: "remote".to_string(),
            model: fallback_model,
            endpoint: String::new(),
            supports_tools: true,
            supports_embeddings: false,
            is_default: true,
            is_managed: false,
        });
    }

    // Guarantee exactly one runtime is flagged default, even if `default_runtime_id`
    // points at something no longer listed (e.g. the config default "anthropic"
    // while the user only has Claude Code + OpenAI). Fall back to the first.
    if !runtimes.iter().any(|r| r.is_default) {
        if let Some(first) = runtimes.first_mut() {
            first.is_default = true;
        }
    }

    Ok(runtimes)
}

/// Set which runtime new chats default to (the UI's per-row "Set default").
/// Persisted; drives `is_default` in `list_runtimes` and the runtime new chats
/// are created with. An id that isn't a configured runtime (e.g. "claude-code")
/// resolves to the Claude Code CLI fallback in `resolve_runtime`.
#[tauri::command]
fn set_default_runtime(state: State<AppState>, runtime_id: String) -> Result<(), String> {
    let id = runtime_id.trim().to_string();
    if id.is_empty() {
        return Err("a runtime id is required".to_string());
    }
    *state.default_runtime_id.lock().unwrap() = id.clone();
    let mut s = state.settings.lock().unwrap();
    s.default_runtime_id = Some(id);
    save_settings(&state.data_dir, &s);
    Ok(())
}

#[tauri::command]
fn add_runtime(
    state: State<AppState>,
    id: String,
    name: String,
    provider: String,
    location: String,
    endpoint: String,
    model: String,
    supports_tools: bool,
    supports_embeddings: bool,
    model_base_url: Option<String>,
    model_provider_id: Option<String>,
    context_window: Option<u32>,
) -> Result<(), String> {
    let provider_kind = parse_provider_kind(&provider)?;
    let model_base_url = model_base_url
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let model_provider_id = model_provider_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let runtime_location = parse_runtime_location(&location)?;
    let runtime_id = if id.trim().is_empty() {
        slugify_runtime_id(&name)
    } else {
        slugify_runtime_id(&id)
    };
    let runtime_name = if name.trim().is_empty() {
        runtime_id.clone()
    } else {
        name.trim().to_string()
    };
    let runtime = RuntimeTarget {
        id: runtime_id,
        name: runtime_name,
        provider_kind,
        location: runtime_location,
        model_id: model.trim().to_string(),
        available_models: model
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect(),
        endpoint: endpoint.trim().to_string(),
        metrics_endpoint: None,
        model_provider_id,
        model_base_url,
        request_keep_alive: None,
        estimated_performance_score: 0.0,
        estimated_cost_per_1m_input_tokens_usd: 0.0,
        capabilities: hive_core::RuntimeCapabilities {
            supports_embeddings,
            supports_tools,
            supports_streaming: true,
            supports_agent_orchestration: false,
            // User-supplied window override — matters most for Ollama/custom
            // endpoints whose window can't be inferred from the model name.
            context_window_tokens: context_window.filter(|w| *w > 0),
        },
    };
    let root = state.workspace_root.lock().unwrap().clone();
    upsert_runtime_in_config(&state.data_dir, &root, &runtime)?;
    let runtimes = {
        let mut managed = state.managed_runtimes.lock().unwrap();
        managed.retain(|candidate| candidate.id != runtime.id);
        if let Some(existing) = managed.iter_mut().find(|candidate| candidate.id == runtime.id) {
            *existing = runtime;
        }
        managed.clone()
    };
    save_managed_runtimes(&root, &runtimes)?;
    state.reload_workspace_catalogs(&root);
    Ok(())
}

#[tauri::command]
fn remove_runtime(state: State<AppState>, runtime_id: String) -> Result<(), String> {
    let root = state.workspace_root.lock().unwrap().clone();
    let removed_from_config = remove_runtime_from_config(&state.data_dir, &root, &runtime_id)?;
    let runtimes = {
        let mut managed = state.managed_runtimes.lock().unwrap();
        managed.retain(|runtime| runtime.id != runtime_id);
        managed.clone()
    };
    save_managed_runtimes(&root, &runtimes)?;
    state.reload_workspace_catalogs(&root);
    if !removed_from_config && runtimes.is_empty() {
        return Err(format!("unknown runtime {runtime_id}"));
    }
    Ok(())
}

#[tauri::command]
fn set_chat_runtime(
    state: State<AppState>,
    session_id: String,
    runtime_id: String,
) -> Result<(), String> {
    let id = Uuid::parse_str(&session_id).map_err(map_err)?;
    let runtimes = state.combined_runtimes();
    if !runtimes.is_empty() && !runtimes.iter().any(|rt| rt.id == runtime_id) {
        return Err(format!("unknown runtime id {runtime_id}"));
    }
    let mut svc = state.service.lock().unwrap();
    let workspace_id = svc
        .load(id)
        .map_err(map_err)?
        .map(|s| s.workspace_id)
        .ok_or_else(|| format!("unknown session {session_id}"))?;
    svc.set_session_runtime(id, workspace_id, runtime_id)
        .map_err(map_err)
}

#[tauri::command]
fn set_workspace_root(state: State<AppState>, path: String) -> Result<(), String> {
    let normalized = resolve_workspace_root(&path)?;
    *state.workspace_root.lock().unwrap() = normalized.clone();
    remember_workspace(&state.data_dir, &normalized)?;
    state.reload_workspace_catalogs(&normalized);
    Ok(())
}

#[tauri::command]
fn add_workspace_to_list(state: State<AppState>, path: String) -> Result<Vec<String>, String> {
    let normalized = resolve_workspace_root(&path)?;
    remember_workspace(&state.data_dir, &normalized)
}

#[tauri::command]
fn pick_workspace_folder(state: State<AppState>) -> Result<Option<String>, String> {
    let current_root = state.workspace_root.lock().unwrap().clone();
    let mut dialog = rfd::FileDialog::new();
    if !current_root.trim().is_empty() {
        dialog = dialog.set_directory(&current_root);
    }
    Ok(dialog
        .pick_folder()
        .map(|path| path.to_string_lossy().to_string()))
}

#[tauri::command]
fn remove_workspace_from_list(state: State<AppState>, path: String) -> Result<Vec<String>, String> {
    let normalized = normalize_workspace_path(&path)?;
    forget_workspace(&state.data_dir, &normalized)
}

#[tauri::command]
fn set_display_name(state: State<AppState>, name: String) -> Result<(), String> {
    state.identity.update_display_name(&name).map_err(map_err)?;
    // Refresh the live ChatService author so new messages + signatures use the
    // new name immediately (updating the persisted identity alone doesn't).
    state
        .service
        .lock()
        .unwrap()
        .set_author_display_name(name.trim());
    Ok(())
}

#[tauri::command]
fn open_in_editor(state: State<AppState>) -> Result<(), String> {
    let root = state.workspace_root.lock().unwrap().clone();
    if let Ok(editor) = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")) {
        return std::process::Command::new(editor)
            .arg(&root)
            .spawn()
            .map(|_| ())
            .map_err(map_err);
    }
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let program = "xdg-open";
    std::process::Command::new(program)
        .arg(&root)
        .spawn()
        .map(|_| ())
        .map_err(map_err)
}

/// Open a URL in the system default browser. `window.open` doesn't reach the OS
/// browser from the Tauri webview, so URL opens (e.g. the GitHub device-flow
/// page) route through here. Only http(s) URLs are accepted.
#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    let url = url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("only http(s) URLs can be opened".to_string());
    }
    open_url_in_browser(url).map_err(map_err)
}

/// Launch the system default browser at `url`. Shared by `open_external` and the
/// MCP OAuth flow (which must open the authorization page).
fn open_url_in_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let (program, args): (&str, Vec<&str>) = ("open", vec![url]);
    #[cfg(target_os = "windows")]
    let (program, args): (&str, Vec<&str>) = ("cmd", vec!["/C", "start", "", url]);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let (program, args): (&str, Vec<&str>) = ("xdg-open", vec![url]);
    std::process::Command::new(program).args(&args).spawn().map(|_| ())
}

/// Public version manifest — a small JSON the docs site serves, holding the
/// latest published release. Fetched in Rust (no webview CORS); maintained
/// alongside each release.
const VERSION_MANIFEST_URL: &str = "https://docs.apiaryhq.ai/version.json";

/// The release tag this build was cut from (baked by build.rs). `dev` for local
/// builds, which never prompt to update.
fn baked_release_tag() -> &'static str {
    option_env!("HIVE_RELEASE_TAG").unwrap_or("dev")
}

/// What the update banner needs: the latest tag + a human label, where to get
/// it, and (optional) notes.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInfo {
    tag: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    notes: String,
}

/// Lightweight "is there a newer version?" check, independent of the Tauri
/// auto-updater (which stays inert until signing lands). Fetches the public
/// version manifest and returns `Some(info)` only when its tag differs from this
/// build's baked tag. Returns `None` for dev builds, network errors, or when
/// already current — so the UI fails quiet (never nags on its own errors).
#[tauri::command]
async fn check_for_app_update() -> Option<UpdateInfo> {
    let current = baked_release_tag();
    if current == "dev" {
        return None;
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;
    let info: UpdateInfo = client
        .get(VERSION_MANIFEST_URL)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    if info.tag.trim().is_empty() || info.tag == current {
        None
    } else {
        Some(info)
    }
}

/// Check the update feed. Returns `Some(version)` when a newer signed build is
/// available, `None` when up to date. Inert until the updater is configured
/// (keys + signed artifacts, #144) — reports a friendly message until then.
#[tauri::command]
async fn check_for_update(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app
        .updater()
        .map_err(|e| format!("auto-update isn't configured yet ({e})"))?;
    match updater.check().await {
        Ok(Some(update)) => Ok(Some(update.version)),
        Ok(None) => Ok(None),
        Err(e) => Err(format!("couldn't check for updates: {e}")),
    }
}

/// Tint the native title bar to match the app background. The frontend calls
/// this with the live `--hive-canvas` color (and whether the active theme is
/// dark) on launch and on every theme change, so the OS chrome tracks the UI.
///
/// Windows 11 only (DWM caption color, build 22000+); a no-op everywhere else
/// and on older Windows — the call simply does nothing and the build stays
/// happy. macOS already blends its title bar via `titleBarStyle`.
#[tauri::command]
fn set_titlebar_color(
    window: tauri::WebviewWindow,
    r: u8,
    g: u8,
    b: u8,
    dark: bool,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::Graphics::Dwm::{
            DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
        };
        let hwnd = window.hwnd().map_err(map_err)?.0 as HWND;
        // COLORREF is 0x00BBGGRR.
        let caption: u32 = (r as u32) | ((g as u32) << 8) | ((b as u32) << 16);
        let dark_flag: i32 = if dark { 1 } else { 0 };
        unsafe {
            // Match the system light/dark hint first so the caption text/glyphs
            // stay legible, then paint the caption background itself. Both
            // return HRESULT; failures (e.g. Windows 10, which lacks these
            // attributes) are intentionally ignored — the bar stays default.
            // windows-sys types the attribute id as `u32`, but the DWMWA_*
            // consts are `i32` (DWMWINDOWATTRIBUTE) — cast explicitly.
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
                std::ptr::addr_of!(dark_flag).cast(),
                std::mem::size_of::<i32>() as u32,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_CAPTION_COLOR as u32,
                std::ptr::addr_of!(caption).cast(),
                std::mem::size_of::<u32>() as u32,
            );
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (&window, r, g, b, dark);
    }
    Ok(())
}

/// Factory reset: wipe every local data file (chats, identity, keys, settings,
/// workspaces, attachments) and relaunch fresh.
///
/// We can't delete the live SQLite DB out from under the running app — on
/// Windows the file is locked — so instead we drop a `.reset-pending` sentinel
/// and restart. On the next launch `build_state` sees the sentinel and clears
/// the data dir *before* anything opens, then bootstraps a brand-new identity.
/// The frontend clears its own localStorage right before calling this.
#[tauri::command]
fn reset_local_data(app: AppHandle) -> Result<(), String> {
    let data_dir: PathBuf = app.path().app_data_dir().map_err(map_err)?;
    std::fs::create_dir_all(&data_dir).map_err(map_err)?;
    std::fs::write(data_dir.join(RESET_SENTINEL), b"1").map_err(map_err)?;
    app.restart();
    #[allow(unreachable_code)]
    Ok(())
}

/// Marker file that requests a full wipe on the next launch (see
/// [`reset_local_data`]).
const RESET_SENTINEL: &str = ".reset-pending";

// ---------------------------------------------------------------------------
// Setup / run
// ---------------------------------------------------------------------------

fn build_state(app: &AppHandle) -> Result<AppState, String> {
    let data_dir: PathBuf = app.path().app_data_dir().map_err(map_err)?;
    std::fs::create_dir_all(&data_dir).map_err(map_err)?;

    // A factory reset was requested last run: the previous process wrote the
    // sentinel and restarted, so the DB/identity files are now closed and safe
    // to delete. Wipe everything in the data dir (keeping only the sentinel,
    // which we remove last), then fall through to a fresh bootstrap below.
    let reset_marker = data_dir.join(RESET_SENTINEL);
    if reset_marker.exists() {
        if let Ok(entries) = std::fs::read_dir(&data_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path == reset_marker {
                    continue;
                }
                if path.is_dir() {
                    let _ = std::fs::remove_dir_all(&path);
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        let _ = std::fs::remove_file(&reset_marker);
    }

    let identity = IdentityStore::new(&data_dir, FileKeyVault::new(&data_dir));
    let stored = identity.bootstrap("You", "you", "This device").map_err(map_err)?;
    let device_kp = identity.device_keypair(stored.device.id).map_err(map_err)?;

    let db_path = data_dir.join("hive.db");
    let store = EventStore::open(&db_path).map_err(map_err)?;
    // NB: chunk-row pruning (one-time DB shrink) is deferred to a background
    // thread after the window is live — see `run()`. Running it here scanned
    // the events table on every launch and blocked first paint.
    let service = ChatService::new(store, stored.device.id, device_kp, stored.account.actor());

    let workspace_root = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .ok()
        .and_then(|path| resolve_workspace_root(&path).ok())
        .unwrap_or_default();
    let _ = remember_workspace(&data_dir, &workspace_root);

    let mut config_relay_endpoint: Option<String> = None;
    for candidate in [
        PathBuf::from(&workspace_root).join("hive.config.toml"),
        data_dir.join("hive.config.toml"),
    ] {
        if let Ok(text) = std::fs::read_to_string(&candidate) {
            if let Ok(cfg) = hive_core::config::load_from_str(&text) {
                config_relay_endpoint = cfg
                    .transport
                    .relay_endpoint
                    .filter(|s| !s.is_empty());
                break;
            }
        }
    }

    let (base_runtimes, default_runtime_id, base_mcp, managed_runtimes, managed_mcp) =
        load_workspace_catalogs(&workspace_root, &data_dir);

    let fallback_model =
        std::env::var("HIVE_ANTHROPIC_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());

    // Connection settings: a persisted settings.json wins; on first launch we
    // seed it from env/config so existing `HIVE_*` workflows keep working. After
    // that the Settings UI edits the file live and the sync loop polls it.
    let env_seed = LiveSettings {
        relay_url: std::env::var("HIVE_RELAY_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .or(config_relay_endpoint),
        relay_access_token: std::env::var("HIVE_RELAY_ACCESS_TOKEN")
            .ok()
            .filter(|s| !s.is_empty()),
        sync_room: std::env::var("HIVE_WORKSPACE")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(default_sync_room),
        workspace_passphrase: std::env::var("HIVE_WORKSPACE_KEY")
            .ok()
            .filter(|s| !s.is_empty()),
        api_key: std::env::var("ANTHROPIC_API_KEY").ok().filter(|s| !s.is_empty()),
        claude_permission_mode: default_permission_mode(),
            claude_code_model: String::new(),
            summarize_prompt: None,
            compact_prompt: None,
            default_model: None,
            default_runtime_id: None,
        local_workspace_id: None,
        joined_rooms: Vec::new(),
        git_email: None,
        p2p_secret: None,
        ka_secret: None,
        github_account: None,
        github_token: None,
        github_client_id: None,
        provider_keys: std::collections::HashMap::new(),
        provider_base_urls: std::collections::HashMap::new(),
        workspaces: Vec::new(),
        agent_templates: Vec::new(),
        local_workspace_icon: None,
        schedules: Vec::new(),
        mcp_oauth: std::collections::HashMap::new(),
    };
    let settings = Arc::new(Mutex::new(load_or_seed_settings(&data_dir, env_seed)));

    // A persisted default-model (set via the UI) overrides the env/compiled
    // default computed above.
    let fallback_model = settings
        .lock()
        .unwrap()
        .default_model
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(fallback_model);

    // A persisted default-runtime (set via the UI's "Set default") overrides the
    // config `default_runtime` computed above.
    let default_runtime_id = settings
        .lock()
        .unwrap()
        .default_runtime_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(default_runtime_id);

    // Carry the user's git email on their identity so commits made on their
    // behalf (on a host) are attributed to them.
    let mut service = service;
    service.set_author_git_email(settings.lock().unwrap().git_email.clone());

    // Stamp this device's X25519 key-agreement public key onto the local actor
    // so it rides in the roster — an owner seals a rotated workspace key to each
    // member's device when revoking access (see e2ee + rotate_workspace_key).
    {
        let mut s = settings.lock().unwrap();
        let kp = match s.ka_secret.as_deref().and_then(parse_hex32) {
            Some(seed) => hive_core::e2ee::KeyAgreementKeypair::from_seed(&seed).ok(),
            None => None,
        };
        let kp = kp.unwrap_or_else(|| {
            let kp = hive_core::e2ee::KeyAgreementKeypair::generate().expect("ka keypair");
            s.ka_secret = Some(hex32(&kp.seed_bytes()));
            save_settings(&data_dir, &s);
            kp
        });
        service.set_author_key_agreement_public(Some(kp.public_key_bytes().to_vec()));
    }

    // If signed in to GitHub, bind the local actor to that account identity so
    // the same person is recognized as one member across all their devices.
    if let Some(acct) = settings.lock().unwrap().github_account.clone() {
        service.set_author_account(acct.account_id().to_string(), acct.display_name(), acct.email.clone());
    }

    // Ensure a stable local workspace id, and record the configured room (if
    // any) as joined so its chats are scoped out of "My workspace".
    let local_workspace_id = {
        let mut s = settings.lock().unwrap();
        let id = *s.local_workspace_id.get_or_insert_with(Uuid::new_v4);
        let relay_on = s.relay_url.as_ref().map(|u| !u.is_empty()).unwrap_or(false);
        if relay_on {
            let room = s.sync_room.clone();
            if !s.joined_rooms.iter().any(|r| r == &room) {
                s.joined_rooms.push(room);
            }
            // Migrate the single legacy relay config into the workspaces list so
            // it shows in the rail alongside any others added later.
            if !s.workspaces.iter().any(|w| w.room == s.sync_room) {
                let conn = WorkspaceConn {
                    name: s.sync_room.clone(),
                    relay_url: s.relay_url.clone().unwrap_or_default(),
                    room: s.sync_room.clone(),
                    key: s.workspace_passphrase.clone(),
                    icon: None,
                    dm_account: None,
                };
                s.workspaces.push(conn);
            }
        }
        save_settings(&data_dir, &s);
        id
    };

    Ok(AppState {
        service: Mutex::new(service),
        identity,
        data_dir,
        base_runtimes: Mutex::new(base_runtimes),
        managed_runtimes: Mutex::new(managed_runtimes),
        default_runtime_id: Mutex::new(default_runtime_id),
        fallback_model: Mutex::new(fallback_model),
        device_name: stored.device.device_name.clone(),
        local_workspace_id,
        active_workspace: Mutex::new(local_workspace_id),
        workspace_root: Mutex::new(workspace_root),
        summary_cache: Mutex::new(HashMap::new()),
        vault_cache: Mutex::new(HashMap::new()),
        base_mcp: Mutex::new(base_mcp),
        managed_mcp: Mutex::new(managed_mcp),
        db_path,
        settings,
        responding: Mutex::new(std::collections::HashSet::new()),
        notified: Mutex::new(std::collections::HashSet::new()),
        run_wakers: Mutex::new(HashMap::new()),
        gate_runs: Mutex::new(HashMap::new()),
        canceled_runs: Mutex::new(std::collections::HashSet::new()),
    })
}

/// Background relay sync: every few seconds, push new local envelopes and pull
/// remote ones into a dedicated SQLite connection (WAL — the live UI connection
/// sees the ingested rows). Emits `workspace://synced` when remote events land
/// so the frontend refetches. This is the relay-forwarding multiuser path.
/// Highest-version workspace key from `rotations` that `kp` can open. Returns
/// `None` if none are sealed to this device — which is exactly what locks out a
/// removed member (they can't open rotations issued after their removal).
fn latest_openable_key(
    kp: &hive_core::e2ee::KeyAgreementKeypair,
    rotations: &[hive_core::e2ee::WorkspaceKeyRotation],
) -> Option<[u8; 32]> {
    let mut best: Option<(u32, [u8; 32])> = None;
    for r in rotations {
        for blob in r.sealed.values() {
            if let Ok(bytes) = hive_core::e2ee::open(kp, blob) {
                if bytes.len() == 32 {
                    let mut k = [0u8; 32];
                    k.copy_from_slice(&bytes);
                    if best.map(|(v, _)| r.version > v).unwrap_or(true) {
                        best = Some((r.version, k));
                    }
                    break;
                }
            }
        }
    }
    best.map(|(_, k)| k)
}

// ---------------------------------------------------------------------------
// Scheduled / triggered agents
// ---------------------------------------------------------------------------

#[tauri::command]
fn list_schedules(state: State<AppState>) -> Vec<ScheduledAgentConfig> {
    state.settings.lock().unwrap().schedules.clone()
}

#[tauri::command]
fn add_schedule(
    state: State<AppState>,
    label: String,
    prompt: String,
    runtime_id: Option<String>,
    workspace_id: Option<String>,
    trigger: hive_core::ScheduleTrigger,
) -> Result<ScheduledAgentConfig, String> {
    trigger.validate()?;
    if prompt.trim().is_empty() {
        return Err("a prompt is required".into());
    }
    let workspace_id = match workspace_id {
        Some(s) if !s.is_empty() => Some(Uuid::parse_str(&s).map_err(map_err)?),
        _ => None,
    };
    let entry = ScheduledAgentConfig {
        id: Uuid::new_v4().to_string(),
        enabled: true,
        label: label.trim().to_string(),
        workspace_id,
        runtime_id: runtime_id.unwrap_or_default(),
        prompt,
        trigger,
        last_run: None,
    };
    let mut s = state.settings.lock().unwrap();
    s.schedules.push(entry.clone());
    save_settings(&state.data_dir, &s);
    Ok(entry)
}

#[tauri::command]
fn remove_schedule(state: State<AppState>, id: String) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    s.schedules.retain(|x| x.id != id);
    save_settings(&state.data_dir, &s);
    Ok(())
}

#[tauri::command]
fn set_schedule_enabled(state: State<AppState>, id: String, enabled: bool) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    if let Some(e) = s.schedules.iter_mut().find(|x| x.id == id) {
        e.enabled = enabled;
    }
    save_settings(&state.data_dir, &s);
    Ok(())
}

/// Background scheduler: every 30s, fire any due scheduled agents. Each fire
/// opens a fresh chat in the target workspace, posts the prompt, and runs one
/// turn through the normal dispatch path. `last_run` is stamped + persisted so
/// each schedule fires once per due window, even across restarts. Idle when no
/// schedules are configured. DailyAt triggers are evaluated in UTC.
async fn run_scheduler_loop(app: AppHandle, settings: Arc<Mutex<LiveSettings>>, data_dir: PathBuf) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        tick.tick().await;
        let now = hive_core::Timestamp::now().inner();
        // Snapshot due schedules; never hold the settings lock across dispatch.
        let due: Vec<ScheduledAgentConfig> = {
            let s = settings.lock().unwrap();
            s.schedules
                .iter()
                .filter(|sc| sc.enabled)
                .filter(|sc| sc.trigger.is_due(sc.last_run.map(|t| t.inner()), now))
                .cloned()
                .collect()
        };
        for sched in due {
            if let Err(e) = fire_schedule(&app, &sched).await {
                eprintln!("scheduler: schedule '{}' failed: {e}", sched.label);
            }
            // Stamp last_run even on failure, so a broken schedule doesn't
            // hot-loop every tick.
            {
                let mut s = settings.lock().unwrap();
                if let Some(entry) = s.schedules.iter_mut().find(|x| x.id == sched.id) {
                    entry.last_run = Some(hive_core::Timestamp::now());
                }
                save_settings(&data_dir, &s);
            }
        }
    }
}

/// Run one scheduled fire: open a chat, record the prompt as the user turn,
/// then take a single turn with the workspace's responder.
async fn fire_schedule(app: &AppHandle, sched: &ScheduledAgentConfig) -> Result<(), String> {
    let state = app.state::<AppState>();
    let workspace_id = sched.workspace_id.unwrap_or(state.local_workspace_id);
    let runtime_id = if sched.runtime_id.is_empty() {
        state.current_default_runtime_id()
    } else {
        sched.runtime_id.clone()
    };
    let title = if sched.label.trim().is_empty() {
        "Scheduled run".to_string()
    } else {
        sched.label.clone()
    };
    let session_id = {
        let mut svc = state.service.lock().unwrap();
        let session = svc.create_chat(title, workspace_id, &runtime_id).map_err(map_err)?;
        let sid = session.id;
        svc.post_user_message(sid, workspace_id, &sched.prompt).map_err(map_err)?;
        sid
    };
    let session = {
        let svc = state.service.lock().unwrap();
        svc.load(session_id).map_err(map_err)?.ok_or("scheduled session vanished")?
    };
    let responder = responder_for(&state, &session, None);
    let _ = run_turn(app, &state, session_id, workspace_id, &responder).await?;
    let _ = app.emit("workspace://synced", 1);
    Ok(())
}

async fn run_sync_loop(app: AppHandle, settings: Arc<Mutex<LiveSettings>>, db_path: PathBuf) {
    let mut store = match EventStore::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sync: failed to open store: {e}");
            return;
        }
    };
    // This device's key-agreement keypair — used to open sealed key rotations.
    let ka = {
        let s = settings.lock().unwrap();
        s.ka_secret
            .as_deref()
            .and_then(parse_hex32)
            .and_then(|seed| hive_core::e2ee::KeyAgreementKeypair::from_seed(&seed).ok())
    };
    // The engine is rebuilt whenever the relay url / room / key changes, so the
    // Settings UI can connect, reconnect, or go local-only without a restart.
    type ConnSig = (String, String, Option<[u8; 32]>);
    let mut engine: Option<(hive_runtime::SyncEngine, ConnSig)> = None;
    // Suppress repeated identical sync errors so a persistent condition (e.g. an
    // unauthorized relay) logs once, not on every tick.
    let mut last_sync_err: Option<String> = None;
    loop {
        let (relay_url, room, passphrase_key, access_token, github_token) = {
            let s = settings.lock().unwrap();
            (
                s.relay_url.clone(),
                s.sync_room.clone(),
                s.workspace_key(),
                s.relay_access_token.clone(),
                s.github_token.clone(),
            )
        };
        match relay_url {
            Some(url) if !url.is_empty() => {
                // Adopt the newest rotation we can open; fall back to the
                // passphrase-derived key. A revoked member can't open rotations
                // issued after their removal, so they can't read new traffic.
                let key = match ka.as_ref() {
                    Some(kp) => hive_runtime::RelayClient::new(&url)
                        .with_auth(access_token.clone())
                        .with_github_token(github_token.clone())
                        .fetch_key_rotations(&room)
                        .await
                        .ok()
                        .and_then(|rots| latest_openable_key(kp, &rots))
                        .or(passphrase_key),
                    None => passphrase_key,
                };
                let sig: ConnSig = (url.clone(), room.clone(), key);
                let changed = engine.as_ref().map(|(_, s)| s != &sig).unwrap_or(true);
                if changed {
                    let mut e = hive_runtime::SyncEngine::new(
                        hive_runtime::RelayClient::new(&url)
                            .with_auth(access_token.clone())
                            .with_github_token(github_token.clone()),
                        room.clone(),
                    );
                    if let Some(k) = key {
                        e = e.with_key(k);
                    }
                    eprintln!(
                        "sync: relay {url} room {room} ({})",
                        if key.is_some() { "encrypted" } else { "plaintext" }
                    );
                    engine = Some((e, sig));
                }
                if let Some((eng, _)) = engine.as_mut() {
                    // Split steps keep the (non-Send) store out of any `.await`.
                    let outcome: Result<usize, hive_runtime::SyncError> = async {
                        let to_push = eng.take_unpushed(&store)?;
                        eng.push_envelopes(&to_push).await?;
                        let fetched = eng.fetch_new().await?;
                        eng.apply_fetched(&mut store, &fetched)
                    }
                    .await;
                    match outcome {
                        Ok(pulled) => {
                            if last_sync_err.take().is_some() {
                                eprintln!("sync: recovered");
                            }
                            if pulled > 0 {
                                let _ = app.emit("workspace://synced", pulled);
                            }
                        }
                        Err(e) => {
                            // Log a persistent error only once (until it changes
                            // or recovers). Unauthorized gets an actionable line.
                            let msg = match &e {
                                hive_runtime::SyncError::Relay(
                                    hive_runtime::RelayError::Unauthorized,
                                ) => "sync paused: the relay rejected this device's access token \
                                      — add yourself in Settings → Team (or clear the relay URL \
                                      to work local-only)"
                                    .to_string(),
                                other => format!("sync error: {other}"),
                            };
                            if last_sync_err.as_deref() != Some(msg.as_str()) {
                                eprintln!("{msg}");
                                last_sync_err = Some(msg);
                            }
                        }
                    }
                }
            }
            // No relay configured: idle (local-only) until the UI sets one.
            _ => engine = None,
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}

/// Sync status surfaced to the Settings pane.
#[derive(serde::Serialize, Clone)]
struct SyncStatus {
    relay_configured: bool,
    relay_url: String,
    room: String,
    encrypted: bool,
}

#[tauri::command]
fn sync_status(state: State<AppState>) -> SyncStatus {
    let s = state.settings.lock().unwrap();
    SyncStatus {
        relay_configured: s.relay_url.as_ref().map(|u| !u.is_empty()).unwrap_or(false),
        relay_url: s.relay_url.clone().unwrap_or_default(),
        room: s.sync_room.clone(),
        encrypted: s.workspace_key().is_some(),
    }
}

/// Result of a live relay connectivity + auth probe (Settings "Test connection").
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RelayProbeDto {
    /// `ok` | `unauthorized` | `httpError` | `unreachable` | `unconfigured`.
    status: String,
    detail: String,
}

/// Probe a specific relay URL + token (the caller's GitHub token is attached
/// from settings). Shared by `probe_relay` (configured) and `probe_relay_at`
/// (a URL/token being entered in onboarding/settings, before it's saved).
async fn probe_relay_inner(
    url: String,
    token: Option<String>,
    github: Option<String>,
    room: String,
) -> RelayProbeDto {
    if url.trim().is_empty() {
        return RelayProbeDto {
            status: "unconfigured".into(),
            detail: "No relay URL set — you're local-only.".into(),
        };
    }
    let workspace = if room.trim().is_empty() { "_probe".to_string() } else { room };
    let probe = hive_runtime::RelayClient::new(url)
        .with_auth(token)
        .with_github_token(github)
        .probe(&workspace)
        .await;
    match probe {
        hive_runtime::RelayProbe::Ok => RelayProbeDto {
            status: "ok".into(),
            detail: "Reachable and the access token was accepted.".into(),
        },
        hive_runtime::RelayProbe::Unauthorized => RelayProbeDto {
            status: "unauthorized".into(),
            detail: "Reached the relay, but it rejected the access token.".into(),
        },
        hive_runtime::RelayProbe::HttpStatus(c) => RelayProbeDto {
            status: "httpError".into(),
            detail: format!("Relay returned HTTP {c}. Check the URL — use the base origin, no /v1."),
        },
        hive_runtime::RelayProbe::Unreachable(e) => RelayProbeDto {
            status: "unreachable".into(),
            detail: format!("Couldn't reach the relay: {e}"),
        },
    }
}

/// Actually hit the configured relay and report whether it's reachable and the
/// token is accepted — distinct from `sync_status`, which only reflects whether
/// a URL is set, not whether it works.
#[tauri::command]
async fn probe_relay(state: State<'_, AppState>) -> Result<RelayProbeDto, String> {
    let (url, token, github, room) = {
        let s = state.settings.lock().unwrap();
        (
            s.relay_url.clone().unwrap_or_default(),
            s.relay_access_token.clone(),
            s.github_token.clone(),
            s.sync_room.clone(),
        )
    };
    Ok(probe_relay_inner(url, token, github, room).await)
}

/// Probe a URL + token the user is *entering* (onboarding / Settings), before
/// committing it — so the flow can validate a relay connection and only save a
/// working one. The GitHub identity comes from settings.
#[tauri::command]
async fn probe_relay_at(
    state: State<'_, AppState>,
    url: String,
    access_token: Option<String>,
) -> Result<RelayProbeDto, String> {
    let (github, room) = {
        let s = state.settings.lock().unwrap();
        (s.github_token.clone(), s.sync_room.clone())
    };
    Ok(probe_relay_inner(url, access_token.filter(|t| !t.trim().is_empty()), github, room).await)
}

/// The selectable workspaces: always "My workspace" (local), plus the joined
/// relay room when one is configured. `active` marks the one the chat list is
/// currently scoped to.
#[tauri::command]
fn list_workspaces(state: State<AppState>) -> Vec<WorkspaceInfoDto> {
    let active = state.active_workspace_id();
    let s = state.settings.lock().unwrap();
    let mut out = vec![WorkspaceInfoDto {
        id: state.local_workspace_id.to_string(),
        name: "My workspace".to_string(),
        kind: "local".to_string(),
        active: active == state.local_workspace_id,
        icon_url: s.local_workspace_icon.clone(),
    }];
    for w in &s.workspaces {
        // DMs live in the Friends section, not the workspace rail.
        if w.dm_account.is_some() {
            continue;
        }
        let id = w.id();
        out.push(WorkspaceInfoDto {
            id: id.to_string(),
            name: w.display_name(),
            kind: "room".to_string(),
            active: active == id,
            icon_url: w.icon.clone(),
        });
    }
    out
}

/// Switch which workspace the chat list is scoped to. Unknown ids fall back to
/// the local workspace.
#[tauri::command]
fn set_active_workspace(state: State<AppState>, workspace_id: String) -> Result<(), String> {
    let id = Uuid::parse_str(&workspace_id).map_err(map_err)?;
    if id == state.local_workspace_id {
        *state.active_workspace.lock().unwrap() = state.local_workspace_id;
        return Ok(());
    }
    // Selecting a team workspace points the live sync fields (relay/room/key) at
    // it, so the background sync loop reconnects to that room within ~3s.
    let mut s = state.settings.lock().unwrap();
    if let Some(conn) = s.workspaces.iter().find(|w| w.id() == id).cloned() {
        s.relay_url = (!conn.relay_url.trim().is_empty()).then(|| conn.relay_url.clone());
        s.sync_room = conn.room.clone();
        s.workspace_passphrase = conn.key.clone();
        save_settings(&state.data_dir, &s);
        drop(s);
        *state.active_workspace.lock().unwrap() = id;
    } else {
        drop(s);
        *state.active_workspace.lock().unwrap() = state.local_workspace_id;
    }
    Ok(())
}

/// Set (or clear, with `None`) a workspace's icon. `icon` must be a small
/// `data:image/…` URL; pass `None` to revert to the default mark/initials.
///
/// Today the icon is stored on this device (local rail decoration). When a team
/// workspace gains shared, role-gated icons over the relay, this command becomes
/// the owner/admin-only mutation that propagates the change to every member.
#[tauri::command]
fn set_workspace_icon(
    state: State<AppState>,
    workspace_id: String,
    icon: Option<String>,
) -> Result<(), String> {
    let id = Uuid::parse_str(&workspace_id).map_err(map_err)?;
    // Validate: must be a data: image URL, and small enough to live in settings.
    if let Some(url) = &icon {
        if !url.starts_with("data:image/") {
            return Err("icon must be a data:image/… URL".to_string());
        }
        // ~512 KB of base64 ≈ 384 KB raw; plenty for a rail glyph, bounds bloat.
        if url.len() > 512 * 1024 {
            return Err("icon is too large (max ~512 KB); use a smaller image".to_string());
        }
    }
    let mut s = state.settings.lock().unwrap();
    if id == state.local_workspace_id {
        s.local_workspace_icon = icon;
    } else if let Some(conn) = s.workspaces.iter_mut().find(|w| w.id() == id) {
        conn.icon = icon;
    } else {
        return Err("unknown workspace".to_string());
    }
    save_settings(&state.data_dir, &s);
    Ok(())
}

/// Random hex token (CSPRNG via uuid v4) for a fresh room suffix / workspace
/// key, so two "Team" workspaces don't collide and rooms aren't guessable.
fn random_token(len: usize) -> String {
    let mut s = String::new();
    while s.len() < len {
        s.push_str(&Uuid::new_v4().simple().to_string());
    }
    s.truncate(len);
    s
}

fn slugify(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() { "workspace".to_string() } else { s }
}

/// Create a new team workspace: generates a unique room + E2EE key, adds it to
/// the rail, switches to it, and (if a relay is configured) starts syncing it.
/// Returns the new workspace; use `workspace_invite` to share it.
#[tauri::command]
fn create_workspace(state: State<AppState>, name: String) -> Result<WorkspaceInfoDto, String> {
    let display = name.trim();
    if display.is_empty() {
        return Err("workspace name can't be empty".to_string());
    }
    let conn = {
        let mut s = state.settings.lock().unwrap();
        let room = format!("{}-{}", slugify(display), random_token(6));
        let conn = WorkspaceConn {
            name: display.to_string(),
            // Inherit the currently-configured relay so the new room syncs
            // immediately; if none is set the workspace is created local-until a
            // relay is added (Settings) or a peer is linked.
            relay_url: s.relay_url.clone().unwrap_or_default(),
            room,
            key: Some(random_token(24)),
            icon: None,
            dm_account: None,
        };
        s.workspaces.push(conn.clone());
        save_settings(&state.data_dir, &s);
        conn
    };
    set_active_workspace(state.clone(), conn.id().to_string())?;
    Ok(WorkspaceInfoDto {
        id: conn.id().to_string(),
        name: conn.display_name(),
        kind: "room".to_string(),
        active: true,
        icon_url: conn.icon.clone(),
    })
}

/// Join a team workspace from an invite code (`hivews1:…`). Adds it to the rail
/// and switches to it.
#[tauri::command]
fn join_workspace(state: State<AppState>, invite: String) -> Result<WorkspaceInfoDto, String> {
    let conn = decode_workspace_invite(&invite)?;
    {
        let mut s = state.settings.lock().unwrap();
        if let Some(existing) = s.workspaces.iter_mut().find(|w| w.room == conn.room) {
            *existing = conn.clone(); // refresh relay/key from the invite
        } else {
            s.workspaces.push(conn.clone());
        }
        save_settings(&state.data_dir, &s);
    }
    set_active_workspace(state.clone(), conn.id().to_string())?;
    Ok(WorkspaceInfoDto {
        id: conn.id().to_string(),
        name: conn.display_name(),
        kind: "room".to_string(),
        active: true,
        icon_url: conn.icon.clone(),
    })
}

/// Shareable invite code for a workspace (bundles relay + room + key).
#[tauri::command]
fn workspace_invite(state: State<AppState>, workspace_id: String) -> Result<String, String> {
    let id = Uuid::parse_str(&workspace_id).map_err(map_err)?;
    let s = state.settings.lock().unwrap();
    let conn = s
        .workspaces
        .iter()
        .find(|w| w.id() == id)
        .ok_or_else(|| "unknown workspace".to_string())?;
    Ok(encode_workspace_invite(conn))
}

/// Leave a team workspace (removes it from the rail). If it was active, falls
/// back to "My workspace".
#[tauri::command]
fn remove_workspace(state: State<AppState>, workspace_id: String) -> Result<(), String> {
    let id = Uuid::parse_str(&workspace_id).map_err(map_err)?;
    {
        let mut s = state.settings.lock().unwrap();
        s.workspaces.retain(|w| w.id() != id);
        save_settings(&state.data_dir, &s);
    }
    if state.active_workspace_id() == id {
        set_active_workspace(state.clone(), state.local_workspace_id.to_string())?;
    }
    Ok(())
}

/// Connection settings surfaced to the Settings pane. Secrets are never sent to
/// the frontend — only whether they're configured.
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ConnectionSettings {
    relay_url: String,
    room: String,
    has_workspace_key: bool,
    has_api_key: bool,
    /// Whether a relay access token (for a gated/paid hosted relay) is set.
    /// Self-hosted/open relays leave this off.
    has_relay_access_token: bool,
    permission_mode: String,
}

#[tauri::command]
fn get_connection_settings(state: State<AppState>) -> ConnectionSettings {
    let s = state.settings.lock().unwrap();
    ConnectionSettings {
        relay_url: s.relay_url.clone().unwrap_or_default(),
        room: s.sync_room.clone(),
        has_workspace_key: s.workspace_key().is_some(),
        has_api_key: s.api_key.as_ref().map(|k| !k.is_empty()).unwrap_or(false),
        has_relay_access_token: s
            .relay_access_token
            .as_ref()
            .map(|t| !t.is_empty())
            .unwrap_or(false),
        permission_mode: s.claude_permission_mode.clone(),
    }
}

/// Update connection settings and persist them. `workspace_key`/`api_key` use
/// null = leave unchanged, "" = clear, value = set (so the form needn't echo
/// secrets back). The sync loop picks up relay/room/key changes within ~3s.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn update_connection_settings(
    state: State<AppState>,
    relay_url: String,
    room: String,
    workspace_key: Option<String>,
    api_key: Option<String>,
    relay_access_token: Option<String>,
    permission_mode: String,
) -> ConnectionSettings {
    {
        let mut s = state.settings.lock().unwrap();
        let trimmed = relay_url.trim();
        s.relay_url = (!trimmed.is_empty()).then(|| trimmed.to_string());
        if let Some(t) = relay_access_token {
            let t = t.trim();
            s.relay_access_token = (!t.is_empty()).then(|| t.to_string());
        }
        let room = room.trim();
        s.sync_room = if room.is_empty() {
            default_sync_room()
        } else {
            room.to_string()
        };
        if let Some(k) = workspace_key {
            s.workspace_passphrase = (!k.is_empty()).then_some(k);
        }
        if let Some(k) = api_key {
            s.api_key = (!k.is_empty()).then_some(k);
        }
        s.claude_permission_mode = match permission_mode.as_str() {
            "acceptEdits" => "acceptEdits".to_string(),
            "bypassPermissions" => "bypassPermissions".to_string(),
            _ => "default".to_string(),
        };
        // Remember the room as joined so its chats are scoped to that room (and
        // out of "My workspace") even after disconnecting.
        let relay_on = s.relay_url.as_ref().map(|u| !u.is_empty()).unwrap_or(false);
        if relay_on && !s.joined_rooms.iter().any(|r| r == &s.sync_room) {
            let room = s.sync_room.clone();
            s.joined_rooms.push(room);
        }
        save_settings(&state.data_dir, &s);
    }
    get_connection_settings(state)
}

/// Set the model for the Primary Runtime (the default when no runtime is
/// configured) — e.g. an Anthropic model id. Persisted; applies immediately.
#[tauri::command]
fn set_default_model(state: State<AppState>, model: String) -> Result<(), String> {
    let model = model.trim().to_string();
    if model.is_empty() {
        return Err("model can't be empty".into());
    }
    *state.fallback_model.lock().unwrap() = model.clone();
    let mut s = state.settings.lock().unwrap();
    s.default_model = Some(model);
    save_settings(&state.data_dir, &s);
    Ok(())
}

/// The selected Claude Code model alias (`--model`); empty = the CLI default.
#[tauri::command]
fn get_claude_code_model(state: State<AppState>) -> String {
    state.settings.lock().unwrap().claude_code_model.clone()
}

/// Set the Claude Code model alias (e.g. "sonnet" | "opus" | "haiku" | full id;
/// empty = the CLI default). Applies to the next turn — no restart needed.
#[tauri::command]
fn set_claude_code_model(state: State<AppState>, model: String) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    s.claude_code_model = model.trim().to_string();
    save_settings(&state.data_dir, &s);
    Ok(())
}

/// A model the user can pick for the local Claude Code CLI: the built-in aliases
/// plus whatever Claude Code has cached as available for this account.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeModelOption {
    /// Passed to `claude --model` verbatim — an alias ("opus") or a full value
    /// ("claude-fable-5[1m]"). Empty = the CLI's own default.
    value: String,
    label: String,
    description: Option<String>,
}

/// Models to offer for the local `claude` CLI. The base aliases (which the CLI
/// always understands) merged with `additionalModelOptionsCache` from
/// `~/.claude.json` — the same account-specific list Claude Code's own `/model`
/// picker shows, so new models (Fable, …) appear without a Hive release. That
/// file is an undocumented CLI cache; if it's absent or its shape changes we
/// just return the base aliases.
#[tauri::command]
fn list_claude_code_models() -> Vec<ClaudeModelOption> {
    let base = |value: &str, label: &str| ClaudeModelOption {
        value: value.to_string(),
        label: label.to_string(),
        description: None,
    };
    let mut opts = vec![
        base("", "Default (CLI's own setting)"),
        base("sonnet", "Sonnet"),
        base("opus", "Opus"),
        base("haiku", "Haiku"),
    ];
    for extra in claude_cached_model_options() {
        if !opts.iter().any(|o| o.value == extra.value) {
            opts.push(extra);
        }
    }
    opts
}

/// Parse `additionalModelOptionsCache` out of `~/.claude.json`. Best-effort:
/// any read/parse/shape failure yields an empty list (callers fall back to the
/// base aliases).
fn claude_cached_model_options() -> Vec<ClaudeModelOption> {
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else {
        return Vec::new();
    };
    let path = PathBuf::from(home).join(".claude.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(arr) = json.get("additionalModelOptionsCache").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            let value = item.get("value")?.as_str()?.trim().to_string();
            if value.is_empty() {
                return None;
            }
            let label = item
                .get("label")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| value.clone());
            let description = item
                .get("description")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            Some(ClaudeModelOption { value, label, description })
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn build_tray_icon(app: &AppHandle) -> Result<TrayIcon<tauri::Wry>, Box<dyn std::error::Error>> {
    let show_item = MenuItem::with_id(app, "tray-show", "Show Hive", true, None::<&str>)?;
    let friends_item = MenuItem::with_id(app, "tray-friends", "Friends", true, None::<&str>)?;
    let team_item =
        MenuItem::with_id(app, "tray-team", "Team & Relay Sync…", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "tray-settings", "Settings…", true, None::<&str>)?;
    let sep_top = PredefinedMenuItem::separator(app)?;
    let sep_bottom = PredefinedMenuItem::separator(app)?;
    let quit_item = PredefinedMenuItem::quit(app, Some("Quit Hive"))?;
    let menu = Menu::with_items(
        app,
        &[
            &show_item,
            &sep_top,
            &friends_item,
            &team_item,
            &settings_item,
            &sep_bottom,
            &quit_item,
        ],
    )?;

    // macOS/Linux use the monochrome menu-bar template; Windows' notification
    // area renders a full-color icon that fills the slot, so a larger,
    // non-template app icon reads better (and bigger) there.
    #[cfg(target_os = "windows")]
    let (icon, as_template) = (Image::from_bytes(include_bytes!("../icons/64x64.png"))?, false);
    #[cfg(not(target_os = "windows"))]
    let (icon, as_template) = (
        Image::from_bytes(include_bytes!("../icons/trayTemplate@2x.png"))?,
        true,
    );

    let tray = TrayIconBuilder::with_id("hive-tray")
        .icon(icon)
        .icon_as_template(as_template)
        .show_menu_on_left_click(false)
        .tooltip("Hive")
        .menu(&menu)
        .on_menu_event(|app, event| {
            // Bring the single main window forward, then (optionally) tell the
            // frontend which view to route to. The window is the same one for
            // every item — Settings/Friends are in-window views, not OS windows.
            let focus_and_route = |route: &str| {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
                if !route.is_empty() {
                    let _ = app.emit("tray://navigate", route);
                }
            };
            match event.id().as_ref() {
                "tray-show" => focus_and_route(""),
                "tray-friends" => focus_and_route("friends"),
                "tray-team" => focus_and_route("settings:Team"),
                "tray-settings" => focus_and_route("settings"),
                "quit" => app.exit(0),
                _ => {}
            }
        })
        .build(app)?;

    Ok(tray)
}

// ---------------------------------------------------------------------------
// Direct peer-to-peer (iroh) — connect to a "friend" by their peer code and
// sync signed envelopes directly, device-to-device.
// ---------------------------------------------------------------------------

fn hex32(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Get-or-create this device's stable 32-byte P2P secret (persisted in settings).
fn ensure_p2p_secret(state: &AppState) -> [u8; 32] {
    let mut s = state.settings.lock().unwrap();
    if let Some(b) = s.p2p_secret.as_deref().and_then(parse_hex32) {
        return b;
    }
    let mut b = [0u8; 32];
    b[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    b[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    s.p2p_secret = Some(hex32(&b));
    save_settings(&state.data_dir, &s);
    b
}


#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct ContactsFile {
    #[serde(default)]
    contacts: Vec<hive_runtime::peer::Contact>,
}

fn contacts_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("contacts.json")
}

fn load_contacts(data_dir: &std::path::Path) -> Vec<hive_runtime::peer::Contact> {
    std::fs::read_to_string(contacts_path(data_dir))
        .ok()
        .and_then(|t| serde_json::from_str::<ContactsFile>(&t).ok())
        .map(|f| f.contacts)
        .unwrap_or_default()
}

fn save_contacts(data_dir: &std::path::Path, contacts: &[hive_runtime::peer::Contact]) {
    if let Ok(bytes) = serde_json::to_vec_pretty(&ContactsFile { contacts: contacts.to_vec() }) {
        let _ = std::fs::write(contacts_path(data_dir), bytes);
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ContactDto {
    peer_id: String,
    label: String,
}

/// This device's shareable friend code (its P2P public key).
#[tauri::command]
fn p2p_my_code(state: State<AppState>) -> Result<String, String> {
    #[cfg(feature = "p2p")]
    {
        let bytes = ensure_p2p_secret(&state);
        Ok(hive_runtime::peer_iroh::code_for_secret(bytes))
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = &state;
        Err("This build was compiled without P2P support.".to_string())
    }
}

#[tauri::command]
fn p2p_list_contacts(state: State<AppState>) -> Vec<ContactDto> {
    load_contacts(&state.data_dir)
        .into_iter()
        .map(|c| ContactDto { peer_id: c.peer_id.0, label: c.label })
        .collect()
}

#[tauri::command]
fn p2p_add_contact(state: State<AppState>, code: String, label: String) -> Result<(), String> {
    let peer_id = hive_runtime::peer::PeerId::from_code(&code)
        .ok_or_else(|| "Not a valid Hive friend code.".to_string())?;
    let mut contacts = load_contacts(&state.data_dir);
    if contacts.iter().any(|c| c.peer_id == peer_id) {
        return Err("That peer is already a contact.".to_string());
    }
    contacts.push(hive_runtime::peer::Contact { peer_id, label: label.trim().to_string() });
    save_contacts(&state.data_dir, &contacts);
    Ok(())
}

#[tauri::command]
fn p2p_remove_contact(state: State<AppState>, peer_id: String) -> Result<(), String> {
    let mut contacts = load_contacts(&state.data_dir);
    contacts.retain(|c| c.peer_id.0 != peer_id);
    save_contacts(&state.data_dir, &contacts);
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ShortCodeDto {
    /// The short, human-typeable code (e.g. "K7P2QX").
    code: String,
    /// Seconds until it expires.
    expires_in: u64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RedeemResultDto {
    /// "peer" | "workspace" — what the code resolved to.
    kind: String,
    /// A human label for what was added/joined.
    label: String,
}

/// The configured relay base URL, or a helpful error. Short codes are brokered
/// through the relay (it only relays the handoff, not P2P traffic).
fn configured_relay(state: &AppState) -> Result<String, String> {
    let s = state.settings.lock().unwrap();
    s.relay_url
        .clone()
        .filter(|u| !u.trim().is_empty())
        .ok_or_else(|| {
            "Set a relay URL in Settings → Connection first — short codes are brokered through it."
                .to_string()
        })
}

// ---------------------------------------------------------------------------
// Commands — GitHub sign-in (device flow). The GitHub user is the Hive account;
// the same person on multiple devices resolves to one account id.
// ---------------------------------------------------------------------------

/// Resolve the OAuth App client id, in priority order:
///   1. runtime env `HIVE_GITHUB_CLIENT_ID`
///   2. the per-device setting (pasted in the UI)
///   3. a value baked at build time (compile with `HIVE_GITHUB_CLIENT_ID` set to
///      ship Hive's own OAuth App so users never create one).
fn github_client_id(state: &AppState) -> Option<String> {
    std::env::var("HIVE_GITHUB_CLIENT_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            state
                .settings
                .lock()
                .unwrap()
                .github_client_id
                .clone()
                .filter(|s| !s.trim().is_empty())
        })
        // Baked at compile time by build.rs from env or the gitignored
        // `app/github_client_id` file (empty in forks without it).
        .or_else(|| option_env!("HIVE_GITHUB_CLIENT_ID").map(str::to_string).filter(|s| !s.is_empty()))
}

#[tauri::command]
fn set_github_client_id(state: State<AppState>, client_id: String) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    let c = client_id.trim().to_string();
    s.github_client_id = (!c.is_empty()).then_some(c);
    save_settings(&state.data_dir, &s);
    Ok(())
}

/// The signed-in GitHub account, if any (safe to expose; no token).
#[tauri::command]
fn github_account(state: State<AppState>) -> Option<hive_runtime::github::GithubAccount> {
    state.settings.lock().unwrap().github_account.clone()
}

/// Whether a client id is configured (so the UI can prompt to set one).
#[tauri::command]
fn github_client_configured(state: State<AppState>) -> bool {
    github_client_id(&state).is_some()
}

/// Step 1: begin the device flow — returns the code + URL to show the user.
#[tauri::command]
async fn github_login_start(
    state: State<'_, AppState>,
) -> Result<hive_runtime::github::DeviceStart, String> {
    let client_id = github_client_id(&state).ok_or_else(|| {
        "No GitHub client id. Create an OAuth App (enable Device Flow) and set its client id."
            .to_string()
    })?;
    hive_runtime::github::start_device_flow(&client_id)
        .await
        .map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GithubPollDto {
    /// "pending" | "slowDown" | "success" | "denied" | "expired"
    status: String,
    account: Option<hive_runtime::github::GithubAccount>,
}

/// Step 2: poll once. On success, persists the account + token and binds the
/// local actor to the GitHub account identity.
#[tauri::command]
async fn github_login_poll(
    state: State<'_, AppState>,
    device_code: String,
) -> Result<GithubPollDto, String> {
    use hive_runtime::github::PollOutcome;
    let client_id = github_client_id(&state).ok_or_else(|| "No GitHub client id.".to_string())?;
    let outcome = hive_runtime::github::poll_token(&client_id, &device_code)
        .await
        .map_err(|e| e.to_string())?;
    let token = match outcome {
        PollOutcome::Pending => return Ok(GithubPollDto { status: "pending".into(), account: None }),
        PollOutcome::SlowDown => return Ok(GithubPollDto { status: "slowDown".into(), account: None }),
        PollOutcome::Denied => return Ok(GithubPollDto { status: "denied".into(), account: None }),
        PollOutcome::Expired => return Ok(GithubPollDto { status: "expired".into(), account: None }),
        PollOutcome::Token(t) => t,
    };
    let account = hive_runtime::github::fetch_user(&token)
        .await
        .map_err(|e| e.to_string())?;
    {
        let mut s = state.settings.lock().unwrap();
        s.github_account = Some(account.clone());
        s.github_token = Some(token);
        // Adopt the GitHub account email as the git-commit email while signed in
        // (the field is locked in the UI; sign-out unlocks it). Only overwrite a
        // blank or previously-GitHub-derived value, never a hand-set one.
        if let Some(email) = account.email.as_deref().filter(|e| !e.is_empty()) {
            s.git_email = Some(email.to_string());
        }
        save_settings(&state.data_dir, &s);
    }
    {
        let mut svc = state.service.lock().unwrap();
        svc.set_author_account(
            account.account_id().to_string(),
            account.display_name(),
            account.email.clone(),
        );
        svc.set_author_git_email(account.email.clone().filter(|e| !e.is_empty()));
    }
    // Adopt the GitHub name as the local display name on sign-in, so the user
    // isn't left as the placeholder "You". Only when they haven't set a custom
    // name (still blank or the default) — never clobber a hand-picked one.
    let current = state.identity.load().ok().flatten().map(|s| s.account.display_name).unwrap_or_default();
    if current.trim().is_empty() || current.trim().eq_ignore_ascii_case("you") {
        let gh_name = account.display_name();
        if !gh_name.trim().is_empty() {
            let _ = state.identity.update_display_name(&gh_name);
            state.service.lock().unwrap().set_author_display_name(gh_name.trim());
        }
    }
    Ok(GithubPollDto { status: "success".into(), account: Some(account) })
}

/// This device's key-agreement public key (hex) + a short device id, if set up.
fn this_device_ka(state: &AppState) -> Option<(String, String)> {
    let s = state.settings.lock().unwrap();
    let kp = s
        .ka_secret
        .as_deref()
        .and_then(parse_hex32)
        .and_then(|seed| hive_core::e2ee::KeyAgreementKeypair::from_seed(&seed).ok())?;
    let hex = hex32(&kp.public_key_bytes());
    let device_id = hex[..16].to_string();
    Some((device_id, hex))
}

/// Register this device in the relay directory under the signed-in GitHub
/// account, so teammates can invite this account by handle and seal keys to it.
/// Best-effort: no-op (Ok) if not signed in or no relay.
#[tauri::command]
async fn directory_register(state: State<'_, AppState>) -> Result<(), String> {
    let relay = match configured_relay(&state) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    if state.github_token().is_none() {
        return Ok(());
    }
    let Some((device_id, ka_hex)) = this_device_ka(&state) else {
        return Ok(());
    };
    let client = state.relay_client(&relay);
    client
        .directory_register(&device_id, &ka_hex)
        .await
        .map_err(|e| e.to_string())?;
    // Also register in the social account registry (friends + presence). The
    // node id is wired later (P2P bootstrap); the label is the OS hostname.
    let label = hostname_label();
    let _ = client.account_register(&device_id, None, label.as_deref()).await;
    Ok(())
}

/// A friendly device label for the account registry (the machine's hostname).
fn hostname_label() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct InviteResultDto {
    login: String,
    /// Devices the workspace key was sealed to.
    devices: u32,
    /// Whether a workspace key was sealed (false for a plaintext room).
    sealed: bool,
}

/// Invite a GitHub user (by handle) to this chat's workspace: add them to the
/// roster and seal the current workspace key to *all* their devices via the
/// relay keyring, so any of their machines can read it.
#[tauri::command]
async fn invite_by_handle(
    state: State<'_, AppState>,
    session_id: String,
    handle: String,
) -> Result<InviteResultDto, String> {
    let sid = Uuid::parse_str(&session_id).map_err(map_err)?;
    let relay = configured_relay(&state)?;
    if state.github_token().is_none() {
        return Err("Sign in with GitHub first (Settings → Account).".to_string());
    }
    let client = state.relay_client(&relay);
    let entry = client
        .directory_lookup(&handle)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "@{} isn't on Hive yet — they need to sign in once.",
                handle.trim().trim_start_matches('@')
            )
        })?;
    let account_id = hive_runtime::github::account_id_for(entry.github_id);
    let recipients: Vec<(String, Vec<u8>)> = entry
        .devices
        .iter()
        .filter_map(|d| parse_hex32(&d.ka_public).map(|b| (d.device_id.clone(), b.to_vec())))
        .collect();

    // Add the invitee to the roster (keyed by their stable account id).
    {
        let mut svc = state.service.lock().unwrap();
        let next_index = svc
            .load(sid)
            .map_err(map_err)?
            .map(|s| s.members.iter().map(|m| m.index).max().unwrap_or(0) + 1)
            .unwrap_or(1);
        let member = WorkspaceMember {
            id: Uuid::new_v4().to_string(),
            actor: ActorIdentity {
                id: account_id.to_string(),
                display_name: entry.name.clone().unwrap_or_else(|| entry.login.clone()),
                kind: ActorKind::Human,
                account_id: Some(account_id),
                device_id: None,
                git_email: None,
                key_agreement_public: recipients.first().map(|(_, b)| b.clone()),
            },
            role: WorkspaceRole::Contributor,
            title: String::new(),
            index: next_index,
            joined_at: Default::default(),
        };
        svc.add_member(sid, state.active_workspace_id(), member).map_err(map_err)?;
    }

    // Seal the current workspace key to all their devices (keyring rotation).
    let mut sealed = false;
    if !recipients.is_empty() {
        let (ka, passphrase_key, room) = {
            let s = state.settings.lock().unwrap();
            let ka = s
                .ka_secret
                .as_deref()
                .and_then(parse_hex32)
                .and_then(|seed| hive_core::e2ee::KeyAgreementKeypair::from_seed(&seed).ok());
            (ka, s.workspace_key(), s.sync_room.clone())
        };
        let rotations = client.fetch_key_rotations(&room).await.unwrap_or_default();
        let current_key = ka
            .as_ref()
            .and_then(|kp| latest_openable_key(kp, &rotations))
            .or(passphrase_key);
        if let Some(key) = current_key {
            let version = rotations.iter().map(|r| r.version).max().unwrap_or(0) + 1;
            let rotation = hive_core::e2ee::WorkspaceKeyRotation::seal_for_devices(version, &key, &recipients)
                .map_err(|e| format!("{e:?}"))?;
            client.publish_key_rotation(&room, &rotation).await.map_err(|e| e.to_string())?;
            sealed = true;
        }
    }
    // Best-effort: also register them in server-side membership (no-op on open
    // relays). The relay keys membership on `github:<id>` (its caller identity),
    // distinct from the roster's UUID account id used for E2EE sealing.
    {
        let room = state.settings.lock().unwrap().sync_room.clone();
        let _ = client
            .upsert_member(&room, &format!("github:{}", entry.github_id), &entry.login, "contributor")
            .await;
    }

    Ok(InviteResultDto { login: entry.login, devices: recipients.len() as u32, sealed })
}

/// Claim the active team workspace's room on a membership-enforcing relay, making
/// this account the `Owner` and turning enforcement on. Best-effort: returns
/// `false` on an open/self-host relay (no membership) or if already claimed.
#[tauri::command]
async fn workspace_claim_membership(state: State<'_, AppState>) -> Result<bool, String> {
    let relay = configured_relay(&state)?;
    let room = state.settings.lock().unwrap().sync_room.clone();
    state.relay_client(&relay).claim_membership(&room).await.map_err(|e| e.to_string())
}

/// List the active workspace's server-side members (empty on open relays).
#[tauri::command]
async fn workspace_members(state: State<'_, AppState>) -> Result<Vec<hive_runtime::MemberEntry>, String> {
    let relay = configured_relay(&state)?;
    let room = state.settings.lock().unwrap().sync_room.clone();
    Ok(state
        .relay_client(&relay)
        .list_members(&room)
        .await
        .map_err(|e| e.to_string())?
        .unwrap_or_default())
}

/// Add a member (by GitHub handle) or change their role on the active workspace.
/// Caller must be `Admin`+. `role` = owner|admin|contributor|viewer.
#[tauri::command]
async fn workspace_add_member(
    state: State<'_, AppState>,
    handle: String,
    role: String,
) -> Result<(), String> {
    if state.github_token().is_none() {
        return Err("Sign in with GitHub first (Settings → Account).".to_string());
    }
    let relay = configured_relay(&state)?;
    let room = state.settings.lock().unwrap().sync_room.clone();
    let client = state.relay_client(&relay);
    let entry = client
        .directory_lookup(&handle)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("@{} isn't on Hive yet.", handle.trim().trim_start_matches('@')))?;
    client
        .upsert_member(&room, &format!("github:{}", entry.github_id), &entry.login, &role)
        .await
        .map_err(|e| e.to_string())
}

/// Remove a member from the active workspace (`account` = `github:<id>`). Caller
/// must be `Admin`+. Pair with a key rotation to also revoke read access.
#[tauri::command]
async fn workspace_remove_member(state: State<'_, AppState>, account: String) -> Result<(), String> {
    let relay = configured_relay(&state)?;
    let room = state.settings.lock().unwrap().sync_room.clone();
    state.relay_client(&relay).remove_member(&room, &account).await.map_err(|e| e.to_string())
}

// ── Social graph: friends + presence (P5) ──────────────────────────────────

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FriendDto {
    account_id: String,
    login: String,
    /// "online" | "away" | "offline".
    presence: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct IncomingRequestDto {
    request_id: String,
    from_account: String,
    from_login: String,
    created_at: u64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FriendsOverviewDto {
    /// True once signed in + a relay is configured (the feature is available).
    enabled: bool,
    friends: Vec<FriendDto>,
    incoming: Vec<IncomingRequestDto>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FriendRequestResultDto {
    /// "sent" | "alreadyFriends" | "capReached" | "userNotFound" | "invalid".
    outcome: String,
    request_id: Option<String>,
}

/// The relay for social ops + this device id, or `None` if the feature isn't
/// available (not signed in, or no relay configured).
fn social_ctx(state: &AppState) -> Option<(hive_runtime::RelayClient, String)> {
    let relay = configured_relay(state).ok()?;
    state.github_token()?;
    let (device_id, _) = this_device_ka(state)?;
    Some((state.relay_client(&relay), device_id))
}

/// Heartbeat this device (registering first if the relay doesn't know it yet),
/// then return the caller's friends-with-presence and incoming requests. The UI
/// polls this so presence stays fresh. No-op overview when the feature is off.
#[tauri::command]
async fn friends_overview(state: State<'_, AppState>) -> Result<FriendsOverviewDto, String> {
    let Some((client, device_id)) = social_ctx(&state) else {
        return Ok(FriendsOverviewDto { enabled: false, friends: vec![], incoming: vec![] });
    };
    // Keep presence fresh; (re-)register if this device isn't known yet.
    if !client.account_heartbeat(&device_id).await.unwrap_or(false) {
        let label = hostname_label();
        let _ = client.account_register(&device_id, None, label.as_deref()).await;
    }
    let friends = client
        .friend_presence()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|f| FriendDto { account_id: f.account_id, login: f.login, presence: f.presence })
        .collect();
    let incoming = client
        .incoming_friend_requests()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|r| IncomingRequestDto {
            request_id: r.request_id,
            from_account: r.from_account,
            from_login: r.from_login,
            created_at: r.created_at,
        })
        .collect();
    Ok(FriendsOverviewDto { enabled: true, friends, incoming })
}

/// Send a friend request to a GitHub `@username`.
#[tauri::command]
async fn friend_send_request(
    state: State<'_, AppState>,
    login: String,
) -> Result<FriendRequestResultDto, String> {
    let (client, _) = social_ctx(&state)
        .ok_or("Sign in with GitHub and configure a relay first.".to_string())?;
    use hive_runtime::FriendRequestOutcome::*;
    let (outcome, request_id) = match client.friend_request(&login).await.map_err(|e| e.to_string())? {
        Sent(id) => ("sent", Some(id)),
        AlreadyFriends => ("alreadyFriends", None),
        CapReached => ("capReached", None),
        UserNotFound => ("userNotFound", None),
        Invalid => ("invalid", None),
    };
    Ok(FriendRequestResultDto { outcome: outcome.to_string(), request_id })
}

/// Accept a pending friend request by id.
#[tauri::command]
async fn friend_accept(state: State<'_, AppState>, request_id: String) -> Result<(), String> {
    let (client, _) = social_ctx(&state).ok_or("Sign in with GitHub first.".to_string())?;
    client.friend_accept(&request_id).await.map_err(|e| e.to_string())?;
    Ok(())
}

/// Reject (recipient) or cancel (sender) a pending friend request by id.
#[tauri::command]
async fn friend_reject(state: State<'_, AppState>, request_id: String) -> Result<(), String> {
    let (client, _) = social_ctx(&state).ok_or("Sign in with GitHub first.".to_string())?;
    client.friend_reject(&request_id).await.map_err(|e| e.to_string())?;
    Ok(())
}

/// Remove an accepted friend by their account key (`github:<id>`).
#[tauri::command]
async fn friend_remove(state: State<'_, AppState>, account_id: String) -> Result<(), String> {
    let (client, _) = social_ctx(&state).ok_or("Sign in with GitHub first.".to_string())?;
    client.friend_remove(&account_id).await.map_err(|e| e.to_string())?;
    Ok(())
}

/// Toggle "appear offline" for this account.
#[tauri::command]
async fn friend_set_visibility(state: State<'_, AppState>, appear_offline: bool) -> Result<(), String> {
    let (client, _) = social_ctx(&state).ok_or("Sign in with GitHub first.".to_string())?;
    client.set_visibility(appear_offline).await.map_err(|e| e.to_string())?;
    Ok(())
}

/// Deterministic DM room for two account keys (`github:<id>`): both sides derive
/// the same name from the sorted numeric ids, so a 1:1 chat converges on one room.
fn dm_room_for(a: &str, b: &str) -> String {
    let na = a.trim().trim_start_matches("github:");
    let nb = b.trim().trim_start_matches("github:");
    let (lo, hi) = if na <= nb { (na, nb) } else { (nb, na) };
    format!("dm-{lo}-{hi}")
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DmDto {
    workspace_id: String,
    account: String,
    login: String,
}

/// The provisioned direct-message workspaces (Friends section list).
#[tauri::command]
fn list_dms(state: State<AppState>) -> Vec<DmDto> {
    let s = state.settings.lock().unwrap();
    s.workspaces
        .iter()
        .filter_map(|w| {
            w.dm_account.as_ref().map(|acct| DmDto {
                workspace_id: w.id().to_string(),
                account: acct.clone(),
                login: w.display_name(),
            })
        })
        .collect()
}

/// Open (lazily provisioning) the private 1:1 DM workspace with a friend, and
/// switch the chat list to it. The lexicographically-smaller account "owns" the
/// DM: it mints the E2EE key and seals it to the friend's devices via the relay
/// keyring (reusing the invite path); the other side receives the key from the
/// keyring. DMs are kept out of the rail (see `list_workspaces`). Returns the
/// DM workspace id.
#[tauri::command]
async fn friend_open_dm(
    state: State<'_, AppState>,
    account_id: String,
    login: String,
) -> Result<String, String> {
    let (client, _) = social_ctx(&state)
        .ok_or("Sign in with GitHub and configure a relay first.".to_string())?;
    let relay = configured_relay(&state)?;
    let my_gid = state
        .settings
        .lock()
        .unwrap()
        .github_account
        .as_ref()
        .map(|a| a.id)
        .ok_or("Sign in with GitHub first.".to_string())?;
    let my_account = format!("github:{my_gid}");
    let friend_account = account_id.trim().to_string();
    if friend_account == my_account {
        return Err("That's you.".to_string());
    }
    let room = dm_room_for(&my_account, &friend_account);
    let i_am_owner = my_account <= friend_account;

    // Provision the DM workspace on first open.
    let existed = state.settings.lock().unwrap().workspaces.iter().any(|w| w.room == room);
    if !existed {
        let conn = WorkspaceConn {
            name: login.trim().trim_start_matches('@').to_string(),
            relay_url: relay.clone(),
            room: room.clone(),
            // The owner mints the key; the other side gets it from the keyring.
            key: if i_am_owner { Some(random_token(24)) } else { None },
            icon: None,
            dm_account: Some(friend_account.clone()),
        };
        let mut s = state.settings.lock().unwrap();
        s.workspaces.push(conn);
        save_settings(&state.data_dir, &s);
    }

    let ws_id = room_workspace_id(&room);
    // Switch to it so the chat list scopes here (also points the sync fields at
    // the room and, for the owner, makes `workspace_key()` the DM key).
    set_active_workspace(state.clone(), ws_id.to_string())?;

    // Owner seals the DM key to the friend's devices so they can decrypt it.
    if i_am_owner && !existed {
        if let Ok(Some(entry)) = client.directory_lookup(&login).await {
            let recipients: Vec<(String, Vec<u8>)> = entry
                .devices
                .iter()
                .filter_map(|d| parse_hex32(&d.ka_public).map(|b| (d.device_id.clone(), b.to_vec())))
                .collect();
            let key = state.settings.lock().unwrap().workspace_key();
            if let (Some(key), false) = (key, recipients.is_empty()) {
                let rotations = client.fetch_key_rotations(&room).await.unwrap_or_default();
                let version = rotations.iter().map(|r| r.version).max().unwrap_or(0) + 1;
                if let Ok(rotation) =
                    hive_core::e2ee::WorkspaceKeyRotation::seal_for_devices(version, &key, &recipients)
                {
                    let _ = client.publish_key_rotation(&room, &rotation).await;
                }
            }
        }
    }
    Ok(ws_id.to_string())
}

/// What's available on this machine, to drive first-run onboarding defaults.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvDetectDto {
    claude_code: bool,
    ollama: bool,
    anthropic_env: bool,
    openai_env: bool,
    git_name: Option<String>,
    git_email: Option<String>,
}

/// Is `bin` on PATH (trying common Windows extensions)?
fn on_path(bin: &str) -> bool {
    let exts: &[&str] = if cfg!(windows) { &["", ".exe", ".cmd", ".bat"] } else { &[""] };
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                exts.iter().any(|ext| dir.join(format!("{bin}{ext}")).is_file())
            })
        })
        .unwrap_or(false)
}

fn git_config_value(key: &str) -> Option<String> {
    std::process::Command::new("git")
        .args(["config", "--get", key])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// A configurable LLM provider (connection): a backend kind + optional API key
/// + optional base URL. Runtimes (models) reference a provider kind for their
/// credentials/endpoint.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderDto {
    /// Stable key (provider_config_name) used by set_provider_*.
    kind: String,
    name: String,
    needs_key: bool,
    has_key: bool,
    supports_base_url: bool,
    base_url: String,
    note: String,
}

/// The user-facing providers, in display order.
fn provider_catalog() -> Vec<(ModelProviderKind, &'static str, bool, bool, &'static str)> {
    use ModelProviderKind::*;
    // (kind, display, needs_key, supports_base_url, note)
    vec![
        (Anthropic, "Anthropic", true, false, "Claude models via the Anthropic API"),
        (OpenAI, "OpenAI", true, true, "GPT models, or any OpenAI-compatible endpoint"),
        (OpenRouter, "OpenRouter", true, true, "Many hosted models via OpenRouter"),
        (Ollama, "Ollama (local)", false, true, "Local models; default http://localhost:11434"),
        (Azure, "Azure OpenAI", true, true, "Azure-hosted OpenAI; base URL = your deployment incl. ?api-version="),
        (Custom, "Custom (OpenAI-compatible)", true, true, "Any OpenAI-style API — Gemini, LM Studio, Groq, Together, …"),
        (ClaudeCode, "Claude Code (CLI)", false, false, "Local `claude` CLI — uses its own login"),
        (Pi, "pi (CLI)", false, false, "Local `pi` agent"),
        (Aider, "aider (CLI)", false, false, "Local `aider` agent"),
    ]
}

#[tauri::command]
fn list_providers(state: State<AppState>) -> Vec<ProviderDto> {
    let s = state.settings.lock().unwrap();
    provider_catalog()
        .into_iter()
        .map(|(kind, name, needs_key, supports_base_url, note)| {
            let key = provider_config_name(kind);
            ProviderDto {
                kind: key.to_string(),
                name: name.to_string(),
                needs_key,
                has_key: s.provider_keys.get(key).map(|k| !k.is_empty()).unwrap_or(false),
                supports_base_url,
                base_url: s.provider_base_urls.get(key).cloned().unwrap_or_default(),
                note: note.to_string(),
            }
        })
        .collect()
}

/// Known OpenAI-compatible backend presets (Gemini, LM Studio, Groq, Azure, …)
/// for one-click runtime setup in onboarding/Settings. `provider` is the string
/// `add_runtime` expects; `endpoint` prefills the model base URL.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderPresetDto {
    label: String,
    provider: String,
    endpoint: String,
    needs_key: bool,
}

#[tauri::command]
fn list_provider_presets() -> Vec<ProviderPresetDto> {
    hive_runtime::dispatch::provider_presets()
        .into_iter()
        .map(|p| ProviderPresetDto {
            label: p.label.to_string(),
            provider: provider_name(p.provider).to_string(),
            endpoint: p.endpoint.to_string(),
            needs_key: p.needs_key,
        })
        .collect()
}

/// Set (or clear, if empty) a provider's API key.
#[tauri::command]
fn set_provider_key(state: State<AppState>, kind: String, key: String) -> Result<(), String> {
    let name = provider_config_name(parse_provider_kind(&kind)?).to_string();
    let mut s = state.settings.lock().unwrap();
    let key = key.trim().to_string();
    if key.is_empty() {
        s.provider_keys.remove(&name);
    } else {
        s.provider_keys.insert(name, key);
    }
    save_settings(&state.data_dir, &s);
    Ok(())
}

/// Set (or clear) a provider's base URL (OpenAI-compatible / Ollama / custom).
#[tauri::command]
fn set_provider_base_url(state: State<AppState>, kind: String, base_url: String) -> Result<(), String> {
    let name = provider_config_name(parse_provider_kind(&kind)?).to_string();
    let mut s = state.settings.lock().unwrap();
    let url = base_url.trim().to_string();
    if url.is_empty() {
        s.provider_base_urls.remove(&name);
    } else {
        s.provider_base_urls.insert(name, url);
    }
    save_settings(&state.data_dir, &s);
    Ok(())
}

#[tauri::command]
fn list_agent_templates(state: State<AppState>) -> Vec<AgentTemplate> {
    state.settings.lock().unwrap().agent_templates.clone()
}

#[tauri::command]
fn add_agent_template(
    state: State<AppState>,
    name: String,
    runtime_id: String,
    role: String,
    instructions: String,
) -> Result<(), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Agent name can't be empty.".to_string());
    }
    let mut s = state.settings.lock().unwrap();
    let role = role.trim();
    s.agent_templates.push(AgentTemplate {
        id: Uuid::new_v4().to_string(),
        name,
        runtime_id: runtime_id.trim().to_string(),
        role: if role.is_empty() { "contributor".to_string() } else { role.to_string() },
        instructions: instructions.trim().to_string(),
    });
    save_settings(&state.data_dir, &s);
    Ok(())
}

#[tauri::command]
fn remove_agent_template(state: State<AppState>, id: String) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    s.agent_templates.retain(|t| t.id != id);
    save_settings(&state.data_dir, &s);
    Ok(())
}

#[tauri::command]
fn detect_environment() -> EnvDetectDto {
    let ollama = "127.0.0.1:11434"
        .parse()
        .ok()
        .map(|addr| {
            std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_ok()
        })
        .unwrap_or(false);
    EnvDetectDto {
        claude_code: on_path("claude"),
        ollama,
        anthropic_env: std::env::var("ANTHROPIC_API_KEY").map(|v| !v.is_empty()).unwrap_or(false),
        openai_env: std::env::var("OPENAI_API_KEY").map(|v| !v.is_empty()).unwrap_or(false),
        git_name: git_config_value("user.name"),
        git_email: git_config_value("user.email"),
    }
}

#[tauri::command]
fn github_logout(state: State<AppState>) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    s.github_account = None;
    s.github_token = None;
    save_settings(&state.data_dir, &s);
    Ok(())
}

/// Publish this device's friend code behind a short, speakable pairing code.
#[tauri::command]
async fn p2p_share_code(state: State<'_, AppState>) -> Result<ShortCodeDto, String> {
    #[cfg(feature = "p2p")]
    {
        let relay = configured_relay(&state)?;
        let code = hive_runtime::peer_iroh::code_for_secret(ensure_p2p_secret(&state));
        let (short, ttl) = state
            .relay_client(&relay)
            .publish_pairing(&code, Some(600))
            .await
            .map_err(|e| e.to_string())?;
        Ok(ShortCodeDto { code: short, expires_in: ttl })
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = &state;
        Err("This build was compiled without P2P support.".to_string())
    }
}

/// Publish a workspace invite behind a short, speakable pairing code.
#[tauri::command]
async fn workspace_share_code(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<ShortCodeDto, String> {
    let relay = configured_relay(&state)?;
    let invite = workspace_invite(state.clone(), workspace_id)?;
    let (short, ttl) = state
        .relay_client(&relay)
        .publish_pairing(&invite, Some(600))
        .await
        .map_err(|e| e.to_string())?;
    Ok(ShortCodeDto { code: short, expires_in: ttl })
}

/// Resolve a short pairing code and act on it: add a peer (friend code) or join
/// a workspace (workspace invite).
#[tauri::command]
async fn redeem_short_code(
    state: State<'_, AppState>,
    code: String,
) -> Result<RedeemResultDto, String> {
    let relay = configured_relay(&state)?;
    let payload = state
        .relay_client(&relay)
        .resolve_pairing(&code)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "That code is invalid or has expired.".to_string())?;
    let payload = payload.trim().to_string();

    if payload.starts_with("hivews1:") {
        let ws = join_workspace(state.clone(), payload)?;
        return Ok(RedeemResultDto { kind: "workspace".into(), label: ws.name });
    }
    // Otherwise treat it as a peer friend code.
    let peer_id = hive_runtime::peer::PeerId::from_code(&payload)
        .ok_or_else(|| "Code resolved to an unrecognized payload.".to_string())?;
    let mut contacts = load_contacts(&state.data_dir);
    if !contacts.iter().any(|c| c.peer_id == peer_id) {
        contacts.push(hive_runtime::peer::Contact {
            peer_id: peer_id.clone(),
            label: String::new(),
        });
        save_contacts(&state.data_dir, &contacts);
    }
    let short_id: String = peer_id.0.chars().take(10).collect();
    Ok(RedeemResultDto { kind: "peer".into(), label: short_id })
}

/// Background direct-P2P sync: bind the node, then push local envelopes to each
/// contact on a timer and apply inbound ones, emitting `workspace://synced`.
#[cfg(feature = "p2p")]
/// Supervises direct P2P: keeps iroh **fully dormant** — no endpoint bound, so
/// no continuous netcheck/discovery (the Windows CPU cost, via WMI) — until the
/// user actually has a contact. Then it runs a sync session, and tears it back
/// down if every contact is removed. Most users (no contacts) never bind iroh.
#[cfg(feature = "p2p")]
async fn run_peer_sync_supervisor(
    app: AppHandle,
    data_dir: PathBuf,
    db_path: PathBuf,
    secret: [u8; 32],
) {
    let mut poll = tokio::time::interval(std::time::Duration::from_secs(5));
    loop {
        poll.tick().await;
        if load_contacts(&data_dir).is_empty() {
            continue; // dormant: nothing bound, no background networking
        }
        // A contact exists — run a session (binds iroh) until contacts clear.
        run_peer_sync_session(app.clone(), data_dir.clone(), db_path.clone(), secret).await;
    }
}

async fn run_peer_sync_session(app: AppHandle, data_dir: PathBuf, db_path: PathBuf, secret: [u8; 32]) {
    use hive_runtime::peer::{PeerLink, PeerSync};
    use hive_runtime::peer_iroh::{secret_from_bytes, IrohNode};

    let node = match IrohNode::bind(secret_from_bytes(secret)).await {
        Ok(n) => std::sync::Arc::new(n),
        Err(e) => {
            eprintln!("p2p: failed to bind endpoint: {e}");
            return;
        }
    };
    eprintln!("p2p: listening as {}", node.local_id().0);

    let mut store = match EventStore::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("p2p: failed to open store: {e}");
            return;
        }
    };
    let mut sync = PeerSync::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(3));

    loop {
        tokio::select! {
            _ = tick.tick() => {
                let contacts = load_contacts(&data_dir);
                // All contacts removed → tear the session down so iroh stops its
                // background networking; the supervisor goes back to dormant.
                if contacts.is_empty() { return; }
                let batch = match sync.take_unpushed(&store) {
                    Ok(b) => b,
                    Err(e) => { eprintln!("p2p: take_unpushed: {e}"); continue; }
                };
                if batch.is_empty() { continue; }
                for c in &contacts {
                    for data in &batch {
                        // Offline/unreachable peers just error; retried next tick.
                        let _ = node.send(&c.peer_id, data.clone()).await;
                    }
                }
            }
            inbound = node.recv() => {
                match inbound {
                    Some((_from, data)) => {
                        match sync.apply(&mut store, &data) {
                            Ok(true) => { let _ = app.emit("workspace://synced", 1); }
                            Ok(false) => {}
                            Err(e) => eprintln!("p2p: apply: {e}"),
                        }
                    }
                    None => break,
                }
            }
        }
    }
}

/// macOS/Linux apps launched from Finder/Dock/DMG inherit only a minimal
/// `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`), so user-installed CLIs — `claude`,
/// `aider`, `pi`, `ollama`, or a Homebrew `git` — are invisible to our
/// subprocess bridges (and to `on_path` capability detection). Hydrate the
/// process `PATH` at launch by merging in the login shell's `PATH` plus a
/// curated set of common bin dirs. No-op on Windows, whose GUI inherits the
/// system `PATH` already.
#[cfg(not(target_os = "windows"))]
fn hydrate_process_path() {
    use std::path::PathBuf;

    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let append = |extra: Vec<PathBuf>, dirs: &mut Vec<PathBuf>| {
        for d in extra {
            if !d.as_os_str().is_empty() && !dirs.contains(&d) {
                dirs.push(d);
            }
        }
    };

    // 1) The login shell's PATH — covers nvm/asdf/pyenv/custom setups. Best
    //    effort with a timeout so a slow or misconfigured shell can't stall
    //    startup.
    if let Some(shell_path) = login_shell_path() {
        append(std::env::split_paths(&shell_path).collect(), &mut dirs);
    }

    // 2) Curated common install locations, in case the shell probe returned
    //    nothing (e.g. PATH only set in a file the probe didn't source).
    let mut curated: Vec<PathBuf> =
        vec!["/opt/homebrew/bin".into(), "/usr/local/bin".into()];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for sub in [".local/bin", ".npm-global/bin", ".claude/local", ".cargo/bin", "go/bin"] {
            curated.push(home.join(sub));
        }
    }
    append(curated, &mut dirs);

    if let Ok(joined) = std::env::join_paths(&dirs) {
        std::env::set_var("PATH", joined);
    }
}

/// Ask the user's login+interactive shell for its `PATH`. Runs on a worker
/// thread with a hard timeout; returns `None` on timeout/error so the caller
/// falls back to curated dirs.
#[cfg(not(target_os = "windows"))]
fn login_shell_path() -> Option<std::ffi::OsString> {
    use std::process::Stdio;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // -l (login) sources .zprofile/.profile; -i (interactive) sources
        // .zshrc/.bashrc — between them we catch wherever PATH was set. Print
        // PATH on its own line and read the last non-empty line to skip any
        // shell banner/motd noise. stdin is /dev/null so an interactive shell
        // can't block waiting for input.
        let out = std::process::Command::new(&shell)
            .args(["-lic", "printf '%s\\n' \"$PATH\""])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let _ = tx.send(out);
    });
    match rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(Ok(out)) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map(|l| std::ffi::OsString::from(l.trim())),
        _ => None,
    }
}

/// Build and run the Tauri application.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        // Auto-updater (scaffold): registered but inert until tauri.conf.json's
        // updater pubkey + signed artifacts exist (see #144). `check_for_update`
        // surfaces a friendly "not configured yet" until then.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            // Before anything spawns a subprocess: repair the minimal PATH that
            // Finder/Dock/DMG launches inherit, so `claude`/`aider`/`pi`/`git`
            // installed in the user's shell PATH are actually found.
            #[cfg(not(target_os = "windows"))]
            hydrate_process_path();

            let state = build_state(&app.handle())
                .map_err(|e| Box::<dyn std::error::Error>::from(format!("backend init failed: {e}")))?;
            let settings = state.settings.clone();
            let db_path = state.db_path.clone();
            #[cfg(feature = "p2p")]
            let p2p = (ensure_p2p_secret(&state), state.data_dir.clone(), state.db_path.clone());
            app.manage(state);

            // Deferred one-time DB maintenance (chunk-row shrink). Kept off the
            // launch path entirely: we wait a few seconds so first paint and the
            // frontend's initial queries finish, then prune during idle. This is
            // why startup no longer stalls on a large carried-over hive.db.
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    if let Some(state) = handle.try_state::<AppState>() {
                        if let Ok(mut svc) = state.service.lock() {
                            if let Ok(removed) = svc.prune_superseded_chunks() {
                                if removed > 0 {
                                    eprintln!("pruned {removed} superseded chunk rows");
                                }
                            }
                        }
                    }
                });
            }

            #[cfg(target_os = "macos")]
            {
                let tray = build_tray_icon(&app.handle())?;
                app.manage(tray);
            }

            // Always run the background sync loop: it idles (local-only) until a
            // relay is configured and reconnects live when Settings change.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(run_sync_loop(handle, settings.clone(), db_path));

            // Scheduled/triggered agents: fires due schedules on a 30s tick.
            // Idle until the user adds a schedule (Settings → Schedules).
            {
                let h = app.handle().clone();
                let data_dir = app.state::<AppState>().data_dir.clone();
                tauri::async_runtime::spawn(run_scheduler_loop(h, settings, data_dir));
            }

            // Direct P2P sync (iroh): syncs with added contacts device-to-device.
            // Skippable via HIVE_DISABLE_P2P — a perf diagnostic, since iroh does
            // continuous network/interface probing (heavier on Windows via WMI).
            #[cfg(feature = "p2p")]
            if std::env::var("HIVE_DISABLE_P2P").is_err() {
                let (secret, data_dir, p2p_db) = p2p;
                let h = app.handle().clone();
                tauri::async_runtime::spawn(run_peer_sync_supervisor(h, data_dir, p2p_db, secret));
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            list_relay_users,
            create_relay_user,
            issue_relay_token,
            revoke_relay_token,
            set_relay_user_disabled,
            workflows::list_workflows,
            workflows::save_workflow,
            workflows::remove_workflow,
            workflows::add_workflow_preset,
            workflows::list_workflow_runs,
            workflows::start_workflow_run,
            workflows::cancel_workflow_run,
            workflows::resume_workflow_run,
            list_chats,
            create_chat,
            get_chat,
            get_context_telemetry,
            archive_chat,
            rename_chat,
            delete_chat,
            list_agents,
            add_agent,
            remove_agent,
            list_skills,
            add_skill_inline,
            install_skill,
            remove_skill,
            list_mcp_servers,
            set_mcp_enabled,
            list_proposals,
            create_proposal,
            vote_proposal,
            implement_proposal,
            toggle_reaction,
            list_members,
            add_member,
            import_github_teams,
            remove_member,
            remove_and_revoke,
            set_member_role,
            list_vaults,
            add_vault,
            remove_vault,
            preview_vault,
            send_message,
            get_workspace_diffs,
            get_app_settings,
            get_git_status,
            list_runtimes,
            add_runtime,
            remove_runtime,
            set_chat_runtime,
            set_workspace_root,
            add_workspace_to_list,
            pick_workspace_folder,
            read_workspace_file,
            remove_workspace_from_list,
            set_display_name,
            open_in_editor,
            get_file_diff_sides,
            detect_editors,
            open_path_in_editor,
            open_external,
            set_titlebar_color,
            reset_local_data,
            check_for_update,
            check_for_app_update,
            list_schedules,
            add_schedule,
            remove_schedule,
            set_schedule_enabled,
            summarize_chat,
            compact_chat,
            add_remote_mcp_server,
            authorize_mcp_server,
            set_mcp_oauth_client,
            linear_issues_context,
            install_mcp_server,
            remove_mcp_server,
            sync_status,
            probe_relay,
            probe_relay_at,
            export_chat,
            save_attachment,
            set_git_email,
            p2p_my_code,
            p2p_list_contacts,
            p2p_add_contact,
            p2p_remove_contact,
            p2p_share_code,
            workspace_share_code,
            redeem_short_code,
            set_github_client_id,
            github_account,
            github_client_configured,
            github_login_start,
            github_login_poll,
            github_logout,
            directory_register,
            invite_by_handle,
            workspace_claim_membership,
            workspace_members,
            workspace_add_member,
            workspace_remove_member,
            friends_overview,
            friend_send_request,
            friend_accept,
            friend_reject,
            friend_remove,
            friend_set_visibility,
            friend_open_dm,
            list_dms,
            detect_environment,
            list_providers,
            list_provider_presets,
            set_provider_key,
            set_provider_base_url,
            list_agent_templates,
            add_agent_template,
            remove_agent_template,
            list_workspaces,
            set_active_workspace,
            set_workspace_icon,
            create_workspace,
            join_workspace,
            workspace_invite,
            remove_workspace,
            get_connection_settings,
            update_connection_settings,
            get_claude_code_model,
            set_claude_code_model,
            list_claude_code_models,
            set_default_runtime,
            get_context_commands,
            set_context_commands,
            set_default_model,
            maybe_respond,
            regenerate,
            ensure_self_member,
            notify_mentions,
            presence_ping,
            presence_list
        ])
        .run(tauri::generate_context!())
        .expect("error while running Hive");
}

#[cfg(test)]
mod title_tests {
    use super::{is_default_title, sanitize_title};

    #[test]
    fn default_title_detection() {
        assert!(is_default_title("New chat"));
        assert!(is_default_title("  new CHAT "));
        assert!(is_default_title(""));
        assert!(is_default_title("Untitled"));
        assert!(!is_default_title("Fix the relay sync bug"));
    }

    #[test]
    fn sanitizes_model_titles() {
        assert_eq!(sanitize_title("\"Relay Sync Bug\""), "Relay Sync Bug");
        assert_eq!(sanitize_title("Title: Fix Login Flow."), "Fix Login Flow");
        assert_eq!(sanitize_title("Refactor the auth module\nsome rambling"), "Refactor the auth module");
        assert_eq!(sanitize_title("**Deploy Pipeline**"), "Deploy Pipeline");
    }

    #[test]
    fn caps_overlong_titles_to_eight_words() {
        let out = sanitize_title("one two three four five six seven eight nine ten");
        assert_eq!(out.split_whitespace().count(), 8);
    }
}

#[cfg(test)]
mod export_tests {
    use super::{chat_markdown, export_filename};
    use hive_core::{ChatMessage, ChatSession, MessageRole};
    use uuid::Uuid;

    #[test]
    fn filename_is_slugified() {
        assert_eq!(export_filename("Fix the Login Flow!"), "fix-the-login-flow.md");
        assert_eq!(export_filename("   "), "hive-chat.md");
        assert_eq!(export_filename("a/b\\c"), "a-b-c.md");
    }

    #[test]
    fn markdown_has_title_count_and_messages_in_order() {
        let mut s = ChatSession::new("Launch plan", Uuid::nil(), "anthropic");
        s.messages.push(ChatMessage::new(MessageRole::User, "Mara", "Ship it?"));
        s.messages.push(ChatMessage::new(MessageRole::Assistant, "Hive", "On it."));
        s.messages.push(ChatMessage::new(MessageRole::User, "Mara", "   ")); // blank skipped
        let md = chat_markdown(&s);
        assert!(md.starts_with("# Launch plan\n"));
        assert!(md.contains("2 message(s)"));
        let mara = md.find("Ship it?").unwrap();
        let hive = md.find("On it.").unwrap();
        assert!(mara < hive, "messages preserve order");
        assert!(md.contains("**Mara** · _User_"));
        assert!(md.contains("**Hive** · _Assistant_"));
    }

    #[test]
    fn blank_title_falls_back() {
        let s = ChatSession::new("  ", Uuid::nil(), "anthropic");
        assert!(chat_markdown(&s).starts_with("# Untitled chat\n"));
    }
}

#[cfg(test)]
mod vault_context_tests {
    use super::{vault_section_text, VAULT_MAX_CHARS};

    #[test]
    fn empty_sources_produce_no_section() {
        assert_eq!(vault_section_text(&[]), "");
    }

    #[test]
    fn formats_labels_content_and_failures() {
        let s = vault_section_text(&[
            ("github:acme/docs".into(), Some("Use tabs.".into())),
            ("https://x.example/guide".into(), None),
        ]);
        assert!(s.starts_with("[Reference vaults]"));
        assert!(s.contains("--- github:acme/docs ---\nUse tabs."));
        assert!(s.contains("--- https://x.example/guide — unavailable (fetch failed) ---"));
    }

    #[test]
    fn long_content_is_capped_with_a_marker() {
        let long = "x".repeat(VAULT_MAX_CHARS + 500);
        let s = vault_section_text(&[("v".into(), Some(long))]);
        assert!(s.contains("(truncated)"));
        assert!(s.contains("[…truncated]"));
        // Section stays bounded: cap + small formatting overhead.
        assert!(s.chars().count() < VAULT_MAX_CHARS + 200);
    }
}

#[cfg(test)]
mod attachment_tests {
    use super::sanitize_attachment_name;

    #[test]
    fn sanitizes_names_and_strips_dirs() {
        assert_eq!(sanitize_attachment_name("photo.png"), "photo.png");
        assert_eq!(sanitize_attachment_name("/etc/passwd"), "passwd");
        assert_eq!(sanitize_attachment_name("a b*c.txt"), "a-b-c.txt");
        assert_eq!(sanitize_attachment_name(""), "file");
    }
}

#[cfg(test)]
mod file_ref_tests {
    use super::is_within;
    use std::path::Path;

    #[test]
    fn within_accepts_children_rejects_escapes() {
        let root = Path::new("/home/u/proj");
        assert!(is_within(root, Path::new("/home/u/proj")));
        assert!(is_within(root, Path::new("/home/u/proj/src/lib.rs")));
        assert!(!is_within(root, Path::new("/home/u/other/secret")));
        assert!(!is_within(root, Path::new("/etc/passwd")));
    }
}

#[cfg(test)]
mod workspace_scope_tests {
    use super::{room_workspace_id, session_in_workspace};
    use std::collections::HashSet;
    use uuid::Uuid;

    #[test]
    fn room_id_is_deterministic_and_distinct() {
        assert_eq!(room_workspace_id("team-alpha"), room_workspace_id("team-alpha"));
        assert_eq!(room_workspace_id(" team-alpha "), room_workspace_id("team-alpha"));
        assert_ne!(room_workspace_id("team-alpha"), room_workspace_id("team-beta"));
    }

    #[test]
    fn local_scope_keeps_my_chats_and_hides_synced_room_chats() {
        let local = Uuid::new_v4();
        let room = room_workspace_id("team-alpha");
        let rooms: HashSet<Uuid> = [room].into_iter().collect();
        let me = "me";

        // My own local chat → shown.
        assert!(session_in_workspace(local, me, local, me, &rooms));
        // A legacy chat (no creator) with a random id → shown (re-homed locally).
        assert!(session_in_workspace(Uuid::new_v4(), "", local, me, &rooms));
        // A teammate's chat tagged to the room → hidden in My workspace.
        assert!(!session_in_workspace(room, "teammate", local, me, &rooms));
        // An old leaked synced chat (foreign creator, random id) → hidden.
        assert!(!session_in_workspace(Uuid::new_v4(), "teammate", local, me, &rooms));
    }

    #[test]
    fn room_scope_shows_exactly_that_rooms_chats() {
        let local = Uuid::new_v4();
        let room = room_workspace_id("team-alpha");
        let other_room = room_workspace_id("team-beta");
        let rooms: HashSet<Uuid> = [room, other_room].into_iter().collect();
        let me = "me";

        // Room chats from anyone → shown when that room is active.
        assert!(session_in_workspace(room, "teammate", room, me, &rooms));
        assert!(session_in_workspace(room, me, room, me, &rooms));
        // A different room's chats → hidden.
        assert!(!session_in_workspace(other_room, "teammate", room, me, &rooms));
        // A local chat → not shown in a room scope.
        assert!(!session_in_workspace(local, me, room, me, &rooms));
    }
}
