//! Live smoke test against a real OpenAI-compatible server (a llama.cpp
//! gateway, `llama-server`, LM Studio, …). Skipped unless
//! `HIVE_OPENAI_LIVE_ENDPOINT` is set, so CI never touches the network:
//!
//! ```text
//! HIVE_OPENAI_LIVE_ENDPOINT=http://host:8080/v1/chat/completions HIVE_OPENAI_LIVE_MODEL=gemma4:12b \
//!   cargo test -p hive-runtime --test openai_live -- --nocapture
//! ```
//!
//! The endpoint may be the full chat-completions URL or the server root
//! (`/v1/chat/completions` is appended). `HIVE_OPENAI_LIVE_API_KEY` is sent as
//! a bearer token when set. `HIVE_OPENAI_LIVE_PROPS_ENDPOINT` points the
//! `/props` probe at a `llama-server` that has the route (a gateway may not).

use hive_runtime::provider::openai::{cached_context_window, OpenAiClient};
use hive_runtime::provider::{ChatTurn, StreamActivity};
use hive_runtime::tool_loop::ToolExecutor;
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Live {
    endpoint: String,
    model: String,
    api_key: Option<String>,
}

fn live() -> Option<Live> {
    let raw = std::env::var("HIVE_OPENAI_LIVE_ENDPOINT").ok()?;
    let raw = raw.trim().trim_end_matches('/');
    let endpoint = if raw.ends_with("/chat/completions") {
        raw.to_string()
    } else if raw.ends_with("/v1") {
        format!("{raw}/chat/completions")
    } else {
        format!("{raw}/v1/chat/completions")
    };
    let model = std::env::var("HIVE_OPENAI_LIVE_MODEL").unwrap_or_else(|_| "gemma4:12b".into());
    let api_key = std::env::var("HIVE_OPENAI_LIVE_API_KEY").ok().filter(|k| !k.is_empty());
    Some(Live { endpoint, model, api_key })
}

/// A trivial local tool whose answer the model can't guess.
struct SecretNumber {
    calls: Mutex<Vec<Value>>,
}
impl ToolExecutor for SecretNumber {
    async fn call(&self, name: &str, input: &Value) -> (String, bool) {
        self.calls.lock().unwrap().push(input.clone());
        if name == "get_secret_number" {
            ("The secret number is 7319.".into(), false)
        } else {
            (format!("unknown tool {name}"), true)
        }
    }
}

fn secret_tool() -> Vec<Value> {
    vec![json!({
        "name": "get_secret_number",
        "description": "Returns the user's secret number. Takes a `label` naming which secret to fetch.",
        "input_schema": {
            "type": "object",
            "properties": {"label": {"type": "string", "description": "Which secret, e.g. \"default\""}},
            "required": ["label"]
        }
    })]
}

#[tokio::test]
async fn plain_chat_streams() {
    let Some(l) = live() else {
        eprintln!("HIVE_OPENAI_LIVE_ENDPOINT unset; skipping live OpenAI-wire test");
        return;
    };
    let client = OpenAiClient::new(&l.endpoint).with_provider_label("endpoint").with_idle_timeout(Duration::from_secs(120));
    let mut deltas = 0usize;
    let start = Instant::now();
    let out = client
        .stream_reply(
            l.api_key.as_deref(),
            &l.model,
            Some("You are a connectivity check. Reply with exactly the single word PONG and nothing else."),
            &[ChatTurn::user("ping")],
            |_| deltas += 1,
        )
        .await
        .expect("plain chat");
    eprintln!("plain: {:?} in {:?} ({deltas} deltas)", out.trim(), start.elapsed());
    assert!(!out.trim().is_empty(), "got a reply");
    assert!(deltas > 0, "reply arrived as stream deltas");
    assert!(!out.contains("<think>"), "think tags never leak into the reply");
}

#[tokio::test]
async fn one_tool_round_trip() {
    let Some(l) = live() else {
        eprintln!("HIVE_OPENAI_LIVE_ENDPOINT unset; skipping live OpenAI-wire test");
        return;
    };
    let client = OpenAiClient::new(&l.endpoint).with_provider_label("endpoint").with_idle_timeout(Duration::from_secs(180));
    let exec = SecretNumber { calls: Mutex::new(vec![]) };
    let mut acts = Vec::new();
    let mut streamed = String::new();
    let start = Instant::now();
    let out = client
        .stream_chat_with_tools(
            l.api_key.as_deref(),
            &l.model,
            Some("You are a helpful assistant. You have tools; use them when a question needs information you don't have."),
            &[ChatTurn::user("What is my secret number (label \"default\")? Use the tool, then answer with just the number.")],
            &secret_tool(),
            &exec,
            4,
            |d| streamed.push_str(&d),
            |a| acts.push(a),
        )
        .await
        .expect("chat with tools");
    let tools: Vec<String> = acts
        .iter()
        .filter_map(|a| match a {
            StreamActivity::Tool { name, input_json, .. } => Some(format!("{name}({input_json})")),
            _ => None,
        })
        .collect();
    let thinking_chars: usize = acts
        .iter()
        .map(|a| match a {
            StreamActivity::Thinking { text } => text.len(),
            _ => 0,
        })
        .sum();
    eprintln!(
        "tools: rounds={} calls={tools:?} thinking_chars={thinking_chars} tools_unsupported={} in {:?}\nreply: {:?}",
        out.rounds,
        out.tools_unsupported,
        start.elapsed(),
        out.text.trim()
    );
    assert!(!out.tools_unsupported, "the server accepted the tools field");
    assert!(!exec.calls.lock().unwrap().is_empty(), "the model called the tool");
    assert!(out.rounds >= 2, "a tool round then an answer round");
    assert!(out.text.contains("7319"), "the tool's result reached the answer: {:?}", out.text);
    assert_eq!(streamed, out.text, "the body is exactly what was streamed");
    assert!(acts.iter().any(|a| matches!(a, StreamActivity::ToolResult { is_error: false, .. })));
}

#[tokio::test]
async fn props_probe_is_quiet_or_reports_a_window() {
    let Some(l) = live() else {
        eprintln!("HIVE_OPENAI_LIVE_ENDPOINT unset; skipping live OpenAI-wire test");
        return;
    };
    // The gateway itself: a window, or quietly nothing (no /props route).
    let start = Instant::now();
    let n = OpenAiClient::new(&l.endpoint).probe_context_window(l.api_key.as_deref(), &l.model).await;
    eprintln!("gateway /props: {n:?} in {:?}", start.elapsed());
    assert!(start.elapsed() < Duration::from_secs(10), "a probe never holds up a turn for long");
    assert_eq!(cached_context_window(&l.endpoint, &l.model), n);

    if let Ok(direct) = std::env::var("HIVE_OPENAI_LIVE_PROPS_ENDPOINT") {
        let n = OpenAiClient::new(direct.trim()).probe_context_window(None, &l.model).await;
        eprintln!("llama-server /props at {direct}: n_ctx={n:?}");
        assert!(n.is_some_and(|n| n >= 1024), "a llama-server reports its per-slot window");
    }
}
