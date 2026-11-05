//! Shared plumbing for the subscription CLI agents (Antigravity `agy`, GitHub
//! `copilot`): spawn the CLI headless, read its JSONL stream line by line with
//! an idle timeout, drain stderr, and turn its failures into messages that say
//! what to do (sign in, wait out a usage limit, install the CLI).
//!
//! The login always lives in the CLI's own config / OS keychain. Hive never
//! reads, copies or forwards it — it only runs the binary.

use std::process::{ExitStatus, Stdio};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

use super::anthropic::ProviderError;
use super::dispatch::StreamActivity;

/// Cap on a tool result's text carried in a live activity event.
pub(crate) const RESULT_PREVIEW_CAP: usize = 4000;

/// Which CLI a message is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliKind {
    Antigravity,
    Copilot,
}

impl CliKind {
    pub fn display(self) -> &'static str {
        match self {
            CliKind::Antigravity => "Antigravity CLI (agy)",
            CliKind::Copilot => "GitHub Copilot CLI",
        }
    }
    pub fn default_program(self) -> &'static str {
        match self {
            CliKind::Antigravity => "agy",
            CliKind::Copilot => "copilot",
        }
    }
    /// What the user runs, in a terminal on the host, to sign in.
    pub fn login_command(self) -> &'static str {
        match self {
            CliKind::Antigravity => "agy",
            CliKind::Copilot => "copilot login",
        }
    }
    pub fn login_hint(self) -> &'static str {
        match self {
            CliKind::Antigravity => {
                "Run `agy` once in a terminal on this host and sign in with the Google account that has Google AI Pro / Ultra, then retry."
            }
            CliKind::Copilot => {
                "Run `copilot login` in a terminal on this host (or set COPILOT_GITHUB_TOKEN to a fine-grained token with the Copilot Requests permission on a headless host), then retry."
            }
        }
    }
    pub fn install_command(self) -> &'static str {
        match self {
            CliKind::Antigravity => "curl -fsSL https://antigravity.google/cli/install.sh | bash",
            CliKind::Copilot => "brew install --cask copilot-cli   (or: npm install -g @github/copilot)",
        }
    }
    fn usage_hint(self) -> &'static str {
        match self {
            CliKind::Antigravity => {
                "Your Google AI plan's quota is used up for now — check it with `/usage` (and `/credits`) in an interactive `agy` session, wait for the reset, or pick a lighter model on this runtime."
            }
            CliKind::Copilot => {
                "Your Copilot premium-request / AI-credit budget or rate limit was hit — check `/usage` in an interactive `copilot` session, wait, or set a cheaper model on this runtime (premium models cost several requests per turn)."
            }
        }
    }
}

/// What kind of failure a CLI's error text describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliFailure {
    NotLoggedIn,
    UsageLimit,
    Other,
}

/// Classify a CLI error message (stderr / an error event). Order matters:
/// a 401 is auth, a 429 is a limit.
pub fn classify_failure(text: &str) -> CliFailure {
    let t = text.to_lowercase();
    const AUTH: &[&str] = &[
        "no authentication information found",
        "not logged in",
        "not signed in",
        "not authenticated",
        "unauthenticated",
        "authentication required",
        "authentication failed",
        "please log in",
        "please login",
        "please sign in",
        "login required",
        "sign-in required",
        "could not be validated",
        "invalid credentials",
        "no credentials",
        "401",
    ];
    const LIMIT: &[&str] = &[
        "rate limit",
        "rate-limit",
        "ratelimit",
        "rate_limited",
        "quota",
        "resource_exhausted",
        "resource exhausted",
        "429",
        "usage limit",
        "premium request",
        "out of credits",
        "insufficient credits",
        "ai credit",
        "too many requests",
    ];
    if AUTH.iter().any(|p| t.contains(p)) {
        CliFailure::NotLoggedIn
    } else if LIMIT.iter().any(|p| t.contains(p)) {
        CliFailure::UsageLimit
    } else {
        CliFailure::Other
    }
}

/// A user-facing error for a failed CLI turn: the exact fix for a missing
/// login or a usage limit, else the CLI's own message.
pub fn failure_error(kind: CliKind, detail: &str) -> ProviderError {
    let detail = detail.trim();
    let detail = if detail.len() > 1500 { &detail[..floor_char_boundary(detail, 1500)] } else { detail };
    let msg = match classify_failure(detail) {
        CliFailure::NotLoggedIn => format!("{} is not signed in on this host. {} ({detail})", kind.display(), kind.login_hint()),
        CliFailure::UsageLimit => format!("{} hit a usage limit. {} ({detail})", kind.display(), kind.usage_hint()),
        CliFailure::Other if detail.is_empty() => format!("{} failed without an error message.", kind.display()),
        CliFailure::Other => format!("{}: {detail}", kind.display()),
    };
    ProviderError::Subprocess(msg)
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Truncate a tool-result preview to [`RESULT_PREVIEW_CAP`].
pub(crate) fn cap_preview(mut s: String) -> String {
    if s.len() > RESULT_PREVIEW_CAP {
        s.truncate(floor_char_boundary(&s, RESULT_PREVIEW_CAP));
        s.push('…');
    }
    s
}

/// One parsed unit of a CLI's stream.
#[derive(Debug, Clone)]
pub enum CliOut {
    Delta(String),
    Activity(StreamActivity),
}

/// A per-CLI stream parser: fed one stdout line at a time, then asked for the
/// final result once the process has exited.
pub trait CliStreamParser {
    fn feed(&mut self, line: &str) -> Vec<CliOut>;
    /// The reply body, or the error to show. `exit_ok` and `stderr` come from
    /// the process. May return a last [`CliOut::Delta`] (a non-streamed reply).
    fn finish(&mut self, exit_ok: bool, stderr: &str) -> Result<(String, Option<String>), ProviderError>;
}

/// Spawn `program args…` (prompt already in `args`, stdin closed) in
/// `working_dir`, feed each stdout line to `parser`, and forward its output.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_cli<P: CliStreamParser>(
    kind: CliKind,
    program: &str,
    args: &[String],
    working_dir: Option<&str>,
    extra_env: &[(String, String)],
    parser: &mut P,
    mut on_delta: impl FnMut(String),
    mut on_activity: impl FnMut(StreamActivity),
) -> Result<String, ProviderError> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        // The prompt is an argument; a closed stdin means the CLI never waits
        // on the terminal.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Stop → the future is dropped → the CLI is killed, not orphaned.
        .kill_on_drop(true);
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    super::subprocess::suppress_console_window(&mut cmd);

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ProviderError::Subprocess(format!(
                "{} not found (`{program}` is not on PATH for this host). Install it: {}",
                kind.display(),
                kind.install_command()
            ))
        } else {
            ProviderError::Subprocess(format!("spawn {program}: {e}"))
        }
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProviderError::Subprocess("no stdout".into()))?;
    // Drain stderr concurrently so a chatty CLI can't block on a full pipe.
    let stderr_task = child.stderr.take().map(|mut e| {
        tokio::spawn(async move {
            let mut buf = String::new();
            let _ = e.read_to_string(&mut buf).await;
            buf
        })
    });
    let mut reader = BufReader::new(stdout).lines();
    let idle = super::turn_idle_timeout();
    loop {
        let line = match tokio::time::timeout(idle, reader.next_line()).await {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => break,
            Ok(Err(e)) => return Err(ProviderError::Subprocess(format!("read stdout: {e}"))),
            Err(_) => {
                return Err(ProviderError::Subprocess(format!(
                    "{} produced no output for {}s — it may be waiting on sign-in or a blocked tool. {} Raise HIVE_TURN_IDLE_TIMEOUT_SECS for long silent steps.",
                    kind.display(),
                    idle.as_secs(),
                    kind.login_hint()
                )));
            }
        };
        for out in parser.feed(&line) {
            match out {
                CliOut::Delta(t) => on_delta(t),
                CliOut::Activity(a) => on_activity(a),
            }
        }
    }
    let status: ExitStatus = child
        .wait()
        .await
        .map_err(|e| ProviderError::Subprocess(format!("wait: {e}")))?;
    let stderr = match stderr_task {
        Some(task) => task.await.unwrap_or_default(),
        None => String::new(),
    };
    if !stderr.trim().is_empty() {
        tracing::debug!(target: "cli-agent", cli = ?kind, stderr = %stderr.trim(), "cli stderr");
    }
    let (body, late) = parser.finish(status.success(), &stderr)?;
    if let Some(t) = late {
        on_delta(t);
    }
    Ok(body)
}

/// Appends `text` to `assembled`, starting a new line when a new message /
/// step begins so narration around tool calls doesn't run together.
pub(crate) fn push_segment(assembled: &mut String, new_segment: bool, text: &str, out: &mut Vec<CliOut>) {
    if text.is_empty() {
        return;
    }
    if new_segment && !assembled.is_empty() && !assembled.ends_with('\n') {
        assembled.push('\n');
        out.push(CliOut::Delta("\n".into()));
    }
    assembled.push_str(text);
    out.push(CliOut::Delta(text.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_auth_and_limits() {
        assert_eq!(classify_failure("Error: No authentication information found."), CliFailure::NotLoggedIn);
        assert_eq!(classify_failure("HTTP 401 Unauthorized"), CliFailure::NotLoggedIn);
        assert_eq!(classify_failure("RESOURCE_EXHAUSTED: quota exceeded for model"), CliFailure::UsageLimit);
        assert_eq!(classify_failure("429 Too Many Requests"), CliFailure::UsageLimit);
        assert_eq!(classify_failure("model 'x' not found"), CliFailure::Other);
    }

    #[test]
    fn failure_messages_name_the_fix() {
        let e = failure_error(CliKind::Copilot, "Error: No authentication information found.").to_string();
        assert!(e.contains("copilot login"), "{e}");
        let e = failure_error(CliKind::Antigravity, "not signed in").to_string();
        assert!(e.contains("Run `agy` once"), "{e}");
        let e = failure_error(CliKind::Antigravity, "quota exceeded").to_string();
        assert!(e.contains("usage limit") && e.contains("/usage"), "{e}");
        let e = failure_error(CliKind::Copilot, "").to_string();
        assert!(e.contains("without an error message"), "{e}");
    }

    #[test]
    fn caps_previews_on_char_boundaries() {
        let s = "é".repeat(RESULT_PREVIEW_CAP);
        let c = cap_preview(s);
        assert!(c.ends_with('…'));
        assert!(c.len() <= RESULT_PREVIEW_CAP + '…'.len_utf8());
    }
}
