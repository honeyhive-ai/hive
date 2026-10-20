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

/// A small staff directory for a dependent multi-step task: find each
/// person's department, its manager, then the manager's phone and report
/// count. Ids differ per seed so the model can't carry them over. A call with
/// an id the directory doesn't know (one the model mangled or mixed up) gets
/// an error result and counts as a bad call.
struct Directory {
    seed: u64,
    bad_calls: Mutex<usize>,
    calls: Mutex<usize>,
}

impl Directory {
    fn id(&self, prefix: &str, salt: u64) -> String {
        format!("{prefix}-{}", 1000 + (self.seed * 7919 + salt * 104_729) % 9000)
    }
    fn phone(&self, salt: u64) -> String {
        format!("+351 21 {} {}", 100 + (self.seed * 31 + salt * 17) % 900, 1000 + (self.seed * 131 + salt * 71) % 9000)
    }
    fn reports(&self, salt: u64) -> u64 {
        3 + (self.seed + salt) % 7
    }
}

impl ToolExecutor for Directory {
    async fn call(&self, name: &str, input: &Value) -> (String, bool) {
        *self.calls.lock().unwrap() += 1;
        let arg = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
        // Salt 1 = Ana's side, 2 = Rui Costa (Porto), 3 = the other Rui (Lisbon).
        let dept = |s| self.id("D", s);
        let mgr = |s| self.id("E", s + 10);
        let found = match name {
            "find_employee" => {
                let n = arg("name").to_lowercase();
                if n.contains("ana") {
                    Some(json!({"matches": [{"name": "Ana Duarte", "office": "Lisbon", "department_id": dept(1)}]}))
                } else if n.contains("rui") {
                    Some(json!({"matches": [
                        {"name": "Rui Costa", "office": "Lisbon", "department_id": dept(3)},
                        {"name": "Rui Costa", "office": "Porto", "department_id": dept(2)}
                    ]}))
                } else {
                    Some(json!({"matches": []}))
                }
            }
            "get_department" => (1..=3)
                .find(|s| dept(*s) == arg("department_id"))
                .map(|s| json!({"department_id": dept(s), "manager_employee_id": mgr(s)})),
            "get_contact" => (1..=3).find(|s| mgr(*s) == arg("employee_id")).map(|s| json!({"phone": self.phone(s)})),
            "count_reports" => (1..=3).find(|s| mgr(*s) == arg("employee_id")).map(|s| json!({"direct_reports": self.reports(s)})),
            _ => None,
        };
        match found {
            Some(v) => (v.to_string(), false),
            None => {
                *self.bad_calls.lock().unwrap() += 1;
                (json!({"error": format!("{name}: no such record")}).to_string(), true)
            }
        }
    }
}

fn directory_tools() -> Vec<Value> {
    let tool = |name: &str, description: &str, arg: &str| {
        json!({"name": name, "description": description,
               "input_schema": {"type": "object", "properties": {arg: {"type": "string"}}, "required": [arg]}})
    };
    vec![
        tool("find_employee", "Look up employees by full name; returns every match with office and department id.", "name"),
        tool("get_department", "Get a department by id; returns its manager's employee id.", "department_id"),
        tool("get_contact", "Get contact details (phone) for an employee id.", "employee_id"),
        tool("count_reports", "Number of direct reports of an employee id.", "employee_id"),
    ]
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Reasoning echo, A/B: the same dependent ~8-call task with each round's
/// reasoning echoed back on its assistant message (`reasoning_content`, the
/// default) and without. Scores are printed, not asserted. Runs per arm:
/// `HIVE_OPENAI_LIVE_ECHO_RUNS` (default 3). Meant for a model that reasons
/// in a separate field (gpt-oss); for one that doesn't, there's nothing to
/// echo and the arms match.
///
/// Lab gateway, gpt-oss:20b, `reasoning_effort: "low"`, 2026-10-19:
///
/// ```text
/// run                  arm       correct   calls/run   bad-id calls   time
/// script, 15 seeds     echo      15/15     8.0          0              98 s
///                      no echo   14/15     9.6         20             140 s
/// this test, 6 seeds   echo       6/6      8.0          0              38 s
///                      no echo    5/6      9.2         10              58 s
/// ```
///
/// Without its earlier reasoning the model loses track of which id came from
/// which lookup and passes the wrong one, then spends extra calls recovering.
/// The echoed reasoning is rendered: round prompts grew by its length.
#[tokio::test]
async fn reasoning_echo_ab_on_a_multi_step_task() {
    let Some(l) = live() else {
        eprintln!("HIVE_OPENAI_LIVE_ENDPOINT unset; skipping live OpenAI-wire test");
        return;
    };
    let runs: u64 = std::env::var("HIVE_OPENAI_LIVE_ECHO_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    let mut extra = serde_json::Map::new();
    if l.model.contains("gpt-oss") {
        extra.insert("reasoning_effort".into(), json!("low"));
    } else {
        extra.insert("chat_template_kwargs".into(), json!({"enable_thinking": false}));
    }
    for echo in [true, false] {
        let (mut correct, mut calls, mut bad, mut secs) = (0usize, 0usize, 0usize, 0f64);
        for seed in 0..runs {
            let dir = Directory { seed, bad_calls: Mutex::new(0), calls: Mutex::new(0) };
            let client = OpenAiClient::new(&l.endpoint)
                .with_provider_label("endpoint")
                .with_idle_timeout(Duration::from_secs(180))
                .with_extra_body(extra.clone())
                .with_reasoning_echo(echo);
            let start = Instant::now();
            let out = client
                .stream_chat_with_tools(
                    l.api_key.as_deref(),
                    &l.model,
                    Some("You are a helpful assistant with tools. Call tools to look things up; never guess."),
                    &[ChatTurn::user(format!(
                        "[{seed}] Give me the phone numbers of the department managers of Ana Duarte (Lisbon office) and of \
                         Rui Costa from the Porto office, and tell me which of those two managers has more direct reports."
                    ))],
                    &directory_tools(),
                    &dir,
                    14,
                    |_| {},
                    |_| {},
                )
                .await
                .expect("chat with tools");
            let took = start.elapsed().as_secs_f64();
            let d = digits(&out.text);
            let ok = d.contains(&digits(&dir.phone(1))) && d.contains(&digits(&dir.phone(2))) && !d.contains(&digits(&dir.phone(3)));
            let (n, b) = (*dir.calls.lock().unwrap(), *dir.bad_calls.lock().unwrap());
            println!(
                "{}",
                json!({"check": "reasoning_echo", "model": l.model, "echo": echo, "seed": seed, "ok": ok, "rounds": out.rounds,
                       "calls": n, "bad_calls": b, "secs": (took * 10.0).round() / 10.0})
            );
            correct += ok as usize;
            calls += n;
            bad += b;
            secs += took;
        }
        println!(
            "{}",
            json!({"check": "reasoning_echo_summary", "model": l.model, "echo": echo, "correct": correct, "runs": runs,
                   "calls": calls, "bad_calls": bad, "secs": secs.round()})
        );
    }
}
