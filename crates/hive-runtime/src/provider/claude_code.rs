//! Claude Code CLI bridge — the bring-your-own-subscription path (no API key).
//! Ported from `ClaudeCodeBridge.swift`. Runs the official `claude` CLI in
//! non-interactive **stream-json** mode and surfaces token-level deltas, so a
//! Claude subscription streams just like the Anthropic API backend.
//!
//! Invocation:
//! `claude -p --output-format stream-json --verbose --include-partial-messages
//!  [--add-dir <workspace>] <extra args>` with the rendered prompt on stdin.
//!
//! The CLI emits newline-delimited JSON. With `--include-partial-messages` it
//! produces `stream_event` lines wrapping the Anthropic streaming events
//! (`content_block_delta` / `text_delta`) — token streaming. A final `result`
//! line carries the complete text (used as a fallback if no deltas arrived,
//! e.g. an older CLI without partial messages).

use std::process::Stdio;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use super::anthropic::{ChatTurn, ProviderError};

/// Render the conversation for the CLI prompt (stdin). Unlike the generic
/// bridge there's no `assistant:` trailer — `claude -p` treats the whole text
/// as the user prompt.
pub fn render_prompt(system: Option<&str>, turns: &[ChatTurn]) -> String {
    let mut out = String::new();
    if let Some(s) = system {
        out.push_str(s);
        out.push_str("\n\n");
    }
    for t in turns {
        out.push_str(&format!("{}: {}\n", t.role, t.content));
    }
    out
}

/// Extract an incremental text delta from one stream-json line, if it carries a
/// partial-message `content_block_delta` / `text_delta`.
pub fn extract_text_delta(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type")?.as_str()? != "stream_event" {
        return None;
    }
    let event = v.get("event")?;
    if event.get("type")?.as_str()? != "content_block_delta" {
        return None;
    }
    let delta = event.get("delta")?;
    if delta.get("type")?.as_str()? != "text_delta" {
        return None;
    }
    Some(delta.get("text")?.as_str()?.to_string())
}

/// Extract the final assembled text from a `result` line (fallback path).
pub fn extract_result(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type")?.as_str()? != "result" {
        return None;
    }
    v.get("result")?.as_str().map(str::to_owned)
}

/// Stream a reply from the Claude Code CLI. `program` is the binary (default
/// `claude`); `extra_args` are appended; `working_dir` is added via `--add-dir`
/// and used as the process cwd.
pub async fn stream_reply(
    program: &str,
    extra_args: &[String],
    working_dir: Option<&str>,
    extra_env: &[(String, String)],
    system: Option<&str>,
    turns: &[ChatTurn],
    mut on_delta: impl FnMut(String),
) -> Result<String, ProviderError> {
    let program = if program.is_empty() { "claude" } else { program };

    let mut args: Vec<String> = vec![
        "-p".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
    ];
    if let Some(dir) = working_dir {
        args.push("--add-dir".into());
        args.push(dir.to_string());
    }
    args.extend(extra_args.iter().cloned());

    let mut cmd = Command::new(program);
    cmd.args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }
    // e.g. GIT_AUTHOR_* so commits the agent makes are credited to the requester.
    for (k, v) in extra_env {
        cmd.env(k, v);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| ProviderError::Subprocess(format!("spawn {program}: {e}")))?;

    if let Some(mut stdin) = child.stdin.take() {
        let prompt = render_prompt(system, turns);
        stdin
            .write_all(prompt.as_bytes())
            .await
            .map_err(|e| ProviderError::Subprocess(format!("write stdin: {e}")))?;
        // drop closes stdin → EOF
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProviderError::Subprocess("no stdout".into()))?;
    let mut reader = BufReader::new(stdout).lines();

    let mut assembled = String::new();
    let mut result_fallback: Option<String> = None;
    while let Some(line) = reader
        .next_line()
        .await
        .map_err(|e| ProviderError::Subprocess(format!("read stdout: {e}")))?
    {
        if let Some(text) = extract_text_delta(&line) {
            assembled.push_str(&text);
            on_delta(text);
        } else if let Some(result) = extract_result(&line) {
            result_fallback = Some(result);
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| ProviderError::Subprocess(format!("wait: {e}")))?;
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut stderr).await;
        }
        return Err(ProviderError::Subprocess(format!(
            "{program} exited with {status}: {}",
            stderr.trim()
        )));
    }

    // Prefer streamed text; fall back to the result line (older CLI / no
    // partial messages), emitting it once so the UI shows something.
    if assembled.is_empty() {
        if let Some(result) = result_fallback {
            if !result.is_empty() {
                on_delta(result.clone());
                return Ok(result);
            }
        }
    }
    Ok(assembled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partial_message_text_delta() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}},"session_id":"x"}"#;
        assert_eq!(extract_text_delta(line).as_deref(), Some("Hel"));
        assert!(extract_result(line).is_none());
    }

    #[test]
    fn ignores_non_delta_events() {
        assert!(extract_text_delta(r#"{"type":"system","subtype":"init"}"#).is_none());
        assert!(extract_text_delta(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#
        )
        .is_none());
        assert!(extract_text_delta("not json").is_none());
    }

    #[test]
    fn parses_result_fallback() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"Hello there","session_id":"x"}"#;
        assert_eq!(extract_result(line).as_deref(), Some("Hello there"));
        assert!(extract_text_delta(line).is_none());
    }

    #[test]
    fn assembles_deltas_in_order() {
        let lines = [
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"lo"}}}"#,
            r#"{"type":"result","result":"Hello"}"#,
        ];
        let s: String = lines.iter().filter_map(|l| extract_text_delta(l)).collect();
        assert_eq!(s, "Hello");
    }
}
