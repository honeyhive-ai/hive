//! Google Antigravity CLI bridge (`agy`) — the Google AI Pro / Ultra
//! subscription path. Gemini CLI stopped serving individual Google sign-ins on
//! 2026-06-18; `agy` is the CLI that serves them now. No API key: the Google
//! sign-in lives in the OS keychain, owned by `agy`.
//!
//! Invocation (headless, documented at antigravity.google/docs/cli/headless):
//! `agy --print <prompt> --output-format stream-json --disable-slash-commands
//!  [--model <slug>] [<permission args>] [<extra args>]`, run in the turn's
//! working dir with stdin closed.
//!
//! The stream is NDJSON with an `event` field: one `init`, `step_update`s, and
//! one `result`. Text arrives as `text_delta` fragments on `agent_response`
//! steps (`ACTIVE` partials, then a `DONE` carrying the remainder); tool steps
//! carry `tool_info {name, parameters, output, error}`; `result` has `status`
//! (`SUCCESS` / `ERROR` / …), `response`, `error` and `usage`.
//!
//! Permissions mirror Hive's Claude Code tiers. In headless mode an action
//! that needs approval is soft-denied (the run continues), so the default tier
//! is read-only, exactly like `claude -p` without a permission mode.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::anthropic::{ChatTurn, ProviderError};
use super::cli_agent::{self, cap_preview, push_segment, CliKind, CliOut, CliStreamParser};
use super::dispatch::StreamActivity;

/// `agy` flags for a Hive permission tier (the Claude Code setting's values).
/// - `default`: no flags — edits and commands need approval, which headless
///   mode soft-denies. Reads only.
/// - `acceptEdits`: `--mode accept-edits` — workspace edits run; commands are
///   still denied.
/// - `runCommands`: `--mode auto` — workspace edits plus safe, reversible
///   commands (build/test/lint) in the terminal sandbox; irreversible or
///   risky actions still need approval and are denied. Narrower than Claude's
///   "all Bash", never broader. Falls back to `default` (with a warning) where
///   auto mode isn't available.
/// - `bypassPermissions`: `--dangerously-skip-permissions`.
pub fn permission_args(mode: &str) -> Vec<String> {
    match mode {
        "acceptEdits" => vec!["--mode".into(), "accept-edits".into()],
        "runCommands" => vec!["--mode".into(), "auto".into()],
        "bypassPermissions" => vec!["--dangerously-skip-permissions".into()],
        _ => Vec::new(),
    }
}

/// The full argument list for one headless turn.
pub fn build_args(model: &str, extra_args: &[String], prompt: String) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--output-format".into(),
        "stream-json".into(),
        // A prompt that starts with "/" is text, not a slash command.
        "--disable-slash-commands".into(),
    ];
    if !model.trim().is_empty() {
        args.push("--model".into());
        args.push(model.trim().to_string());
    }
    args.extend(extra_args.iter().cloned());
    // Last, so no variadic flag can swallow it.
    args.push("--print".into());
    args.push(prompt);
    args
}

/// Parser state for `agy --output-format stream-json`.
#[derive(Default)]
pub struct AgyParser {
    assembled: String,
    last_text_step: Option<i64>,
    /// Text seen per agent_response step (to drop a DONE that repeats it).
    step_text: HashMap<i64, String>,
    tools_started: HashSet<String>,
    tools_done: HashSet<String>,
    status: Option<String>,
    response: Option<String>,
    error: Option<String>,
}

/// An event's payload: `{"event":"x","x":{…}}`, tolerating a flat line.
fn payload<'a>(v: &'a Value, event: &str) -> &'a Value {
    v.get(event).filter(|p| p.is_object()).unwrap_or(v)
}

fn error_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Object(o) => o
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| Some(v.to_string())),
        _ => None,
    }
}

impl CliStreamParser for AgyParser {
    fn feed(&mut self, line: &str) -> Vec<CliOut> {
        let mut out = Vec::new();
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            return out;
        };
        match v.get("event").and_then(Value::as_str).unwrap_or("") {
            "step_update" => {
                let s = payload(&v, "step_update");
                let idx = s.get("step_index").and_then(Value::as_i64).unwrap_or(-1);
                let done = s.get("state").and_then(Value::as_str) == Some("DONE");
                match s.get("step_type").and_then(Value::as_str).unwrap_or("") {
                    "agent_response" => {
                        let Some(delta) = s.get("text_delta").and_then(Value::as_str) else {
                            return out;
                        };
                        let seen = self.step_text.entry(idx).or_default();
                        // A DONE that repeats the whole step text only adds its tail.
                        let text = if done && !seen.is_empty() && delta.starts_with(seen.as_str()) {
                            delta[seen.len()..].to_string()
                        } else {
                            delta.to_string()
                        };
                        seen.push_str(&text);
                        let new_segment = self.last_text_step != Some(idx);
                        self.last_text_step = Some(idx);
                        push_segment(&mut self.assembled, new_segment, &text, &mut out);
                    }
                    "tool" => {
                        let conv = s.get("conversation_id").and_then(Value::as_str).unwrap_or("");
                        let id = format!("agy-{conv}-{idx}");
                        let info = s.get("tool_info").cloned().unwrap_or(Value::Null);
                        let name = info
                            .get("name")
                            .and_then(Value::as_str)
                            .or_else(|| s.get("tool_name").and_then(Value::as_str))
                            .unwrap_or("tool")
                            .to_string();
                        if self.tools_started.insert(id.clone()) {
                            out.push(CliOut::Activity(StreamActivity::Tool {
                                id: id.clone(),
                                name,
                                input_json: info.get("parameters").map(Value::to_string).unwrap_or_default(),
                            }));
                        }
                        if done && self.tools_done.insert(id.clone()) {
                            let err = info.get("error").and_then(error_text);
                            let content = err.clone().unwrap_or_else(|| match info.get("output") {
                                Some(Value::String(s)) => s.clone(),
                                Some(Value::Null) | None => String::new(),
                                Some(other) => other.to_string(),
                            });
                            out.push(CliOut::Activity(StreamActivity::ToolResult {
                                call_id: id,
                                is_error: err.is_some(),
                                content: cap_preview(content),
                            }));
                        }
                    }
                    _ => {}
                }
            }
            "result" => {
                let r = payload(&v, "result");
                self.status = r.get("status").and_then(Value::as_str).map(str::to_owned);
                self.response = r.get("response").and_then(Value::as_str).map(str::to_owned);
                self.error = r.get("error").and_then(error_text);
            }
            _ => {}
        }
        out
    }

    fn finish(&mut self, exit_ok: bool, stderr: &str) -> Result<(String, Option<String>), ProviderError> {
        let failed_status = self.status.as_deref().is_some_and(|s| s != "SUCCESS");
        if failed_status || !exit_ok {
            let mut detail = self.error.clone().unwrap_or_default();
            if detail.trim().is_empty() {
                detail = stderr.trim().to_string();
            }
            if detail.trim().is_empty() {
                detail = format!("status {}", self.status.as_deref().unwrap_or("unknown (no result event)"));
            }
            // A turn that streamed text and was then cut off still fails: the
            // user should see why, not a silently truncated reply.
            return Err(cli_agent::failure_error(CliKind::Antigravity, &detail));
        }
        if self.assembled.is_empty() {
            if let Some(r) = self.response.clone().filter(|r| !r.is_empty()) {
                self.assembled = r.clone();
                return Ok((r.clone(), Some(r)));
            }
        }
        Ok((self.assembled.clone(), None))
    }
}

/// Stream a reply from `agy`. `program` is the binary (default `agy`);
/// `model` maps to `--model` (empty = the CLI's saved default); `extra_args`
/// carry the permission tier and any runtime flags.
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
    let program = if program.trim().is_empty() { CliKind::Antigravity.default_program() } else { program };
    let prompt = super::claude_code::render_prompt(system, turns);
    let args = build_args(model, extra_args, prompt);
    let mut parser = AgyParser::default();
    cli_agent::run_cli(CliKind::Antigravity, program, &args, working_dir, extra_env, &mut parser, on_delta, on_activity)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorded from `agy` 1.5.1 (`--output-format stream-json`, a prompt that
    /// reads one file); ids and paths replaced, init's tool list shortened.
    const FIXTURE_TOOL_TURN: &str = include_str!("fixtures/agy_stream_tool.jsonl");
    /// Synthetic, built from the documented schema: a reply streamed as ACTIVE
    /// partials around a soft-denied tool, and a failed (ERROR) run.
    const FIXTURE_MULTI: &str = include_str!("fixtures/agy_stream_multi.jsonl");
    const FIXTURE_ERROR: &str = include_str!("fixtures/agy_stream_error.jsonl");

    fn run(fixture: &str) -> (Vec<CliOut>, AgyParser) {
        let mut p = AgyParser::default();
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
        assert_eq!(text(&outs), "banana\n");
        let acts: Vec<_> = outs
            .iter()
            .filter_map(|o| match o {
                CliOut::Activity(a) => Some(a.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(acts.len(), 2, "{acts:?}");
        match (&acts[0], &acts[1]) {
            (
                StreamActivity::Tool { id, name, input_json },
                StreamActivity::ToolResult { call_id, is_error, content },
            ) => {
                assert_eq!(name, "read_file");
                assert!(input_json.contains("a.txt"));
                assert_eq!(id, call_id);
                assert!(!is_error);
                assert_eq!(content, "1 lines, 7 bytes");
            }
            other => panic!("unexpected {other:?}"),
        }
        let (body, late) = p.finish(true, "").unwrap();
        assert_eq!(body, "banana\n");
        assert!(late.is_none());
    }

    #[test]
    fn partial_deltas_concatenate_and_steps_are_separated() {
        let (outs, mut p) = run(FIXTURE_MULTI);
        assert_eq!(text(&outs), "Looking at the file.\nThe answer is 42.");
        assert!(outs.iter().any(|o| matches!(o, CliOut::Activity(StreamActivity::ToolResult { is_error: true, content, .. }) if content.contains("permission"))));
        assert_eq!(p.finish(true, "").unwrap().0, "Looking at the file.\nThe answer is 42.");
    }

    #[test]
    fn error_result_surfaces_the_sign_in_fix() {
        let (_, mut p) = run(FIXTURE_ERROR);
        let e = p.finish(false, "").unwrap_err().to_string();
        assert!(e.contains("not signed in") && e.contains("Run `agy` once"), "{e}");
    }

    #[test]
    fn non_streamed_reply_falls_back_to_result_response() {
        let mut p = AgyParser::default();
        p.feed(r#"{"event":"result","result":{"status":"SUCCESS","response":"ok"}}"#);
        assert_eq!(p.finish(true, "").unwrap(), ("ok".to_string(), Some("ok".to_string())));
    }

    #[test]
    fn done_repeating_the_step_text_is_not_duplicated() {
        let mut p = AgyParser::default();
        let mut outs = p.feed(r#"{"event":"step_update","step_update":{"step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"Hel"}}"#);
        outs.extend(p.feed(r#"{"event":"step_update","step_update":{"step_index":1,"state":"DONE","step_type":"agent_response","text_delta":"Hello"}}"#));
        assert_eq!(text(&outs), "Hello");
    }

    #[test]
    fn exit_failure_without_result_uses_stderr() {
        let mut p = AgyParser::default();
        let e = p.finish(false, "Error: quota exceeded for this account").unwrap_err().to_string();
        assert!(e.contains("usage limit"), "{e}");
    }

    #[test]
    fn permission_tiers_never_widen_by_default() {
        assert!(permission_args("default").is_empty());
        assert!(permission_args("").is_empty());
        assert_eq!(permission_args("acceptEdits"), vec!["--mode", "accept-edits"]);
        assert_eq!(permission_args("runCommands"), vec!["--mode", "auto"]);
        assert_eq!(permission_args("bypassPermissions"), vec!["--dangerously-skip-permissions"]);
    }

    #[test]
    fn args_put_the_prompt_last() {
        let a = build_args("gemini-3.8-flash-low", &permission_args("acceptEdits"), "/hi".into());
        assert_eq!(&a[a.len() - 2..], ["--print", "/hi"]);
        assert!(a.windows(2).any(|w| w == ["--model", "gemini-3.8-flash-low"]));
        assert!(a.contains(&"--disable-slash-commands".to_string()));
        assert!(!build_args("", &[], "x".into()).contains(&"--model".to_string()));
    }
}
