//! Live prefix-cache check: two consecutive turns of one chat, built the way
//! the app builds them (primary system prompt + runtime identity block, the
//! hysteresis window plan, the summary as the first turn, attributed history)
//! and sent through `dispatch::stream_with_tools` as provider Custom. A small
//! in-process proxy sits between Hive and the server, records every request
//! body, and reads the server's `timings` (`cache_n`, `prompt_n`) from the
//! last streamed chunk. The second turn's first request should reuse nearly
//! all of the first turn's prompt from the server's cache.
//!
//! Skipped unless `HIVE_PREFIX_LIVE_ENDPOINT` is set (a llama.cpp server or
//! gateway: base URL, `/v1`, or the full chat-completions URL):
//!
//! ```text
//! HIVE_PREFIX_LIVE_ENDPOINT=http://host:8080/v1 HIVE_PREFIX_LIVE_MODEL=gemma4:12b \
//!   cargo test -p hive-runtime --test prefix_cache_live -- --nocapture --test-threads=1
//! ```
//!
//! Three variants: `plain` (no tools), `tools` (the read-only built-ins
//! offered, as an agent turn would), and `compacted` (the history overflows
//! the budget, so a fixed summary rides as the first turn ahead of the kept
//! messages, as it does between compaction batches).
//!
//! Results on the lab (2026-10-19, gemma4:12b, turn 2's first request):
//!
//! ```text
//! variant     one llama-server (:8081)          gateway (least-loaded, 2 workers)
//! plain       cache_n 2394 / 2445  (97.9%)      cache_n 1 / 2454  (0%)
//! tools       cache_n 2782 / 2847  (97.7%)      cache_n 1 / 2845  (0%)
//! compacted   cache_n 1873 / 1926  (97.2%)      cache_n 1 / 1924  (0%)
//! ```
//!
//! Turn 1's messages and tools were byte-identical prefixes of turn 2's in
//! every run. Through the gateway, the idle `least-loaded` policy rotates ties
//! across the two gemma4 workers, so turn 2 always lands on the other card;
//! a cache-aware routing policy is needed there.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use hive_core::chat::{ChatMessage, MessageRole};
use hive_core::context_budget::{plan_with_hysteresis, token_estimator, DEFAULT_HIGH_WATERMARK, DEFAULT_LOW_WATERMARK};
use hive_core::{ActorIdentity, ActorKind, ChatSession, ModelProviderKind, WorkspaceMember, WorkspaceRole};
use hive_runtime::agent_tools::{builtin_policy, CompositeTools, MAX_OFFERED_TOOLS};
use hive_runtime::chat_service::turns_with_summary;
use hive_runtime::provider::dispatch::{self, NoTools, ResolvedRuntime, ToolSet};
use hive_runtime::{prompt, BuiltinTools};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone)]
struct Proxy {
    upstream: String,
    http: reqwest::Client,
    seen: Arc<Mutex<Vec<Exchange>>>,
}

/// One request through the proxy and what the server said about its prompt.
#[derive(Clone, Debug)]
struct Exchange {
    body: Value,
    cache_n: Option<u64>,
    prompt_n: Option<u64>,
    cached_tokens: Option<u64>,
}

async fn forward(State(p): State<Proxy>, body: Bytes) -> axum::response::Response {
    let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let resp = p
        .http
        .post(&p.upstream)
        .header("content-type", "application/json")
        .body(body.to_vec())
        .send()
        .await
        .expect("upstream reachable");
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.expect("upstream body");
    let (mut cache_n, mut prompt_n, mut cached_tokens) = (None, None, None);
    for line in String::from_utf8_lossy(&bytes).lines() {
        let Some(v) = line.strip_prefix("data: ").and_then(|d| serde_json::from_str::<Value>(d).ok()) else {
            continue;
        };
        if let Some(t) = v.get("timings") {
            cache_n = t.get("cache_n").and_then(Value::as_u64).or(cache_n);
            prompt_n = t.get("prompt_n").and_then(Value::as_u64).or(prompt_n);
        }
        if let Some(c) = v.pointer("/usage/prompt_tokens_details/cached_tokens").and_then(Value::as_u64) {
            cached_tokens = Some(c);
        }
    }
    p.seen.lock().unwrap().push(Exchange { body: req, cache_n, prompt_n, cached_tokens });
    (
        axum::http::StatusCode::from_u16(status).unwrap(),
        [(header::CONTENT_TYPE, "text/event-stream")],
        bytes,
    )
        .into_response()
}

async fn start_proxy(upstream: String) -> (String, Arc<Mutex<Vec<Exchange>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let proxy = Proxy { upstream, http: reqwest::Client::new(), seen: seen.clone() };
    let app = Router::new().route("/v1/chat/completions", post(forward)).with_state(proxy);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/v1/chat/completions"), seen)
}

fn chat_url(raw: &str) -> String {
    let raw = raw.trim().trim_end_matches('/');
    if raw.ends_with("/chat/completions") {
        raw.to_string()
    } else if raw.ends_with("/v1") {
        format!("{raw}/chat/completions")
    } else {
        format!("{raw}/v1/chat/completions")
    }
}

fn runtime(endpoint: String, model: String) -> ResolvedRuntime {
    ResolvedRuntime {
        provider: ModelProviderKind::Custom,
        model,
        endpoint,
        api_key: None,
        args: vec![],
        model_provider_id: None,
        model_base_url: None,
        context_window_tokens: Some(32768),
        keep_alive: None,
        think: None,
        tools: true,
        max_tool_rounds: None,
        allow_remote_file_tools: false,
    }
}

fn session() -> ChatSession {
    let mut s = ChatSession::new("Offline harness", Uuid::nil(), "lab-gateway");
    s.members.push(WorkspaceMember {
        id: "m1".into(),
        actor: ActorIdentity::new("u1", "Mara", ActorKind::Human),
        role: WorkspaceRole::Owner,
        title: "Engineer".into(),
        index: 1,
        joined_at: Default::default(),
    });
    s
}

/// A chat with some substance, so the cached prefix is worth measuring.
fn history() -> Vec<ChatMessage> {
    let topics = [
        "the deploy pipeline for the gateway",
        "why prefill on ROCm beats the Ollama blob",
        "how slots and the host prompt cache interact",
        "which model to use for titles",
        "the compaction watermarks",
        "retrieval with a reranker",
    ];
    let mut out = Vec::new();
    for (i, t) in topics.iter().enumerate() {
        out.push(ChatMessage::new(
            MessageRole::User,
            "Mara",
            format!("Question {}: can you explain {t}? Keep it to a short paragraph, and mention one risk.", i + 1),
        ));
        out.push(ChatMessage::new(
            MessageRole::Assistant,
            "Hive",
            format!(
                "Here is a short take on {t}. {} The main risk is a configuration drift between machines, \
                 so pin versions and check the settings after every upgrade.",
                "It works by keeping the hot path small, measuring before changing anything, and \
                 preferring stable inputs so caches stay warm. "
                    .repeat(4)
            ),
        ));
    }
    out
}

struct TurnResult {
    first: Exchange,
    boundary: Option<Uuid>,
    moved: bool,
    requests: usize,
    reply: String,
    secs: f64,
}

#[allow(clippy::too_many_arguments)]
async fn run_turn(
    rt: &ResolvedRuntime,
    system: &str,
    history: &[ChatMessage],
    budget: i64,
    prior_boundary: Option<Uuid>,
    summary: Option<&str>,
    tools: Option<&CompositeTools<NoTools>>,
    seen: &Arc<Mutex<Vec<Exchange>>>,
) -> TurnResult {
    let hp = plan_with_hysteresis(history, budget, prior_boundary, DEFAULT_HIGH_WATERMARK, DEFAULT_LOW_WATERMARK);
    let summary = summary.filter(|_| !hp.plan.overflow.is_empty());
    let turns = turns_with_summary(&hp.plan.kept, None, "Hive", summary);
    let before = seen.lock().unwrap().len();
    let start = Instant::now();
    let reply = dispatch::stream_with_tools(
        rt,
        Some(system),
        &turns,
        None,
        &[],
        1024,
        tools.map(|t| ToolSet { defs: t.defs(), executor: t, max_rounds: 4 }),
        |_| {},
        |_| {},
    )
    .await
    .expect("turn completes");
    let secs = start.elapsed().as_secs_f64();
    let all = seen.lock().unwrap();
    TurnResult { first: all[before].clone(), boundary: hp.boundary, moved: hp.moved, requests: all.len() - before, reply, secs }
}

fn messages(e: &Exchange) -> Vec<Value> {
    e.body["messages"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn second_turn_reuses_the_cached_prefix() {
    let Ok(raw) = std::env::var("HIVE_PREFIX_LIVE_ENDPOINT") else {
        eprintln!("HIVE_PREFIX_LIVE_ENDPOINT unset; skipping live prefix-cache test");
        return;
    };
    let model = std::env::var("HIVE_PREFIX_LIVE_MODEL").unwrap_or_else(|_| "gemma4:12b".into());
    let (proxy_url, seen) = start_proxy(chat_url(&raw)).await;
    let rt = runtime(proxy_url, model.clone());
    let session = session();
    let system = format!(
        "{}\n\n{}",
        prompt::primary_system_prompt(&session),
        prompt::runtime_identity_block(&rt, "Lab Mac (this device)")
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "# demo\n").unwrap();

    for variant in ["plain", "tools", "compacted"] {
        let tools = (variant == "tools").then(|| {
            let builtin = BuiltinTools::new(builtin_policy(dir.path(), false, false, false)).unwrap();
            CompositeTools::<NoTools>::new(Some(builtin), None, MAX_OFFERED_TOOLS)
        });
        let summary = "Mara and Hive discussed the gateway deploy pipeline and ROCm prefill speed. \
                       Decisions: pin llama.cpp versions; keep prompts stable for the prompt cache.";
        // A per-variant opener keeps variants from sharing a cached prefix.
        let mut hist = vec![ChatMessage::new(MessageRole::User, "Mara", format!("Session tag: {variant}-{}", Uuid::new_v4()))];
        hist.push(ChatMessage::new(MessageRole::Assistant, "Hive", "Noted."));
        hist.extend(history());
        hist.push(ChatMessage::new(MessageRole::User, "Mara", "In one sentence: what should we cache?"));
        // The compacted variant gets a budget turn 1's history just overflows
        // (so it compacts in one batch down to the low watermark) and turn 2's
        // short exchange stays inside; the others keep the whole history. The
        // summary is held fixed across the two turns, as the app's summary
        // cache does between batches.
        let budget = if variant == "compacted" {
            (token_estimator::estimate_messages(&hist) as f64 / 0.9) as i64
        } else {
            20_000
        };

        let t1 = run_turn(&rt, &system, &hist, budget, None, Some(summary), tools.as_ref(), &seen).await;
        hist.push(ChatMessage::new(MessageRole::Assistant, "Hive", t1.reply.clone()));
        hist.push(ChatMessage::new(MessageRole::User, "Mara", "And in one sentence: what should we never cache?"));
        // The app persists the boundary between turns; carry it forward.
        let t2 = run_turn(&rt, &system, &hist, budget, t1.boundary, Some(summary), tools.as_ref(), &seen).await;

        // The prompt builder's half of the bargain: turn 1's request messages
        // are a byte-identical prefix of turn 2's, system prompt included.
        let (m1, m2) = (messages(&t1.first), messages(&t2.first));
        let shared = m1.iter().zip(&m2).take_while(|(a, b)| a == b).count();
        let tools_same = t1.first.body.get("tools") == t2.first.body.get("tools");
        let t1_prompt = t1.first.cache_n.unwrap_or(0) + t1.first.prompt_n.unwrap_or(0);
        let t2_prompt = t2.first.cache_n.unwrap_or(0) + t2.first.prompt_n.unwrap_or(0);
        let reuse = t2.first.cache_n.map(|c| c as f64 / t2_prompt.max(1) as f64);
        println!(
            "{}",
            serde_json::json!({
                "check": "prefix_cache",
                "variant": variant,
                "model": model,
                "turn1": {"prompt_tokens": t1_prompt, "cache_n": t1.first.cache_n, "prompt_n": t1.first.prompt_n,
                          "requests": t1.requests, "secs": (t1.secs * 10.0).round() / 10.0, "messages": m1.len()},
                "turn2": {"prompt_tokens": t2_prompt, "cache_n": t2.first.cache_n, "prompt_n": t2.first.prompt_n,
                          "cached_tokens": t2.first.cached_tokens, "requests": t2.requests,
                          "secs": (t2.secs * 10.0).round() / 10.0, "messages": m2.len()},
                "turn2_reuse": reuse.map(|r| (r * 1000.0).round() / 1000.0),
                "turn2_reuse_of_turn1_prompt": t2.first.cache_n.map(|c| ((c as f64 / t1_prompt.max(1) as f64) * 1000.0).round() / 1000.0),
                "shared_messages": shared,
                "compacted_on_turn1": t1.moved,
                "boundary_moved_on_turn2": t2.moved,
                "tools_field_identical": tools_same,
            })
        );
        // Turn 1's messages — except a trailing user message, which the
        // repair step may merge with the new user message — stay unchanged.
        assert!(shared + 1 >= m1.len(), "{variant}: turn 2 rewrote turn 1's prompt ({shared}/{} shared)", m1.len());
        assert!(tools_same, "{variant}: the offered tools changed between turns");
        if variant == "compacted" {
            assert!(t1.moved, "compacted: turn 1 did not compact");
        }
        assert!(!t2.moved, "{variant}: the compaction boundary moved on turn 2 (the budget is too tight for this test)");
        if let Some(c) = t2.first.cache_n {
            assert!(c > 0, "{variant}: the server reused nothing from turn 1");
        }
    }
}

/// A weather lookup the model must call; one city per call.
struct Weather;
impl hive_runtime::tool_loop::ToolExecutor for Weather {
    async fn call(&self, _name: &str, input: &Value) -> (String, bool) {
        let city = input.get("city").and_then(Value::as_str).unwrap_or("?");
        (format!("{city}: 21 C, sunny, wind 12 km/h"), false)
    }
}

/// The tool loop's final round, measured: a turn that wants three weather
/// lookups gets a budget of two rounds, so round 2 may not call tools. With
/// `tool_choice: "none"` (the default) round 2 keeps the tools field, so the
/// server reuses round 1's whole prompt from its cache; dropping the tools
/// re-renders the prompt from where the template put them. Either way the
/// last tool result carries the final-round nudge, and the reply must be text
/// (no tool call, no raw call markup). `HIVE_PREFIX_LIVE_RUNS` (default 2)
/// repeats both variants.
///
/// Results on the lab gateway (2026-10-19, llama.cpp; round 2 of each turn):
///
/// ```text
/// model        variant            round-2 cache_n / prompt     reply
/// gemma4:12b   tool_choice none   2526-2533 / ~2620 (99.8%)    text, 3/3
/// gemma4:12b   drop tools         1 / ~2580 (0%)               text, 3/3
/// gpt-oss:20b  tool_choice none   2585-2591 / ~2675 (100%)     text, 3/3
/// gpt-oss:20b  drop tools         0-1419 / ~2630 (0-55%)       text, 3/3
/// ```
///
/// Both templates render the tools ahead of the conversation (gemma4 inside
/// the system turn, gpt-oss in the developer message), so dropping them on the
/// final round re-prefills the whole history. llama.cpp honours
/// `tool_choice: "none"` on both: no `tool_calls` came back. Without the
/// nudge, though, the request alone isn't enough: with this system prompt
/// gemma4 wrote the call it wanted as raw `<|tool_call>call:get_weather…`
/// text 5/5 times (and gpt-oss tends to answer with nothing), which is why the
/// loop sends the nudge as a trailing user message.
#[tokio::test]
async fn final_round_keeps_the_tool_prefix_cached() {
    use hive_runtime::provider::openai::OpenAiClient;
    let Ok(raw) = std::env::var("HIVE_PREFIX_LIVE_ENDPOINT") else {
        eprintln!("HIVE_PREFIX_LIVE_ENDPOINT unset; skipping live final-round test");
        return;
    };
    let model = std::env::var("HIVE_PREFIX_LIVE_MODEL").unwrap_or_else(|_| "gemma4:12b".into());
    let (proxy_url, seen) = start_proxy(chat_url(&raw)).await;
    let rt = runtime(proxy_url.clone(), model.clone());
    let session = session();
    let tools = vec![serde_json::json!({
        "name": "get_weather",
        "description": "Current weather for one city.",
        "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}
    })];
    let system = prompt::with_tools_offered_for(
        &format!("{}\n\n{}", prompt::primary_system_prompt(&session), prompt::runtime_identity_block(&rt, "Lab Mac (this device)")),
        &tools,
    );
    let runs: usize = std::env::var("HIVE_PREFIX_LIVE_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
    for run in 0..runs {
        for variant in ["tool_choice_none", "drop_tools"] {
            let mut hist = vec![ChatMessage::new(MessageRole::User, "Mara", format!("Session tag: {variant}-{}", Uuid::new_v4()))];
            hist.push(ChatMessage::new(MessageRole::Assistant, "Hive", "Noted."));
            hist.extend(history());
            hist.push(ChatMessage::new(
                MessageRole::User,
                "Mara",
                "Compare the weather in Lisbon, Porto and Faro right now. Call get_weather for one city at a time.",
            ));
            let turns = turns_with_summary(&hist, None, "Hive", None);
            let client = OpenAiClient::new(&proxy_url)
                .with_extra_body(dispatch::openai_request_extras(&rt))
                .with_final_tool_choice(variant == "tool_choice_none");
            let before = seen.lock().unwrap().len();
            let out = client
                .stream_chat_with_tools(None, &model, Some(&system), &turns, &tools, &Weather, 2, |_| {}, |_| {})
                .await
                .expect("turn completes");
            let all = seen.lock().unwrap()[before..].to_vec();
            if all.len() < 2 {
                eprintln!("{variant}: the model answered without calling the tool; nothing to measure");
                continue;
            }
            let (r1, r2) = (&all[0], &all[1]);
            let r1_prompt = r1.cache_n.unwrap_or(0) + r1.prompt_n.unwrap_or(0);
            let r2_prompt = r2.cache_n.unwrap_or(0) + r2.prompt_n.unwrap_or(0);
            let leaked = ["<|tool_call>", "<tool_call>", "<|channel|>", "to=functions"].iter().any(|m| out.text.contains(m));
            println!(
                "{}",
                serde_json::json!({
                    "check": "final_round",
                    "model": model,
                    "variant": variant,
                    "run": run,
                    "requests": all.len(),
                    "round1_prompt": r1_prompt,
                    "round2_prompt": r2_prompt,
                    "round2_cache_n": r2.cache_n,
                    "round2_prompt_n": r2.prompt_n,
                    "round2_reuse_of_round1": r2.cache_n.map(|c| ((c as f64 / r1_prompt.max(1) as f64) * 1000.0).round() / 1000.0),
                    "round2_tool_choice": r2.body.get("tool_choice"),
                    "round2_has_tools": r2.body.get("tools").is_some(),
                    "reply_chars": out.text.len(),
                    "leaked_markup": leaked,
                    "reply": out.text.chars().take(160).collect::<String>(),
                })
            );
            assert!(!out.text.trim().is_empty(), "{variant}: the final round produced no answer");
            assert!(!leaked, "{variant}: the reply is raw tool-call markup: {}", out.text);
            if variant == "tool_choice_none" {
                assert_eq!(r2.body["tool_choice"], "none", "the final round forbids tools");
                assert_eq!(r2.body.get("tools"), r1.body.get("tools"), "the tools field is unchanged");
                if let Some(c) = r2.cache_n {
                    assert!(c as f64 >= 0.9 * r1_prompt as f64, "round 2 reused only {c} of round 1's {r1_prompt} tokens");
                }
            } else {
                assert!(r2.body.get("tools").is_none());
            }
        }
    }
}
