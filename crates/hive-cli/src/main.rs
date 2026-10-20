//! `hive` — a headless Hive client.
//!
//! Runs the same runtime the desktop app does (`hive-core` + `hive-runtime`),
//! wired to flags/env instead of a GUI, so a cloud-hosted agent (or a script)
//! can connect to a relay, sync a workspace's E2EE event log, and read/write
//! signed events without the full application. This is the "worker" client the
//! design spec §12 (D21–D23) calls for; v0 proves the path end-to-end:
//! connect → sync → read → post.
//!
//! Config (all via env; a persistent data dir keeps a stable identity):
//!   HIVE_DATA_DIR            where identity + hive.db live (default: $HOME/.hive)
//!   HIVE_RELAY_URL           relay base URL (needed for `sync`/`send --push`)
//!   HIVE_RELAY_ACCESS_TOKEN  relay entitlement token (hrt1/hex), provisioned out-of-band
//!   HIVE_RELAY_GITHUB_TOKEN  optional, only on membership-enforcing relays
//!   HIVE_WORKSPACE           relay room / workspace (default: "default")
//!   HIVE_WORKSPACE_KEY       E2EE passphrase; omit for a plaintext relay
//!
//! Commands:
//!   hive whoami              print this client's identity (account/device ids)
//!   hive chats               list chats in the local store
//!   hive tail <chat-id>      print a chat's transcript (add --watch to follow)
//!   hive send <chat-id> <…>  post a message (add --push to sync it out)
//!   hive sync                run one sync round with the relay (add --watch to loop)
//!   hive serve               home host for the iOS app: sync + agent worker +
//!                            HTTP/SSE API on a tailnet address (see serve/)

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use hive_core::{
    derive_workspace_key, workspace_config_session_id, HostKind, MessageRole, ModelProviderKind,
    Timestamp, WorkspaceAgent, WorkspaceCredential, WorkspaceHost, WorkspaceRuntime,
};
use hive_runtime::{
    parse_mentions, pending_mentions, resolve_workspace_credential, turns_for, ChatService,
    ChatTurn, EventStore, FileKeyVault, IdentityStore, RelayClient, ResolvedRuntime, SyncEngine,
};
use uuid::Uuid;

mod mcp_config;
mod reply;
mod repos;
mod serve;
mod worker;

#[derive(Clone)]
struct Config {
    data_dir: PathBuf,
    relay_url: Option<String>,
    token: Option<String>,
    github_token: Option<String>,
    room: String,
    key: Option<String>,
}

fn env_opt(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|s| !s.trim().is_empty())
}

/// Persisted connection config (written by `hive enroll`), read as a fallback
/// under the env vars so a headless box needn't re-export them each run.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct FileConfig {
    #[serde(default)]
    relay_url: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    room: Option<String>,
    #[serde(default)]
    key: Option<String>,
}

fn config_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("config.toml")
}

fn load_file_config(data_dir: &std::path::Path) -> FileConfig {
    std::fs::read_to_string(config_path(data_dir))
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

fn config() -> Config {
    let data_dir = env_opt("HIVE_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| env_opt("HOME").map(|h| PathBuf::from(h).join(".hive")))
        .unwrap_or_else(|| PathBuf::from("./hive-data"));
    // Env wins; the saved config.toml (from `hive enroll`) fills the rest.
    let f = load_file_config(&data_dir);
    Config {
        data_dir,
        relay_url: env_opt("HIVE_RELAY_URL").or(f.relay_url),
        token: env_opt("HIVE_RELAY_ACCESS_TOKEN").or(f.token),
        github_token: env_opt("HIVE_RELAY_GITHUB_TOKEN"),
        room: env_opt("HIVE_WORKSPACE").or(f.room).unwrap_or_else(|| "default".into()),
        key: env_opt("HIVE_WORKSPACE_KEY").or(f.key),
    }
}

/// Decode a `hivews1:` workspace invite → (relay_url, room, key). Mirrors the
/// app's encoder (base64url(json) of the connection; snake_case fields).
fn decode_hivews_invite(invite: &str) -> Result<(String, String, Option<String>)> {
    use base64::Engine;
    #[derive(serde::Deserialize)]
    struct Conn {
        #[serde(default)]
        relay_url: String,
        #[serde(default)]
        room: String,
        #[serde(default)]
        key: Option<String>,
    }
    let body = invite
        .trim()
        .strip_prefix("hivews1:")
        .ok_or_else(|| anyhow!("not a Hive workspace invite (expected hivews1:…)"))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body.trim().as_bytes())
        .context("invalid invite encoding")?;
    let c: Conn = serde_json::from_slice(&bytes).context("invalid invite payload")?;
    if c.room.trim().is_empty() {
        bail!("invite is missing a room");
    }
    Ok((c.relay_url, c.room, c.key))
}

/// `hive enroll <hivews1:…> [--token <access-token>]` — one-command bootstrap:
/// decode the invite, persist the connection to config.toml, and (with --token)
/// wire the relay identity. After this a headless box just runs `hive worker`.
fn cmd_enroll(cfg: &Config, invite: &str, token: Option<String>) -> Result<()> {
    let (relay_url, room, key) = decode_hivews_invite(invite)?;
    std::fs::create_dir_all(&cfg.data_dir)?;
    let mut f = load_file_config(&cfg.data_dir);
    f.relay_url = Some(relay_url.clone());
    f.room = Some(room.clone());
    f.key = key;
    if let Some(t) = token {
        f.token = Some(t);
    }
    std::fs::write(config_path(&cfg.data_dir), toml::to_string_pretty(&f)?)?;
    println!("Enrolled workspace '{room}' at {relay_url}.");
    println!("Saved to {}.", config_path(&cfg.data_dir).display());
    if f.token.is_none() {
        println!("No access token set — for a membership-enforcing relay, add one:");
        println!("  hive enroll <invite> --token <hrt1-token>   (or export HIVE_RELAY_ACCESS_TOKEN)");
    }
    println!("Next: `hive sync` then `hive worker` to bring the agent online.");
    Ok(())
}

/// `hive join <invite-code>` — redeem a relay invite code to self-enroll this
/// box's identity in the configured workspace at the invite's role.
async fn cmd_join(cfg: &Config, code: &str) -> Result<()> {
    let relay_url = cfg
        .relay_url
        .clone()
        .ok_or_else(|| anyhow!("no relay — run `hive enroll <invite>` or set HIVE_RELAY_URL first"))?;
    let relay = RelayClient::new(&relay_url)
        .with_auth(cfg.token.clone())
        .with_github_token(cfg.github_token.clone());
    match relay.join_via_invite(&cfg.room, code.trim(), "").await? {
        Some(role) => {
            println!("Joined workspace '{}' as {role}. Run `hive sync`.", cfg.room);
            Ok(())
        }
        None => bail!("invite refused (invalid, expired, revoked, or used up)"),
    }
}

/// Open the local identity + event store and build a ChatService. Bootstraps a
/// fresh identity on first run (idempotent thereafter).
fn open_service(cfg: &Config) -> Result<ChatService> {
    std::fs::create_dir_all(&cfg.data_dir)?;
    let identity = IdentityStore::new(&cfg.data_dir, FileKeyVault::new(&cfg.data_dir));
    let stored = identity.bootstrap("hive-cli", "hive-cli", "hive-cli")?;
    let device_kp = identity.device_keypair(stored.device.id)?;
    let account_kp = identity.account_keypair(stored.account.id)?;
    let store = EventStore::open(cfg.data_dir.join("hive.db"))?;
    Ok(ChatService::new(
        store,
        stored.device.id,
        device_kp,
        account_kp,
        stored.account.actor(),
    ))
}

/// One sync round against the relay: push everything unpushed, then fetch +
/// ingest. Uses its own EventStore connection (shares the WAL file), as the app
/// does. Returns (pushed, pulled) counts.
async fn sync_once(cfg: &Config) -> Result<(usize, usize)> {
    let relay_url = cfg
        .relay_url
        .clone()
        .ok_or_else(|| anyhow!("set HIVE_RELAY_URL to sync"))?;
    let relay = RelayClient::new(&relay_url)
        .with_auth(cfg.token.clone())
        .with_github_token(cfg.github_token.clone());
    let mut engine = SyncEngine::new(relay, cfg.room.clone());
    if let Some(pass) = &cfg.key {
        engine = engine.with_key(derive_workspace_key(pass));
    }
    let mut store = EventStore::open(cfg.data_dir.join("hive.db"))?;
    let to_push = engine.take_unpushed(&store)?;
    let pushed = to_push.len();
    engine.push_envelopes(&to_push).await?;
    let fetched = engine.fetch_new().await?;
    let pulled = engine.apply_fetched(&mut store, &fetched)?;
    Ok((pushed, pulled))
}

/// Resolve any `[Attached-blob: …]` references in agent turns to local files
/// (downloading + decrypting from the relay's blob channel and caching them), so
/// a headless agent can read attachments other members shared. No-op when no
/// relay/workspace key is configured or no turn carries a blob marker.
async fn materialize_turns(cfg: &Config, turns: &mut [ChatTurn]) {
    let (Some(relay_url), Some(pass)) = (cfg.relay_url.clone(), cfg.key.clone()) else {
        return;
    };
    if !turns.iter().any(|t| t.content.contains("[Attached-blob:")) {
        return;
    }
    let relay = RelayClient::new(&relay_url)
        .with_auth(cfg.token.clone())
        .with_github_token(cfg.github_token.clone());
    let key = derive_workspace_key(&pass);
    let cache = cfg.data_dir.join("attachments").join("blobs");
    for t in turns.iter_mut() {
        if t.content.contains("[Attached-blob:") {
            t.content = hive_runtime::attachment_blob::materialize_body(
                &t.content, &relay, &cfg.room, &key, &cache,
            )
            .await;
        }
    }
}

fn cmd_whoami(cfg: &Config) -> Result<()> {
    let identity = IdentityStore::new(&cfg.data_dir, FileKeyVault::new(&cfg.data_dir));
    let stored = identity.bootstrap("hive-cli", "hive-cli", "hive-cli")?;
    println!("account : {}", stored.account.id);
    println!("device  : {}", stored.device.id);
    println!("name    : {}", stored.account.display_name);
    println!("data dir: {}", cfg.data_dir.display());
    println!("room    : {}", cfg.room);
    Ok(())
}

fn cmd_chats(cfg: &Config) -> Result<()> {
    let svc = open_service(cfg)?;
    let config_id = workspace_config_session_id(uuid_of_room(&cfg.room));
    let mut any = false;
    for id in svc.store().list_session_ids()? {
        // Never list the workspace-config log — it isn't a chat.
        if id == config_id {
            continue;
        }
        if let Some(s) = svc.load(id)? {
            // Skip config logs of other workspaces too (empty-title, has channels).
            if id == workspace_config_session_id(s.workspace_id) {
                continue;
            }
            any = true;
            let ch = if s.channel_id.is_empty() { String::new() } else { format!("  #{}", s.channel_id) };
            println!("{}  {:<32}  {} msgs{}", s.id, truncate(&s.title, 32), s.messages.len(), ch);
        }
    }
    if !any {
        println!("(no chats — run `hive sync` to pull a workspace, or open the app to create one)");
    }
    Ok(())
}

/// `hive proposals` — every proposal in the workspace's chats, newest first,
/// with its status and the votes cast on it (who, at what role).
fn cmd_proposals(cfg: &Config) -> Result<()> {
    let svc = open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let mut rows = Vec::new();
    for id in svc.store().list_session_ids()? {
        if id == workspace_config_session_id(ws) {
            continue;
        }
        let Some(s) = svc.load(id)? else { continue };
        if s.workspace_id != ws || id == workspace_config_session_id(s.workspace_id) {
            continue;
        }
        for p in &s.proposals {
            let name_of = |actor: &str| {
                s.members
                    .iter()
                    .find(|m| m.id == actor)
                    .map(|m| m.actor.display_name.clone())
                    .unwrap_or_else(|| actor.to_string())
            };
            let votes: Vec<String> = p
                .approvals
                .iter()
                .map(|a| format!("{} {} ({:?})", if a.approved { "+" } else { "-" }, name_of(&a.actor_id), a.role))
                .collect();
            let status = format!("{:?}", p.status).to_lowercase();
            let mut line = format!("{}  {:<9} {}  [chat {}]", p.id, status, truncate(&p.title, 48), s.id);
            if !votes.is_empty() {
                line.push_str(&format!("\n    votes: {}", votes.join(", ")));
            }
            rows.push((p.created_at, line));
        }
    }
    if rows.is_empty() {
        println!("(no proposals)");
    }
    rows.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    for (_, line) in rows {
        println!("{line}");
    }
    Ok(())
}

fn print_transcript(svc: &ChatService, chat_id: Uuid, from: usize) -> Result<usize> {
    let Some(s) = svc.load(chat_id)? else {
        bail!("unknown chat {chat_id}");
    };
    for m in s.messages.iter().skip(from) {
        println!("{:>9}  {}", m.author, m.body);
    }
    Ok(s.messages.len())
}

fn cmd_tail(cfg: &Config, chat_id: Uuid, watch: bool) -> Result<()> {
    let svc = open_service(cfg)?;
    let mut seen = print_transcript(&svc, chat_id, 0)?;
    if !watch {
        return Ok(());
    }
    let rt = tokio::runtime::Runtime::new()?;
    loop {
        std::thread::sleep(Duration::from_secs(3));
        if cfg.relay_url.is_some() {
            let _ = rt.block_on(sync_once(cfg));
        }
        // Reopen to pick up ingested events (a fresh projection off the WAL file).
        let svc = open_service(cfg)?;
        seen = print_transcript(&svc, chat_id, seen)?;
    }
}

async fn cmd_send(cfg: &Config, chat_id: Uuid, body: String, push: bool) -> Result<()> {
    let mut svc = open_service(cfg)?;
    let Some(session) = svc.load(chat_id)? else {
        bail!("unknown chat {chat_id}");
    };
    svc.post_user_message(chat_id, session.workspace_id, body)?;
    println!("posted.");
    if push {
        let (pushed, pulled) = sync_once(cfg).await?;
        println!("synced: pushed {pushed}, pulled {pulled}");
    }
    Ok(())
}

async fn cmd_sync(cfg: &Config, watch: bool) -> Result<()> {
    loop {
        let (pushed, pulled) = sync_once(cfg).await?;
        println!("pushed {pushed}, pulled {pulled}");
        if !watch {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

fn cmd_new(cfg: &Config, title: String) -> Result<()> {
    let mut svc = open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let session = svc.create_chat(title, ws, "")?;
    println!("{}", session.id);
    Ok(())
}

/// Build a runtime target from env (HIVE_PROVIDER / HIVE_MODEL / a key). This is
/// a PERSONAL credential today — spec §12.5's workspace-owned credential for a
/// detached worker doesn't exist yet.
fn resolve_runtime_from_env() -> Result<(ResolvedRuntime, String)> {
    let provider_str = env_opt("HIVE_PROVIDER").unwrap_or_else(|| "anthropic".into());
    let model = env_opt("HIVE_MODEL").unwrap_or_else(|| "claude-sonnet-4-5".into());
    let (provider, endpoint, key_env): (ModelProviderKind, String, &str) =
        match provider_str.to_lowercase().as_str() {
            "anthropic" => (ModelProviderKind::Anthropic, String::new(), "ANTHROPIC_API_KEY"),
            "openai" => (
                ModelProviderKind::OpenAI,
                "https://api.openai.com/v1/chat/completions".into(),
                "OPENAI_API_KEY",
            ),
            "openrouter" => (
                ModelProviderKind::OpenRouter,
                "https://openrouter.ai/api/v1/chat/completions".into(),
                "OPENROUTER_API_KEY",
            ),
            "ollama" => (
                ModelProviderKind::Ollama,
                env_opt("HIVE_MODEL_BASE_URL")
                    .unwrap_or_else(|| "http://localhost:11434/v1/chat/completions".into()),
                "",
            ),
            other => bail!("unsupported HIVE_PROVIDER '{other}' (anthropic|openai|openrouter|ollama)"),
        };
    let api_key = env_opt("HIVE_PROVIDER_API_KEY")
        .or_else(|| (!key_env.is_empty()).then(|| env_opt(key_env)).flatten());
    let rt = ResolvedRuntime {
        provider,
        model: model.clone(),
        endpoint,
        api_key,
        args: Vec::new(),
        model_provider_id: None,
        model_base_url: None,
        context_window_tokens: None,
        keep_alive: None,
        think: None,
        tools: false,
        max_tool_rounds: None,
        allow_remote_file_tools: false,
    };
    Ok((rt, format!("{provider_str}/{model}")))
}

fn provider_kind(s: &str) -> Result<ModelProviderKind> {
    Ok(match s.to_lowercase().as_str() {
        "anthropic" => ModelProviderKind::Anthropic,
        "openai" => ModelProviderKind::OpenAI,
        "openrouter" => ModelProviderKind::OpenRouter,
        "ollama" => ModelProviderKind::Ollama,
        // Any OpenAI-compatible server (a llama.cpp gateway): keyless when the
        // endpoint is local / LAN / tailnet, and runs the agent tool loop.
        "custom" => ModelProviderKind::Custom,
        other => bail!("unknown provider '{other}' (anthropic|openai|openrouter|ollama|custom)"),
    })
}

/// Define (or replace by id) a workspace-owned runtime. `--secret-ref <name>`
/// keeps the key off the synced log (worker env `HIVE_WS_SECRET_<name>`);
/// `--secret <value>` carries it E2EE on the config log; neither = keyless.
#[allow(clippy::too_many_arguments)]
fn cmd_add_runtime(
    cfg: &Config,
    id: &str,
    name: &str,
    provider: &str,
    model: &str,
    endpoint: Option<String>,
    secret_ref: Option<String>,
    secret: Option<String>,
) -> Result<()> {
    let mut wr = WorkspaceRuntime::new(id, name, provider_kind(provider)?, model);
    wr.endpoint = endpoint.unwrap_or_default();
    wr.credential = match (secret_ref, secret) {
        (Some(r), _) => WorkspaceCredential::SecretRef { secret_ref: r },
        (None, Some(s)) => WorkspaceCredential::Inline { secret: s },
        (None, None) => WorkspaceCredential::None,
    };
    let mut svc = open_service(cfg)?;
    svc.add_workspace_runtime(uuid_of_room(&cfg.room), wr)?;
    println!("runtime '{id}' defined (sync to share it with the workspace).");
    Ok(())
}

fn cmd_rm_runtime(cfg: &Config, id: &str) -> Result<()> {
    let mut svc = open_service(cfg)?;
    svc.remove_workspace_runtime(uuid_of_room(&cfg.room), id)?;
    println!("removed.");
    Ok(())
}

/// Is `bin` an executable on PATH? (headless detection for `hive setup`).
fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file()))
        .unwrap_or(false)
}

/// `hive setup` — headless detect + configure. Reports what this box can already
/// reach and (with --apply) defines workspace runtimes for the *keyless* ones
/// (Ollama, claude/codex CLIs). Key-backed providers are printed as ready-to-run
/// commands rather than auto-wired, so a personal key is never inlined onto the
/// shared config log (workspace credentials use a secretRef the worker holds).
fn cmd_setup(cfg: &Config, apply: bool) -> Result<()> {
    let ollama = "127.0.0.1:11434"
        .parse()
        .ok()
        .map(|addr| {
            std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(300)).is_ok()
        })
        .unwrap_or(false);
    let claude = on_path("claude");
    let codex = on_path("codex");

    println!("Detected on this box:");
    for (env_var, provider, model) in [
        ("ANTHROPIC_API_KEY", "anthropic", "claude-sonnet-4-6"),
        ("OPENAI_API_KEY", "openai", "gpt-4o"),
        ("OPENROUTER_API_KEY", "openrouter", "openai/gpt-4o-mini"),
    ] {
        if std::env::var(env_var).map(|v| !v.is_empty()).unwrap_or(false) {
            println!("  ✓ {env_var} present (key-backed)");
            println!(
                "      → hive add-runtime {provider} \"{provider}\" {provider} {model} --secret-ref {provider}   (worker sets HIVE_WS_SECRET_{provider})"
            );
        }
    }
    if ollama {
        println!("  ✓ Ollama (local, keyless)");
    }
    if claude {
        println!("  ✓ claude CLI (keyless)");
    }
    if codex {
        println!("  ✓ codex CLI (keyless)");
    }

    if apply {
        let mut svc = open_service(cfg)?;
        let ws = uuid_of_room(&cfg.room);
        let mut n = 0;
        if ollama {
            let mut wr = WorkspaceRuntime::new("ollama", "Ollama (local)", ModelProviderKind::Ollama, "llama3.2");
            wr.endpoint = "http://localhost:11434".to_string();
            svc.add_workspace_runtime(ws, wr)?;
            n += 1;
        }
        if claude {
            svc.add_workspace_runtime(ws, WorkspaceRuntime::new("claude-code", "Claude Code", ModelProviderKind::ClaudeCode, ""))?;
            n += 1;
        }
        if codex {
            svc.add_workspace_runtime(ws, WorkspaceRuntime::new("codex", "Codex", ModelProviderKind::Codex, ""))?;
            n += 1;
        }
        println!("\nDefined {n} keyless runtime(s). Run the add-runtime line(s) above for key-backed providers, then `hive sync` to share.");
    } else {
        println!("\nRe-run `hive setup --apply` to define the keyless runtimes automatically.");
    }
    Ok(())
}

/// TOML wrapper for `hive config export/import` — the workspace's runtime set.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct WorkspaceRuntimesToml {
    #[serde(default, rename = "runtime")]
    runtimes: Vec<WorkspaceRuntime>,
}

/// `hive config export` — print the workspace runtimes as portable TOML. Inline
/// secrets are NEVER exported: they're downgraded to a secretRef the importer
/// resolves from HIVE_WS_SECRET_<ref>.
fn cmd_config_export(cfg: &Config) -> Result<()> {
    let svc = open_service(cfg)?;
    let mut runtimes = svc.list_workspace_runtimes(uuid_of_room(&cfg.room))?;
    for r in &mut runtimes {
        if matches!(r.credential, WorkspaceCredential::Inline { .. }) {
            r.credential = WorkspaceCredential::SecretRef { secret_ref: r.id.clone() };
        }
    }
    print!("{}", toml::to_string_pretty(&WorkspaceRuntimesToml { runtimes })?);
    Ok(())
}

/// `hive config import <file>` — define workspace runtimes from a TOML file
/// (additive; upserts by id). `hive sync` to share them with the team.
fn cmd_config_import(cfg: &Config, path: &str) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let doc: WorkspaceRuntimesToml = toml::from_str(&text).context("parsing config TOML")?;
    let mut svc = open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let mut n = 0;
    for wr in doc.runtimes {
        let id = wr.id.clone();
        svc.add_workspace_runtime(ws, wr)?;
        println!("+ runtime {id}");
        n += 1;
    }
    println!("Imported {n} workspace runtime(s). `hive sync` to share.");
    Ok(())
}

/// List the workspace-owned runtimes synced onto this box (spec §12.5).
fn cmd_runtimes(cfg: &Config) -> Result<()> {
    let svc = open_service(cfg)?;
    let rts = svc.list_workspace_runtimes(uuid_of_room(&cfg.room))?;
    if rts.is_empty() {
        println!("(no workspace runtimes — define one in the app, or `hive sync` a workspace that has them)");
    }
    for r in rts {
        let cred = match &r.credential {
            WorkspaceCredential::SecretRef { secret_ref } => {
                let have = std::env::var(WorkspaceCredential::env_var(secret_ref)).is_ok();
                format!("secretRef:{secret_ref}{}", if have { " ✓" } else { " (missing)" })
            }
            WorkspaceCredential::Inline { .. } => "inline".into(),
            WorkspaceCredential::None => "keyless".into(),
        };
        println!("{:<16} {:<22} {:?}/{}  [{cred}]", r.id, r.name, r.provider, r.model);
    }
    Ok(())
}

/// Resolve the runtime an agent should use: a workspace-owned runtime by id
/// (spec §12.5 — credential resolved via the worker's secret store, never a
/// personal key), else the personal env fallback.
fn resolve_agent_runtime(cfg: &Config, runtime_id: Option<&str>) -> Result<(ResolvedRuntime, String)> {
    let Some(id) = runtime_id else {
        return resolve_runtime_from_env();
    };
    let svc = open_service(cfg)?;
    let wr = svc
        .list_workspace_runtimes(uuid_of_room(&cfg.room))?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| anyhow!("no workspace runtime '{id}' (run `hive runtimes`, or define one in the app)"))?;
    let api_key = resolve_workspace_credential(&wr.credential);
    let rt = ResolvedRuntime {
        provider: wr.provider,
        model: wr.model.clone(),
        endpoint: wr.endpoint.clone(),
        api_key,
        args: Vec::new(),
        model_provider_id: None,
        model_base_url: None,
        context_window_tokens: None,
        keep_alive: None,
        think: None,
        tools: false,
        max_tool_rounds: None,
        allow_remote_file_tools: false,
    };
    Ok((rt, format!("workspace:{}", wr.name)))
}

/// v1 — act as an agent. After each sync, reply (as the primary responder) to
/// any new user message that is un-mentioned or `@primary`, via the resolved
/// runtime (a workspace-owned one with `--runtime`, else the personal env), and
/// post + sync the reply back as signed events.
async fn cmd_agent(cfg: &Config, name: String, runtime_id: Option<String>) -> Result<()> {
    // §12.5: a detached agent must run on a WORKSPACE-owned credential, never a
    // personal key. `hive agent` resolves a personal/local runtime, so it is a
    // local-dev convenience only — the production daemon is `hive worker`, which
    // requires a workspace-owned runtime. Refuse to run unattended in production
    // unless explicitly opted in for dev.
    if std::env::var("HIVE_ALLOW_PERSONAL_AGENT").is_err() {
        bail!(
            "`hive agent` runs on a personal credential, which isn't workspace-owned (\u{a7}12.5) — \
             use `hive worker` for production. For local dev only, set HIVE_ALLOW_PERSONAL_AGENT=1."
        );
    }
    // Pull config first so a `--runtime` workspace runtime is available.
    if cfg.relay_url.is_some() {
        let _ = sync_once(cfg).await;
    }
    let (rt, rt_label) = resolve_agent_runtime(cfg, runtime_id.as_deref())?;
    if rt.api_key.is_none() && rt.provider != ModelProviderKind::Ollama {
        bail!("runtime '{rt_label}' has no resolvable key (set a provider key, or provision HIVE_WS_SECRET_… for a workspace runtime)");
    }
    // Seed "seen" with existing messages so we don't reply to history.
    let mut seen: HashSet<Uuid> = HashSet::new();
    {
        let svc = open_service(cfg)?;
        for id in svc.store().list_session_ids()? {
            if let Some(s) = svc.load(id)? {
                seen.extend(s.messages.iter().map(|m| m.id));
            }
        }
    }
    println!("agent @{name} online ({rt_label}); replying to @primary / un-mentioned turns. Ctrl-C to stop.");
    let host_label = env_opt("HOSTNAME").unwrap_or_else(|| "this worker".into());
    let system = format!(
        "You are @{name}, an agent in a Hive workspace chat. Reply to the latest message concisely and helpfully.\n\n{}",
        hive_runtime::prompt::runtime_identity_block(&rt, &host_label)
    );
    loop {
        if cfg.relay_url.is_some() {
            if let Err(e) = sync_once(cfg).await {
                eprintln!("sync: {e}");
            }
        }
        let mut svc = open_service(cfg)?;
        for id in svc.store().list_session_ids()? {
            let Some(s) = svc.load(id)? else { continue };
            if id == workspace_config_session_id(s.workspace_id) {
                continue; // the config log isn't a chat
            }
            let Some(last) = s.messages.last() else { continue };
            let respond =
                last.role == MessageRole::User && !seen.contains(&last.id) && {
                    let t = parse_mentions(&last.body, &s);
                    t.primary || t.is_empty()
                };
            seen.extend(s.messages.iter().map(|m| m.id));
            if !respond {
                continue;
            }
            // Non-Send store is held across .await — cmd_agent runs on a
            // current-thread runtime (see run()), so this is allowed.
            let mut turns = turns_for(&s, None, &name);
            materialize_turns(cfg, &mut turns).await;
            print!("↳ @{name} replying in {} … ", s.id);
            // H1 gate: only execute MCP tools when the triggering author holds
            // ≥ HIVE_MCP_MIN_ROLE (default Contributor). Unresolved author →
            // fail closed. Under-privileged requesters still get a tool-free
            // reply, but cannot drive tools on this worker's key.
            let author_role = last.actor_identity.as_ref().map(|a| s.role_of(&a.id));
            let min_role = reply::mcp_min_role();
            let allow_mcp = reply::mcp_allowed_for(author_role, min_role);
            if !allow_mcp {
                eprintln!(
                    "withholding MCP tools for message {} — author role {:?} below minimum {:?}",
                    last.id, author_role, min_role
                );
            }
            // MCP tools when HIVE_MCP_CONFIG provisions them and the gate allows,
            // else a plain stream.
            let reply = reply::generate_reply(&rt, &system, &turns, allow_mcp, None).await?;
            let mid = svc.begin_assistant_message(s.id, s.workspace_id, name.clone(), rt_label.clone(), None, Some(last.id))?;
            svc.complete_assistant_message(s.id, s.workspace_id, mid, reply)?;
            seen.insert(mid);
            println!("done");
        }
        drop(svc);
        if cfg.relay_url.is_some() {
            let _ = sync_once(cfg).await;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Create a workspace agent (bound to a runtime; optionally a host + role).
fn cmd_add_agent(
    cfg: &Config,
    name: &str,
    runtime_id: &str,
    host: Option<String>,
    role: Option<String>,
) -> Result<()> {
    let ws = uuid_of_room(&cfg.room);
    let mut agent = WorkspaceAgent::new(name, runtime_id);
    agent.role = role.unwrap_or_default();
    agent.host_id = host.unwrap_or_default();
    let mut svc = open_service(cfg)?;
    agent.owner_actor_id = svc.author().id.clone();
    svc.add_agent(ws, ws, agent)?;
    println!("agent @{name} created on runtime '{runtime_id}' (sync to share).");
    Ok(())
}

fn cmd_agents(cfg: &Config) -> Result<()> {
    let svc = open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let agents = svc.load(workspace_config_session_id(ws))?.map(|s| s.workspace_agents).unwrap_or_default();
    if agents.is_empty() {
        println!("(no agents — `hive add-agent <name> <runtime-id>`)");
    }
    for a in agents {
        let host = if a.host_id.is_empty() { "owner-device".into() } else { a.host_id.clone() };
        println!("@{:<20} runtime={:<14} host={}", a.name, a.runtime_id, host);
    }
    Ok(())
}

// ── Hosts + worker daemon (spec §12.4) ──────────────────────────────────────

fn cmd_hosts(cfg: &Config) -> Result<()> {
    let svc = open_service(cfg)?;
    let me = svc.device_id().to_string();
    let hosts = svc.list_hosts(uuid_of_room(&cfg.room))?;
    if hosts.is_empty() {
        println!("(no hosts — `hive register-worker` to register this box)");
    }
    for h in hosts {
        let mark = if h.id == me { "  ← this box" } else { "" };
        println!("{:<38} {:?}  {}{mark}", h.id, h.kind, h.label);
    }
    Ok(())
}

fn cmd_register_worker(cfg: &Config, label: Option<String>) -> Result<String> {
    let mut svc = open_service(cfg)?;
    let id = svc.device_id().to_string();
    let label = label.or_else(|| env_opt("HOSTNAME")).unwrap_or_else(|| "worker".into());
    svc.register_host(uuid_of_room(&cfg.room), WorkspaceHost::new(id.clone(), HostKind::Worker, label))?;
    Ok(id)
}

fn cmd_set_agent_host(cfg: &Config, agent_name: &str, host_id: &str) -> Result<()> {
    let mut svc = open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let agents = svc
        .load(workspace_config_session_id(ws))?
        .map(|s| s.workspace_agents)
        .unwrap_or_default();
    let agent = agents
        .iter()
        .find(|a| a.name == agent_name)
        .ok_or_else(|| anyhow!("no agent '{agent_name}' in this workspace"))?;
    let aid = agent.id;
    svc.set_agent_host(ws, aid, host_id)?;
    println!("bound @{agent_name} to host {host_id} (sync to apply).");
    Ok(())
}

/// The worker daemon (spec §12.4-12.5). Registers this box as a worker host and
/// heartbeats its presence, then continuously drains **queued work** for the
/// agents bound to it: every unanswered `@mention` (backlog included, so it
/// catches up on work addressed while it was down, and picks up an agent freshly
/// reassigned to it) is answered via that agent's workspace runtime and synced
/// back. A failing turn is isolated and not retried in a tight loop. See
/// `worker.rs` for the turn itself (tool loop, worktrees, proposals).
async fn cmd_worker(cfg: &Config, label: Option<String>) -> Result<()> {
    let ws = uuid_of_room(&cfg.room);
    // Same label `cmd_register_worker` publishes, reused for the identity block
    // so an agent can say which worker it runs on.
    let host_label = label
        .clone()
        .or_else(|| env_opt("HOSTNAME"))
        .unwrap_or_else(|| "worker".into());
    let my_host = cmd_register_worker(cfg, label)?;
    if cfg.relay_url.is_some() {
        let _ = sync_once(cfg).await;
    }
    println!("worker {my_host} online — draining queued mentions for its agents. Ctrl-C to stop.");
    let svc = std::sync::Mutex::new(open_service(cfg)?);
    let ctx = worker::WorkerCtx {
        cfg,
        ws,
        my_host: &my_host,
        host_label: &host_label,
        settings: reply::ToolSettings::from_env(),
    };
    // Mentions whose reply hard-failed, so we don't retry them every tick.
    let mut failed: HashSet<Uuid> = HashSet::new();
    // Exponential backoff on repeated relay-sync failures, so a relay outage
    // isn't hammered every 3s. Resets to the base cadence on the first success.
    let mut sync_fails: u32 = 0;
    loop {
        if cfg.relay_url.is_some() {
            match sync_once(cfg).await {
                Ok(_) => sync_fails = 0,
                Err(e) => {
                    sync_fails = sync_fails.saturating_add(1);
                    let secs = sync_backoff_secs(sync_fails);
                    eprintln!("sync: {e} — retrying in {secs}s");
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                    continue;
                }
            }
        }
        // A transient store/DB error inside a tick must NOT kill the daemon —
        // log it and retry on the next tick (bare `?` here used to abort the loop).
        if let Err(e) = worker::drain_tick(&ctx, &svc, &mut failed, &worker::NoSink).await {
            eprintln!("worker: tick failed (retrying): {e}");
        }
        if cfg.relay_url.is_some() {
            let _ = sync_once(cfg).await;
        }
        // Sleep until the next tick, but wake immediately on a shutdown signal so
        // a redeploy/stop (SIGTERM) or Ctrl-C exits cleanly between ticks rather
        // than being SIGKILLed mid-poll.
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(3)) => {}
            _ = shutdown_signal() => {
                println!("worker: shutdown signal received — stopping.");
                return Ok(());
            }
        }
    }
}

/// Backoff after `fails` consecutive relay-sync failures: 6,12,24,48,96,120… s.
fn sync_backoff_secs(fails: u32) -> u64 {
    (3u64 << fails.min(5)).min(120)
}

/// Resolves when the process receives SIGTERM (orchestrator stop/redeploy) or
/// SIGINT (Ctrl-C), so the worker loop can exit gracefully.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Show queued work — unanswered agent mentions — and whether each agent's host
/// is online. A mention to an agent on an offline host is genuinely *queued*;
/// `hive set-agent-host <agent> <this-worker>` reassigns it here to drain it.
fn cmd_queue(cfg: &Config) -> Result<()> {
    let svc = open_service(cfg)?;
    let ws = uuid_of_room(&cfg.room);
    let hosts = svc.list_hosts(ws)?;
    let now = Timestamp::now();
    let mut any = false;
    for id in svc.store().list_session_ids()? {
        let Some(s) = svc.load(id)? else { continue };
        if id == workspace_config_session_id(s.workspace_id) {
            continue;
        }
        for pm in pending_mentions(&s) {
            any = true;
            let host_id = s
                .workspace_agents
                .iter()
                .find(|a| a.id == pm.agent_id)
                .map(|a| a.host_id.clone())
                .unwrap_or_default();
            let status = if host_id.is_empty() {
                "owner-device".to_string()
            } else {
                match hosts.iter().find(|h| h.id == host_id) {
                    Some(h) if h.is_online(&now, 90) => format!("{} · online", h.label),
                    Some(h) => format!("{} · OFFLINE → queued", h.label),
                    None => format!("{host_id} · unknown"),
                }
            };
            println!("@{:<18} in {}  [{status}]", pm.agent_name, s.id);
        }
    }
    if !any {
        println!("(nothing queued)");
    }
    Ok(())
}

/// Deterministic workspace id for a relay room (mirrors the app's
/// `room_workspace_id`), so config-log filtering matches the app.
fn uuid_of_room(room: &str) -> Uuid {
    const NS: Uuid = Uuid::from_u128(0x6869_7665_726f_6f6d_776f_726b_7370_6163);
    Uuid::new_v5(&NS, room.trim().as_bytes())
}

fn truncate(s: &str, n: usize) -> String {
    let t = if s.is_empty() { "(untitled)" } else { s };
    if t.chars().count() <= n {
        t.to_string()
    } else {
        format!("{}…", t.chars().take(n - 1).collect::<String>())
    }
}

fn usage() -> ! {
    eprintln!(
        "hive — headless Hive client\n\n\
         USAGE:\n  \
         hive whoami\n  \
         hive chats\n  \
         hive new [title]\n  \
         hive tail <chat-id> [--watch]\n  \
         hive send <chat-id> <message…> [--push]\n  \
         hive sync [--watch]\n  \
         hive runtimes                  list workspace-owned runtimes (spec §12.5)\n  \
         hive agent [name] [--runtime <id>]   reply as the primary agent; --runtime uses a\n                                       workspace-owned credential (else a personal env key)\n  \
         hive hosts                     list workspace hosts (devices + workers)\n  \
         hive register-worker [--label <name>]   register this box as a worker host\n  \
         hive set-agent-host <agent> <host-id>   bind an agent to a host\n  \
         hive proposals                 list the workspace's proposals with status + votes\n  \
         hive worker [--label <name>]   run the worker daemon (host detached agents, §12.4)\n  \
         hive queue                     show queued work (unanswered mentions + host status)\n  \
         hive serve [--listen ADDR] [--pair] [--token-file PATH] [--label NAME]\n                                       \
         run the home host for the iOS app (sync + worker + HTTP/SSE API)\n  \
         hive bind-repo <name|path> [--chat <id>] | --clear   bind a checkout for agent file tools\n  \
         hive repos                     show the repos dir and bindings\n  \
         hive pair [--listen ADDR] [--public-url URL]   show the phone pairing QR code\n\n\
         Setup:\n  \
         hive enroll <hivews1:invite> [--token <hrt1>]   one-shot: save the workspace connection\n  \
         hive join <invite-code>        redeem a relay invite to self-enroll this box's identity\n  \
         hive setup [--apply]           detect providers; --apply defines keyless runtimes\n  \
         hive config <export|import <file>>   round-trip the workspace runtime set\n\n\
         Config via env (override the saved config.toml from `enroll`):\n  \
         HIVE_DATA_DIR, HIVE_RELAY_URL, HIVE_RELAY_ACCESS_TOKEN, HIVE_WORKSPACE, HIVE_WORKSPACE_KEY."
    );
    std::process::exit(2);
}

/// Whether the command line asks for help (`-h` / `--help` / `help` anywhere
/// before a `--` separator).
fn wants_help(args: &[String]) -> bool {
    args.first().is_some_and(|a| a == "help")
        || args.iter().take_while(|a| *a != "--").any(|a| a == "--help" || a == "-h")
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cfg = config();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let rest = &args[args.len().min(1)..];
    // `hive <cmd> --help` prints usage instead of running the command (a bare
    // `hive serve --help` used to start a real server on the default data dir).
    if wants_help(&args) {
        usage();
    }
    let has = |flag: &str| rest.iter().any(|a| a == flag);
    let flag_val = |flag: &str| {
        rest.iter()
            .position(|a| a == flag)
            .and_then(|i| rest.get(i + 1))
            .cloned()
    };
    // Positional args exclude flags and any value immediately after a known
    // value-flag (so `agent bot --runtime ws1` → positional = [bot]).
    let value_flags = [
        "--runtime",
        "--endpoint",
        "--secret-ref",
        "--secret",
        "--label",
        "--host",
        "--chat",
        "--listen",
        "--token-file",
        "--ntfy-url",
        "--ntfy-topic",
        "--public-url",
        "--name",
        "--role",
    ];
    let mut positional: Vec<&String> = Vec::new();
    let mut skip = false;
    for a in rest {
        if skip {
            skip = false;
            continue;
        }
        if a.starts_with("--") {
            if value_flags.contains(&a.as_str()) {
                skip = true;
            }
            continue;
        }
        positional.push(a);
    }

    // A current-thread runtime is all the CLI needs — it only ever `block_on`s
    // (never spawns), and keeps the non-Send EventStore on one thread.
    let rt = || -> Result<tokio::runtime::Runtime> {
        Ok(tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?)
    };

    match cmd {
        "whoami" => cmd_whoami(&cfg),
        "chats" => cmd_chats(&cfg),
        "new" => cmd_new(&cfg, positional.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ")),
        "tail" => {
            let id = positional
                .first()
                .ok_or_else(|| anyhow!("usage: hive tail <chat-id> [--watch]"))?;
            cmd_tail(&cfg, Uuid::parse_str(id)?, has("--watch"))
        }
        "send" => {
            let id = positional
                .first()
                .ok_or_else(|| anyhow!("usage: hive send <chat-id> <message…> [--push]"))?;
            let chat_id = Uuid::parse_str(id)?;
            let body = positional[1..]
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if body.trim().is_empty() {
                bail!("message is empty");
            }
            rt()?.block_on(cmd_send(&cfg, chat_id, body, has("--push")))
        }
        "sync" => rt()?.block_on(cmd_sync(&cfg, has("--watch"))),
        "runtimes" => cmd_runtimes(&cfg),
        "add-runtime" => {
            if positional.len() < 4 {
                bail!("usage: hive add-runtime <id> <name> <provider> <model> [--endpoint <url>] [--secret-ref <name> | --secret <value>]");
            }
            cmd_add_runtime(
                &cfg,
                positional[0],
                positional[1],
                positional[2],
                positional[3],
                flag_val("--endpoint"),
                flag_val("--secret-ref"),
                flag_val("--secret"),
            )
        }
        "rm-runtime" => {
            let id = positional.first().ok_or_else(|| anyhow!("usage: hive rm-runtime <id>"))?;
            cmd_rm_runtime(&cfg, id)
        }
        "enroll" => {
            let invite = positional
                .first()
                .ok_or_else(|| anyhow!("usage: hive enroll <hivews1:invite> [--token <access-token>]"))?;
            cmd_enroll(&cfg, invite, flag_val("--token"))
        }
        "join" => {
            let code = positional
                .first()
                .ok_or_else(|| anyhow!("usage: hive join <invite-code>"))?;
            rt()?.block_on(cmd_join(&cfg, code))
        }
        "setup" => cmd_setup(&cfg, has("--apply")),
        "config" => match positional.first().map(|s| s.as_str()) {
            Some("export") => cmd_config_export(&cfg),
            Some("import") => {
                let path = positional
                    .get(1)
                    .ok_or_else(|| anyhow!("usage: hive config import <file.toml>"))?;
                cmd_config_import(&cfg, path)
            }
            _ => bail!("usage: hive config <export|import <file.toml>>"),
        },
        "agents" => cmd_agents(&cfg),
        "proposals" => cmd_proposals(&cfg),
        "add-agent" => {
            if positional.len() < 2 {
                bail!("usage: hive add-agent <name> <runtime-id> [--host <id>] [--role <role>]");
            }
            cmd_add_agent(&cfg, positional[0], positional[1], flag_val("--host"), flag_val("--role"))
        }
        "hosts" => cmd_hosts(&cfg),
        "register-worker" => {
            let id = cmd_register_worker(&cfg, flag_val("--label"))?;
            println!("registered this box as worker host {id} (sync to share).");
            Ok(())
        }
        "set-agent-host" => {
            if positional.len() < 2 {
                bail!("usage: hive set-agent-host <agent-name> <host-id>");
            }
            cmd_set_agent_host(&cfg, positional[0], positional[1])
        }
        "worker" => rt()?.block_on(cmd_worker(&cfg, flag_val("--label"))),
        "queue" => cmd_queue(&cfg),
        "serve" => {
            let opts = serve::ServeOptions::from_flags(
                &cfg,
                flag_val("--listen"),
                flag_val("--token-file"),
                flag_val("--ntfy-url"),
                flag_val("--ntfy-topic"),
                flag_val("--public-url"),
                flag_val("--label"),
                flag_val("--name"),
                has("--pair"),
            )?;
            serve::cmd_serve(cfg, opts)
        }
        "pair" => {
            let opts = serve::ServeOptions::from_flags(
                &cfg,
                flag_val("--listen"),
                flag_val("--token-file"),
                None,
                None,
                flag_val("--public-url"),
                None,
                None,
                true,
            )?;
            serve::cmd_pair(&opts)
        }
        "bind-repo" => repos::cmd_bind_repo(
            &cfg.data_dir,
            positional.first().map(|s| s.as_str()),
            flag_val("--chat").as_deref(),
            has("--clear"),
        ),
        "repos" => repos::cmd_repos(&cfg.data_dir),
        "agent" => {
            let name = positional.first().map(|s| s.as_str()).unwrap_or("assistant").to_string();
            let runtime_id = flag_val("--runtime").or_else(|| env_opt("HIVE_RUNTIME"));
            rt()?.block_on(cmd_agent(&cfg, name, runtime_id))
        }
        _ => usage(),
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn help_flags_never_run_the_command() {
        assert!(wants_help(&argv(&["serve", "--help"])));
        assert!(wants_help(&argv(&["serve", "-h"])));
        assert!(wants_help(&argv(&["help"])));
        assert!(!wants_help(&argv(&["serve", "--pair"])));
        assert!(!wants_help(&argv(&["send", "c", "--", "--help"])));
    }

    #[test]
    fn custom_provider_is_accepted() {
        assert_eq!(provider_kind("custom").unwrap(), ModelProviderKind::Custom);
        assert_eq!(provider_kind("Custom").unwrap(), ModelProviderKind::Custom);
        assert!(provider_kind("nope").is_err());
    }
}
