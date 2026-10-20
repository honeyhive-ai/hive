//! Agentic tool-call loop — the MCP follow-up (#124). Runs the
//! model→tool_use→tool_result cycle until the model stops asking for tools,
//! then returns the final text. Ported from the multi-turn tool loop in
//! `Providers.swift` (`streamReply` tool handling).
//!
//! The loop is generic over a [`MessagesApi`] (one non-streaming model turn) and
//! a [`ToolExecutor`] (run a tool), so it's unit-tested with fakes; the live
//! wiring uses the Anthropic client + an `McpRegistry`-backed executor.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::provider::anthropic::AnthropicResponse;
use crate::provider::ProviderError;

/// How many times a local model may repeat one tool call (same name, same
/// canonical arguments, nothing changed in between) in a turn. Repeats are
/// answered from the first result; after this many the loop stops offering
/// tools and asks for the final answer. Live, gemma4:12b re-read a test file
/// or `check.sh` twice before moving on to the fix, so a tighter limit (two
/// repeats) cut off runs that would have succeeded.
pub const REPEAT_CALL_LIMIT: usize = 3;

/// Sent as a user message after the last tool result, before a round that
/// may not call tools (the round budget, the context guard or the repeat
/// guard ended the loop). Without it, a model whose question still needs a
/// tool emits the call as raw template markup (gemma4: `<|tool_call>call:…`)
/// or nothing at all (gpt-oss). Live on the lab gateway with the app's system
/// prompt, gemma4:12b's final round leaked the markup 5/5 times with no nudge
/// and 5/5 with the nudge appended to the tool result, but answered 12/12
/// with it as a trailing user message.
pub const FINAL_ROUND_NUDGE: &str = "[Tool budget for this turn is used up. Do not call any more tools: \
write your final answer now from the results above, and say what you could not check.]";

/// Longest slice of an earlier result quoted back to a repeated call.
const REPEAT_ECHO_CHARS: usize = 1_500;

/// Tools that only look things up. A successful call to any other tool may
/// have changed what a lookup returns (an edit, a command, a `remember`), so
/// it invalidates the remembered results. Matched on the tool's own name —
/// after an MCP `server__` prefix — by its leading verb.
pub fn is_lookup_tool(name: &str) -> bool {
    let bare = name.rsplit("__").next().unwrap_or(name).to_ascii_lowercase();
    const VERBS: [&str; 14] = [
        "read", "list", "search", "get", "fetch", "find", "recall", "query", "lookup", "grep", "glob", "view", "show",
        "describe",
    ];
    VERBS.iter().any(|v| bare == *v || bare.starts_with(&format!("{v}_")))
        || bare.ends_with("_search")
}

/// `value` with object keys sorted and strings trimmed, so two spellings of
/// one call compare equal.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), canonical(&map[k]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        Value::String(s) => Value::String(s.trim().to_string()),
        other => other.clone(),
    }
}

/// The identity of a call for [`RepeatGuard`]: its name and canonical arguments.
pub fn call_key(name: &str, args: &Value) -> String {
    format!("{name}\u{0}{}", canonical(args))
}

/// Catches a local model re-issuing a call it already made this turn — the
/// failure mode where a small model re-reads one file every round until the
/// budget runs out. A repeat is not run again: it gets a short result naming
/// the earlier one. A successful non-lookup call (see [`is_lookup_tool`])
/// starts a new epoch, so re-reading a file after editing it, or re-running
/// the tests after a fix, runs for real. A call that errored is run again
/// when repeated (the error may have been transient), but still counts.
#[derive(Debug)]
pub struct RepeatGuard {
    limit: usize,
    epoch: u64,
    seen: HashMap<String, SeenCall>,
    tripped: bool,
}

#[derive(Debug)]
struct SeenCall {
    epoch: u64,
    count: usize,
    result: String,
    is_error: bool,
}

impl Default for RepeatGuard {
    fn default() -> Self {
        Self::new(REPEAT_CALL_LIMIT)
    }
}

impl RepeatGuard {
    /// `limit`: repeats allowed per call (see [`REPEAT_CALL_LIMIT`]).
    pub fn new(limit: usize) -> Self {
        Self { limit: limit.max(1), epoch: 0, seen: HashMap::new(), tripped: false }
    }

    /// Before running a call: `Some(reply)` when it repeats one already
    /// answered in this epoch — send `reply` as its result and don't run it.
    /// `None`: run it, then [`Self::record`] the result.
    pub fn check(&mut self, name: &str, args: &Value) -> Option<String> {
        let epoch = self.epoch;
        let seen = self.seen.get_mut(&call_key(name, args)).filter(|s| s.epoch == epoch)?;
        seen.count += 1;
        // `count` includes the first, real call.
        let stop = seen.count > self.limit;
        self.tripped |= stop;
        if seen.is_error {
            return None;
        }
        let earlier: String = seen.result.chars().take(REPEAT_ECHO_CHARS).collect();
        let clipped = if earlier.len() < seen.result.len() { " (start shown)" } else { "" };
        let next = if stop {
            "You keep repeating this call. Stop calling tools and write your final answer now from the results you have."
        } else {
            "Don't make this call again. Take the next step with what it gave you: make the change it points \
             to, call a different tool, or answer."
        };
        Some(format!(
            "Not run again: you already called {name} with exactly these arguments in this turn, and nothing has \
             changed since. Its result{clipped} was:\n{earlier}\n\n{next}"
        ))
    }

    /// After running a call (one [`Self::check`] let through).
    pub fn record(&mut self, name: &str, args: &Value, result: &str, is_error: bool) {
        if !is_error && !is_lookup_tool(name) {
            // It may have changed what earlier lookups would return.
            self.epoch += 1;
        }
        let epoch = self.epoch;
        let entry = self
            .seen
            .entry(call_key(name, args))
            .and_modify(|s| {
                if s.epoch != epoch {
                    s.count = 0;
                }
            })
            .or_insert(SeenCall { epoch, count: 0, result: String::new(), is_error });
        entry.epoch = epoch;
        entry.count = entry.count.max(1);
        entry.result = result.to_string();
        entry.is_error = is_error;
    }

    /// One call has now been repeated more than the limit: stop offering
    /// tools and ask for the answer.
    pub fn tripped(&self) -> bool {
        self.tripped
    }
}

/// Append [`FINAL_ROUND_NUDGE`] as a user message when the conversation ends
/// on a tool result (OpenAI and Ollama shapes alike: `role: "tool"`). Nothing
/// already sent changes, so the server's cached prefix still matches.
pub fn nudge_final_round(messages: &mut Vec<Value>) {
    if messages.last().is_some_and(|m| m["role"] == "tool") {
        messages.push(json!({ "role": "user", "content": FINAL_ROUND_NUDGE }));
    }
}

/// Whether `message` is the final-round nudge.
pub fn is_final_round_nudge(message: &Value) -> bool {
    message["role"] == "user" && message["content"] == FINAL_ROUND_NUDGE
}

/// One non-streaming model turn: given the running message array + tool defs,
/// return the model's response (text and/or tool_use blocks).
#[allow(async_fn_in_trait)] // used generically, never as `dyn`
pub trait MessagesApi {
    async fn run(&self, messages: &[Value], tools: &[Value]) -> Result<AnthropicResponse, ProviderError>;
}

/// Executes a tool call, returning `(content, is_error)`.
#[allow(async_fn_in_trait)] // used generically, never as `dyn`
pub trait ToolExecutor {
    async fn call(&self, name: &str, input: &Value) -> (String, bool);
}

/// Run the tool loop. Seeds with a single user message, then alternates model
/// turns and tool executions until the model returns no tool_use (or
/// `max_iters` is hit). Returns the final assistant text.
pub async fn run_tool_loop<M: MessagesApi, E: ToolExecutor>(
    model: &M,
    executor: &E,
    initial_user: &str,
    tools: Vec<Value>,
    max_iters: usize,
) -> Result<String, ProviderError> {
    let initial = vec![json!({
        "role": "user",
        "content": [{ "type": "text", "text": initial_user }]
    })];
    run_with_messages(model, executor, initial, tools, max_iters).await
}

/// Like [`run_tool_loop`] but seeded with a full message history (Anthropic
/// message format) instead of a single user string.
pub async fn run_with_messages<M: MessagesApi, E: ToolExecutor>(
    model: &M,
    executor: &E,
    initial_messages: Vec<Value>,
    tools: Vec<Value>,
    max_iters: usize,
) -> Result<String, ProviderError> {
    let mut messages = initial_messages;
    let mut last_text = String::new();
    for _ in 0..max_iters {
        let resp = model.run(&messages, &tools).await?;
        last_text = resp.text.clone();

        if resp.tool_uses.is_empty() {
            return Ok(resp.text);
        }

        // Echo the assistant's turn (text + tool_use blocks) back into history.
        messages.push(json!({ "role": "assistant", "content": resp.content }));

        // Execute each requested tool and gather tool_result blocks.
        let mut results = Vec::with_capacity(resp.tool_uses.len());
        for tu in &resp.tool_uses {
            let (content, is_error) = executor.call(&tu.name, &tu.input).await;
            results.push(json!({
                "type": "tool_result",
                "tool_use_id": tu.id,
                "content": content,
                "is_error": is_error,
            }));
        }
        messages.push(json!({ "role": "user", "content": results }));
    }
    Ok(last_text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::anthropic::{parse_messages_response, ToolUse};
    use std::cell::RefCell;

    /// Scripted model: returns a tool_use the first call, then final text.
    struct ScriptedModel {
        calls: RefCell<usize>,
    }
    impl MessagesApi for ScriptedModel {
        async fn run(&self, messages: &[Value], tools: &[Value]) -> Result<AnthropicResponse, ProviderError> {
            let n = *self.calls.borrow();
            *self.calls.borrow_mut() += 1;
            if n == 0 {
                assert!(!tools.is_empty(), "tools should be offered");
                Ok(parse_messages_response(&json!({
                    "content": [
                        { "type": "text", "text": "calling search" },
                        { "type": "tool_use", "id": "tu_1", "name": "search", "input": { "q": "x" } }
                    ],
                    "stop_reason": "tool_use"
                })))
            } else {
                // the tool_result must have been appended before this turn
                assert!(messages.iter().any(|m| m["content"]
                    .as_array()
                    .map(|c| c.iter().any(|b| b["type"] == "tool_result"))
                    .unwrap_or(false)));
                Ok(parse_messages_response(&json!({
                    "content": [{ "type": "text", "text": "final answer" }],
                    "stop_reason": "end_turn"
                })))
            }
        }
    }

    struct FakeExecutor {
        calls: RefCell<Vec<String>>,
    }
    impl ToolExecutor for FakeExecutor {
        async fn call(&self, name: &str, _input: &Value) -> (String, bool) {
            self.calls.borrow_mut().push(name.to_string());
            ("search result: 42".into(), false)
        }
    }

    #[tokio::test]
    async fn loops_through_tool_then_returns_final_text() {
        let model = ScriptedModel { calls: RefCell::new(0) };
        let exec = FakeExecutor { calls: RefCell::new(vec![]) };
        let tools = vec![json!({ "name": "search", "description": "", "input_schema": {} })];
        let out = run_tool_loop(&model, &exec, "find x", tools, 5).await.unwrap();
        assert_eq!(out, "final answer");
        assert_eq!(exec.calls.borrow().as_slice(), &["search".to_string()]);
    }

    /// A model that always asks for a tool — must be bounded by max_iters.
    struct LoopingModel;
    impl MessagesApi for LoopingModel {
        async fn run(&self, _messages: &[Value], _tools: &[Value]) -> Result<AnthropicResponse, ProviderError> {
            Ok(AnthropicResponse {
                content: json!([{ "type": "tool_use", "id": "t", "name": "x", "input": {} }]),
                text: "still working".into(),
                tool_uses: vec![ToolUse { id: "t".into(), name: "x".into(), input: json!({}) }],
                stop_reason: Some("tool_use".into()),
            })
        }
    }

    #[test]
    fn call_keys_ignore_key_order_and_padding() {
        let a = call_key("read_file", &json!({"path": "src/a.rs", "offset": 1, "opts": {"b": 1, "a": [" x "]}}));
        let b = call_key("read_file", &json!({"opts": {"a": ["x"], "b": 1}, "offset": 1, "path": " src/a.rs"}));
        assert_eq!(a, b);
        assert_ne!(a, call_key("read_file", &json!({"path": "src/a.rs", "offset": 2, "opts": {"a": ["x"], "b": 1}})));
        assert_ne!(call_key("read_file", &json!({})), call_key("list_dir", &json!({})));
    }

    #[test]
    fn lookups_are_told_apart_from_actions() {
        for n in ["read_file", "list_dir", "search", "rdna__web_search", "rdna__fetch_url", "rdna__search_docs", "mem__recall", "get_weather"] {
            assert!(is_lookup_tool(n), "{n}");
        }
        for n in ["write_file", "edit_file", "run_command", "rdna__remember", "rdna__index_document", "deploy", "readme_writer"] {
            assert!(!is_lookup_tool(n), "{n}");
        }
    }

    #[test]
    fn a_repeated_call_is_answered_from_the_first_result_until_the_limit() {
        let mut g = RepeatGuard::new(3);
        let args = json!({"path": "inventory/stock.py"});
        assert_eq!(g.check("read_file", &args), None, "first call runs");
        g.record("read_file", &args, "1\tdef low(x): return x > 3", false);
        let second = g.check("read_file", &json!({"path": " inventory/stock.py "})).expect("a repeat");
        assert!(second.contains("already called read_file") && second.contains("def low(x)"), "{second}");
        assert!(second.contains("Take the next step") && !g.tripped());
        // A different call doesn't count against this one.
        assert_eq!(g.check("read_file", &json!({"path": "other.py"})), None);
        g.record("read_file", &json!({"path": "other.py"}), "x", false);
        let third = g.check("read_file", &args).expect("still a repeat");
        assert!(third.contains("Take the next step") && !g.tripped(), "two repeats are allowed: {third}");
        let fourth = g.check("read_file", &args).expect("still a repeat");
        assert!(fourth.contains("final answer") && g.tripped(), "the third repeat ends the loop: {fourth}");
    }

    #[test]
    fn an_action_in_between_makes_the_same_lookup_fresh_again() {
        let mut g = RepeatGuard::new(3);
        let read = json!({"path": "a.py"});
        g.check("read_file", &read);
        g.record("read_file", &read, "old", false);
        let edit = json!({"path": "a.py", "old_string": "a", "new_string": "b"});
        assert_eq!(g.check("edit_file", &edit), None);
        g.record("edit_file", &edit, "edited", false);
        assert_eq!(g.check("read_file", &read), None, "the file may have changed: read it for real");
        g.record("read_file", &read, "new", false);
        assert!(g.check("read_file", &read).unwrap().contains("new"));
        // The same edit again, with only a read since, is an action already done.
        assert!(g.check("edit_file", &edit).unwrap().contains("edited"));
        // A failed action doesn't start a new epoch: the read is still a repeat.
        let bad = json!({"path": "a.py", "old_string": "zz", "new_string": "b"});
        assert_eq!(g.check("edit_file", &bad), None);
        g.record("edit_file", &bad, "Error: old_string not found", true);
        assert!(g.check("read_file", &read).is_some());
        assert!(!g.tripped(), "two repeats of the read since the edit");
        assert!(g.check("read_file", &read).is_some());
        assert!(g.tripped(), "that was the third repeat since the edit");
    }

    #[test]
    fn errored_calls_run_again_but_still_trip_the_limit() {
        let mut g = RepeatGuard::new(3);
        let cmd = json!({"command": "./check.sh"});
        for n in 1..=4 {
            assert!(!g.tripped());
            assert_eq!(g.check("run_command", &cmd), None, "run {n} goes through");
            g.record("run_command", &cmd, "exit 1: FAILED", true);
        }
        assert!(g.tripped(), "four identical failing runs with nothing changed in between");
    }

    #[test]
    fn the_nudge_follows_the_last_tool_result_once() {
        let mut msgs = vec![json!({"role": "user", "content": "go"}), json!({"role": "tool", "content": "42"})];
        nudge_final_round(&mut msgs);
        nudge_final_round(&mut msgs);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["content"], "42", "the tool result is left as reported");
        assert!(is_final_round_nudge(&msgs[2]));
        let mut plain = vec![json!({"role": "user", "content": "go"})];
        nudge_final_round(&mut plain);
        assert_eq!(plain[0]["content"], "go", "no tool result: nothing to nudge");
    }

    #[tokio::test]
    async fn max_iters_guards_runaway_loops() {
        let exec = FakeExecutor { calls: RefCell::new(vec![]) };
        let out = run_tool_loop(&LoopingModel, &exec, "go", vec![], 3).await.unwrap();
        assert_eq!(out, "still working");
        assert_eq!(exec.calls.borrow().len(), 3, "executed once per bounded iteration");
    }
}
