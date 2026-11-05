//! GitHub Copilot CLI bridge (`copilot`) — the Copilot subscription path. No
//! API key: the login is `copilot login` (OS credential store) or a token in
//! the CLI's own env vars, all owned by the CLI.
//!
//! Invocation: `copilot --output-format json --stream on --no-ask-user
//!  [--model <id>] [<permission args>] [<extra args>] -p <prompt>`, run in the
//! turn's working dir (Copilot restricts file access to the cwd tree) with
//! stdin closed.
//!
//! `--output-format json` is JSONL. The event schema is not documented; the
//! shapes parsed here were recorded from Copilot CLI 1.0.x:
//! - `assistant.message_delta` `{data:{messageId, deltaContent}}` — text
//! - `assistant.message` `{data:{messageId, content, toolRequests}}` — the
//!   whole message (used when no deltas arrived for it, e.g. `--stream off`)
//! - `tool.execution_start` `{data:{toolCallId, toolName, arguments}}`
//! - `tool.execution_complete` `{data:{toolCallId, success, result:{content}}}`
//! - `session.error` `{data:{errorType, message}}`
//! - `result` `{exitCode, usage:{premiumRequests, …}}` — last line
//!
//! Unknown events are ignored, so new event types don't break a turn.
//!
//! Permissions mirror Hive's Claude Code tiers. Without an allow rule a
//! non-interactive run denies the request, so the default tier is read-only.

use std::collections::HashSet;

use serde_json::Value;

use super::anthropic::{ChatTurn, ProviderError};
use super::cli_agent::{self, cap_preview, push_segment, CliKind, CliOut, CliStreamParser};
use super::dispatch::StreamActivity;

/// `copilot` flags for a Hive permission tier (the Claude Code setting's
/// values). Deny rules beat allow rules in Copilot, so each tier denies what
/// it doesn't grant. URL access and paths outside the working dir stay gated
/// in every tier.
/// - `default`: read files; no writes, no shell.
/// - `acceptEdits`: read + write files; no shell.
/// - `runCommands`: read + write + shell.
/// - `bypassPermissions`: `--allow-all-tools` (paths and URLs still verified —
///   never `--allow-all` / `--yolo`).
pub fn permission_args(mode: &str) -> Vec<String> {
    let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match mode {
        "acceptEdits" => v(&["--allow-tool=read", "--allow-tool=write", "--deny-tool=shell"]),
        "runCommands" => v(&["--allow-tool=read", "--allow-tool=write", "--allow-tool=shell"]),
        "bypassPermissions" => v(&["--allow-all-tools"]),
        _ => v(&["--allow-tool=read", "--deny-tool=write", "--deny-tool=shell"]),
    }
}

/// The full argument list for one headless turn.
pub fn build_args(model: &str, extra_args: &[String], prompt: String) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--output-format".into(),
        "json".into(),
        "--stream".into(),
        "on".into(),
        // Never pause to ask the (absent) user a question.
        "--no-ask-user".into(),
        "--no-color".into(),
    ];
    if !model.trim().is_empty() {
        args.push("--model".into());
        args.push(model.trim().to_string());
    }
    args.extend(extra_args.iter().cloned());
    // Last, so a variadic `--allow-tool` can't swallow it.
    args.push("-p".into());
    args.push(prompt);
    args
}

/// Parser state for `copilot --output-format json`.
#[derive(Default)]
pub struct CopilotParser {
    assembled: String,
    last_message: Option<String>,
    /// Messages that streamed deltas (their final `assistant.message` is a repeat).
    streamed: HashSet<String>,
    thinking_marked: HashSet<String>,
    errors: Vec<String>,
    exit_code: Option<i64>,
    pub premium_requests: Option<f64>,
}

impl CopilotParser {
    fn text(&mut self, message_id: &str, text: &str, out: &mut Vec<CliOut>) {
        let new_segment = self.last_message.as_deref() != Some(message_id);
        self.last_message = Some(message_id.to_string());
        push_segment(&mut self.assembled, new_segment, text, out);
    }
}

fn str_at<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

impl CliStreamParser for CopilotParser {
    fn feed(&mut self, line: &str) -> Vec<CliOut> {
        let mut out = Vec::new();
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            return out;
        };
        let data = v.get("data").cloned().unwrap_or(Value::Null);
        match str_at(&v, "type") {
            "assistant.message_delta" => {
                let id = str_at(&data, "messageId").to_string();
                let delta = str_at(&data, "deltaContent").to_string();
                if !delta.is_empty() {
                    self.streamed.insert(id.clone());
                    self.text(&id, &delta, &mut out);
                }
            }
            "assistant.message" => {
                let id = str_at(&data, "messageId").to_string();
                let content = str_at(&data, "content").to_string();
                if !content.is_empty() && !self.streamed.contains(&id) {
                    self.streamed.insert(id.clone());
                    self.text(&id, &content, &mut out);
                }
            }
            t if t.starts_with("assistant.reasoning") => {
                let id = str_at(&data, "reasoningId").to_string();
                if self.thinking_marked.insert(id) {
                    out.push(CliOut::Activity(StreamActivity::Thinking { text: String::new() }));
                }
            }
            "tool.execution_start" => out.push(CliOut::Activity(StreamActivity::Tool {
                id: str_at(&data, "toolCallId").to_string(),
                name: data.get("toolName").and_then(Value::as_str).unwrap_or("tool").to_string(),
                input_json: data.get("arguments").map(Value::to_string).unwrap_or_default(),
            })),
            "tool.execution_complete" => {
                let ok = data.get("success").and_then(Value::as_bool).unwrap_or(true);
                let result = data.get("result").cloned().unwrap_or(Value::Null);
                let content = match &result {
                    Value::String(s) => s.clone(),
                    Value::Object(_) => result
                        .get("content")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| result.to_string()),
                    _ => data
                        .get("error")
                        .map(|e| e.get("message").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| e.to_string()))
                        .unwrap_or_default(),
                };
                out.push(CliOut::Activity(StreamActivity::ToolResult {
                    call_id: str_at(&data, "toolCallId").to_string(),
                    is_error: !ok,
                    content: cap_preview(content),
                }));
            }
            "session.error" => {
                let msg = str_at(&data, "message");
                if !msg.is_empty() {
                    self.errors.push(msg.to_string());
                }
            }
            "result" => {
                self.exit_code = v.get("exitCode").and_then(Value::as_i64);
                self.premium_requests = v
                    .get("usage")
                    .and_then(|u| u.get("premiumRequests"))
                    .and_then(Value::as_f64);
            }
            _ => {}
        }
        out
    }

    fn finish(&mut self, exit_ok: bool, stderr: &str) -> Result<(String, Option<String>), ProviderError> {
        let failed = !exit_ok || self.exit_code.is_some_and(|c| c != 0);
        // An error event with no reply at all is a failed turn even on exit 0.
        if failed || (self.assembled.is_empty() && !self.errors.is_empty()) {
            let mut detail = self.errors.join("; ");
            if !stderr.trim().is_empty() {
                if !detail.is_empty() {
                    detail.push_str("; ");
                }
                detail.push_str(stderr.trim());
            }
            if detail.is_empty() {
                detail = format!("exit code {}", self.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "non-zero".into()));
            }
            return Err(cli_agent::failure_error(CliKind::Copilot, &detail));
        }
        if !self.errors.is_empty() {
            tracing::warn!(target: "cli-agent", errors = ?self.errors, "copilot reported errors but replied");
        }
        if let Some(n) = self.premium_requests {
            tracing::info!(target: "cli-agent", premium_requests = n, "copilot turn usage");
        }
        Ok((self.assembled.clone(), None))
    }
}

/// Stream a reply from `copilot`. `program` is the binary (default
/// `copilot`); `model` maps to `--model` (empty = the CLI's configured
/// default); `extra_args` carry the permission tier and any runtime flags.
#[allow(clippy::too_many_arguments)]
pub async fn stream_reply(
    program: &str,
    model: &str,
    extra_args: &[String],
    working_dir: Option<&str>,
    extra_env: &[(String, String)],
    system: Option<&str>,
    turns: &[ChatTurn],
    on_delta: impl FnMut(String),
    on_activity: impl FnMut(StreamActivity),
) -> Result<String, ProviderError> {
    let program = if program.trim().is_empty() { CliKind::Copilot.default_program() } else { program };
    let prompt = super::claude_code::render_prompt(system, turns);
    let args = build_args(model, extra_args, prompt);
    let mut parser = CopilotParser::default();
    cli_agent::run_cli(CliKind::Copilot, program, &args, working_dir, extra_env, &mut parser, on_delta, on_activity)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorded from Copilot CLI 1.0.106 (`--output-format json --stream on`,
    /// a prompt that reads one file); ids, paths and the session-setup events
    /// trimmed or replaced.
    const FIXTURE_TOOL_TURN: &str = include_str!("fixtures/copilot_stream_tool.jsonl");

    fn run(fixture: &str) -> (Vec<CliOut>, CopilotParser) {
        let mut p = CopilotParser::default();
        let mut outs = Vec::new();
        for line in fixture.lines() {
            outs.extend(p.feed(line));
        }
        (outs, p)
    }

    fn text(outs: &[CliOut]) -> String {
        outs.iter()
            .filter_map(|o| match o {
                CliOut::Delta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn recorded_tool_turn_streams_text_and_tool_activity() {
        let (outs, mut p) = run(FIXTURE_TOOL_TURN);
        assert_eq!(text(&outs), "banana", "deltas once, final message not repeated");
        let acts: Vec<_> = outs
            .iter()
            .filter_map(|o| match o {
                CliOut::Activity(a) => Some(a.clone()),
                _ => None,
            })
            .collect();
        match acts.as_slice() {
            [StreamActivity::Tool { id, name, input_json }, StreamActivity::ToolResult { call_id, is_error, content }] => {
                assert_eq!(name, "view");
                assert!(input_json.contains("a.txt"));
                assert_eq!(id, call_id);
                assert!(!is_error);
                assert_eq!(content, "banana\n");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(p.finish(true, "").unwrap().0, "banana");
        assert_eq!(p.premium_requests, Some(15.0), "a premium model costs several requests");
    }

    #[test]
    fn whole_message_without_deltas_is_used() {
        let mut p = CopilotParser::default();
        let mut outs = p.feed(r#"{"type":"assistant.message","data":{"messageId":"m1","content":"first","toolRequests":[]}}"#);
        outs.extend(p.feed(r#"{"type":"assistant.message","data":{"messageId":"m2","content":"second","toolRequests":[]}}"#));
        assert_eq!(text(&outs), "first\nsecond");
    }

    #[test]
    fn not_logged_in_names_copilot_login() {
        // What 1.0.x writes to stderr (exit 1) with no stored login or token.
        let mut p = CopilotParser::default();
        let e = p
            .finish(false, "Error: No authentication information found.\n\nCopilot can be authenticated with GitHub using an OAuth Token or a Fine-Grained Personal Access Token.")
            .unwrap_err()
            .to_string();
        assert!(e.contains("not signed in") && e.contains("copilot login"), "{e}");
    }

    #[test]
    fn session_error_without_reply_fails_with_limit_hint() {
        let mut p = CopilotParser::default();
        p.feed(r#"{"type":"session.error","data":{"errorType":"session","message":"Turn error: 429 rate limit exceeded"}}"#);
        p.feed(r#"{"type":"result","sessionId":"s","exitCode":0,"usage":{"premiumRequests":0}}"#);
        let e = p.finish(true, "").unwrap_err().to_string();
        assert!(e.contains("usage limit"), "{e}");
    }

    #[test]
    fn failed_tool_is_an_error_result() {
        let mut p = CopilotParser::default();
        let outs = p.feed(r#"{"type":"tool.execution_complete","data":{"toolCallId":"t1","success":false,"error":{"message":"Permission denied: shell"}}}"#);
        assert!(matches!(&outs[..], [CliOut::Activity(StreamActivity::ToolResult { is_error: true, content, .. })] if content.contains("Permission denied")));
    }

    #[test]
    fn permission_tiers_mirror_claude_code() {
        let d = permission_args("default");
        assert!(d.contains(&"--deny-tool=write".to_string()) && d.contains(&"--deny-tool=shell".to_string()));
        assert!(!d.iter().any(|a| a.starts_with("--allow-all")));
        let e = permission_args("acceptEdits");
        assert!(e.contains(&"--allow-tool=write".to_string()) && e.contains(&"--deny-tool=shell".to_string()));
        assert!(permission_args("runCommands").contains(&"--allow-tool=shell".to_string()));
        assert_eq!(permission_args("bypassPermissions"), vec!["--allow-all-tools"]);
        for mode in ["default", "acceptEdits", "runCommands", "bypassPermissions"] {
            let a = permission_args(mode);
            assert!(!a.iter().any(|x| x == "--yolo" || x == "--allow-all" || x == "--allow-all-paths" || x == "--allow-all-urls"));
        }
    }

    #[test]
    fn args_put_the_prompt_last() {
        let a = build_args("gpt-5.3-codex", &permission_args("default"), "hello".into());
        assert_eq!(&a[a.len() - 2..], ["-p", "hello"]);
        assert!(a.windows(2).any(|w| w == ["--model", "gpt-5.3-codex"]));
        assert!(a.windows(2).any(|w| w == ["--output-format", "json"]));
        assert!(a.contains(&"--no-ask-user".to_string()));
    }
}
