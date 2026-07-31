//! OpenAI Codex CLI bridge — the bring-your-own-ChatGPT-subscription path (no
//! API key, `codex login`). Runs `codex exec --json` non-interactively and
//! surfaces the agent's reply.
//!
//! Invocation:
//! `codex exec --json --skip-git-repo-check --sandbox workspace-write
//!  [--color never] [-m <model>] [<extra args>] <prompt>` with the rendered
//! prompt as a positional argument; stdin is closed so codex doesn't block
//! waiting for "additional input".
//!
//! Codex emits newline-delimited JSON events. The reply is carried by
//! `item.completed` events whose `item.type` is `agent_message`
//! (`{"type":"item.completed","item":{"type":"agent_message","text":"…"}}`);
//! reasoning / command / file-change items are ignored. `--sandbox
//! workspace-write` lets it edit files in the working dir (an isolated worktree,
//! upstream) while staying sandboxed.

use std::process::Stdio;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

use super::anthropic::{ChatTurn, ProviderError};

/// The agent-message text from a codex JSONL event line, if it is one.
fn extract_agent_message(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type")?.as_str()? != "item.completed" {
        return None;
    }
    let item = v.get("item")?;
    if item.get("type")?.as_str()? != "agent_message" {
        return None;
    }
    Some(item.get("text")?.as_str()?.to_string())
}

/// Stream a reply from the Codex CLI. `program` is the binary (default `codex`);
/// `model` maps to `-m`; `extra_args` are appended before the prompt.
pub async fn stream_reply(
    program: &str,
    model: &str,
    extra_args: &[String],
    working_dir: Option<&str>,
    extra_env: &[(String, String)],
    system: Option<&str>,
    turns: &[ChatTurn],
    mut on_delta: impl FnMut(String),
) -> Result<String, ProviderError> {
    let program = if program.is_empty() { "codex" } else { program };
    let prompt = super::subprocess::render_prompt(system, turns);

    let mut args: Vec<String> = vec![
        "exec".into(),
        "--json".into(),
        "--skip-git-repo-check".into(),
        "--sandbox".into(),
        "workspace-write".into(),
        "--color".into(),
        "never".into(),
    ];
    if !model.is_empty() {
        args.push("-m".into());
        args.push(model.to_string());
    }
    args.extend(extra_args.iter().cloned());
    args.push(prompt);

    let mut cmd = Command::new(program);
    cmd.args(&args)
        // Null stdin: the prompt is a positional arg, so codex must not sit
        // "Reading additional input from stdin…".
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Kill codex if this future is dropped (the user hits Stop).
        .kill_on_drop(true);
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| ProviderError::Subprocess(format!("spawn {program}: {e}")))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProviderError::Subprocess("no stdout".into()))?;
    // Drain stderr concurrently so a chatty CLI can't deadlock by filling the
    // stderr pipe while we're blocked reading stdout.
    let stderr_task = child.stderr.take().map(|mut e| {
        tokio::spawn(async move {
            let mut buf = String::new();
            let _ = e.read_to_string(&mut buf).await;
            buf
        })
    });
    let mut reader = BufReader::new(stdout).lines();

    let mut assembled = String::new();
    while let Some(line) = reader
        .next_line()
        .await
        .map_err(|e| ProviderError::Subprocess(format!("read stdout: {e}")))?
    {
        if let Some(text) = extract_agent_message(&line) {
            assembled.push_str(&text);
            on_delta(text);
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
    Ok(assembled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agent_message_events() {
        let line = r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"PINGOK"}}"#;
        assert_eq!(extract_agent_message(line).as_deref(), Some("PINGOK"));
    }

    #[test]
    fn ignores_non_agent_events() {
        assert!(extract_agent_message(r#"{"type":"turn.started"}"#).is_none());
        assert!(extract_agent_message(r#"{"type":"thread.started","thread_id":"x"}"#).is_none());
        // reasoning/command items are not the reply
        assert!(extract_agent_message(
            r#"{"type":"item.completed","item":{"type":"reasoning","text":"thinking"}}"#
        )
        .is_none());
    }
}
