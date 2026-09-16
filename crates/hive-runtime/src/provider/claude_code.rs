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
use super::dispatch::StreamActivity;

/// Cap on a tool result's text carried in a live activity event — the raw output
/// (a full file read, a long build log) can be huge, and it's only a preview.
const RESULT_PREVIEW_CAP: usize = 4000;

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

/// Background-activity signals from one non-text stream-json line: `tool_use`
/// blocks on an `assistant` line, `tool_result` blocks on a `user` line, and a
/// `thinking` block (either). Returns empty for text/delta/result/system lines.
/// The CLI re-emits an assistant snapshot as blocks fill in, so the same
/// `tool_use` id can appear more than once — the UI dedups by id.
pub fn extract_activity(line: &str) -> Vec<StreamActivity> {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
    let content = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array);
    let Some(blocks) = content else {
        return out;
    };
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("tool_use") if ty == "assistant" => out.push(StreamActivity::Tool {
                id: b.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                name: b.get("name").and_then(Value::as_str).unwrap_or("tool").to_string(),
                input_json: b.get("input").map(Value::to_string).unwrap_or_default(),
            }),
            Some("tool_result") if ty == "user" => {
                let mut content = tool_result_text(b.get("content"));
                if content.len() > RESULT_PREVIEW_CAP {
                    content.truncate(RESULT_PREVIEW_CAP);
                    content.push('…');
                }
                out.push(StreamActivity::ToolResult {
                    call_id: b
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                    content,
                });
            }
            Some("thinking") => out.push(StreamActivity::Thinking),
            _ => {}
        }
    }
    out
}

/// A `tool_result` `content` is either a plain string or an array of blocks
/// (`{type:"text", text:…}`); flatten either to text.
fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|x| x.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
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
    // Live tool/thinking activity parsed from the same stream (Read/Bash/Edit,
    // their results, thinking). Ephemeral — surfaced under the generating bubble.
    mut on_activity: impl FnMut(StreamActivity),
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
        .stderr(Stdio::piped())
        // Kill the CLI if this future is dropped (e.g. the user hits Stop and the
        // caller's `select!` aborts the turn) — otherwise the subprocess would
        // keep generating, orphaned, after we stopped reading it.
        .kill_on_drop(true);
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }
    // e.g. GIT_AUTHOR_* so commits the agent makes are credited to the requester.
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    super::subprocess::suppress_console_window(&mut cmd);

    tracing::debug!(target: "claude", %program, ?args, ?working_dir, "spawning claude");
    let mut child = cmd
        .spawn()
        .map_err(|e| ProviderError::Subprocess(format!("spawn {program}: {e}")))?;
    tracing::debug!(target: "claude", "claude spawned; streaming");

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
    // Drain stderr concurrently. Otherwise a chatty CLI — `--verbose` can emit a
    // lot — fills the OS stderr pipe buffer (~64KB), which blocks the child's
    // next stderr write, which stops it writing stdout, which hangs our read
    // loop forever: the UI sits on "thinking" with no error and no output. We
    // read it in parallel and use the captured text for the failure path below.
    let stderr_task = child.stderr.take().map(|mut e| {
        tokio::spawn(async move {
            let mut buf = String::new();
            let _ = e.read_to_string(&mut buf).await;
            buf
        })
    });
    let mut reader = BufReader::new(stdout).lines();

    // Idle timeout, not wall-clock. The cap exists to catch a runtime that streams
    // *nothing* (wedged on auth/input/a blocked tool) — not to bound how long real
    // work takes. So we time out only when NO line arrives for the idle window;
    // ANY stream-json line (a text delta OR a tool_use/tool_result/system event)
    // resets it, so a long-but-active turn — e.g. a multi-minute `cargo test` tool
    // run that emits no text — won't trip it, while a truly silent CLI still does.
    // Override with HIVE_TURN_IDLE_TIMEOUT_SECS. (#100)
    let idle = std::time::Duration::from_secs(
        std::env::var("HIVE_TURN_IDLE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&s| s > 0)
            .unwrap_or(300),
    );
    let mut assembled = String::new();
    let mut result_fallback: Option<String> = None;
    loop {
        let line = match tokio::time::timeout(idle, reader.next_line()).await {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => break, // clean EOF
            Ok(Err(e)) => return Err(ProviderError::Subprocess(format!("read stdout: {e}"))),
            Err(_) => {
                // No output at all for the idle window — dropping the future kills
                // the CLI (kill_on_drop) once we return.
                return Err(ProviderError::Subprocess(format!(
                    "claude produced no output for {}s — it may be waiting on login/auth, input, or a blocked tool. Any activity (text or a tool call) resets this; a long but active build won't trip it. Verify with Settings \u{2192} Models \u{2192} Test, or raise HIVE_TURN_IDLE_TIMEOUT_SECS.",
                    idle.as_secs()
                )));
            }
        };
        if let Some(text) = extract_text_delta(&line) {
            assembled.push_str(&text);
            on_delta(text);
        } else if let Some(result) = extract_result(&line) {
            result_fallback = Some(result);
        } else {
            // Non-text line: surface any tool calls / results / thinking so the UI
            // can show what the agent is doing while it works.
            for act in extract_activity(&line) {
                on_activity(act);
            }
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| ProviderError::Subprocess(format!("wait: {e}")))?;
    let stderr = match stderr_task {
        Some(task) => task.await.unwrap_or_default(),
        None => String::new(),
    };
    if !status.success() {
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
    fn extracts_tool_use_from_assistant_line() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Let me look"},{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"src/lib.rs"}}]}}"#;
        let acts = extract_activity(line);
        assert_eq!(acts.len(), 1, "text block ignored, tool_use captured");
        match &acts[0] {
            StreamActivity::Tool { id, name, input_json } => {
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "Read");
                assert!(input_json.contains("src/lib.rs"));
            }
            other => panic!("expected Tool, got {other:?}"),
        }
    }

    #[test]
    fn extracts_tool_result_from_user_line_string_and_array() {
        let s = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":false,"content":"file contents"}]}}"#;
        match &extract_activity(s)[0] {
            StreamActivity::ToolResult { call_id, is_error, content } => {
                assert_eq!(call_id, "toolu_1");
                assert!(!is_error);
                assert_eq!(content, "file contents");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
        let arr = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","is_error":true,"content":[{"type":"text","text":"boom"}]}]}}"#;
        match &extract_activity(arr)[0] {
            StreamActivity::ToolResult { is_error, content, .. } => {
                assert!(is_error);
                assert_eq!(content, "boom");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn thinking_and_non_activity_lines() {
        let think = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}"#;
        assert!(matches!(extract_activity(think).as_slice(), [StreamActivity::Thinking]));
        // Text deltas, result lines, and junk carry no activity.
        assert!(extract_activity(r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hi"}}}"#).is_empty());
        assert!(extract_activity(r#"{"type":"result","result":"done"}"#).is_empty());
        assert!(extract_activity("not json").is_empty());
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
