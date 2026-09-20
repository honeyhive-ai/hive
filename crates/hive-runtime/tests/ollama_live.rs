//! Live smoke test against a real Ollama server. Skipped unless
//! `HIVE_OLLAMA_LIVE_ENDPOINT` is set, so CI never touches the network:
//!
//! ```text
//! HIVE_OLLAMA_LIVE_ENDPOINT=http://host:11434 HIVE_OLLAMA_LIVE_MODEL=qwen3.5:latest \
//!   cargo test -p hive-runtime --test ollama_live -- --nocapture
//! ```

use hive_runtime::provider::ollama::{ChatOptions, OllamaClient};
use hive_runtime::provider::{ChatTurn, StreamActivity};
use hive_runtime::tool_loop::ToolExecutor;
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::Duration;

fn live() -> Option<(String, String)> {
    let endpoint = std::env::var("HIVE_OLLAMA_LIVE_ENDPOINT").ok()?;
    let model = std::env::var("HIVE_OLLAMA_LIVE_MODEL").unwrap_or_else(|_| "qwen3.5:latest".into());
    Some((endpoint, model))
}

/// `/api/ps` reports the context window the loaded model was started with —
/// the only observable proof that `num_ctx` reached the server.
async fn loaded_context_length(client: &OllamaClient, model: &str) -> Option<u64> {
    client
        .list_running()
        .await
        .ok()?
        .into_iter()
        .find(|m| m.name == model)
        .and_then(|m| m.context_length)
}

/// Model management against the live server: the configured model is in the
/// installed list, and the probe's details agree with the tag listing.
#[tokio::test]
async fn lists_installed_models() {
    let Some((endpoint, model)) = live() else {
        eprintln!("HIVE_OLLAMA_LIVE_ENDPOINT unset; skipping live Ollama test");
        return;
    };
    let client = OllamaClient::new(&endpoint).with_idle_timeout(Duration::from_secs(60));
    let models = client.list_models().await.expect("/api/tags");
    for m in &models {
        eprintln!("installed: {} {} {} {:.1} GB", m.name, m.parameter_size, m.quantization, m.size_bytes as f64 / 1e9);
    }
    let found = models.iter().find(|m| m.name == model || m.name == format!("{model}:latest"));
    assert!(found.is_some(), "{model} is installed on the live server");
    assert!(found.unwrap().size_bytes > 0, "tag listing carries a size");
}

#[tokio::test]
async fn probe_then_chat_with_and_without_thinking() {
    let Some((endpoint, model)) = live() else {
        eprintln!("HIVE_OLLAMA_LIVE_ENDPOINT unset; skipping live Ollama test");
        return;
    };
    let client = OllamaClient::new(&endpoint).with_idle_timeout(Duration::from_secs(120));

    // Probe.
    let caps = client.show(&model).await.expect("/api/show");
    eprintln!("probe: {}", caps.summary());
    assert!(client.capabilities_cached(&model).await.is_some(), "probe result is cached");

    let system = "You are a connectivity check. Reply with exactly the single word PONG and nothing else.";
    let turns = [ChatTurn::user("ping")];
    let num_ctx = 8192;

    // Default: thinking off. No reasoning deltas, reply text has no think tags.
    let opts = ChatOptions { num_ctx: Some(num_ctx), keep_alive: Some("5m".into()), think: Some(false) };
    let (mut text_deltas, mut think_deltas) = (0usize, 0usize);
    let out = client
        .stream_chat(&model, Some(system), &turns, &opts, |_| text_deltas += 1, |_| think_deltas += 1)
        .await
        .expect("chat with think=false");
    eprintln!(
        "think=false: text={:?} prompt_tokens={:?} completion_tokens={:?} think_unsupported={}",
        out.text.trim(),
        out.prompt_tokens,
        out.completion_tokens,
        out.think_unsupported
    );
    assert!(!out.text.trim().is_empty(), "got a reply");
    assert!(text_deltas > 0, "reply arrived as stream deltas");
    assert!(!out.text.contains("<think>"), "think tags never leak into the reply");
    assert!(out.prompt_tokens.is_some(), "server reported prompt_eval_count");
    if caps.thinking {
        assert!(!out.think_unsupported, "a thinking-capable model must accept `think`");
        assert_eq!(think_deltas, 0, "think=false ⇒ no reasoning stream");
    }

    // num_ctx reached the server: the loaded model reports our window.
    let loaded = loaded_context_length(&client, &model).await;
    eprintln!("loaded context_length per /api/ps: {loaded:?}");
    assert_eq!(loaded, Some(num_ctx as u64), "num_ctx was applied by the server");

    // Thinking on, for a model that supports it: reasoning arrives on the
    // separate stream and still never in the transcript text.
    if caps.thinking {
        let opts = ChatOptions { think: Some(true), ..opts };
        let mut reasoning = String::new();
        let out = client
            .stream_chat(&model, Some(system), &turns, &opts, |_| {}, |t| reasoning.push_str(&t))
            .await
            .expect("chat with think=true");
        eprintln!("think=true: text={:?} reasoning_chars={}", out.text.trim(), reasoning.len());
        assert!(!out.text.trim().is_empty());
        assert!(!out.text.contains("<think>"));
        assert!(!reasoning.is_empty(), "think=true ⇒ reasoning stream is populated");
    }
}

/// Pull a small model over the network, update it (a second pull of the same
/// tag is one manifest round trip with no bytes), then remove it. This
/// writes to the server, so it has its own gate on top of the endpoint:
///
/// ```text
/// HIVE_OLLAMA_LIVE_ENDPOINT=http://host:11434 HIVE_OLLAMA_LIVE_PULL_MODEL=all-minilm:22m \
///   cargo test -p hive-runtime --test ollama_live pull_update_and_remove -- --nocapture
/// ```
///
/// Pick a tag the server does not already have: the test refuses to run
/// against an installed model so it never removes something the user wanted.
#[tokio::test]
async fn pull_update_and_remove_a_small_model() {
    let Some((endpoint, _)) = live() else {
        eprintln!("HIVE_OLLAMA_LIVE_ENDPOINT unset; skipping live Ollama test");
        return;
    };
    let Ok(model) = std::env::var("HIVE_OLLAMA_LIVE_PULL_MODEL") else {
        eprintln!("HIVE_OLLAMA_LIVE_PULL_MODEL unset; skipping live pull test");
        return;
    };
    let client = OllamaClient::new(&endpoint).with_idle_timeout(Duration::from_secs(60));
    let installed = |models: &[hive_runtime::provider::ollama::LocalModel]| models.iter().any(|m| m.name == model);

    let before = client.list_models().await.expect("/api/tags before");
    assert!(
        !installed(&before),
        "{model} is already installed on {}; pick a tag the server doesn't have so the test can remove it afterwards",
        client.host()
    );

    // Pull: progress lines stream in, at least one layer reports bytes, and
    // the stream ends on the server's success line.
    let started = std::time::Instant::now();
    let (mut lines, mut byte_lines, mut max_total, mut last_completed) = (0usize, 0usize, 0u64, 0u64);
    let mut phases: Vec<String> = Vec::new();
    client
        .pull(&model, |p| {
            lines += 1;
            if let (Some(t), Some(c)) = (p.total, p.completed) {
                byte_lines += 1;
                max_total = max_total.max(t);
                last_completed = c;
            }
            if phases.last() != Some(&p.status) {
                phases.push(p.status.clone());
            }
        })
        .await
        .expect("pull");
    eprintln!(
        "pull {model}: {lines} progress lines ({byte_lines} with byte counts), largest layer {:.1} MB, {:.1}s, phases: {}",
        max_total as f64 / 1e6,
        started.elapsed().as_secs_f64(),
        phases.join(" -> ")
    );
    assert!(lines > 0, "pull streamed progress");
    assert!(byte_lines > 0 && max_total > 0, "at least one layer reported total/completed bytes");
    assert_eq!(phases.last().map(String::as_str), Some("success"), "stream ended on success");
    let after = client.list_models().await.expect("/api/tags after pull");
    let entry = after.iter().find(|m| m.name == model).expect("pulled model is now listed");
    assert!(entry.size_bytes > 0, "listed with a size");

    // Update: pulling an installed, current tag is a quick success.
    let started = std::time::Instant::now();
    let mut update_lines = 0usize;
    client.pull(&model, |_| update_lines += 1).await.expect("update pull");
    eprintln!("update {model}: {update_lines} lines, {:.2}s", started.elapsed().as_secs_f64());
    assert!(update_lines > 0, "update streamed at least the success line");

    // Remove, and confirm the server agrees. A second delete is a 404.
    client.delete(&model).await.expect("delete");
    let final_list = client.list_models().await.expect("/api/tags after delete");
    assert!(!installed(&final_list), "{model} is gone after delete");
    let again = client.delete(&model).await;
    eprintln!("second delete: {again:?}");
    assert!(again.is_err(), "deleting an absent model is an error");
}

/// A one-tool executor for the live loop: records what the model asked for
/// and answers with a value the model can only know by calling it.
struct Weather {
    calls: Mutex<Vec<(String, Value)>>,
}
impl ToolExecutor for Weather {
    async fn call(&self, name: &str, input: &Value) -> (String, bool) {
        self.calls.lock().unwrap().push((name.to_string(), input.clone()));
        let city = input.get("city").and_then(Value::as_str).unwrap_or("?");
        (format!("Weather in {city}: 23°C, clear sky, wind 9 km/h"), false)
    }
}

/// The tool loop against the live model: the probe must say `tools`, the
/// model must call the offered tool (not narrate it), the result must come
/// back through history, and the final answer must use it. This is the
/// harness's phase-3 acceptance check — "does qwen3.5 actually drop tool
/// calls?" — so it prints every round.
#[tokio::test]
async fn tool_call_round_trip_with_the_live_model() {
    let Some((endpoint, model)) = live() else {
        eprintln!("HIVE_OLLAMA_LIVE_ENDPOINT unset; skipping live Ollama test");
        return;
    };
    let client = OllamaClient::new(&endpoint).with_idle_timeout(Duration::from_secs(120));
    let caps = client.show(&model).await.expect("/api/show");
    eprintln!("probe: {}", caps.summary());
    assert!(caps.tools, "{model} must report tool support for the tool loop to be offered");

    let tools = vec![json!({
        "name": "get_weather",
        "description": "Current weather for a city. Call this whenever asked about weather.",
        "input_schema": {
            "type": "object",
            "properties": { "city": { "type": "string", "description": "City name" } },
            "required": ["city"]
        }
    })];
    let exec = Weather { calls: Mutex::new(vec![]) };
    let system = hive_runtime::prompt::with_tools_offered(
        "You are a terse assistant. Answer in one short sentence."
    );
    let turns = [ChatTurn::user("What is the weather in Lisbon right now?")];
    let opts = ChatOptions { num_ctx: Some(8192), keep_alive: Some("5m".into()), think: Some(false) };
    let mut acts: Vec<String> = Vec::new();
    let mut deltas = 0usize;
    let started = std::time::Instant::now();
    let out = client
        .stream_chat_with_tools(&model, Some(&system), &turns, &opts, &tools, &exec, 4, |_| deltas += 1, |a| {
            acts.push(match a {
                StreamActivity::Tool { name, input_json, .. } => format!("call {name}({input_json})"),
                StreamActivity::ToolResult { content, is_error, .. } => format!("result err={is_error} {content}"),
                StreamActivity::Thinking { text } => format!("thinking {} chars", text.len()),
            });
        })
        .await
        .expect("tool loop");
    eprintln!("{:.1}s, {deltas} deltas, activity: {acts:#?}", started.elapsed().as_secs_f64());
    eprintln!("final: {:?} (prompt_tokens={:?}, completion_tokens={:?})", out.text.trim(), out.prompt_tokens, out.completion_tokens);

    let calls = exec.calls.lock().unwrap();
    assert!(!out.tools_unsupported, "server accepted the tool definitions");
    assert_eq!(calls.len(), 1, "the model called the tool exactly once: {calls:?}");
    assert_eq!(calls[0].0, "get_weather");
    let city = calls[0].1.get("city").and_then(Value::as_str).unwrap_or("").to_lowercase();
    assert!(city.contains("lisbon"), "the model passed the city through: {:?}", calls[0].1);
    assert!(out.text.contains("23"), "the answer uses the tool's result: {:?}", out.text);
    assert!(deltas > 0, "the final answer was streamed");
}
