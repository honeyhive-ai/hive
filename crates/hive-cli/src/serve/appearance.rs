//! `GET /v1/appearance`: the theme the phone follows when it is set to "Match
//! host".
//!
//! The desktop app keeps its palette and light/dark mode in the web view's
//! localStorage, and mirrors both to `<app data dir>/appearance.json` whenever
//! they change (the `save_appearance` command in `app/src/lib.rs`). `hive
//! serve` reads, in order:
//!
//! 1. its own config: `HIVE_SERVE_PALETTE` and `HIVE_SERVE_APPEARANCE`, for a
//!    headless home host with no desktop app;
//! 2. the desktop app's `appearance.json` on this machine
//!    (`HIVE_DESKTOP_DATA_DIR`, else the OS app-data dir for `com.hive.desktop`).
//!
//! Each field resolves on its own, so a box can pin the palette in config and
//! still follow the desktop's mode. The file is read per request (it is tiny),
//! so a theme change on the desktop reaches the phone without a restart.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::env_opt;

/// The desktop's palette families (`THEMES` in `web/src/lib/theme.ts`).
pub const PALETTES: [&str; 6] = ["pollen", "studio", "harbor", "slate", "meadow", "obsidian"];
/// The desktop's appearance modes (`AppearanceMode` in `theme.ts`).
pub const MODES: [&str; 3] = ["auto", "light", "dark"];

/// The desktop app's bundle identifier (`app/tauri.conf.json`), which names its
/// app-data directory.
const DESKTOP_APP_ID: &str = "com.hive.desktop";
/// The file the desktop app writes into its data dir.
pub const FILE_NAME: &str = "appearance.json";

/// Where `hive serve` looks for the host's theme.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppearanceConfig {
    /// Palette from serve config (`HIVE_SERVE_PALETTE`), already validated.
    pub palette: Option<String>,
    /// Mode from serve config (`HIVE_SERVE_APPEARANCE`), already validated.
    pub mode: Option<String>,
    /// The desktop app's data dir on this machine, if there is one.
    pub desktop_dir: Option<PathBuf>,
}

impl AppearanceConfig {
    /// From the environment. An unknown palette or mode is ignored (with a
    /// warning) rather than refusing to start: it only themes the phone.
    pub fn from_env() -> Self {
        let palette = env_opt("HIVE_SERVE_PALETTE").and_then(|p| {
            let v = normalize_palette(&p);
            if v.is_none() {
                eprintln!("hive serve: ignoring HIVE_SERVE_PALETTE={p:?} (one of {})", PALETTES.join(", "));
            }
            v
        });
        let mode = env_opt("HIVE_SERVE_APPEARANCE").and_then(|m| {
            let v = normalize_mode(&m);
            if v.is_none() {
                eprintln!("hive serve: ignoring HIVE_SERVE_APPEARANCE={m:?} (one of {})", MODES.join(", "));
            }
            v
        });
        let desktop_dir = env_opt("HIVE_DESKTOP_DATA_DIR").map(PathBuf::from).or_else(default_desktop_dir);
        Self { palette, mode, desktop_dir }
    }
}

/// A known palette name, lowercased. The desktop renamed `midnight` to
/// `slate` (`loadTheme` remaps it), so this does too.
pub fn normalize_palette(raw: &str) -> Option<String> {
    let p = raw.trim().to_ascii_lowercase();
    let p = if p == "midnight" { "slate".to_string() } else { p };
    PALETTES.contains(&p.as_str()).then_some(p)
}

/// A known mode, lowercased (`system` is accepted as `auto`).
pub fn normalize_mode(raw: &str) -> Option<String> {
    let m = raw.trim().to_ascii_lowercase();
    let m = if m == "system" { "auto".to_string() } else { m };
    MODES.contains(&m.as_str()).then_some(m)
}

/// The OS app-data dir Tauri gives the desktop app (`app_data_dir()`):
/// `~/Library/Application Support/<id>` on macOS, `$XDG_DATA_HOME/<id>` (or
/// `~/.local/share/<id>`) on Linux, `%APPDATA%\<id>` on Windows.
fn default_desktop_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        return env_opt("APPDATA").map(|d| PathBuf::from(d).join(DESKTOP_APP_ID));
    }
    let home = env_opt("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Application Support").join(DESKTOP_APP_ID));
    }
    env_opt("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".local/share")))
        .map(|d| d.join(DESKTOP_APP_ID))
}

#[derive(Deserialize, Default)]
struct DesktopFile {
    #[serde(default)]
    palette: Option<String>,
    #[serde(default)]
    mode: Option<String>,
}

/// The desktop app's saved palette and mode, if its file is there and valid.
/// Unknown values are dropped field by field.
pub fn read_desktop(dir: &Path) -> (Option<String>, Option<String>) {
    let Ok(bytes) = std::fs::read(dir.join(FILE_NAME)) else { return (None, None) };
    // The file is small; refuse anything that isn't (it only themes the phone).
    if bytes.len() > 4096 {
        return (None, None);
    }
    let f: DesktopFile = serde_json::from_slice(&bytes).unwrap_or_default();
    (f.palette.as_deref().and_then(normalize_palette), f.mode.as_deref().and_then(normalize_mode))
}

/// The `/v1/appearance` body: `{palette, mode, source}`. `palette` and `mode`
/// are `null` when neither source has them, and `source` says where the
/// palette came from (`config`, `desktop` or `none`).
pub fn resolve(cfg: &AppearanceConfig) -> Value {
    let (desk_palette, desk_mode) = cfg.desktop_dir.as_deref().map(read_desktop).unwrap_or((None, None));
    let (palette, source) = match (&cfg.palette, desk_palette) {
        (Some(p), _) => (Some(p.clone()), "config"),
        (None, Some(p)) => (Some(p), "desktop"),
        (None, None) => (None, "none"),
    };
    let mode = cfg.mode.clone().or(desk_mode);
    json!({ "palette": palette, "mode": mode, "source": source })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(FILE_NAME), body).unwrap();
    }

    #[test]
    fn names_are_validated_and_migrated() {
        assert_eq!(normalize_palette(" Pollen "), Some("pollen".into()));
        assert_eq!(normalize_palette("midnight"), Some("slate".into()));
        assert_eq!(normalize_palette("neon"), None);
        assert_eq!(normalize_mode("system"), Some("auto".into()));
        assert_eq!(normalize_mode("DARK"), Some("dark".into()));
        assert_eq!(normalize_mode("dim"), None);
    }

    #[test]
    fn nothing_configured_is_all_null() {
        let tmp = tempfile::tempdir().unwrap();
        let v = resolve(&AppearanceConfig { desktop_dir: Some(tmp.path().join("missing")), ..Default::default() });
        assert_eq!(v, json!({"palette": null, "mode": null, "source": "none"}));
        assert_eq!(resolve(&AppearanceConfig::default()), json!({"palette": null, "mode": null, "source": "none"}));
    }

    #[test]
    fn desktop_file_is_read_and_config_wins_field_by_field() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("com.hive.desktop");
        write(&dir, r#"{"palette":"harbor","mode":"dark"}"#);
        let desk = AppearanceConfig { desktop_dir: Some(dir.clone()), ..Default::default() };
        assert_eq!(resolve(&desk), json!({"palette": "harbor", "mode": "dark", "source": "desktop"}));

        let pinned = AppearanceConfig { palette: Some("meadow".into()), ..desk.clone() };
        assert_eq!(resolve(&pinned), json!({"palette": "meadow", "mode": "dark", "source": "config"}));

        let mode_only = AppearanceConfig { mode: Some("light".into()), ..desk };
        assert_eq!(resolve(&mode_only), json!({"palette": "harbor", "mode": "light", "source": "desktop"}));
    }

    #[test]
    fn bad_or_partial_desktop_files_degrade_per_field() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let cfg = AppearanceConfig { desktop_dir: Some(dir.clone()), ..Default::default() };

        write(&dir, r#"{"palette":"neon","mode":"light"}"#);
        assert_eq!(resolve(&cfg), json!({"palette": null, "mode": "light", "source": "none"}));

        write(&dir, r#"{"palette":"midnight"}"#);
        assert_eq!(resolve(&cfg), json!({"palette": "slate", "mode": null, "source": "desktop"}));

        write(&dir, "not json");
        assert_eq!(resolve(&cfg), json!({"palette": null, "mode": null, "source": "none"}));

        write(&dir, &format!(r#"{{"palette":"pollen","pad":"{}"}}"#, "x".repeat(5000)));
        assert_eq!(resolve(&cfg)["palette"], Value::Null, "oversized files are ignored");
    }
}
