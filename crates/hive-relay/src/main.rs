//! `hive-relay` binary — serves the rendezvous + forwarding router (see `lib.rs`).
//!
//! Address resolution (cloud-friendly):
//! 1. `$PORT` (set by Render / Cloud Run / Railway / Heroku) → `0.0.0.0:$PORT`
//! 2. `$HIVE_RELAY_ADDR` (full `host:port`)
//! 3. default `0.0.0.0:8443`
//!
//! Durability: set `$HIVE_RELAY_DATA_DIR` to a writable directory (e.g. a Fly
//! volume mount) and the relay snapshots its state there, reloading on boot so a
//! restart/redeploy keeps the social graph + message history. Unset ⇒ in-memory.
//! `$HIVE_RELAY_FRIEND_CAP` (optional) caps accepted friends per account.

use hive_relay::token::{self, TokenClaims};
use hive_relay::{EntitlementPolicy, RelayState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Operator subcommands (run before the server). `keygen` mints the issuer
    // keypair; `issue` signs an entitlement token for a customer. The serving
    // relay only ever verifies (with the public key), never mints.
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("keygen") => return cmd_keygen(),
        Some("issue") => return cmd_issue(&args[2..]),
        Some("help" | "--help" | "-h") => {
            print_usage();
            return Ok(());
        }
        _ => {} // fall through to serve
    }

    let addr = match std::env::var("PORT") {
        Ok(port) if !port.is_empty() => format!("0.0.0.0:{port}"),
        _ => std::env::var("HIVE_RELAY_ADDR").unwrap_or_else(|_| "0.0.0.0:8443".to_string()),
    };

    // Build state: entitlement from env, optional friend cap, optional durable
    // snapshot dir (loaded here so a restart resumes where it left off).
    let mut state = RelayState::with_entitlement(EntitlementPolicy::from_env());
    if let Some(cap) = std::env::var("HIVE_RELAY_FRIEND_CAP").ok().and_then(|v| v.parse().ok()) {
        state = state.with_friend_cap(Some(cap));
    }
    if let Ok(dir) = std::env::var("HIVE_RELAY_DATA_DIR") {
        if !dir.trim().is_empty() {
            state = state.with_persistence(dir);
        }
    }

    // Periodically snapshot durable state so an unexpected crash loses at most a
    // few seconds; a graceful shutdown flushes once more below.
    if state.persistence_enabled() {
        let bg = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tick.tick().await;
                if let Err(e) = bg.flush() {
                    eprintln!("hive-relay: snapshot flush failed: {e}");
                }
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("hive-relay listening on {addr}");
    axum::serve(listener, hive_relay::router_with_state(state.clone()))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // Final flush on the way out so a planned redeploy never loses state.
    if let Err(e) = state.flush() {
        eprintln!("hive-relay: final snapshot flush failed: {e}");
    }
    Ok(())
}

/// Resolve when the process is asked to stop (Ctrl-C, or SIGTERM from the host /
/// orchestrator on a redeploy).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

fn print_usage() {
    eprintln!(
        "hive-relay — hosted rendezvous + forwarding relay\n\n\
         Run the server (default):   hive-relay\n\
         Generate an issuer keypair: hive-relay keygen\n\
         Mint an entitlement token:  hive-relay issue --key <priv-hex> --sub <id> \\\n\
         \x20                            [--plan team] [--exp-days 365] [--max-members 50] \\\n\
         \x20                            [--turn] [--cap remove_member --cap rotate_key]\n\n\
         Set the relay's HIVE_RELAY_TOKEN_PUBKEY to the keygen public key; keep the\n\
         private key with your issuer/billing backend only."
    );
}

/// `hive-relay keygen` — print a fresh issuer keypair.
fn cmd_keygen() -> anyhow::Result<()> {
    let sk = token::generate_signing_key();
    let priv_hex = token::to_hex(&sk.to_bytes());
    let pub_hex = token::to_hex(&sk.verifying_key().to_bytes());
    println!("# Ed25519 issuer keypair");
    println!("private_key={priv_hex}   # SECRET — keep on the issuer only (never on the relay)");
    println!("public_key={pub_hex}    # set as HIVE_RELAY_TOKEN_PUBKEY on the relay");
    Ok(())
}

/// `hive-relay issue …` — sign an entitlement token for a customer.
fn cmd_issue(args: &[String]) -> anyhow::Result<()> {
    let mut key_hex = String::new();
    let mut claims = TokenClaims::default();
    let mut exp_days: Option<u64> = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut val = || it.next().cloned().unwrap_or_default();
        match flag.as_str() {
            "--key" => key_hex = val(),
            "--sub" => claims.sub = val(),
            "--plan" => claims.plan = val(),
            "--exp-days" => exp_days = val().parse().ok(),
            "--max-members" => claims.max_members = val().parse().ok(),
            "--retention-days" => claims.retention_days = val().parse().ok(),
            "--turn" => claims.turn = true,
            "--cap" => claims.caps.push(val()),
            other => anyhow::bail!("unknown flag {other} (try `hive-relay help`)"),
        }
    }
    let signing = token::parse_signing_key(&key_hex)
        .ok_or_else(|| anyhow::anyhow!("--key must be a 64-char hex Ed25519 secret (from `keygen`)"))?;
    if claims.sub.trim().is_empty() {
        anyhow::bail!("--sub <account-or-org-id> is required");
    }
    if let Some(days) = exp_days {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        claims.exp = now + days * 86_400;
    }
    println!("{}", token::issue(&signing, &claims));
    Ok(())
}
