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
        println!("serve:     {}", describe_status(&st, super::now()));
    }
    let phones = Registry::load(&dir)?.list();
    println!("phones:    {} active, {} revoked", phones.iter().filter(|p| p.is_active()).count(), phones.iter().filter(|p| !p.is_active()).count());
    Ok(())
}

/// `secs` as a short age: `45s`, `12m`, `6h 33m`, `3d 4h`.
fn short_age(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        s => format!("{}d {}h", s / 86_400, s % 86_400 / 3600),
    }
}

/// The `serve:` line of `hive remote status`. Connected shows how long the
/// relay link has been up, not when the file last changed (a link that stays
/// up never changes it, so "updated 6h ago" read like a stale file). A file
/// whose heartbeat stopped was left by a `hive serve` that isn't running.
pub(crate) fn describe_status(st: &LiveStatus, now: u64) -> String {
    let detail = format!(
        "status {}, push {}",
        if st.status.is_empty() { "-" } else { st.status.as_str() },
        if st.push { "available" } else { "unavailable" }
    );
    let error = st.last_error.as_ref().map(|e| format!(", last error: {e}")).unwrap_or_default();
    let beat = (st.heartbeat > 0).then(|| now.saturating_sub(st.heartbeat));
    if let Some(b) = beat.filter(|b| *b > 3 * super::STATUS_HEARTBEAT.as_secs()) {
        let was = if st.connected { "connected" } else { "not connected" };
        return format!("unknown: hive serve last reported {} ago ({was}, {detail}) — is it running?{error}", short_age(b));
    }
    if st.connected {
        match (st.connected_since, beat) {
            (Some(since), Some(b)) => {
                format!("connected for {} ({detail}, checked {} ago){error}", short_age(now.saturating_sub(since)), short_age(b))
            }
            // A status file from before connection age was recorded.
            _ => format!("connected ({detail}, last change {} ago){error}", short_age(now.saturating_sub(st.at))),
        }
    } else {
        format!("not connected ({detail}, since {} ago){error}", short_age(now.saturating_sub(st.at)))
    }
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

#[cfg(test)]
mod status_tests {
    use super::*;

    fn st(connected: bool) -> LiveStatus {
        LiveStatus { connected, status: "active".into(), push: true, ..Default::default() }
    }

    #[test]
    fn connected_shows_live_connection_age_not_last_change() {
        let now = 1_000_000;
        let mut s = st(true);
        s.at = now - 23_589;
        s.connected_since = Some(now - 23_589);
        s.heartbeat = now - 12;
        assert_eq!(describe_status(&s, now), "connected for 6h 33m (status active, push available, checked 12s ago)");
    }

    #[test]
    fn a_stopped_serve_is_not_reported_connected() {
        let now = 1_000_000;
        let mut s = st(true);
        s.at = now - 7200;
        s.connected_since = Some(now - 7200);
        s.heartbeat = now - 300;
        let line = describe_status(&s, now);
        assert!(line.starts_with("unknown: hive serve last reported 5m ago (connected,"), "{line}");
        assert!(line.contains("is it running?"));
    }

    #[test]
    fn down_and_legacy_files_read_sensibly() {
        let now = 1_000_000;
        let mut down = st(false);
        down.push = false;
        down.at = now - 45;
        down.heartbeat = now - 5;
        down.last_error = Some("relay unreachable".into());
        assert_eq!(
            describe_status(&down, now),
            "not connected (status active, push unavailable, since 45s ago), last error: relay unreachable"
        );
        // Written by an older hive serve: no heartbeat, no connection time.
        let mut legacy = st(true);
        legacy.at = now - 90_000;
        assert_eq!(describe_status(&legacy, now), "connected (status active, push available, last change 1d 1h ago)");
        assert_eq!(short_age(59), "59s");
        assert_eq!(short_age(3599), "59m");
    }
}
