//! Agentic tool-call loop — the MCP follow-up (#124). Runs the
//! model→tool_use→tool_result cycle until the model stops asking for tools,
//! then returns the final text. Ported from the multi-turn tool loop in
//! `Providers.swift` (`streamReply` tool handling).
//!
//! The loop is generic over a [`MessagesApi`] (one non-streaming model turn) and
//! a [`ToolExecutor`] (run a tool), so it's unit-tested with fakes; the live
//! wiring uses the Anthropic client + an `McpRegistry`-backed executor.

use serde_json::{json, Value};

use crate::provider::anthropic::AnthropicResponse;
use crate::provider::ProviderError;

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

    #[tokio::test]
    async fn max_iters_guards_runaway_loops() {
        let exec = FakeExecutor { calls: RefCell::new(vec![]) };
        let out = run_tool_loop(&LoopingModel, &exec, "go", vec![], 3).await.unwrap();
        assert_eq!(out, "still working");
        assert_eq!(exec.calls.borrow().len(), 3, "executed once per bounded iteration");
    }
}
