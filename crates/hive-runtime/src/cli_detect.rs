//! Detect the subscription CLI agents on this host — installed? which version?
//! signed in? — without changing any auth state. Shared by the desktop's
//! Settings → Models panel and `hive setup` (serve / worker hosts).
//!
//! Sign-in checks never read a token's value:
//! - Copilot: a token in `COPILOT_GITHUB_TOKEN` / `GH_TOKEN` / `GITHUB_TOKEN`
//!   (presence only), else a non-empty `loggedInUsers` in the CLI's own
//!   `config.json` (`$COPILOT_HOME`, default `~/.copilot`). The token itself
//!   lives in the OS credential store and is never touched.
//! - Antigravity: the Google sign-in lives in the OS keychain and `agy` has no
//!   status command, so the passive scan reports "unknown". An explicit probe
//!   runs `agy models` (read-only; it needs a valid sign-in and uses no
//!   quota) and reports signed-in plus the model list.
//! - Gemini CLI: detected only to explain that it no longer serves Google AI
//!   Pro / Ultra sign-ins (since 2026-06-18) and to point at `agy`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use hive_core::ModelProviderKind;
use serde::Serialize;

use crate::provider::cli_agent::{classify_failure, CliFailure, CliKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LoginState {
    LoggedIn,
    NotLoggedIn,
    Unknown,
}

/// One CLI's detection result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedCli {
    /// "antigravity" | "copilot" | "gemini".
    pub id: String,
    /// The runtime provider string `add_runtime` expects; `None` when Hive
    /// can't drive this CLI (Gemini CLI for subscription logins).
    pub provider: Option<String>,
    pub label: String,
    /// The binary name looked up on PATH.
    pub binary: String,
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub login: LoginState,
    /// How the sign-in state was determined (never a secret).
    pub login_detail: String,
    /// What to run in a terminal on this host to sign in.
    pub login_command: String,
    pub install_command: String,
    /// Hive can run it headless with streaming.
    pub supported: bool,
    pub note: String,
    /// Model ids the CLI reported (Antigravity probe only).
    pub models: Vec<String>,
}

/// The process environment detection reads (injectable for tests).
#[derive(Debug, Clone)]
pub struct DetectEnv {
    pub path: OsString,
    pub home: PathBuf,
    pub vars: HashMap<String, String>,
}

impl DetectEnv {
    pub fn from_process() -> Self {
        Self {
            path: std::env::var_os("PATH").unwrap_or_default(),
            home: home_dir(),
            vars: std::env::vars().collect(),
        }
    }

    fn var(&self, k: &str) -> Option<&str> {
        self.vars.get(k).map(String::as_str).filter(|v| !v.trim().is_empty())
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Locate `bin` on `path` (Windows extensions included).
pub fn find_on_path(bin: &str, path: &OsString) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    std::env::split_paths(path).find_map(|dir| {
        exts.iter()
            .map(|ext| dir.join(format!("{bin}{ext}")))
            .find(|p| p.is_file())
    })
}

/// Run `program args…` with stdin closed, killing it after `timeout`.
/// Returns (exited successfully, stdout + stderr) or `None` if it couldn't
/// start or timed out.
fn run_bounded(program: &Path, args: &[&str], env: &DetectEnv, timeout: Duration) -> Option<(bool, String)> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PATH", &env.path)
        .env("NO_COLOR", "1");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().ok()?;
    let mut out = child.stdout.take()?;
    let mut err = child.stderr.take()?;
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut text = t_out.join().unwrap_or_default();
    text.push_str(&t_err.join().unwrap_or_default());
    Some((status.success(), text))
}

/// The first version-looking token (`1.5.1`, `1.0.106-6`) in `--version`
/// output, trailing punctuation trimmed.
pub fn parse_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .map(|w| w.trim_start_matches('v').trim_end_matches(['.', ',', ')']))
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()) && w.contains('.'))
        .map(str::to_owned)
}

fn version_of(path: &Path, env: &DetectEnv) -> Option<String> {
    run_bounded(path, &["--version"], env, Duration::from_secs(5))
        .filter(|(ok, _)| *ok)
        .and_then(|(_, out)| parse_version(&out))
}

/// Strip `//` comment lines (Copilot's config.json is JSON with a comment
/// header).
fn strip_line_comments(s: &str) -> String {
    s.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Copilot sign-in, from env-token presence or the CLI's config file.
pub fn copilot_login(env: &DetectEnv) -> (LoginState, String) {
    for var in ["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"] {
        if let Some(v) = env.var(var) {
            // Copilot rejects classic PATs; say so instead of "signed in".
            if v.trim().starts_with("ghp_") {
                return (
                    LoginState::NotLoggedIn,
                    format!("{var} holds a classic personal access token, which Copilot CLI does not accept"),
                );
            }
            return (LoginState::LoggedIn, format!("token in {var}"));
        }
    }
    let dir = env
        .var("COPILOT_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| env.home.join(".copilot"));
    let cfg = dir.join("config.json");
    let Ok(raw) = std::fs::read_to_string(&cfg) else {
        return (LoginState::NotLoggedIn, "no Copilot CLI login found".into());
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&strip_line_comments(&raw)) else {
        return (LoginState::Unknown, "couldn't read the Copilot CLI config".into());
    };
    let users = v.get("loggedInUsers").and_then(|u| u.as_array()).cloned().unwrap_or_default();
    if users.is_empty() {
        return (LoginState::NotLoggedIn, "no Copilot CLI login found".into());
    }
    let who = v
        .get("lastLoggedInUser")
        .and_then(|u| u.get("login"))
        .or_else(|| users[0].get("login"))
        .and_then(|l| l.as_str())
        .unwrap_or("");
    let detail = if who.is_empty() {
        "signed in (copilot login)".to_string()
    } else {
        format!("signed in as {who} (copilot login)")
    };
    (LoginState::LoggedIn, detail)
}

/// Parse `agy models` output: `<id>\t<display name>` lines.
pub fn parse_agy_models(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.split_once('\t').map(|(id, _)| id.trim()))
        .filter(|id| !id.is_empty() && !id.contains(' '))
        .map(str::to_owned)
        .collect()
}

/// Antigravity sign-in via `agy models` (read-only, no quota).
fn agy_probe(path: &Path, env: &DetectEnv) -> (LoginState, String, Vec<String>) {
    match run_bounded(path, &["models"], env, Duration::from_secs(20)) {
        Some((true, out)) => {
            let models = parse_agy_models(&out);
            if models.is_empty() {
                (LoginState::Unknown, "`agy models` listed no models".into(), models)
            } else {
                (LoginState::LoggedIn, format!("signed in — {} models available", models.len()), models)
            }
        }
        Some((false, out)) => match classify_failure(&out) {
            CliFailure::NotLoggedIn => (LoginState::NotLoggedIn, "agy reports no Google sign-in".into(), Vec::new()),
            _ => (
                LoginState::Unknown,
                format!("`agy models` failed: {}", out.trim().chars().take(200).collect::<String>()),
                Vec::new(),
            ),
        },
        None => (LoginState::Unknown, "`agy models` didn't answer within 20s".into(), Vec::new()),
    }
}

/// Detect agy, copilot and (legacy) gemini. `probe_sign_in` additionally runs
/// `agy models` to confirm its Google sign-in.
pub fn detect_cli_agents(env: &DetectEnv, probe_sign_in: bool) -> Vec<DetectedCli> {
    let mut out = Vec::new();

    // Google Antigravity CLI.
    let kind = CliKind::Antigravity;
    let path = find_on_path("agy", &env.path);
    let (login, login_detail, models) = match &path {
        None => (LoginState::Unknown, String::new(), Vec::new()),
        Some(p) if probe_sign_in => agy_probe(p, env),
        Some(_) => (
            LoginState::Unknown,
            "the Google sign-in is kept in the OS keychain — check it to confirm".into(),
            Vec::new(),
        ),
    };
    out.push(DetectedCli {
        id: "antigravity".into(),
        provider: Some("antigravity".into()),
        label: "Google Antigravity CLI".into(),
        binary: "agy".into(),
        installed: path.is_some(),
        version: path.as_deref().and_then(|p| version_of(p, env)),
        path: path.map(|p| p.display().to_string()),
        login,
        login_detail,
        login_command: kind.login_command().into(),
        install_command: kind.install_command().into(),
        supported: true,
        note: "Google AI Pro / Ultra via Sign in with Google. Streams text and tool steps.".into(),
        models,
    });

    // GitHub Copilot CLI.
    let kind = CliKind::Copilot;
    let path = find_on_path("copilot", &env.path);
    let (login, login_detail) = if path.is_some() { copilot_login(env) } else { (LoginState::Unknown, String::new()) };
    out.push(DetectedCli {
        id: "copilot".into(),
        provider: Some("copilot".into()),
        label: "GitHub Copilot CLI".into(),
        binary: "copilot".into(),
        installed: path.is_some(),
        version: path.as_deref().and_then(|p| version_of(p, env)),
        path: path.map(|p| p.display().to_string()),
        login,
        login_detail,
        login_command: kind.login_command().into(),
        install_command: kind.install_command().into(),
        supported: true,
        note: "Copilot subscription. Each turn spends premium requests / AI credits; pick the model on the runtime.".into(),
        models: Vec::new(),
    });

    // Gemini CLI — only to explain the change, never added as a runtime.
    if let Some(p) = find_on_path("gemini", &env.path) {
        out.push(DetectedCli {
            id: "gemini".into(),
            provider: None,
            label: "Gemini CLI".into(),
            binary: "gemini".into(),
            installed: true,
            version: version_of(&p, env),
            path: Some(p.display().to_string()),
            login: LoginState::Unknown,
            login_detail: String::new(),
            login_command: CliKind::Antigravity.login_command().into(),
            install_command: CliKind::Antigravity.install_command().into(),
            supported: false,
            note: "Gemini CLI stopped serving Google AI Pro / Ultra and free personal sign-ins on 2026-06-18 (API-key and Code Assist Standard/Enterprise use is unaffected). For a Google AI subscription, install the Antigravity CLI (agy).".into(),
            models: Vec::new(),
        });
    }
    out
}

/// The provider kind for a detected CLI id.
pub fn provider_for(id: &str) -> Option<ModelProviderKind> {
    match id {
        "antigravity" => Some(ModelProviderKind::Antigravity),
        "copilot" => Some(ModelProviderKind::Copilot),
        _ => None,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_bin(dir: &Path, name: &str, script: &str) {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn env_for(bin: &Path, home: &Path) -> DetectEnv {
        let mut path = OsString::from(bin);
        path.push(":/usr/bin:/bin");
        DetectEnv { path, home: home.to_path_buf(), vars: HashMap::new() }
    }

    #[test]
    fn nothing_installed() {
        let bin = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let found = detect_cli_agents(&env_for(bin.path(), home.path()), true);
        assert_eq!(found.len(), 2, "gemini only listed when present");
        assert!(found.iter().all(|c| !c.installed && c.version.is_none()));
    }

    #[test]
    fn fake_agy_and_copilot_are_detected_with_versions_and_login() {
        let bin = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        fake_bin(
            bin.path(),
            "agy",
            r#"case "$1" in --version) echo 1.5.1;; models) printf 'Fetching available models...\ngemini-3.8-flash-low\tGemini 3.8 Flash (Low)\ngemini-4-argon-high\tGemini 4 Argon (High)\n';; *) exit 2;; esac"#,
        );
        fake_bin(bin.path(), "copilot", r#"[ "$1" = --version ] && printf "GitHub Copilot CLI 1.0.106-6.\nRun 'copilot update' to check for updates.\n""#);
        std::fs::create_dir_all(home.path().join(".copilot")).unwrap();
        std::fs::write(
            home.path().join(".copilot/config.json"),
            "// User settings belong in settings.json.\n// This file is managed automatically.\n{\n  \"loggedInUsers\": [{\"host\": \"https://github.com\", \"login\": \"octo\"}],\n  \"lastLoggedInUser\": {\"host\": \"https://github.com\", \"login\": \"octo\"}\n}\n",
        )
        .unwrap();
        let env = env_for(bin.path(), home.path());

        let passive = detect_cli_agents(&env, false);
        let agy = passive.iter().find(|c| c.id == "antigravity").unwrap();
        assert!(agy.installed && agy.supported);
        assert_eq!(agy.version.as_deref(), Some("1.5.1"));
        assert_eq!(agy.login, LoginState::Unknown, "passive scan never runs agy models");
        let cp = passive.iter().find(|c| c.id == "copilot").unwrap();
        assert_eq!(cp.version.as_deref(), Some("1.0.106-6"));
        assert_eq!(cp.login, LoginState::LoggedIn);
        assert!(cp.login_detail.contains("octo"));

        let probed = detect_cli_agents(&env, true);
        let agy = probed.iter().find(|c| c.id == "antigravity").unwrap();
        assert_eq!(agy.login, LoginState::LoggedIn);
        assert_eq!(agy.models, vec!["gemini-3.8-flash-low", "gemini-4-argon-high"]);
    }

    #[test]
    fn signed_out_states() {
        let bin = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        fake_bin(bin.path(), "agy", r#"[ "$1" = models ] && { echo "Error: not signed in" >&2; exit 1; }; echo 1.5.1"#);
        fake_bin(bin.path(), "copilot", "echo 1.0.0");
        let mut env = env_for(bin.path(), home.path());
        let found = detect_cli_agents(&env, true);
        assert_eq!(found[0].login, LoginState::NotLoggedIn);
        assert_eq!(found[0].login_command, "agy");
        assert_eq!(found[1].login, LoginState::NotLoggedIn);
        assert_eq!(found[1].login_command, "copilot login");

        // An env token counts (presence only); a classic PAT does not.
        env.vars.insert("GH_TOKEN".into(), "gho_example".into());
        assert_eq!(copilot_login(&env).0, LoginState::LoggedIn);
        env.vars.insert("COPILOT_GITHUB_TOKEN".into(), "ghp_example".into());
        let (state, detail) = copilot_login(&env);
        assert_eq!(state, LoginState::NotLoggedIn);
        assert!(!detail.contains("ghp_example"), "never echo a token");
    }

    #[test]
    fn hung_cli_is_killed_and_reported_unknown() {
        let bin = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        fake_bin(bin.path(), "copilot", "sleep 30");
        let env = env_for(bin.path(), home.path());
        let t = Instant::now();
        let found = detect_cli_agents(&env, false);
        assert!(t.elapsed() < Duration::from_secs(15));
        assert!(found[1].installed && found[1].version.is_none());
    }

    #[test]
    fn legacy_gemini_is_explained_not_offered() {
        let bin = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        fake_bin(bin.path(), "gemini", "echo 0.40.0");
        let found = detect_cli_agents(&env_for(bin.path(), home.path()), false);
        let g = found.iter().find(|c| c.id == "gemini").unwrap();
        assert!(!g.supported && g.provider.is_none());
        assert!(g.note.contains("2026-06-18") && g.note.contains("agy"));
    }

    #[test]
    fn parses_versions() {
        assert_eq!(parse_version("GitHub Copilot CLI 1.0.106-6.\nRun 'copilot update'").as_deref(), Some("1.0.106-6"));
        assert_eq!(parse_version("1.5.1\n").as_deref(), Some("1.5.1"));
        assert_eq!(parse_version("agy version v2.0.0").as_deref(), Some("2.0.0"));
        assert_eq!(parse_version("unknown flag: --version"), None);
    }
}
