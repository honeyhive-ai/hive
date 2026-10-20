//! Local-harness eval suite: scores open models served by an OpenAI-compatible
//! endpoint (a llama.cpp gateway, provider Custom) on the jobs Hive's offline
//! harness gives them, through the same pieces a chat turn uses — the
//! dispatcher (`stream_with_tools`), the composite executor (built-in file
//! tools + MCP), the hysteresis compaction planner and the summary turn.
//! Skipped unless `HIVE_EVAL_ENDPOINT` is set, so CI never touches the lab:
//!
//! ```text
//! HIVE_EVAL_ENDPOINT=http://host:8080/v1 HIVE_EVAL_MODELS=gemma4:12b,lfm2:8b \
//!   cargo test -p hive-runtime --test local_harness_eval -- --nocapture --test-threads=1
//! ```
//!
//! Each scenario × model × run prints one JSON line on stdout (`"eval":
//! "local_harness"`), then one summary line per model with its pass rate and
//! total seconds. Scores are reported, not asserted — this measures models,
//! it doesn't gate builds. Scenarios:
//!
//! - `tool_call`: one tool call with correct, correctly-typed arguments picked
//!   from three offered tools, then an answer that uses the result.
//! - `bug_fix`: a temp git repo whose `./check.sh` fails because of an inverted
//!   comparison one module away from the failing test; the model gets the
//!   built-in tools in an isolated per-turn worktree and passes if `check.sh`
//!   then succeeds with only the code file changed.
//! - `compaction_recall`: a long synthetic chat with five facts planted early
//!   (beyond the window); `plan_with_hysteresis` compacts, the model writes the
//!   summary with the app's default instruction, and then answers from the
//!   summary turn + kept history. Passes on 5/5 facts; reports what the summary
//!   kept as well as what the answer recalled.
//! - `web_search` (optional): with `HIVE_EVAL_MCP_URL` + `HIVE_EVAL_MCP_TOKEN`
//!   (or `HIVE_EVAL_MCP_TOKEN_FILE`; never printed), the MCP tools are offered
//!   and the model is asked for the current stable Rust version; ground truth
//!   comes from static.rust-lang.org.
//!
//! Optional knobs: `HIVE_EVAL_RUNS` (default 1), `HIVE_EVAL_SCENARIOS` (comma
//! filter), `HIVE_EVAL_WINDOW` (default 32768), `HIVE_EVAL_HISTORY_BUDGET`
//! (compaction history budget, default 3000 tokens), `HIVE_EVAL_COMMANDS=1`
//! (offer `run_command` in `bug_fix`), `HIVE_EVAL_TIMEOUT_SECS` (per scenario,
//! default 600), `HIVE_EVAL_API_KEY`.
//!
//! `pi_smoke_fixes_a_planted_bug` is a separate smoke test of Hive's Pi
//! runtime against the same endpoint (`HIVE_EVAL_PI=1`, or a path to the `pi`
//! binary; model `HIVE_EVAL_PI_MODEL`, else the first of `HIVE_EVAL_MODELS`).
//! It goes through `dispatch::stream` with provider Pi, which writes a
//! temporary `models.json` and points `PI_CODING_AGENT_DIR` at it, so the
//! user's own pi config is never read or changed.
//!
//! Results on the lab gateway (llama.cpp, 2026-10-19; gemma4:12b on GPU with a
//! 32k slot, lfm2:8b on CPU; `HIVE_EVAL_RUNS=3`, rdnamcp for web_search):
//!
//! ```text
//! scenario            gemma4:12b                 lfm2:8b
//! tool_call           3/3   2.6-4.7 s            3/3   1.6-3.3 s
//! bug_fix             0/3   ~44 s (loops)        0/3   3-5 s (gives up)
//! bug_fix +commands   2/2   22-31 s, 10 calls    0/2   4-9 s
//! compaction_recall   3/3   5/5 facts, 17-21 s   0/3   0-3/5 facts, 12 s
//! web_search          2/3   3-5 s                3/3   3-4 s
//! total (default)     8/12 in 210 s              6/12 in 66 s
//! ```
//!
//! - bug_fix without `run_command`: gemma4 found and read `inventory/stock.py`
//!   but then re-read it every round until the 24-round budget ran out (23
//!   calls, no edit), and its final tool-less round leaked a raw
//!   `<|tool_call>…` token as the reply. Offered `run_command`, it ran the
//!   check, traced the failure, made the one-line fix and re-ran the check
//!   both times. lfm2:8b ran one `search`, got nothing, and asked the user for
//!   the files.
//! - compaction_recall: gemma4's summaries kept all five facts (summary 14-18
//!   s, answer 3.4 s from the summary turn). lfm2:8b's summaries kept 1-3 and
//!   its answers invented the rest (port 5432, "ledger-v2"), consistent with
//!   using a strong model for summaries.
//! - web_search: gemma4 called the tool every time, but once answered from
//!   memory (1.86.0) over a result that said 1.99.1.
//!
//! Loop robustness (repeat guard, `tool_choice: "none"` + nudge on the final
//! round, reasoning echo, "trust tool results" prompt line), same gateway,
//! later on 2026-10-19, `HIVE_EVAL_RUNS=3`, run_command off. "Before" is the
//! harness without those changes, measured the same afternoon:
//!
//! ```text
//! scenario            gemma4:12b before / after     gpt-oss:20b before / after
//! tool_call           3/3  / 3/3                    3/3  / 3/3
//! bug_fix             2/3  / 3/3  (12-14 s)         3/3  / 3/3  (5-6 s)
//!   6 more runs       3/6  / 4/6                    -
//! compaction_recall   3/3  / 3/3                    0/3  / 0/3  (4/5 facts)
//! web_search          3/3  / 0/3 *                  3/3  / 2/3, rerun 3/3 *
//! total               11/12 in 107 s / 9/12 *       9/12 in 59 s / 8/12, 9/12 *
//! ```
//!
//! - bug_fix: gemma4's failures used to burn all 24 rounds re-reading files
//!   (23 calls) and end on leaked `<|tool_call>` markup. Now a repeated read
//!   is answered from the first result, and a run that still loops ends after
//!   the third repeat (13-15 calls) with a correct diagnosis in text.
//! - * web_search: during the "after" runs the rdnamcp search backend answered
//!   "No results." to nearly every query; the "before" build re-run at the
//!   same time also scored 0/2. gemma4 searched until the round budget ran
//!   out and then (5 of 6 runs) said it could not find the version; gpt-oss
//!   passed by fetching release pages with `fetch_url`.
//! - compaction_recall: gpt-oss's summaries dropped the fifth fact in every
//!   run, before and after; nothing here touches summaries.
//!
//! pi smoke (Hive's Pi runtime, pi 0.78.0, gemma4:12b through the gateway):
//! tool calling works. Two of two runs read the code, edited
//! `inventory/stock.py` and left `./check.sh` passing, in 81 s and 60 s; the
//! temporary config dir was removed afterwards.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hive_core::chat::{ChatMessage, MessageRole};
use hive_core::context_budget::{plan_with_hysteresis, DEFAULT_HIGH_WATERMARK, DEFAULT_LOW_WATERMARK};
use hive_core::ModelProviderKind;
use hive_runtime::agent_tools::{builtin_policy, tool_round_budget, CompositeTools, MAX_OFFERED_TOOLS};
use hive_runtime::chat_service::turns_with_summary;
use hive_runtime::git_worktree::{chat_worktree, Worktree};
use hive_runtime::provider::dispatch::{self, NoTools, ResolvedRuntime, StreamActivity, ToolSet};
use hive_runtime::provider::ChatTurn;
use hive_runtime::tool_loop::ToolExecutor;
use hive_runtime::{BuiltinTools, McpRegistry, McpServerSpec, McpTransport};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn env_num<T: std::str::FromStr>(name: &str, default: T) -> T {
    env(name).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// `http://host:8080`, `…/v1` or `…/v1/chat/completions` → the full chat URL.
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

fn models() -> Vec<String> {
    env("HIVE_EVAL_MODELS")
        .unwrap_or_else(|| "gemma4:12b".into())
        .split(',')
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect()
}

fn runtime(endpoint: &str, model: &str) -> ResolvedRuntime {
    ResolvedRuntime {
        provider: ModelProviderKind::Custom,
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        api_key: env("HIVE_EVAL_API_KEY"),
        args: vec![],
        model_provider_id: None,
        model_base_url: None,
        context_window_tokens: Some(env_num("HIVE_EVAL_WINDOW", 32_768)),
        keep_alive: None,
        think: None,
        tools: true,
        max_tool_rounds: None,
    }
}

fn wants(scenario: &str) -> bool {
    match env("HIVE_EVAL_SCENARIOS") {
        Some(list) => list.split(',').any(|s| s.trim() == scenario),
        None => true,
    }
}

// ---------------------------------------------------------------------------
// Driving a turn
// ---------------------------------------------------------------------------

/// What one scenario reports: pass/fail plus scenario-specific detail.
struct Outcome {
    pass: bool,
    detail: Value,
}

#[derive(Default)]
struct Trace {
    calls: Vec<(String, String)>,
    errors: usize,
    thinking_chars: usize,
    first_delta: Option<f64>,
}

impl Trace {
    fn names(&self) -> Vec<&str> {
        self.calls.iter().map(|(n, _)| n.as_str()).collect()
    }
}

fn system_prompt(rt: &ResolvedRuntime) -> String {
    format!(
        "You are @hive, the assistant in the user's Hive workspace.\n\n{}",
        hive_runtime::prompt::runtime_identity_block(rt, "local harness eval")
    )
}

/// One turn through the dispatcher, the way `run_prepared_turn` drives it.
async fn drive<E: ToolExecutor>(
    rt: &ResolvedRuntime,
    system: &str,
    turns: &[ChatTurn],
    tools: Option<(&[Value], &E)>,
) -> (Result<String, String>, Trace, f64) {
    let trace = Mutex::new(Trace::default());
    let start = Instant::now();
    let set = tools.map(|(defs, executor)| ToolSet {
        defs,
        executor,
        max_rounds: tool_round_budget(rt.provider, rt.max_tool_rounds),
    });
    let out = dispatch::stream_with_tools(
        rt,
        Some(system),
        turns,
        None,
        &[],
        1024,
        set,
        |_| {
            let mut t = trace.lock().unwrap();
            if t.first_delta.is_none() {
                t.first_delta = Some(start.elapsed().as_secs_f64());
            }
        },
        |act| {
            let mut t = trace.lock().unwrap();
            match act {
                StreamActivity::Tool { name, input_json, .. } => {
                    eprintln!("    -> {name} {}", input_json.chars().take(200).collect::<String>());
                    t.calls.push((name, input_json));
                }
                StreamActivity::ToolResult { is_error, content, .. } => {
                    eprintln!(
                        "    <- {}{}",
                        if is_error { "ERROR " } else { "" },
                        content.chars().take(160).collect::<String>().replace('\n', " | ")
                    );
                    t.errors += usize::from(is_error);
                }
                StreamActivity::Thinking { text } => t.thinking_chars += text.len(),
            }
        },
    )
    .await
    .map_err(|e| e.to_string());
    (out, trace.into_inner().unwrap(), start.elapsed().as_secs_f64())
}

fn round1(secs: f64) -> f64 {
    (secs * 10.0).round() / 10.0
}

fn excerpt(text: &Result<String, String>, n: usize) -> Value {
    match text {
        Ok(s) => json!(s.chars().take(n).collect::<String>()),
        Err(e) => json!(format!("ERROR: {e}")),
    }
}

// ---------------------------------------------------------------------------
// (a) Single tool call with correct arguments
// ---------------------------------------------------------------------------

/// Records calls and answers them with canned results.
#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<(String, Value)>>,
}

impl ToolExecutor for Recorder {
    async fn call(&self, name: &str, input: &Value) -> (String, bool) {
        self.calls.lock().unwrap().push((name.to_string(), input.clone()));
        match name {
            "convert_currency" => (
                json!({"amount": 125, "from": "EUR", "to": "JPY", "rate": 171.32, "result": 21415.0}).to_string(),
                false,
            ),
            "get_weather" => (json!({"city": input["city"], "forecast": "rain", "high_c": 14}).to_string(), false),
            "search_flights" => (json!({"flights": []}).to_string(), false),
            _ => (format!("unknown tool {name}"), true),
        }
    }
}

fn tool_call_defs() -> Vec<Value> {
    vec![
        json!({
            "name": "get_weather",
            "description": "Current weather forecast for a city.",
            "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}
        }),
        json!({
            "name": "convert_currency",
            "description": "Convert an amount between currencies at today's exchange rate.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "amount": {"type": "number", "description": "Amount in the source currency"},
                    "from": {"type": "string", "description": "ISO 4217 code of the source currency, e.g. USD"},
                    "to": {"type": "string", "description": "ISO 4217 code of the target currency"}
                },
                "required": ["amount", "from", "to"]
            }
        }),
        json!({
            "name": "search_flights",
            "description": "Search flights between two airports on a date.",
            "input_schema": {
                "type": "object",
                "properties": {"origin": {"type": "string"}, "destination": {"type": "string"}, "date": {"type": "string"}},
                "required": ["origin", "destination", "date"]
            }
        }),
    ]
}

/// The arguments are right: the number 125 (not a string), EUR → JPY.
fn currency_args_ok(args: &Value) -> bool {
    let code = |k: &str| args.get(k).and_then(Value::as_str).map(|s| s.trim().to_uppercase());
    args.get("amount").and_then(Value::as_f64) == Some(125.0)
        && code("from").as_deref() == Some("EUR")
        && code("to").as_deref() == Some("JPY")
}

/// The answer quotes the tool's result (21415, any thousands separator).
fn quotes_result(reply: &str) -> bool {
    let digits: String = reply.chars().filter(|c| !matches!(c, ',' | ' ' | '\u{a0}' | '\u{202f}' | '.')).collect();
    digits.contains("21415")
}

async fn scenario_tool_call(rt: &ResolvedRuntime) -> Outcome {
    let defs = tool_call_defs();
    let rec = Recorder::default();
    let (out, trace, secs) = drive(
        rt,
        &system_prompt(rt),
        &[ChatTurn::user("How much is 125 euros in Japanese yen at today's rate?")],
        Some((&defs, &rec)),
    )
    .await;
    let calls = rec.calls.into_inner().unwrap();
    let first_ok = calls.first().is_some_and(|(n, a)| n == "convert_currency" && currency_args_ok(a));
    let answered = out.as_deref().is_ok_and(quotes_result);
    Outcome {
        pass: out.is_ok() && first_ok && calls.len() == 1 && answered,
        detail: json!({
            "secs": round1(secs),
            "first_token": trace.first_delta.map(round1),
            "calls": calls.iter().map(|(n, a)| json!({"name": n, "args": a})).collect::<Vec<_>>(),
            "args_ok": first_ok,
            "single_call": calls.len() == 1,
            "answer_uses_result": answered,
            "thinking_chars": trace.thinking_chars,
            "reply": excerpt(&out, 240),
        }),
    }
}

// ---------------------------------------------------------------------------
// (b) Multi-step bug fix with the built-in tools
// ---------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let ok = hive_core::process_util::command("git").arg("-C").arg(dir).args(args).output().unwrap().status.success();
    assert!(ok, "git {args:?} failed");
}

/// A small package whose report test fails because `needs_reorder` (one
/// module away from the test) compares the wrong way round. `check.sh` runs
/// the tests; the tests are right.
fn planted_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hive-eval-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["inventory", "tests"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    git(&dir, &["init", "-q"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    let files: &[(&str, &str)] = &[
        ("README.md", "# inventory\n\nStock helpers. Run `./check.sh` to test.\n"),
        ("check.sh", "#!/bin/sh\ncd \"$(dirname \"$0\")\" && exec python3 -m unittest discover -s tests -t . -q\n"),
        ("inventory/__init__.py", ""),
        (
            "inventory/models.py",
            "from dataclasses import dataclass\n\n\n@dataclass\nclass Item:\n    sku: str\n    on_hand: int\n    reorder_point: int\n",
        ),
        (
            "inventory/stock.py",
            "\"\"\"Stock-level rules.\"\"\"\n\n\ndef needs_reorder(item):\n    \"\"\"True when stock has fallen to or below the item's reorder point.\"\"\"\n    return item.on_hand > item.reorder_point\n\n\ndef total_units(items):\n    return sum(i.on_hand for i in items)\n",
        ),
        (
            "inventory/report.py",
            "from .stock import needs_reorder\n\n\ndef reorder_list(items):\n    \"\"\"SKUs that need reordering, sorted.\"\"\"\n    return sorted(i.sku for i in items if needs_reorder(i))\n",
        ),
        ("tests/__init__.py", ""),
        (
            "tests/test_report.py",
            "import unittest\n\nfrom inventory.models import Item\nfrom inventory.report import reorder_list\n\n\nclass ReportTest(unittest.TestCase):\n    def test_reorder_list(self):\n        items = [Item(\"A-1\", 3, 5), Item(\"B-2\", 10, 5), Item(\"C-3\", 5, 5)]\n        self.assertEqual(reorder_list(items), [\"A-1\", \"C-3\"])\n\n\nif __name__ == \"__main__\":\n    unittest.main()\n",
        ),
    ];
    for (path, text) in files {
        std::fs::write(dir.join(path), text).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("check.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "init"]);
    dir
}

const BUG_PROMPT: &str =
    "Running `./check.sh` fails. The tests are correct; find the bug in the project's code and fix it.";

fn check_passes(dir: &Path) -> (bool, String) {
    let out = std::process::Command::new("sh").arg("check.sh").current_dir(dir).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text.trim().chars().take(300).collect())
}

async fn scenario_bug_fix(rt: &ResolvedRuntime, run: usize) -> Outcome {
    let commands = env("HIVE_EVAL_COMMANDS").as_deref() == Some("1");
    let repo = planted_repo(&format!("bug-{}-{run}", rt.model.replace([':', '/'], "_")));
    let chat = "eval-chat";
    chat_worktree(&repo, chat).unwrap();
    let wt = Worktree::create_from(&repo, chat, &format!("hive/chat/{chat}")).unwrap();
    assert!(!check_passes(&wt.path).0, "the planted bug must fail first");

    let builtin = BuiltinTools::new(builtin_policy(&wt.path, true, false, commands)).unwrap();
    let tools: CompositeTools<NoTools> = CompositeTools::new(Some(builtin), None, MAX_OFFERED_TOOLS);
    let (out, trace, secs) =
        drive(rt, &system_prompt(rt), &[ChatTurn::user(BUG_PROMPT)], Some((tools.defs(), &tools))).await;
    let (passes, check_out) = check_passes(&wt.path);
    let files = wt.changed_files().unwrap_or_default();
    let only_code = files == ["inventory/stock.py"];
    let detail = json!({
        "secs": round1(secs),
        "first_token": trace.first_delta.map(round1),
        "tool_calls": trace.calls.len(),
        "tool_errors": trace.errors,
        "calls": trace.names(),
        "run_command_offered": commands,
        "check_passes": passes,
        "check_output": check_out,
        "changed_files": files,
        "thinking_chars": trace.thinking_chars,
        "reply": excerpt(&out, 240),
    });
    let _ = wt.remove();
    let _ = std::fs::remove_dir_all(&repo);
    Outcome { pass: out.is_ok() && passes && only_code, detail }
}

// ---------------------------------------------------------------------------
// (c) Fact recall after compaction
// ---------------------------------------------------------------------------

/// Built-in summary instruction — the app's `DEFAULT_CONTEXT_SUMMARY_PROMPT`
/// (app/src/lib.rs), copied so the eval measures what users get by default.
const SUMMARY_INSTRUCTION: &str = "Summarize the conversation for context continuity: decisions, open \
questions, action items, file/code changes, and owners. Be concise (<=200 words).";

/// (planted message, accepted answer spellings — any one matches, lowercased)
const FACTS: &[(&str, &[&str])] = &[
    ("Heads up: the staging database now listens on port 6543, not the default.", &["6543"]),
    ("We agreed the release codename is Bluefinch.", &["bluefinch"]),
    ("Priya Raman owns the billing migration end to end.", &["priya"]),
    ("The new ledger code ships behind the feature flag enable_split_ledger.", &["enable_split_ledger", "split_ledger"]),
    ("Hard deadline for the release is March 14.", &["march 14", "14 march", "mar 14", "march 14th", "14th of march", "03-14", "3/14"]),
];

const RECALL_QUESTION: &str = "Quick check before I write the handoff doc. From our conversation: \
(1) which port does the staging database listen on, (2) what is the release codename, (3) who \
owns the billing migration, (4) what is the feature flag for the new ledger code called, and \
(5) what is the release deadline? Answer as a numbered list.";

/// Plausible engineering chatter that carries none of the facts.
fn filler(i: usize) -> String {
    const TOPICS: &[&str] = &[
        "the logging cleanup in the ingest service",
        "flaky integration tests on the CI runners",
        "renaming the config loader's environment variables",
        "the dashboard's slow first paint",
        "pagination in the admin API",
        "dependency upgrades for the web client",
        "retry behaviour in the webhook dispatcher",
        "the onboarding email copy",
        "cache invalidation for user avatars",
        "the lint rules the team disagrees on",
    ];
    let topic = TOPICS[i % TOPICS.len()];
    format!(
        "On {topic}: I went through it again today. The current approach mostly works, but the edge \
cases around empty inputs and partial failures still need a closer look, and the error messages \
are vague enough that support keeps escalating them. I'd rather tidy the obvious parts now and \
leave a short note for whoever picks it up next than start a big rewrite this week. (note {i})"
    )
}

fn synthetic_history() -> (Vec<ChatMessage>, Vec<uuid::Uuid>) {
    let mut history = Vec::new();
    let mut fact_ids = Vec::new();
    let user = |b: String| ChatMessage::new(MessageRole::User, "Dana", b);
    let hive = |b: String| ChatMessage::new(MessageRole::Assistant, "Hive", b);
    history.push(user("Kicking off release planning; I'll drop the key facts as we go.".into()));
    history.push(hive("Sounds good — I'll keep track.".into()));
    for (i, (fact, _)) in FACTS.iter().enumerate() {
        let m = user(fact.to_string());
        fact_ids.push(m.id);
        history.push(m);
        history.push(hive(format!("Noted. {}", filler(1000 + i))));
    }
    for i in 0..44 {
        history.push(user(filler(i)));
        history.push(hive(format!("Agreed. {}", filler(500 + i))));
    }
    history.push(user(RECALL_QUESTION.into()));
    (history, fact_ids)
}

fn recalled(text: &str) -> Vec<bool> {
    let t = text.to_lowercase();
    FACTS.iter().map(|(_, keys)| keys.iter().any(|k| t.contains(k))).collect()
}

async fn scenario_compaction_recall(rt: &ResolvedRuntime) -> Outcome {
    let (history, fact_ids) = synthetic_history();
    let budget: i64 = env_num("HIVE_EVAL_HISTORY_BUDGET", 3000);
    let plan = plan_with_hysteresis(&history, budget, None, DEFAULT_HIGH_WATERMARK, DEFAULT_LOW_WATERMARK);
    assert!(plan.moved, "the synthetic history must overflow the budget");
    assert!(
        fact_ids.iter().all(|id| plan.plan.overflow.iter().any(|m| m.id == *id)),
        "every planted fact must be beyond the window"
    );

    // The summary, as `windowed_context`/`summarize` write it.
    let mut body = String::new();
    for m in &plan.plan.overflow {
        body.push_str(&format!("{}: {}\n", m.author, m.body));
    }
    let start = Instant::now();
    let summary = dispatch::stream(rt, Some(SUMMARY_INSTRUCTION), &[ChatTurn::user(body)], None, &[], 512, |_| {}, |_| {})
        .await
        .map_err(|e| e.to_string());
    let summary_secs = start.elapsed().as_secs_f64();
    let Ok(summary) = summary else {
        return Outcome {
            pass: false,
            detail: json!({"stage": "summary", "error": summary.err(), "summary_secs": round1(summary_secs)}),
        };
    };

    // Then the turn: system prompt unchanged, summary as the first turn.
    let turns = turns_with_summary(&plan.plan.kept, None, "Hive", Some(&summary));
    let (out, trace, answer_secs) = drive::<NoTools>(rt, &system_prompt(rt), &turns, None).await;
    let in_summary = recalled(&summary);
    let in_answer = out.as_deref().map(recalled).unwrap_or_else(|_| vec![false; FACTS.len()]);
    let score = in_answer.iter().filter(|b| **b).count();
    Outcome {
        pass: out.is_ok() && score == FACTS.len(),
        detail: json!({
            "secs": round1(summary_secs + answer_secs),
            "summary_secs": round1(summary_secs),
            "answer_secs": round1(answer_secs),
            "answer_first_token": trace.first_delta.map(round1),
            "history_messages": history.len(),
            "overflow_messages": plan.plan.overflow.len(),
            "kept_messages": plan.plan.kept.len(),
            "recall": format!("{score}/{}", FACTS.len()),
            "facts_in_summary": in_summary.iter().filter(|b| **b).count(),
            "facts_in_answer": in_answer,
            "summary_chars": summary.chars().count(),
            "thinking_chars": trace.thinking_chars,
            "reply": excerpt(&out, 400),
        }),
    }
}

// ---------------------------------------------------------------------------
// (d) Web search through MCP
// ---------------------------------------------------------------------------

/// Routes `server__tool` names back to their MCP server, like the app does.
struct McpExec {
    registry: McpRegistry,
    names: HashMap<String, (String, String)>,
}

impl ToolExecutor for McpExec {
    async fn call(&self, name: &str, input: &Value) -> (String, bool) {
        match self.names.get(name) {
            Some((server, tool)) => match self.registry.call_tool(server, tool, input).await {
                Ok(text) => (text, false),
                Err(e) => (format!("tool error: {e}"), true),
            },
            None => (format!("unknown tool {name}"), true),
        }
    }
}

async fn live_mcp() -> Option<(Vec<Value>, McpExec)> {
    let url = env("HIVE_EVAL_MCP_URL")?;
    let token = env("HIVE_EVAL_MCP_TOKEN")
        .or_else(|| std::fs::read_to_string(env("HIVE_EVAL_MCP_TOKEN_FILE")?).ok())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let registry = McpRegistry::new(vec![McpServerSpec {
        id: "rdnamcp".into(),
        transport: McpTransport::Http { url },
        enabled: true,
        auth: token,
    }]);
    let mut defs = Vec::new();
    let mut names = HashMap::new();
    for (server, tool) in registry.list_all_tools().await {
        let name = format!("{server}__{}", tool.name);
        defs.push(json!({ "name": name, "description": tool.description, "input_schema": tool.input_schema }));
        names.insert(name, (server, tool.name));
    }
    (!defs.is_empty()).then_some((defs, McpExec { registry, names }))
}

/// Current stable Rust version (e.g. "1.99.1") from the release channel file.
async fn rust_stable_version() -> Option<String> {
    let text = reqwest::Client::new()
        .get("https://static.rust-lang.org/dist/channel-rust-stable.toml")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    let after = text.split("[pkg.rust]").nth(1)?;
    let line = after.lines().find(|l| l.trim_start().starts_with("version"))?;
    let quoted = line.split('"').nth(1)?;
    quoted.split_whitespace().next().map(str::to_string)
}

async fn scenario_web_search(rt: &ResolvedRuntime, mcp: &(Vec<Value>, McpExec), truth: Option<&str>) -> Outcome {
    // MCP tools alone (no built-ins), as a chat outside a workspace gets them.
    let (defs, exec) = mcp;
    let (out, trace, secs) = drive(
        rt,
        &system_prompt(rt),
        &[ChatTurn::user("What is the latest stable version of the Rust programming language? Look it up; give the exact version number.")],
        Some((defs.as_slice(), exec)),
    )
    .await;
    let used_web = trace.names().iter().any(|n| n.ends_with("__web_search") || n.ends_with("__fetch_url"));
    let reply = out.clone().unwrap_or_default();
    // Exact (1.99.1) or at least the minor line (1.99) — search snippets lag
    // point releases by a few days.
    let (exact, minor) = match truth {
        Some(v) => {
            let minor_line: String = v.split('.').take(2).collect::<Vec<_>>().join(".");
            (reply.contains(v), reply.contains(&minor_line))
        }
        None => (false, false),
    };
    Outcome {
        pass: out.is_ok() && used_web && minor,
        detail: json!({
            "secs": round1(secs),
            "first_token": trace.first_delta.map(round1),
            "truth": truth,
            "exact": exact,
            "minor_match": minor,
            "used_web_tool": used_web,
            "calls": trace.names(),
            "tool_errors": trace.errors,
            "reply": excerpt(&out, 300),
        }),
    }
}

// ---------------------------------------------------------------------------
// The suite
// ---------------------------------------------------------------------------

#[tokio::test]
async fn local_harness_eval() {
    let Some(endpoint) = env("HIVE_EVAL_ENDPOINT").map(|e| chat_url(&e)) else {
        eprintln!("HIVE_EVAL_ENDPOINT unset; skipping the local-harness eval");
        return;
    };
    let runs: usize = env_num("HIVE_EVAL_RUNS", 1usize).max(1);
    let timeout = Duration::from_secs(env_num("HIVE_EVAL_TIMEOUT_SECS", 600u64));
    let mcp = if wants("web_search") { live_mcp().await } else { None };
    if wants("web_search") && mcp.is_none() {
        eprintln!("HIVE_EVAL_MCP_URL unset or no tools listed; skipping web_search");
    }
    let truth = if mcp.is_some() { rust_stable_version().await } else { None };

    for model in models() {
        let rt = runtime(&endpoint, &model);
        let mut passed = 0usize;
        let mut total = 0usize;
        let mut secs_total = 0.0f64;
        let mut per: Vec<(String, usize, usize)> = Vec::new();
        for scenario in ["tool_call", "bug_fix", "compaction_recall", "web_search"] {
            if !wants(scenario) || (scenario == "web_search" && mcp.is_none()) {
                continue;
            }
            let mut ok = 0usize;
            for run in 0..runs {
                eprintln!("[{model}] {scenario} run {run}");
                let start = Instant::now();
                let fut = async {
                    match scenario {
                        "tool_call" => scenario_tool_call(&rt).await,
                        "bug_fix" => scenario_bug_fix(&rt, run).await,
                        "compaction_recall" => scenario_compaction_recall(&rt).await,
                        _ => scenario_web_search(&rt, mcp.as_ref().unwrap(), truth.as_deref()).await,
                    }
                };
                let outcome = tokio::time::timeout(timeout, fut).await.unwrap_or_else(|_| Outcome {
                    pass: false,
                    detail: json!({"error": format!("timed out after {}s", timeout.as_secs())}),
                });
                let secs = start.elapsed().as_secs_f64();
                secs_total += secs;
                total += 1;
                ok += usize::from(outcome.pass);
                let mut line = json!({
                    "eval": "local_harness",
                    "model": model,
                    "scenario": scenario,
                    "run": run,
                    "pass": outcome.pass,
                    "wall_secs": round1(secs),
                });
                if let (Some(obj), Value::Object(detail)) = (line.as_object_mut(), outcome.detail) {
                    obj.extend(detail);
                }
                println!("{line}");
            }
            passed += ok;
            per.push((scenario.to_string(), ok, runs));
        }
        println!(
            "{}",
            json!({
                "eval": "local_harness_summary",
                "model": model,
                "passed": passed,
                "total": total,
                "pass_rate": if total == 0 { 0.0 } else { (passed as f64 / total as f64 * 100.0).round() / 100.0 },
                "secs": round1(secs_total),
                "scenarios": per.iter().map(|(s, ok, n)| json!({"scenario": s, "passed": ok, "runs": n})).collect::<Vec<_>>(),
            })
        );
    }
}

/// Hive's Pi runtime against the same endpoint: does pi's own tool loop fix a
/// planted bug through an OpenAI-compatible gateway? See the module docs.
#[tokio::test]
async fn pi_smoke_fixes_a_planted_bug() {
    let (Some(endpoint), Some(pi)) = (env("HIVE_EVAL_ENDPOINT"), env("HIVE_EVAL_PI")) else {
        eprintln!("HIVE_EVAL_ENDPOINT or HIVE_EVAL_PI unset; skipping the pi smoke test");
        return;
    };
    let base_url = chat_url(&endpoint).trim_end_matches("/chat/completions").to_string();
    let model = env("HIVE_EVAL_PI_MODEL").unwrap_or_else(|| models().remove(0));
    let program = if pi == "1" { "pi".to_string() } else { pi };
    let rt = ResolvedRuntime {
        provider: ModelProviderKind::Pi,
        model: model.clone(),
        endpoint: program,
        api_key: None,
        args: vec![],
        model_provider_id: Some("hive-gateway".into()),
        model_base_url: Some(base_url),
        context_window_tokens: None,
        keep_alive: None,
        think: None,
        tools: false,
        max_tool_rounds: None,
    };
    let repo = planted_repo("pi");
    assert!(!check_passes(&repo).0, "the planted bug must fail first");
    let start = Instant::now();
    let out = dispatch::stream(
        &rt,
        None,
        &[ChatTurn::user(BUG_PROMPT)],
        Some(repo.to_str().unwrap()),
        &[],
        1024,
        |_| {},
        |_| {},
    )
    .await
    .map_err(|e| e.to_string());
    let secs = start.elapsed().as_secs_f64();
    let (passes, check_out) = check_passes(&repo);
    let status = hive_core::process_util::command("git")
        .arg("-C")
        .arg(&repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    let changed: Vec<String> = String::from_utf8_lossy(&status.stdout)
        .lines()
        .map(|l| l.get(3..).unwrap_or("").to_string())
        .filter(|p| !p.contains("__pycache__"))
        .collect();
    println!(
        "{}",
        json!({
            "eval": "pi_smoke",
            "model": model,
            "pass": out.is_ok() && passes,
            "secs": round1(secs),
            "check_passes": passes,
            "check_output": check_out,
            "changed_files": changed,
            "reply": excerpt(&out, 400),
        })
    );
    let _ = std::fs::remove_dir_all(&repo);
    assert!(out.is_ok(), "pi failed: {out:?}");
    assert!(passes, "pi did not fix the bug: {check_out}");
}

// ---------------------------------------------------------------------------
// Offline checks of the scoring helpers (always run)
// ---------------------------------------------------------------------------

#[test]
fn scoring_helpers() {
    assert_eq!(chat_url("http://h:8080"), "http://h:8080/v1/chat/completions");
    assert_eq!(chat_url("http://h:8080/v1/"), "http://h:8080/v1/chat/completions");
    assert_eq!(chat_url("http://h/v1/chat/completions"), "http://h/v1/chat/completions");

    assert!(currency_args_ok(&json!({"amount": 125, "from": "eur", "to": "JPY"})));
    assert!(!currency_args_ok(&json!({"amount": "125", "from": "EUR", "to": "JPY"})), "a string amount is wrong");
    assert!(!currency_args_ok(&json!({"amount": 125, "from": "JPY", "to": "EUR"})));
    assert!(quotes_result("about 21,415.00 yen") && quotes_result("21 415 JPY") && !quotes_result("21,400"));

    let r = recalled("1. 6543 2. Bluefinch 3. Priya Raman 4. `enable_split_ledger` 5. March 14");
    assert_eq!(r, vec![true; FACTS.len()]);
    assert_eq!(recalled("port 5432, flag ledger-v2").iter().filter(|b| **b).count(), 0);
}

#[test]
fn planted_facts_fall_beyond_the_default_window() {
    let (history, fact_ids) = synthetic_history();
    let plan = plan_with_hysteresis(&history, 3000, None, DEFAULT_HIGH_WATERMARK, DEFAULT_LOW_WATERMARK);
    assert!(plan.moved);
    assert!(fact_ids.iter().all(|id| plan.plan.overflow.iter().any(|m| m.id == *id)));
    assert_eq!(plan.plan.kept.last().map(|m| m.body.as_str()), Some(RECALL_QUESTION));
}

#[test]
fn planted_repo_fails_its_check_until_fixed() {
    if std::process::Command::new("python3").arg("--version").output().is_err() {
        eprintln!("python3 missing; skipping");
        return;
    }
    let repo = planted_repo("selftest");
    assert!(!check_passes(&repo).0);
    let stock = repo.join("inventory/stock.py");
    let fixed = std::fs::read_to_string(&stock).unwrap().replace("on_hand > item", "on_hand <= item");
    std::fs::write(&stock, fixed).unwrap();
    assert!(check_passes(&repo).0, "the one-line fix makes the check pass");
    let _ = std::fs::remove_dir_all(&repo);
}
