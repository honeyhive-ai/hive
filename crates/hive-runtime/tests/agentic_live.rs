//! Live end-to-end check of the agentic loop for local HTTP runtimes: the
//! dispatcher (`stream_with_tools`, provider Custom → an OpenAI-compatible
//! gateway) with the composite executor (built-in file tools + MCP tools),
//! the same pieces a chat turn wires together. Skipped unless
//! `HIVE_AGENTIC_LIVE_ENDPOINT` is set, so CI never touches the network:
//!
//! ```text
//! HIVE_AGENTIC_LIVE_ENDPOINT=http://host:8080/v1/chat/completions \
//! HIVE_AGENTIC_LIVE_MODEL=gemma4:12b HIVE_AGENTIC_LIVE_WINDOW=32768 \
//!   cargo test -p hive-runtime --test agentic_live -- --nocapture --test-threads=1
//! ```
//!
//! - `fixes_a_planted_bug_with_builtin_tools`: a temp repo with a wrong
//!   discount formula and a one-line test; the model must find, read, edit
//!   and fix it in an isolated worktree within the round budget. Set
//!   `HIVE_AGENTIC_LIVE_COMMANDS=1` to also offer `run_command`.
//! - `calls_an_mcp_tool_through_the_same_loop`: needs
//!   `HIVE_AGENTIC_LIVE_MCP_URL` (Streamable HTTP) and
//!   `HIVE_AGENTIC_LIVE_MCP_TOKEN` (bearer; never printed) or
//!   `HIVE_AGENTIC_LIVE_MCP_TOKEN_FILE` (a file holding it), plus optionally
//!   `HIVE_AGENTIC_LIVE_MCP_TOOL` (default `search_docs`) and
//!   `HIVE_AGENTIC_LIVE_MCP_COLLECTION` (default `rdna-docs`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use hive_core::ModelProviderKind;
use hive_runtime::agent_tools::{builtin_policy, tool_round_budget, CompositeTools, MAX_OFFERED_TOOLS};
use hive_runtime::git_worktree::{chat_worktree, Worktree};
use hive_runtime::provider::dispatch::{self, NoTools, ResolvedRuntime, StreamActivity, ToolSet};
use hive_runtime::provider::ChatTurn;
use hive_runtime::tool_loop::ToolExecutor;
use hive_runtime::{BuiltinTools, McpRegistry, McpServerSpec, McpTransport};
use serde_json::{json, Value};

fn live_rt() -> Option<ResolvedRuntime> {
    let raw = std::env::var("HIVE_AGENTIC_LIVE_ENDPOINT").ok()?;
    let raw = raw.trim().trim_end_matches('/');
    let endpoint = if raw.ends_with("/chat/completions") {
        raw.to_string()
    } else if raw.ends_with("/v1") {
        format!("{raw}/chat/completions")
    } else {
        format!("{raw}/v1/chat/completions")
    };
    Some(ResolvedRuntime {
        provider: ModelProviderKind::Custom,
        model: std::env::var("HIVE_AGENTIC_LIVE_MODEL").unwrap_or_else(|_| "gemma4:12b".into()),
        endpoint,
        api_key: std::env::var("HIVE_AGENTIC_LIVE_API_KEY").ok().filter(|k| !k.is_empty()),
        args: vec![],
        model_provider_id: None,
        model_base_url: None,
        context_window_tokens: std::env::var("HIVE_AGENTIC_LIVE_WINDOW").ok().and_then(|v| v.parse().ok()),
        keep_alive: None,
        think: None,
        tools: true,
        max_tool_rounds: None,
        allow_remote_file_tools: false,
    })
}

fn git(dir: &Path, args: &[&str]) {
    let ok = hive_core::process_util::command("git").arg("-C").arg(dir).args(args).output().unwrap().status.success();
    assert!(ok, "git {args:?} failed");
}

/// A repo with a planted bug: `apply_discount` multiplies by the percentage
/// instead of taking it off. The test file is right; the code is wrong.
fn planted_repo() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hive-agentic-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("shop")).unwrap();
    git(&dir, &["init", "-q"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    std::fs::write(dir.join("README.md"), "# shop\n\nPricing helpers. Run `python3 test_pricing.py` to test.\n").unwrap();
    std::fs::write(dir.join("shop/__init__.py"), "").unwrap();
    std::fs::write(
        dir.join("shop/pricing.py"),
        "\"\"\"Price helpers.\"\"\"\n\n\ndef apply_discount(price, percent):\n    \"\"\"Return price reduced by percent (0-100).\"\"\"\n    return price * percent / 100\n\n\ndef add_tax(price, rate):\n    return price * (1 + rate)\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("test_pricing.py"),
        "from shop.pricing import apply_discount\nassert apply_discount(200, 25) == 150, apply_discount(200, 25)\nprint(\"ok\")\n",
    )
    .unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "init"]);
    dir
}

fn python_test_passes(dir: &Path) -> (bool, String) {
    let out = std::process::Command::new("python3").arg("test_pricing.py").current_dir(dir).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text.trim().to_string())
}

/// Routes `server__tool` names back to their MCP server, like the app's
/// executor.
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
    let url = std::env::var("HIVE_AGENTIC_LIVE_MCP_URL").ok()?;
    let token = std::env::var("HIVE_AGENTIC_LIVE_MCP_TOKEN")
        .ok()
        .or_else(|| std::fs::read_to_string(std::env::var("HIVE_AGENTIC_LIVE_MCP_TOKEN_FILE").ok()?).ok())
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
    Some((defs, McpExec { registry, names }))
}

#[derive(Default)]
struct Trace {
    calls: Vec<(String, String)>,
    errors: usize,
    thinking_chars: usize,
    first_delta: Option<f64>,
}

/// Drive one turn through the dispatcher, the way `run_prepared_turn` does.
async fn drive<M: ToolExecutor>(rt: &ResolvedRuntime, tools: &CompositeTools<M>, user: &str) -> (Result<String, String>, Trace, f64, usize) {
    let system = format!(
        "You are @hive, a coding agent in the user's project.\n\n{}",
        hive_runtime::prompt::runtime_identity_block(rt, "agentic live test")
    );
    let rounds = tool_round_budget(rt.provider, rt.max_tool_rounds);
    let trace = Mutex::new(Trace::default());
    let start = Instant::now();
    let out = dispatch::stream_with_tools(
        rt,
        Some(&system),
        &[ChatTurn::user(user)],
        None,
        &[],
        1024,
        Some(ToolSet { defs: tools.defs(), executor: tools, max_rounds: rounds }),
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
                    eprintln!("  -> {name} {}", input_json.chars().take(200).collect::<String>());
                    t.calls.push((name, input_json));
                }
                StreamActivity::ToolResult { is_error, content, .. } => {
                    eprintln!("  <- {}{}", if is_error { "ERROR " } else { "" }, content.chars().take(160).collect::<String>().replace('\n', " | "));
                    t.errors += usize::from(is_error);
                }
                StreamActivity::Thinking { text } => t.thinking_chars += text.len(),
            }
        },
    )
    .await
    .map_err(|e| e.to_string());
    (out, trace.into_inner().unwrap(), start.elapsed().as_secs_f64(), rounds)
}

#[tokio::test]
async fn fixes_a_planted_bug_with_builtin_tools() {
    let Some(rt) = live_rt() else {
        eprintln!("HIVE_AGENTIC_LIVE_ENDPOINT unset; skipping live agentic test");
        return;
    };
    let commands = std::env::var("HIVE_AGENTIC_LIVE_COMMANDS").map(|v| v == "1").unwrap_or(false);
    let repo = planted_repo();
    let chat = "live-chat";
    chat_worktree(&repo, chat).unwrap();
    let wt = Worktree::create_from(&repo, chat, &format!("hive/chat/{chat}")).unwrap();
    assert!(!python_test_passes(&wt.path).0, "the planted bug must fail first");

    let builtin = BuiltinTools::new(builtin_policy(&wt.path, true, false, commands)).unwrap();
    let tools: CompositeTools<NoTools> = CompositeTools::new(Some(builtin), None, MAX_OFFERED_TOOLS);
    eprintln!(
        "model {} @ {} window {} tools {:?}",
        rt.model,
        rt.endpoint,
        rt.context_window(),
        tools.defs().iter().filter_map(|d| d["name"].as_str()).collect::<Vec<_>>()
    );
    let (out, trace, secs, rounds) = drive(
        &rt,
        &tools,
        "Running `python3 test_pricing.py` fails with an AssertionError. The test is correct; find the bug in the project's code and fix it.",
    )
    .await;
    let (passes, test_out) = python_test_passes(&wt.path);
    let diff = wt.diff().unwrap();
    let files = wt.changed_files().unwrap();
    let names: Vec<&str> = trace.calls.iter().map(|(n, _)| n.as_str()).collect();
    eprintln!(
        "\nRESULT passes={passes} ({test_out}) secs={secs:.1} first_token={:?} tool_calls={} errors={} round_budget={rounds} thinking_chars={} files={files:?}\ncalls={names:?}\nreply={:?}\ndiff:\n{diff}",
        trace.first_delta.map(|s| (s * 10.0).round() / 10.0),
        trace.calls.len(),
        trace.errors,
        trace.thinking_chars,
        out.as_ref().map(|s| s.chars().take(400).collect::<String>()),
    );
    assert!(out.is_ok(), "turn failed: {out:?}");
    assert!(names.contains(&"read_file"), "model should read before editing: {names:?}");
    assert!(names.iter().any(|n| *n == "edit_file" || *n == "write_file"), "model should edit: {names:?}");
    assert!(trace.calls.len() < rounds * 4, "stayed within the budget");
    assert!(passes, "the fix must make the test pass: {test_out}\n{diff}");
    assert_eq!(files, ["shop/pricing.py"], "only the code changed, not the test");
    // The main tree is untouched: the change is a reviewable proposal.
    assert!(std::fs::read_to_string(repo.join("shop/pricing.py")).unwrap().contains("price * percent / 100"));
    wt.remove().unwrap();
    let _ = std::fs::remove_dir_all(&repo);
}

#[tokio::test]
async fn calls_an_mcp_tool_through_the_same_loop() {
    let Some(rt) = live_rt() else {
        eprintln!("HIVE_AGENTIC_LIVE_ENDPOINT unset; skipping live agentic MCP test");
        return;
    };
    let Some((mcp_defs, mcp)) = live_mcp().await else {
        eprintln!("HIVE_AGENTIC_LIVE_MCP_URL unset; skipping live agentic MCP test");
        return;
    };
    assert!(!mcp_defs.is_empty(), "the MCP server listed no tools (auth?)");
    let tool = std::env::var("HIVE_AGENTIC_LIVE_MCP_TOOL").unwrap_or_else(|_| "search_docs".into());
    let collection = std::env::var("HIVE_AGENTIC_LIVE_MCP_COLLECTION").unwrap_or_else(|_| "rdna-docs".into());
    let want = format!("rdnamcp__{tool}");
    // Read-only built-ins alongside the MCP tools, as an in-place turn gets.
    let dir = std::env::temp_dir().join(format!("hive-agentic-live-mcp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let builtin = BuiltinTools::new(builtin_policy(&dir, false, false, false)).unwrap();
    let tools = CompositeTools::new(Some(builtin), Some((mcp_defs, mcp)), MAX_OFFERED_TOOLS);
    let offered: Vec<&str> = tools.defs().iter().filter_map(|d| d["name"].as_str()).collect();
    eprintln!("offered {offered:?} dropped {:?}", tools.dropped());
    assert!(offered.contains(&want.as_str()), "{want} not offered: {offered:?}");
    let (out, trace, secs, _) = drive(
        &rt,
        &tools,
        &format!(
            "Use the {want} tool on the \"{collection}\" collection to find out how ROCm exposes GPU wavefront size, then answer in two sentences citing what the docs say."
        ),
    )
    .await;
    let names: Vec<&str> = trace.calls.iter().map(|(n, _)| n.as_str()).collect();
    eprintln!(
        "\nRESULT secs={secs:.1} first_token={:?} tool_calls={} errors={} calls={names:?}\nreply={:?}",
        trace.first_delta.map(|s| (s * 10.0).round() / 10.0),
        trace.calls.len(),
        trace.errors,
        out.as_ref().map(|s| s.chars().take(600).collect::<String>()),
    );
    assert!(out.is_ok(), "turn failed: {out:?}");
    assert!(names.contains(&want.as_str()), "model never called {want}: {names:?}");
    assert!(!out.unwrap().trim().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
