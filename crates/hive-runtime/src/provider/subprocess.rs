//! Subprocess agent bridge — ported from `SubprocessAgentBridge.swift` /
//! `ClaudeCodeBridge.swift`. Runs an external CLI agent (aider / pi /
//! claude-code), feeds it the rendered prompt on stdin, and streams stdout
//! lines back through the delta callback.
//!
//! This is the transport mechanism; per-CLI specifics (flags, JSON streaming
//! protocols, working-dir handling) are layered on when targeting each tool.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use super::anthropic::{ChatTurn, ProviderError};

/// Default wall-clock budget for a single subprocess agent turn. Overridable
/// via `HIVE_AGENT_TIMEOUT_SECS` (a slow local model may want more; a flaky
/// backend may want less).
const DEFAULT_AGENT_TIMEOUT_SECS: u64 = 300;

fn agent_timeout() -> Duration {
    let secs = std::env::var("HIVE_AGENT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_AGENT_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Render a system prompt + turns into a single prompt string for a CLI that
/// takes plain text on stdin.
pub fn render_prompt(system: Option<&str>, turns: &[ChatTurn]) -> String {
    let mut out = String::new();
    if let Some(s) = system {
        out.push_str(s);
        out.push_str("\n\n");
    }
    for t in turns {
        out.push_str(&format!("{}: {}\n", t.role, t.content));
    }
    out.push_str("assistant: ");
    out
}

/// How the prompt reaches the child process.
pub enum PromptInput<'a> {
    /// Write the prompt to the child's stdin, then close it (EOF).
    Stdin(&'a str),
    /// The prompt is already baked into `args` (e.g. a positional argument);
    /// stdin is closed immediately.
    InArgs,
}

/// Spawn `program` with `args` in `working_dir`, write `input` to its stdin,
/// and stream stdout lines via `on_delta`. Returns the full stdout.
pub async fn run(
    program: &str,
    args: &[String],
    working_dir: Option<&str>,
    input: &str,
    on_delta: impl FnMut(String),
) -> Result<String, ProviderError> {
    run_with(program, args, working_dir, &[], PromptInput::Stdin(input), on_delta).await
}

/// Like [`run`], but the caller chooses whether the prompt is delivered on
/// stdin or already present in `args`, and can inject extra environment
/// variables. Captures stderr so a nonzero exit surfaces the tool's actual
/// error message instead of a bare exit code.
pub async fn run_with(
    program: &str,
    args: &[String],
    working_dir: Option<&str>,
    envs: &[(String, String)],
    prompt: PromptInput<'_>,
    mut on_delta: impl FnMut(String),
) -> Result<String, ProviderError> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| ProviderError::Subprocess(format!("spawn {program}: {e}")))?;

    // Always close stdin so a tool that waits on EOF can proceed. When the
    // prompt is a positional arg we just drop the handle.
    if let Some(mut stdin) = child.stdin.take() {
        if let PromptInput::Stdin(input) = prompt {
            stdin
                .write_all(input.as_bytes())
                .await
                .map_err(|e| ProviderError::Subprocess(format!("write stdin: {e}")))?;
        }
        // dropping stdin closes it, signaling EOF to the child
    }

    // Drain stderr concurrently so a chatty tool can't deadlock by filling its
    // stderr pipe while we're blocked reading stdout.
    let stderr_handle = child.stderr.take().map(|mut err| {
        tokio::spawn(async move {
            let mut buf = String::new();
            let _ = err.read_to_string(&mut buf).await;
            buf
        })
    });

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProviderError::Subprocess("no stdout".into()))?;
    let mut reader = BufReader::new(stdout).lines();

    // Bound the whole run so a tool that hangs (e.g. `pi` waiting on a
    // down Ollama) can't block the turn forever. Kill the child on timeout.
    let mut assembled = String::new();
    let timeout = agent_timeout();
    let driven = tokio::time::timeout(timeout, async {
        while let Some(line) = reader
            .next_line()
            .await
            .map_err(|e| ProviderError::Subprocess(format!("read stdout: {e}")))?
        {
            let chunk = format!("{line}\n");
            assembled.push_str(&chunk);
            on_delta(chunk);
        }
        child
            .wait()
            .await
            .map_err(|e| ProviderError::Subprocess(format!("wait: {e}")))
    })
    .await;

    let status = match driven {
        Ok(result) => result?,
        Err(_elapsed) => {
            // The inner future (and its borrow of `child`) is dropped here, so
            // we can reclaim the handle to terminate the process.
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(ProviderError::Subprocess(format!(
                "{program} timed out after {}s (set HIVE_AGENT_TIMEOUT_SECS to adjust)",
                timeout.as_secs()
            )));
        }
    };

    let stderr = match stderr_handle {
        Some(h) => h.await.unwrap_or_default(),
        None => String::new(),
    };

    if !status.success() {
        // Prefer the tool's own message (stderr, falling back to stdout) so the
        // user sees *why* it failed, not just the exit code.
        let detail = {
            let s = stderr.trim();
            let s = if s.is_empty() { assembled.trim() } else { s };
            if s.is_empty() {
                String::new()
            } else {
                format!(": {s}")
            }
        };
        return Err(ProviderError::Subprocess(format!(
            "{program} exited with {status}{detail}"
        )));
    }
    Ok(assembled)
}

/// Normalize a base URL to the OpenAI-compatible `/v1` form `pi` expects
/// (ported from `SubprocessAgentBridge.normalizeOpenAICompatibleBaseURL`).
pub fn normalize_openai_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.ends_with("/v1") || trimmed.contains("/v1?") {
        return trimmed.to_string();
    }
    if trimmed.ends_with('/') {
        format!("{trimmed}v1")
    } else {
        format!("{trimmed}/v1")
    }
}

/// Write a temporary `models.json` describing one OpenAI-compatible provider
/// (e.g. a local Ollama) and return the directory to point `PI_CODING_AGENT_DIR`
/// at. Mirrors the Swift bridge's `bootstrapModelProviderConfig`. The caller
/// owns cleanup of the returned directory.
pub fn bootstrap_pi_provider(
    provider_id: &str,
    base_url: &str,
    models: &[String],
) -> std::io::Result<std::path::PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let root = std::env::temp_dir().join(format!("hive-pi-{}-{}", std::process::id(), nanos));
    std::fs::create_dir_all(&root)?;

    let prefix = format!("{provider_id}/");
    let mut seen = std::collections::HashSet::new();
    let model_objs: Vec<serde_json::Value> = models
        .iter()
        .map(|m| m.trim())
        .filter(|m| !m.is_empty())
        .map(|m| m.strip_prefix(&prefix).unwrap_or(m).to_string())
        .filter(|m| seen.insert(m.clone()))
        .map(|id| serde_json::json!({ "id": id }))
        .collect();

    let config = serde_json::json!({
        "providers": {
            provider_id: {
                "baseUrl": normalize_openai_base_url(base_url),
                "api": "openai-completions",
                "apiKey": "ollama",
                "compat": {
                    "supportsDeveloperRole": false,
                    "supportsReasoningEffort": false,
                },
                "models": model_objs,
            }
        }
    });
    std::fs::write(
        root.join("models.json"),
        serde_json::to_vec_pretty(&config).unwrap_or_default(),
    )?;
    Ok(root)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn echoes_stdin_through_cat() {
        // `cat` echoes stdin to stdout — a portable stand-in for a CLI agent.
        let mut deltas = Vec::new();
        let out = run("cat", &[], None, "hello\nworld", |c| deltas.push(c))
            .await
            .unwrap();
        assert!(out.contains("hello"));
        assert!(out.contains("world"));
        assert!(!deltas.is_empty());
    }

    #[tokio::test]
    async fn nonzero_exit_is_an_error() {
        let err = run("sh", &["-c".into(), "exit 3".into()], None, "", |_| {}).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn slow_command_times_out_and_is_killed() {
        std::env::set_var("HIVE_AGENT_TIMEOUT_SECS", "1");
        let err = run("sh", &["-c".into(), "sleep 30".into()], None, "", |_| {})
            .await
            .unwrap_err();
        std::env::remove_var("HIVE_AGENT_TIMEOUT_SECS");
        assert!(format!("{err}").contains("timed out"), "{err}");
    }

    #[tokio::test]
    async fn nonzero_exit_surfaces_stderr() {
        let err = run(
            "sh",
            &["-c".into(), "echo 'boom: no api key' >&2; exit 1".into()],
            None,
            "",
            |_| {},
        )
        .await
        .unwrap_err();
        // The tool's own message must reach the user, not just the exit code.
        assert!(format!("{err}").contains("boom: no api key"), "{err}");
    }

    #[tokio::test]
    async fn prompt_in_args_path_streams_stdout() {
        // `echo <prompt>` stands in for a CLI that takes the prompt as an arg.
        let mut deltas = Vec::new();
        let out = run_with(
            "echo",
            &["hello from args".into()],
            None,
            &[],
            PromptInput::InArgs,
            |c| deltas.push(c),
        )
        .await
        .unwrap();
        assert!(out.contains("hello from args"));
        assert!(!deltas.is_empty());
    }

    #[test]
    fn normalizes_base_url_to_v1() {
        assert_eq!(normalize_openai_base_url("http://localhost:11434"), "http://localhost:11434/v1");
        assert_eq!(normalize_openai_base_url("http://localhost:11434/"), "http://localhost:11434/v1");
        assert_eq!(normalize_openai_base_url("http://localhost:11434/v1"), "http://localhost:11434/v1");
    }

    #[test]
    fn pi_bootstrap_writes_provider_config() {
        let dir = bootstrap_pi_provider(
            "ollama",
            "http://localhost:11434",
            &["ollama/qwen2.5".into(), "llama3".into()],
        )
        .unwrap();
        let text = std::fs::read_to_string(dir.join("models.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let provider = &json["providers"]["ollama"];
        assert_eq!(provider["baseUrl"], "http://localhost:11434/v1");
        // The "ollama/" prefix is stripped from model ids.
        let models = provider["models"].as_array().unwrap();
        assert_eq!(models[0]["id"], "qwen2.5");
        assert_eq!(models[1]["id"], "llama3");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn render_prompt_includes_system_and_turns() {
        let p = render_prompt(Some("You are Hive."), &[ChatTurn::user("hi")]);
        assert!(p.contains("You are Hive."));
        assert!(p.contains("user: hi"));
        assert!(p.trim_end().ends_with("assistant:"));
    }
}
