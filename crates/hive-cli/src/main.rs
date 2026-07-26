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

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use hive_core::{workspace_config_session_id, derive_workspace_key};
use hive_runtime::{ChatService, EventStore, FileKeyVault, IdentityStore, RelayClient, SyncEngine};
use uuid::Uuid;

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

fn config() -> Config {
    let data_dir = env_opt("HIVE_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| env_opt("HOME").map(|h| PathBuf::from(h).join(".hive")))
        .unwrap_or_else(|| PathBuf::from("./hive-data"));
    Config {
        data_dir,
        relay_url: env_opt("HIVE_RELAY_URL"),
        token: env_opt("HIVE_RELAY_ACCESS_TOKEN"),
        github_token: env_opt("HIVE_RELAY_GITHUB_TOKEN"),
        room: env_opt("HIVE_WORKSPACE").unwrap_or_else(|| "default".into()),
        key: env_opt("HIVE_WORKSPACE_KEY"),
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
         hive tail <chat-id> [--watch]\n  \
         hive send <chat-id> <message…> [--push]\n  \
         hive sync [--watch]\n\n\
         Config via env: HIVE_DATA_DIR, HIVE_RELAY_URL, HIVE_RELAY_ACCESS_TOKEN,\n\
         HIVE_WORKSPACE, HIVE_WORKSPACE_KEY."
    );
    std::process::exit(2);
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
    let has = |flag: &str| rest.iter().any(|a| a == flag);
    let positional: Vec<&String> = rest.iter().filter(|a| !a.starts_with("--")).collect();

    match cmd {
        "whoami" => cmd_whoami(&cfg),
        "chats" => cmd_chats(&cfg),
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
            tokio::runtime::Runtime::new()?.block_on(cmd_send(&cfg, chat_id, body, has("--push")))
        }
        "sync" => tokio::runtime::Runtime::new()?.block_on(cmd_sync(&cfg, has("--watch"))),
        _ => usage(),
    }
}
