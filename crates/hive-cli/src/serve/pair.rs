//! Bearer token + pairing for `hive serve`.
//!
//! The token is 32 random bytes, hex-encoded, kept in a 0600 file
//! (`<HIVE_DATA_DIR>/serve.token` by default) and generated on first run. The
//! phone gets it once, through the pairing link
//! `hive://pair?url=<base64url(serverURL)>&token=<token>` shown as a terminal
//! QR code by `hive serve --pair`, and keeps it in the iOS Keychain.

use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use anyhow::{bail, Context, Result};
use base64::Engine;

/// Read the token at `path`, creating it (0600, 32 random bytes as hex) when
/// the file is missing or empty.
pub fn load_or_create_token(path: &Path) -> Result<String> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        let t = existing.trim().to_string();
        if !t.is_empty() {
            if t.len() < 32 {
                bail!("{} holds a token shorter than 32 characters; delete it to generate a new one", path.display());
            }
            return Ok(t);
        }
    }
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow::anyhow!("no OS randomness for the serve token: {e}"))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).with_context(|| format!("creating {}", path.display()))?;
    f.write_all(token.as_bytes())?;
    #[cfg(unix)]
    {
        // The mode above only applies on create; tighten a pre-existing empty file too.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}

/// Constant-time token comparison (no early exit on the first differing byte).
pub fn token_matches(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `hive://pair?url=<base64url(serverURL), no padding>&token=<token>`.
pub fn pairing_url(server_url: &str, token: &str) -> String {
    let url = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(server_url.trim_end_matches('/').as_bytes());
    format!("hive://pair?url={url}&token={token}")
}

/// Whether `ip` is an address the API may listen on without an explicit
/// override: loopback, a private LAN range, or the tailnet (Tailscale's
/// 100.64.0.0/10 and fd7a:115c:a1e0::/48). Never the unspecified address.
pub fn bind_allowed(ip: IpAddr) -> bool {
    !ip.is_unspecified() && hive_core::endpoint::host_is_private(&ip.to_string())
}

/// The base URL the phone should use for a server listening on `addr`.
pub fn default_server_url(addr: SocketAddr) -> String {
    format!("http://{addr}")
}

/// Render `text` as a QR code for a terminal: two module rows per text line
/// with half-block characters, explicit black-on-white ANSI colors (so it
/// scans on dark and light terminal themes alike) and a 2-module quiet zone.
pub fn render_qr(text: &str) -> Result<String> {
    use qrcodegen::{QrCode, QrCodeEcc};
    let qr = QrCode::encode_text(text, QrCodeEcc::Medium).map_err(|e| anyhow::anyhow!("QR encode: {e:?}"))?;
    const QUIET: i32 = 2;
    let size = qr.size();
    let dark = |x: i32, y: i32| x >= 0 && y >= 0 && x < size && y < size && qr.get_module(x, y);
    let mut out = String::new();
    let mut y = -QUIET;
    while y < size + QUIET {
        out.push_str("\x1b[30;47m");
        for x in -QUIET..size + QUIET {
            out.push(match (dark(x, y), dark(x, y + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        out.push_str("\x1b[0m\n");
        y += 2;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_url_has_the_contract_shape() {
        let link = pairing_url("http://100.101.102.104:8787/", "abc123");
        let rest = link.strip_prefix("hive://pair?").expect("hive://pair? prefix");
        let mut url = None;
        let mut token = None;
        for kv in rest.split('&') {
            let (k, v) = kv.split_once('=').unwrap();
            match k {
                "url" => url = Some(v.to_string()),
                "token" => token = Some(v.to_string()),
                other => panic!("unexpected key {other}"),
            }
        }
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(url.unwrap()).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "http://100.101.102.104:8787");
        assert_eq!(token.as_deref(), Some("abc123"));
        assert!(!link.contains('='.to_string().repeat(2).as_str()), "no base64 padding");
    }

    #[test]
    fn token_file_is_created_0600_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("serve.token");
        let t1 = load_or_create_token(&p).unwrap();
        assert_eq!(t1.len(), 64);
        assert!(t1.chars().all(|c| c.is_ascii_hexdigit()));
        let t2 = load_or_create_token(&p).unwrap();
        assert_eq!(t1, t2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn token_compare() {
        assert!(token_matches("abcd", "abcd"));
        assert!(!token_matches("abcd", "abce"));
        assert!(!token_matches("abcd", "abc"));
        assert!(!token_matches("abcd", ""));
    }

    #[test]
    fn binds_only_loopback_lan_or_tailnet() {
        assert!(bind_allowed("127.0.0.1".parse().unwrap()));
        assert!(bind_allowed("100.101.102.104".parse().unwrap()));
        assert!(bind_allowed("192.168.1.5".parse().unwrap()));
        assert!(bind_allowed("::1".parse().unwrap()));
        assert!(!bind_allowed("0.0.0.0".parse().unwrap()));
        assert!(!bind_allowed("::".parse().unwrap()));
        assert!(!bind_allowed("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn qr_renders_square_block() {
        let q = render_qr("hive://pair?url=aHR0cDovL3g&token=t").unwrap();
        let lines: Vec<&str> = q.lines().collect();
        assert!(lines.len() > 10);
        assert!(q.contains('█'));
    }
}
