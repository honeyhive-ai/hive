//! `hive remote …` and `hive phones …`: separate processes that hand work to a
//! running `hive serve` through files in `tunnel/` (§16).

use std::path::Path;

use anyhow::{bail, Result};

use super::{
    ensure_private_dir, pending_dir, read_json, tunnel_dir, write_json, HostKeys, LiveStatus, Registry, RelaySettings,
    RevokeFile,
};

/// `hive remote enable --relay <url> [--reg-token T] [--insecure-relay]`.
pub fn remote_enable(data_dir: &Path, relay: Option<String>, reg_token: Option<String>, insecure: bool) -> Result<()> {
    let Some(relay) = relay else { bail!("usage: hive remote enable --relay <https://relay.example.com> [--reg-token T]") };
    let settings = RelaySettings::new(&relay, reg_token, insecure)?;
    let dir = tunnel_dir(data_dir);
    ensure_private_dir(&dir)?;
    let keys = HostKeys::load_or_create(&dir)?;
    write_json(&RelaySettings::file(&dir), &settings)?;
    println!("Remote access enabled through {} (host id {}).", settings.relay, keys.host_id());
    println!("Restart `hive serve` to connect, then run `hive pair` to pair a phone.");
    Ok(())
}

/// `hive remote disable`: keeps keys and phones, so enabling again needs no re-pairing.
pub fn remote_disable(data_dir: &Path) -> Result<()> {
    let dir = tunnel_dir(data_dir);
    match read_json::<RelaySettings>(&RelaySettings::file(&dir))? {
        Some(mut s) => {
            s.enabled = false;
            write_json(&RelaySettings::file(&dir), &s)?;
            println!("Remote access disabled. Restart `hive serve` to drop the relay connection.");
        }
        None => println!("Remote access is not enabled."),
    }
    Ok(())
}

/// `hive remote status`.
pub fn remote_status(data_dir: &Path) -> Result<()> {
    let dir = tunnel_dir(data_dir);
    match read_json::<RelaySettings>(&RelaySettings::file(&dir))? {
        Some(s) => println!("relay:     {} ({})", s.relay, if s.enabled { "enabled" } else { "disabled" }),
        None => println!("relay:     not configured (hive remote enable --relay <url>)"),
    }
    match HostKeys::load(&dir)? {
        Some(k) => println!("host id:   {}", k.host_id()),
        None => println!("host id:   (no keys yet)"),
    }
    if let Some(st) = read_json::<LiveStatus>(&dir.join("status.json"))? {
        let age = super::now().saturating_sub(st.at);
        println!(
            "serve:     {} (status {}, push {}, updated {age}s ago){}",
            if st.connected { "connected" } else { "not connected" },
            if st.status.is_empty() { "-" } else { st.status.as_str() },
            if st.push { "available" } else { "unavailable" },
            st.last_error.map(|e| format!(", last error: {e}")).unwrap_or_default()
        );
    }
    let phones = Registry::load(&dir)?.list();
    println!("phones:    {} active, {} revoked", phones.iter().filter(|p| p.is_active()).count(), phones.iter().filter(|p| !p.is_active()).count());
    Ok(())
}

/// `hive remote reset`: new host keys (new host id); every phone must re-pair.
pub fn remote_reset(data_dir: &Path) -> Result<()> {
    let dir = tunnel_dir(data_dir);
    ensure_private_dir(&dir)?;
    let keys = HostKeys::generate();
    keys.save(&dir)?;
    for f in [Registry::file(&dir), dir.join("status.json")] {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all(pending_dir(&dir));
    println!("New host keys: host id {}. Every phone must pair again; restart `hive serve`.", keys.host_id());
    Ok(())
}

/// `hive phones list`.
pub fn phones_list(data_dir: &Path) -> Result<()> {
    let phones = Registry::load(&tunnel_dir(data_dir))?.list();
    if phones.is_empty() {
        println!("No paired phones. Run `hive pair` with remote access enabled.");
        return Ok(());
    }
    println!("{:<24} {:<24} {:<10} {:<12} last seen", "PID", "NAME", "STATE", "PAIRED");
    for p in phones {
        let fmt_day = |t: u64| {
            time::OffsetDateTime::from_unix_timestamp(t as i64).map(|d| d.date().to_string()).unwrap_or_default()
        };
        println!(
            "{:<24} {:<24} {:<10} {:<12} {}",
            p.pid,
            p.name,
            if p.is_active() { "active" } else { "revoked" },
            fmt_day(p.enrolled_at),
            p.last_seen.map(fmt_day).unwrap_or_else(|| "-".into())
        );
    }
    Ok(())
}

/// `hive phones revoke <pid|name> [--keep-token]` (§13.2). Marks the phone
/// revoked in `phones.json` and leaves `pending/revoke-<pid>.json` for
/// `hive serve` (end live sessions, tell the relay until it acks). Unless
/// `--keep-token`, it also rotates the direct-path bearer token.
pub fn phones_revoke(data_dir: &Path, who: Option<&str>, token_file: &Path, keep_token: bool) -> Result<()> {
    let Some(who) = who else { bail!("usage: hive phones revoke <pid|name> [--keep-token]") };
    let dir = tunnel_dir(data_dir);
    let reg = Registry::load(&dir)?;
    let Some(rec) = reg.resolve(who) else { bail!("no phone '{who}' (see `hive phones list`; names must be unique)") };
    let exp = reg.revoke(&rec.pid)?.unwrap_or(rec.cred_exp);
    write_json(&pending_dir(&dir).join(format!("revoke-{}.json", rec.pid)), &RevokeFile { pid: rec.pid.clone(), exp })?;
    println!("Revoked {} ({}). A running `hive serve` ends its sessions and tells the relay within seconds.", rec.name, rec.pid);
    if keep_token {
        println!("The direct-path bearer token was kept (--keep-token).");
    } else if token_file.exists() {
        crate::serve::pair::rotate_token(token_file)?;
        println!(
            "Rotated the direct-path bearer token ({}): phones using the direct path must pair again (`hive pair`).",
            token_file.display()
        );
    }
    Ok(())
}
