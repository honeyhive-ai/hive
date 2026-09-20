//! Live smoke test against a real Ollama server. Skipped unless
//! `HIVE_OLLAMA_LIVE_ENDPOINT` is set, so CI never touches the network:
//!
//! ```text
//! HIVE_OLLAMA_LIVE_ENDPOINT=http://host:11434 HIVE_OLLAMA_LIVE_MODEL=qwen3.5:latest \
//!   cargo test -p hive-runtime --test ollama_live -- --nocapture
//! ```

use hive_runtime::provider::ollama::{ChatOptions, OllamaClient};
use hive_runtime::provider::ChatTurn;
use std::time::Duration;

fn live() -> Option<(String, String)> {
    let endpoint = std::env::var("HIVE_OLLAMA_LIVE_ENDPOINT").ok()?;
    let model = std::env::var("HIVE_OLLAMA_LIVE_MODEL").unwrap_or_else(|_| "qwen3.5:latest".into());
    Some((endpoint, model))
}

/// `/api/ps` reports the context window the loaded model was started with —
/// the only observable proof that `num_ctx` reached the server.
async fn loaded_context_length(base: &str, model: &str) -> Option<u64> {
    let v: serde_json::Value = reqwest::get(format!("{base}/api/ps")).await.ok()?.json().await.ok()?;
    v["models"]
        .as_array()?
        .iter()
        .find(|m| m["name"].as_str() == Some(model) || m["model"].as_str() == Some(model))
        .and_then(|m| m["context_length"].as_u64())
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
    let loaded = loaded_context_length(client.base(), &model).await;
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
