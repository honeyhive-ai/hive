//! OpenAI-compatible streaming client — covers OpenAI, OpenRouter, custom
//! gateways (llama.cpp `llama-server`, LM Studio, vLLM), and Ollama's `/v1`
//! compatibility endpoint (ported from the corresponding backends in
//! `Providers.swift`). Shares `ChatTurn` with the Anthropic client.
//!
//! Hardened for local / remote-LAN models (Ollama over Tailscale, LM Studio):
//! a connect timeout, one retry on a refused/dropped connection, an *idle*
//! timeout on the response stream (any byte resets it, so a slow-but-alive
//! model isn't killed), errors that name the host, and separation of a
//! reasoning model's `<think>…</think>` (or `reasoning` delta field) from the
//! reply so chain-of-thought never lands in the transcript.
//!
//! Tool loop ([`OpenAiClient::stream_chat_with_tools`]): the OpenAI-wire twin
//! of the native-Ollama loop. Tools go out as `[{type:"function", function:
//! {name, description, parameters}}]`; streamed `delta.tool_calls` fragments
//! are assembled by `index` (id and name arrive once, `arguments` in pieces),
//! run through a [`ToolExecutor`], and answered with one `role: "tool"`
//! message per call (`tool_call_id` matching the call). The last permitted
//! round goes out without tools so a model that keeps calling them still ends
//! on an answer. A server that rejects the `tools` field (400/422 naming
//! tools) is retried once without it and the turn finishes as plain chat.
//!
//! Context probe ([`OpenAiClient::probe_context_window`]): `llama-server`
//! reports its per-slot window at `GET <base>/props`
//! (`default_generation_settings.n_ctx`); the result is cached per endpoint +
//! model so the planner budgets against what the server actually allocated.
//!
//! The SSE delta parser (`extract_delta`), the tool-call assembler and the
//! think splitter are pure and unit-tested; the network path is exercised
//! against an in-process fake server in the `wire` tests below.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde_json::{json, Map, Value};

use super::anthropic::{ChatTurn, ProviderError};
use super::dispatch::StreamActivity;
use super::http::{self, Peer};
use super::ollama::{clip_tool_result, seed_messages};
use super::thinking::ThinkFilter;
use super::turn_idle_timeout;
use crate::tool_loop::ToolExecutor;

/// OpenAI-compatible chat client. The endpoint is the full
/// `/chat/completions` URL (so OpenRouter / Ollama / custom gateways all work).
#[derive(Debug, Clone)]
pub struct OpenAiClient {
    http: reqwest::Client,
    endpoint: String,
    /// `true` → send the key as an `api-key` header (Azure OpenAI); `false` →
    /// `Authorization: Bearer` (OpenAI / OpenRouter / Ollama / custom gateways).
    api_key_header: bool,
    /// Human label used in errors ("ollama", "openrouter", …) so a failure
    /// names the backend the user actually configured.
    provider_label: &'static str,
    /// Max silence tolerated while waiting for headers or the next stream
    /// chunk. Any activity resets it.
    idle_timeout: Duration,
    /// Reconnect attempts after a refused / dropped connection (before any
    /// response byte). One by default.
    connect_retries: u32,
    /// Extra top-level request fields merged into every body — per-model
    /// knobs a local server understands (`chat_template_kwargs`,
    /// `reasoning_effort`). Dropped for the rest of the turn if the server
    /// rejects one.
    extra_body: Map<String, Value>,
    /// The context window the tool loop guards against (tokens); 0 = unknown.
    context_window: u32,
}

/// The assembled result of a chat with tools on offer.
#[derive(Debug, Clone, Default)]
pub struct ToolChatOutcome {
    /// Reply text (reasoning excluded): the rounds' replies, blank-line
    /// separated — exactly what was streamed.
    pub text: String,
    /// Model rounds that ran (1 = answered without calling a tool).
    pub rounds: usize,
    /// The server rejected the `tools` field and the turn finished as a plain
    /// chat (a model/server without tool support).
    pub tools_unsupported: bool,
}

/// A tool call assembled from a round's stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssembledCall {
    /// The server's call id, when it assigned one.
    pub id: Option<String>,
    pub name: String,
    /// The raw `function.arguments` text (a JSON object, if the model behaved).
    pub arguments: String,
}

impl AssembledCall {
    /// The arguments as a JSON object. Empty text is `{}`; a model that
    /// double-encodes (a JSON string holding the object) is tolerated.
    /// Anything else is an error message suitable for the model.
    pub fn parsed_arguments(&self) -> Result<Value, String> {
        let raw = self.arguments.trim();
        if raw.is_empty() {
            return Ok(json!({}));
        }
        match serde_json::from_str::<Value>(raw) {
            Ok(v @ Value::Object(_)) => Ok(v),
            Ok(Value::String(inner)) => match serde_json::from_str::<Value>(&inner) {
                Ok(v @ Value::Object(_)) => Ok(v),
                _ => Err("arguments must be a JSON object".into()),
            },
            Ok(_) => Err("arguments must be a JSON object".into()),
            Err(e) => Err(format!("arguments are not valid JSON ({e})")),
        }
    }
}

/// Folds streamed `delta.tool_calls` fragments into whole calls. Fragments
/// are keyed by `index`; a server that omits it (sending each call whole) is
/// keyed by `id`, else treated as a new call when it names a function and as
/// a continuation of the last call when it doesn't.
#[derive(Debug, Default)]
pub struct ToolCallAssembler {
    calls: BTreeMap<u64, AssembledCall>,
}

impl ToolCallAssembler {
    pub fn push(&mut self, fragments: &[Value]) {
        for frag in fragments {
            let id = frag
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let function = frag.get("function");
            let name = function
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let key = match frag.get("index").and_then(Value::as_u64) {
                Some(i) => i,
                None => {
                    let by_id = id.and_then(|id| {
                        self.calls.iter().find(|(_, c)| c.id.as_deref() == Some(id)).map(|(k, _)| *k)
                    });
                    let last = self.calls.keys().next_back().copied();
                    match (by_id, last) {
                        (Some(k), _) => k,
                        (None, Some(k)) if id.is_none() && name.is_none() => k,
                        (None, Some(k)) => k + 1,
                        (None, None) => 0,
                    }
                }
            };
            let call = self.calls.entry(key).or_default();
            if call.id.is_none() {
                call.id = id.map(str::to_string);
            }
            // The name arrives once; some servers repeat it on every fragment.
            if call.name.is_empty() {
                if let Some(name) = name {
                    call.name = name.to_string();
                }
            }
            match function.and_then(|f| f.get("arguments")) {
                Some(Value::String(s)) => call.arguments.push_str(s),
                Some(Value::Null) | None => {}
                // A server that sends the arguments as an object, whole.
                Some(v) => call.arguments = v.to_string(),
            }
        }
    }

    /// Replace everything assembled so far with a complete, non-streamed list
    /// (`choices[0].message.tool_calls`), which is authoritative.
    pub fn replace(&mut self, calls: &[Value]) {
        self.calls.clear();
        self.push(calls);
    }

    /// The calls, in index order; entries that never got a name are dropped.
    pub fn finish(self) -> Vec<AssembledCall> {
        self.calls.into_values().filter(|c| !c.name.is_empty()).collect()
    }
}

/// What one SSE chunk (or a whole non-streamed completion) carries.
#[derive(Debug, Default, PartialEq)]
pub struct ChunkParts {
    pub content: String,
    pub reasoning: String,
    /// Streamed `delta.tool_calls` fragments.
    pub tool_call_deltas: Vec<Value>,
    /// A complete `message.tool_calls` list (non-streamed final chunk).
    pub whole_tool_calls: Option<Vec<Value>>,
    /// `true` when the text came from a whole `message` rather than a delta.
    pub whole_message: bool,
}

/// Parse one chunk object. An `{"error": …}` chunk is an API error (status 0:
/// the HTTP response had already succeeded).
pub fn parse_chunk(v: &Value, provider: &'static str) -> Result<ChunkParts, ProviderError> {
    if let Some(err) = v.get("error") {
        let body = err
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| err.as_str().map(str::to_string).unwrap_or_else(|| err.to_string()));
        return Err(ProviderError::Api { provider, status: 0, body });
    }
    let mut parts = ChunkParts::default();
    let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
        return Ok(parts);
    };
    let text = |o: &Value, k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    if let Some(delta) = choice.get("delta") {
        parts.content = text(delta, "content");
        parts.reasoning = text(delta, "reasoning");
        if parts.reasoning.is_empty() {
            parts.reasoning = text(delta, "reasoning_content");
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            parts.tool_call_deltas = calls.clone();
        }
    } else if let Some(msg) = choice.get("message") {
        parts.whole_message = true;
        parts.content = text(msg, "content");
        parts.reasoning = text(msg, "reasoning");
        if parts.reasoning.is_empty() {
            parts.reasoning = text(msg, "reasoning_content");
        }
        if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
            parts.whole_tool_calls = Some(calls.clone());
        }
    }
    Ok(parts)
}

/// Fallbacks carried across the rounds of one turn.
#[derive(Debug, Default)]
struct RoundState {
    tools_rejected: bool,
    /// The server refused one of the `extra_body` fields.
    extras_rejected: bool,
}

/// One round's result: the streamed text plus any tool calls the model made.
#[derive(Debug, Default)]
struct Round {
    text: String,
    calls: Vec<AssembledCall>,
}

/// Folds chunks into a round: reasoning to its sink, content through the
/// `<think>` filter to the delta sink, tool-call fragments assembled.
struct RoundAssembly {
    round: Round,
    filter: ThinkFilter,
    calls: ToolCallAssembler,
}

impl RoundAssembly {
    fn new() -> Self {
        Self { round: Round::default(), filter: ThinkFilter::new(), calls: ToolCallAssembler::default() }
    }

    fn absorb(
        &mut self,
        parts: ChunkParts,
        on_delta: &mut impl FnMut(String),
        on_thinking: &mut impl FnMut(String),
    ) {
        if !parts.reasoning.is_empty() {
            on_thinking(parts.reasoning);
        }
        // A whole message after streamed deltas repeats them; only take its
        // text when nothing was streamed.
        let take_text = !parts.whole_message || self.round.text.is_empty();
        if take_text && !parts.content.is_empty() {
            let split = self.filter.push(&parts.content);
            if !split.thinking.is_empty() {
                on_thinking(split.thinking);
            }
            if !split.content.is_empty() {
                self.round.text.push_str(&split.content);
                on_delta(split.content);
            }
        }
        self.calls.push(&parts.tool_call_deltas);
        if let Some(whole) = parts.whole_tool_calls {
            self.calls.replace(&whole);
        }
    }

    fn finish(mut self, on_delta: &mut impl FnMut(String), on_thinking: &mut impl FnMut(String)) -> Round {
        let tail = self.filter.finish();
        if !tail.thinking.is_empty() {
            on_thinking(tail.thinking);
        }
        if !tail.content.is_empty() {
            self.round.text.push_str(&tail.content);
            on_delta(tail.content);
        }
        self.round.calls = self.calls.finish();
        self.round
    }
}

/// A call ready to run: the id its result answers, the call, and its parsed
/// arguments (or why they didn't parse).
type PreparedCall<'a> = (String, &'a AssembledCall, Result<Value, String>);

/// The assistant turn to echo back after a tool round: its text and the calls
/// it made, each under the id its result will answer. `arguments` must be a
/// string on this wire; a malformed one goes back as `{}` (its error result
/// carries the raw text) so a server that re-parses history doesn't choke.
fn assistant_message(text: &str, calls: &[PreparedCall<'_>]) -> Value {
    let tool_calls: Vec<Value> = calls
        .iter()
        .map(|(id, call, args)| {
            let arguments = match args {
                Ok(v) => v.to_string(),
                Err(_) => "{}".to_string(),
            };
            json!({ "id": id, "type": "function", "function": { "name": call.name, "arguments": arguments } })
        })
        .collect();
    json!({ "role": "assistant", "content": text, "tool_calls": tool_calls })
}

/// Whether a non-2xx body is the server refusing the `tools` field (as
/// opposed to any other bad request).
fn rejects_tools(status: u16, body: &str) -> bool {
    (status == 400 || status == 422) && body.to_lowercase().contains("tool")
}

/// Whether a non-2xx body is the server refusing one of the extra fields
/// (`chat_template_kwargs`, `reasoning_effort`, …) by name.
fn rejects_extras(status: u16, body: &str, extra: &Map<String, Value>) -> bool {
    let lower = body.to_lowercase();
    (status == 400 || status == 422) && extra.keys().any(|k| lower.contains(&k.to_lowercase()))
}

/// Longest slice of a malformed argument string echoed back to the model.
const MALFORMED_ECHO_CHARS: usize = 500;

/// Process-wide cache of `/props` probes: `Some(n_ctx)` is kept for the
/// process (a server's slot size only changes on restart); a failed probe is
/// retried after [`PROBE_RETRY`] so a server that was down isn't written off.
type PropsCache = Mutex<HashMap<String, (Option<u32>, Instant)>>;

fn props_cache() -> &'static PropsCache {
    static CACHE: OnceLock<PropsCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// How long a failed `/props` probe is remembered before trying again.
const PROBE_RETRY: Duration = Duration::from_secs(600);

/// A `/props` probe is a hint, not part of the turn: bound it tightly.
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);

fn props_key(endpoint: &str, model: &str) -> String {
    format!("{}|{model}", server_base(endpoint))
}

/// The server root of a chat-completions URL: `http://h:8080/v1/chat/completions`
/// → `http://h:8080` (`/props` lives at the root on `llama-server`).
pub fn server_base(endpoint: &str) -> String {
    let mut base = endpoint.trim().trim_end_matches('/');
    for suffix in ["/chat/completions", "/v1"] {
        base = base.strip_suffix(suffix).unwrap_or(base).trim_end_matches('/');
    }
    base.to_string()
}

/// The per-slot context window out of a `llama-server` `/props` body
/// (`default_generation_settings.n_ctx`; older builds put `n_ctx` at the top).
pub fn n_ctx_from_props(v: &Value) -> Option<u32> {
    v.get("default_generation_settings")
        .and_then(|s| s.get("n_ctx"))
        .or_else(|| v.get("n_ctx"))
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
}

/// A previously probed window for `endpoint` + `model`, without probing.
pub fn cached_context_window(endpoint: &str, model: &str) -> Option<u32> {
    props_cache().lock().unwrap().get(&props_key(endpoint, model)).and_then(|(n, _)| *n)
}

/// Record a probe result without a server (dispatch tests).
#[cfg(test)]
pub(crate) fn seed_context_probe(endpoint: &str, model: &str, n_ctx: Option<u32>) {
    props_cache().lock().unwrap().insert(props_key(endpoint, model), (n_ctx, Instant::now()));
}

impl OpenAiClient {
    /// `endpoint` is the full chat-completions URL, e.g.
    /// `https://api.openai.com/v1/chat/completions` or
    /// `http://localhost:11434/v1/chat/completions`.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            http: http::client(),
            endpoint: endpoint.into(),
            api_key_header: false,
            provider_label: "openai",
            idle_timeout: turn_idle_timeout(),
            connect_retries: 1,
            extra_body: Map::new(),
            context_window: 0,
        }
    }

    /// Merge these top-level fields into every request (see `extra_body`).
    pub fn with_extra_body(mut self, extra: Map<String, Value>) -> Self {
        self.extra_body = extra;
        self
    }

    /// The window the tool loop keeps the running request under: once a round
    /// would pass [`crate::agent_tools::CONTEXT_GUARD_FRACTION`] of it, tools
    /// stop being offered and that round is the last.
    pub fn with_context_window(mut self, tokens: u32) -> Self {
        self.context_window = tokens;
        self
    }

    /// Authenticate with an `api-key` header instead of a bearer token (Azure
    /// OpenAI). The endpoint should already include `?api-version=...`.
    pub fn with_api_key_header(mut self, yes: bool) -> Self {
        self.api_key_header = yes;
        self
    }

    /// Label used in error messages (defaults to "openai").
    pub fn with_provider_label(mut self, label: &'static str) -> Self {
        self.provider_label = label;
        self
    }

    /// Override the idle timeout (default: `HIVE_TURN_IDLE_TIMEOUT_SECS`, 300s).
    pub fn with_idle_timeout(mut self, idle: Duration) -> Self {
        self.idle_timeout = idle;
        self
    }

    /// Override the reconnect attempts (default 1; 0 disables the retry).
    pub fn with_connect_retries(mut self, retries: u32) -> Self {
        self.connect_retries = retries;
        self
    }

    /// The `host[:port]` of the configured endpoint, for messages.
    pub fn host(&self) -> String {
        endpoint_host(&self.endpoint)
    }

    /// Stream a reply; reasoning (if any) is dropped. See
    /// [`Self::stream_reply_with_thinking`] to receive it.
    pub async fn stream_reply(
        &self,
        api_key: Option<&str>,
        model: &str,
        system: Option<&str>,
        turns: &[ChatTurn],
        on_delta: impl FnMut(String),
    ) -> Result<String, ProviderError> {
        self.stream_reply_with_thinking(api_key, model, system, turns, on_delta, |_| {})
            .await
    }

    /// Stream a reply, delivering reply text to `on_delta` and the model's
    /// reasoning (inline `<think>` blocks or `reasoning` deltas) to
    /// `on_thinking`. The returned body is reply text only.
    pub async fn stream_reply_with_thinking(
        &self,
        api_key: Option<&str>,
        model: &str,
        system: Option<&str>,
        turns: &[ChatTurn],
        on_delta: impl FnMut(String),
        on_thinking: impl FnMut(String),
    ) -> Result<String, ProviderError> {
        let messages = seed_messages(system, turns);
        let mut state = RoundState::default();
        let round = self
            .stream_round(api_key, model, &messages, &[], &mut state, on_delta, on_thinking)
            .await?;
        Ok(round.text)
    }

    /// Stream a chat with tools on offer: the agentic loop over the OpenAI
    /// wire. `tools` are neutral definitions (`{name, description,
    /// input_schema}`), converted to OpenAI's function shape. Each round
    /// streams its text to `on_delta`; each tool call is announced on
    /// `on_activity` ([`StreamActivity::Tool`]), run through `executor` (a call
    /// whose arguments aren't a JSON object gets an error result instead, so
    /// the model can retry), its result reported
    /// ([`StreamActivity::ToolResult`]) and appended as a `role: "tool"`
    /// message, and the next round begins. The loop ends when a round makes
    /// no tool call; the last of `max_rounds` goes out *without* tools so the
    /// model must answer in text.
    #[allow(clippy::too_many_arguments)]
    pub async fn stream_chat_with_tools<E: ToolExecutor>(
        &self,
        api_key: Option<&str>,
        model: &str,
        system: Option<&str>,
        turns: &[ChatTurn],
        tools: &[Value],
        executor: &E,
        max_rounds: usize,
        mut on_delta: impl FnMut(String),
        mut on_activity: impl FnMut(StreamActivity),
    ) -> Result<ToolChatOutcome, ProviderError> {
        // Ollama's tool shape is OpenAI's function shape.
        let defs = super::ollama::ollama_tool_defs(tools);
        let mut messages = seed_messages(system, turns);
        let mut state = RoundState::default();
        let mut out = ToolChatOutcome::default();
        let rounds = max_rounds.max(1);
        for round in 0..rounds {
            let last = round + 1 == rounds;
            // Near the window, stop offering tools: this round is the last and
            // must answer from what the earlier rounds gathered.
            let fits = crate::agent_tools::tools_fit_context(&messages, &defs, self.context_window);
            if !fits && !last && !state.tools_rejected {
                tracing::info!(target: "dispatch", host = %self.host(), model, round, window = self.context_window, "context nearly full; final round goes out without tools");
            }
            let offered: &[Value] = if last || state.tools_rejected || !fits { &[] } else { &defs };
            // Separate this round's text from the previous round's with a blank
            // line — in the stream and in the body alike, so they match.
            let mut need_sep = !out.text.is_empty();
            let r = self
                .stream_round(
                    api_key,
                    model,
                    &messages,
                    offered,
                    &mut state,
                    |t| {
                        if need_sep {
                            need_sep = false;
                            on_delta("\n\n".into());
                        }
                        on_delta(t)
                    },
                    |text| on_activity(StreamActivity::Thinking { text }),
                )
                .await?;
            out.rounds = round + 1;
            out.tools_unsupported |= state.tools_rejected;
            if !r.text.is_empty() {
                if !out.text.is_empty() {
                    out.text.push_str("\n\n");
                }
                out.text.push_str(&r.text);
            }
            // No calls — or calls the server made anyway on a tool-less round,
            // which there's no tool to answer — end the turn.
            if r.calls.is_empty() || offered.is_empty() {
                return Ok(out);
            }
            tracing::debug!(
                target: "dispatch",
                host = %self.host(),
                model,
                round,
                calls = r.calls.len(),
                "openai-wire tool round"
            );
            let calls: Vec<PreparedCall<'_>> = r
                .calls
                .iter()
                .enumerate()
                .map(|(idx, c)| {
                    let id = c.id.clone().unwrap_or_else(|| format!("call_{round}_{idx}"));
                    (id, c, c.parsed_arguments())
                })
                .collect();
            messages.push(assistant_message(&r.text, &calls));
            for (id, call, args) in &calls {
                on_activity(StreamActivity::Tool {
                    id: id.clone(),
                    name: call.name.clone(),
                    input_json: match args {
                        Ok(v) => v.to_string(),
                        Err(_) => call.arguments.clone(),
                    },
                });
                let (content, is_error) = match args {
                    Ok(v) => executor.call(&call.name, v).await,
                    Err(why) => {
                        let raw: String = call.arguments.chars().take(MALFORMED_ECHO_CHARS).collect();
                        tracing::info!(target: "dispatch", host = %self.host(), model, tool = %call.name, "malformed tool arguments: {why}");
                        (
                            format!(
                                "Error: the call to {} was not run because its {why}. You sent: {raw}\nCall it again with the arguments as a single JSON object matching the tool's schema.",
                                call.name
                            ),
                            true,
                        )
                    }
                };
                let content = clip_tool_result(content);
                on_activity(StreamActivity::ToolResult { call_id: id.clone(), is_error, content: content.clone() });
                messages.push(json!({ "role": "tool", "tool_call_id": id, "content": content }));
            }
        }
        Ok(out)
    }

    /// One streamed round over an explicit message array, with `tools`
    /// (already in OpenAI's shape) on offer. A server that rejects the tools
    /// field is retried once without it; `state` remembers that so later
    /// rounds don't send them again.
    #[allow(clippy::too_many_arguments)]
    async fn stream_round(
        &self,
        api_key: Option<&str>,
        model: &str,
        messages: &[Value],
        tools: &[Value],
        state: &mut RoundState,
        mut on_delta: impl FnMut(String),
        mut on_thinking: impl FnMut(String),
    ) -> Result<Round, ProviderError> {
        let resp = loop {
            let mut body = json!({ "model": model, "stream": true, "messages": messages });
            if !state.extras_rejected {
                for (k, v) in &self.extra_body {
                    body[k] = v.clone();
                }
            }
            if !tools.is_empty() && !state.tools_rejected {
                body["tools"] = json!(tools);
            }
            let body = serde_json::to_vec(&body)
                .map_err(|e| ProviderError::Decode(format!("encode request: {e}")))?;
            let resp = self.send_with_retry(api_key, body).await?;
            if resp.status().is_success() {
                break resp;
            }
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            if !self.extra_body.is_empty() && !state.extras_rejected && rejects_extras(status, &text, &self.extra_body) {
                tracing::warn!(
                    target: "dispatch",
                    provider = self.provider_label,
                    host = %self.host(),
                    model,
                    status,
                    "server rejected extra request fields ({}); continuing without them",
                    text.trim()
                );
                state.extras_rejected = true;
                continue;
            }
            if !tools.is_empty() && !state.tools_rejected && rejects_tools(status, &text) {
                tracing::warn!(
                    target: "dispatch",
                    provider = self.provider_label,
                    host = %self.host(),
                    model,
                    status,
                    "server rejected tool definitions ({}); continuing without tools",
                    text.trim()
                );
                state.tools_rejected = true;
                continue;
            }
            return Err(ProviderError::Api { provider: self.provider_label, status, body: text });
        };

        let mut asm = RoundAssembly::new();
        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|ct| !ct.contains("application/json"))
            .unwrap_or(true);
        if !is_sse {
            // A server that ignored `stream: true` and answered with one whole
            // completion object.
            let bytes = match tokio::time::timeout(self.idle_timeout, resp.bytes()).await {
                Ok(b) => b?,
                Err(_) => return Err(self.idle_error()),
            };
            let v: Value = serde_json::from_slice(&bytes)
                .map_err(|e| ProviderError::Decode(format!("completion: {e}")))?;
            asm.absorb(parse_chunk(&v, self.provider_label)?, &mut on_delta, &mut on_thinking);
            return Ok(asm.finish(&mut on_delta, &mut on_thinking));
        }

        let mut buffer = String::new();
        let mut stream = resp.bytes_stream();
        loop {
            let chunk = match tokio::time::timeout(self.idle_timeout, stream.next()).await {
                Ok(Some(chunk)) => chunk?,
                Ok(None) => break,
                Err(_) => return Err(self.idle_error()),
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(idx) = buffer.find('\n') {
                let line = buffer[..idx].trim_end_matches('\r').to_string();
                buffer.drain(..=idx);
                self.absorb_line(&line, &mut asm, &mut on_delta, &mut on_thinking)?;
            }
        }
        // A final line without a trailing newline.
        let rest = buffer.trim().to_string();
        if !rest.is_empty() {
            self.absorb_line(&rest, &mut asm, &mut on_delta, &mut on_thinking)?;
        }
        Ok(asm.finish(&mut on_delta, &mut on_thinking))
    }

    /// Fold one SSE line into the round. Non-data lines, `[DONE]` and
    /// unparseable payloads are skipped; an error object fails the turn.
    fn absorb_line(
        &self,
        line: &str,
        asm: &mut RoundAssembly,
        on_delta: &mut impl FnMut(String),
        on_thinking: &mut impl FnMut(String),
    ) -> Result<(), ProviderError> {
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            return Ok(());
        };
        if data.is_empty() || data == "[DONE]" {
            return Ok(());
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return Ok(());
        };
        asm.absorb(parse_chunk(&v, self.provider_label)?, on_delta, on_thinking);
        Ok(())
    }

    /// Ask a local server for its per-slot context window (`GET
    /// <base>/props`, `llama-server`), cached per endpoint + model. Any
    /// failure — no such route (a gateway, LM Studio, a hosted API), a
    /// timeout, an unexpected body — is `None`, quietly: the caller falls back
    /// to the model-name table.
    pub async fn probe_context_window(&self, api_key: Option<&str>, model: &str) -> Option<u32> {
        let key = props_key(&self.endpoint, model);
        if let Some((n, at)) = props_cache().lock().unwrap().get(&key).copied() {
            if n.is_some() || at.elapsed() < PROBE_RETRY {
                return n;
            }
        }
        let n = self.fetch_props_n_ctx(api_key, model).await;
        tracing::debug!(target: "dispatch", host = %self.host(), model, n_ctx = ?n, "server /props probe");
        props_cache().lock().unwrap().insert(key, (n, Instant::now()));
        n
    }

    async fn fetch_props_n_ctx(&self, api_key: Option<&str>, model: &str) -> Option<u32> {
        let url = format!("{}/props", server_base(&self.endpoint));
        let mut req = self.http.get(&url).query(&[("model", model)]).timeout(PROBE_TIMEOUT);
        if let Some(key) = api_key {
            req = if self.api_key_header {
                req.header("api-key", key)
            } else {
                req.header("authorization", format!("Bearer {key}"))
            };
        }
        let resp = req.send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: Value = resp.json().await.ok()?;
        n_ctx_from_props(&v)
    }

    fn request(&self, api_key: Option<&str>, body: &[u8]) -> reqwest::RequestBuilder {
        let mut req = self
            .http
            .post(&self.endpoint)
            .header("content-type", "application/json")
            .body(body.to_vec());
        if let Some(key) = api_key {
            req = if self.api_key_header {
                req.header("api-key", key)
            } else {
                req.header("authorization", format!("Bearer {key}"))
            };
        }
        req
    }

    fn peer(&self) -> Peer<'_> {
        Peer { provider: self.provider_label, url: &self.endpoint }
    }

    /// Send the request with the shared connect/idle/retry policy (see
    /// [`http::send_with_retry`]).
    async fn send_with_retry(
        &self,
        api_key: Option<&str>,
        body: Vec<u8>,
    ) -> Result<reqwest::Response, ProviderError> {
        http::send_with_retry(self.peer(), self.idle_timeout, self.connect_retries, || {
            self.request(api_key, &body)
        })
        .await
    }

    fn idle_error(&self) -> ProviderError {
        self.peer().idle_error(self.idle_timeout)
    }
}

/// `host[:port]` of a URL (scheme and path stripped); the input itself when it
/// doesn't look like a URL.
pub fn endpoint_host(endpoint: &str) -> String {
    let rest = endpoint
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(endpoint);
    let rest = rest.split('/').next().unwrap_or(rest);
    // Drop userinfo if any.
    rest.rsplit('@').next().unwrap_or(rest).to_string()
}

/// Extract incremental text from an OpenAI-style SSE `data:` line
/// (`choices[0].delta.content`). Returns `None` for `[DONE]`, role-only frames,
/// and non-data lines.
pub fn extract_delta(line: &str) -> Option<String> {
    let delta = delta_object(line)?;
    let content = delta.get("content")?.as_str()?;
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

/// Extract a reasoning delta (`choices[0].delta.reasoning` as sent by Ollama
/// and OpenRouter, or `reasoning_content` as sent by DeepSeek-style servers).
pub fn extract_reasoning_delta(line: &str) -> Option<String> {
    let delta = delta_object(line)?;
    let text = delta
        .get("reasoning")
        .or_else(|| delta.get("reasoning_content"))?
        .as_str()?;
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn delta_object(line: &str) -> Option<serde_json::Value> {
    let data = line.strip_prefix("data:")?.trim();
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    value.get("choices")?.get(0)?.get("delta").cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_delta_content() {
        let line = r#"data: {"choices":[{"delta":{"content":"Hello"},"index":0}]}"#;
        assert_eq!(extract_delta(line).as_deref(), Some("Hello"));
    }

    #[test]
    fn ignores_done_and_role_frames() {
        assert!(extract_delta("data: [DONE]").is_none());
        assert!(extract_delta(r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#).is_none());
        assert!(extract_delta("event: foo").is_none());
        assert!(extract_delta("").is_none());
    }

    #[test]
    fn reconstructs_message_from_deltas() {
        let lines = [
            r#"data: {"choices":[{"delta":{"content":"Hel"}}]}"#,
            r#"data: {"choices":[{"delta":{"content":"lo"}}]}"#,
            "data: [DONE]",
        ];
        let s: String = lines.iter().filter_map(|l| extract_delta(l)).collect();
        assert_eq!(s, "Hello");
    }

    #[test]
    fn reasoning_field_is_separate_from_content() {
        let line = r#"data: {"choices":[{"delta":{"reasoning":"hmm","content":""}}]}"#;
        assert_eq!(extract_reasoning_delta(line).as_deref(), Some("hmm"));
        assert!(extract_delta(line).is_none());
        let line = r#"data: {"choices":[{"delta":{"reasoning_content":"deep"}}]}"#;
        assert_eq!(extract_reasoning_delta(line).as_deref(), Some("deep"));
    }

    #[test]
    fn endpoint_host_strips_scheme_and_path() {
        assert_eq!(endpoint_host("http://100.64.0.5:11434/v1/chat/completions"), "100.64.0.5:11434");
        assert_eq!(endpoint_host("https://api.openai.com/v1/chat/completions"), "api.openai.com");
        assert_eq!(endpoint_host("localhost:1234"), "localhost:1234");
        assert_eq!(endpoint_host("http://u:p@host/x"), "host");
    }
}

/// Wire-level tests against an in-process fake OpenAI-compatible server.
#[cfg(test)]
mod wire {
    use super::*;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::Router;
    use std::sync::{Arc, Mutex};

    /// What the fake server saw, for assertions.
    #[derive(Default)]
    struct Seen {
        headers: Vec<HeaderMap>,
        bodies: Vec<serde_json::Value>,
    }

    #[derive(Clone)]
    struct Fake {
        seen: Arc<Mutex<Seen>>,
        /// SSE lines to send back, joined with "\n".
        reply: Arc<Vec<String>>,
        /// Stall (never finish) after the first line.
        stall: bool,
    }

    fn sse(content: &str) -> String {
        format!(r#"data: {{"choices":[{{"delta":{{"content":{}}}}}]}}"#, serde_json::json!(content))
    }

    async fn handler(State(f): State<Fake>, headers: HeaderMap, body: String) -> axum::response::Response {
        {
            let mut s = f.seen.lock().unwrap();
            s.headers.push(headers);
            s.bodies.push(serde_json::from_str(&body).unwrap_or(serde_json::Value::Null));
        }
        let lines = f.reply.clone();
        let stall = f.stall;
        let stream = async_stream(lines, stall);
        axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(stream))
            .unwrap()
    }

    fn async_stream(
        lines: Arc<Vec<String>>,
        stall: bool,
    ) -> impl futures_util::Stream<Item = Result<String, std::io::Error>> {
        futures_util::stream::unfold(0usize, move |i| {
            let lines = lines.clone();
            async move {
                if stall && i == 1 {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                if i < lines.len() {
                    Some((Ok(format!("{}\n", lines[i])), i + 1))
                } else if i == lines.len() {
                    Some((Ok("data: [DONE]\n".to_string()), i + 1))
                } else {
                    None
                }
            }
        })
    }

    async fn serve(reply: Vec<String>, stall: bool) -> (String, Arc<Mutex<Seen>>) {
        let fake = Fake { seen: Arc::new(Mutex::new(Seen::default())), reply: Arc::new(reply), stall };
        let seen = fake.seen.clone();
        let app = Router::new().route("/v1/chat/completions", post(handler)).with_state(fake);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1/chat/completions"), seen)
    }

    #[tokio::test]
    async fn sends_bearer_and_expected_body_shape() {
        let (url, seen) = serve(vec![sse("Hi")], false).await;
        let client = OpenAiClient::new(&url);
        let turns = vec![ChatTurn::user("hello")];
        let mut deltas = Vec::new();
        let out = client
            .stream_reply(Some("sk-test"), "qwen3.5", Some("be brief"), &turns, |d| deltas.push(d))
            .await
            .unwrap();
        assert_eq!(out, "Hi");
        assert_eq!(deltas, vec!["Hi".to_string()]);
        let s = seen.lock().unwrap();
        assert_eq!(s.headers[0].get("authorization").unwrap(), "Bearer sk-test");
        assert!(s.headers[0].get("api-key").is_none());
        let body = &s.bodies[0];
        assert_eq!(body["model"], "qwen3.5");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be brief");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "hello");
    }

    #[tokio::test]
    async fn azure_uses_api_key_header_and_keyless_sends_no_auth() {
        let (url, seen) = serve(vec![sse("ok")], false).await;
        OpenAiClient::new(&url)
            .with_api_key_header(true)
            .stream_reply(Some("azkey"), "m", None, &[ChatTurn::user("x")], |_| {})
            .await
            .unwrap();
        OpenAiClient::new(&url)
            .stream_reply(None, "m", None, &[ChatTurn::user("x")], |_| {})
            .await
            .unwrap();
        let s = seen.lock().unwrap();
        assert_eq!(s.headers[0].get("api-key").unwrap(), "azkey");
        assert!(s.headers[0].get("authorization").is_none());
        assert!(s.headers[1].get("authorization").is_none());
        assert!(s.headers[1].get("api-key").is_none());
        // No system message when none is given.
        assert_eq!(s.bodies[1]["messages"][0]["role"], "user");
    }

    #[tokio::test]
    async fn think_blocks_go_to_the_thinking_sink_not_the_reply() {
        let (url, _) = serve(
            vec![sse("<think>let me "), sse("plan</think>\n\n"), sse("Answer"), sse(" here")],
            false,
        )
        .await;
        let mut deltas = Vec::new();
        let mut thinking = Vec::new();
        let out = OpenAiClient::new(&url)
            .stream_reply_with_thinking(
                None,
                "qwen3.5",
                None,
                &[ChatTurn::user("x")],
                |d| deltas.push(d),
                |t| thinking.push(t),
            )
            .await
            .unwrap();
        assert_eq!(out, "Answer here");
        assert_eq!(deltas.concat(), "Answer here");
        assert_eq!(thinking.concat(), "let me plan");
    }

    #[tokio::test]
    async fn reasoning_field_goes_to_the_thinking_sink() {
        let reasoning = r#"data: {"choices":[{"delta":{"reasoning":"step 1","content":""}}]}"#.to_string();
        let (url, _) = serve(vec![reasoning, sse("Done")], false).await;
        let mut thinking = Vec::new();
        let out = OpenAiClient::new(&url)
            .stream_reply_with_thinking(None, "m", None, &[ChatTurn::user("x")], |_| {}, |t| thinking.push(t))
            .await
            .unwrap();
        assert_eq!(out, "Done");
        assert_eq!(thinking, vec!["step 1".to_string()]);
    }

    #[tokio::test]
    async fn stalled_stream_trips_the_idle_timeout_and_names_the_host() {
        let (url, _) = serve(vec![sse("partial"), sse("never")], true).await;
        let client = OpenAiClient::new(&url)
            .with_provider_label("ollama")
            .with_idle_timeout(Duration::from_millis(300));
        let err = client
            .stream_reply(None, "m", None, &[ChatTurn::user("x")], |_| {})
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, ProviderError::Idle { .. }), "{msg}");
        assert!(msg.contains("ollama"), "{msg}");
        assert!(msg.contains(&client.host()), "{msg}");
        assert!(msg.contains("HIVE_TURN_IDLE_TIMEOUT_SECS"), "{msg}");
    }

    #[tokio::test]
    async fn refused_connection_is_retried_then_reported_with_host() {
        // Bind then drop a listener to get a port nothing is listening on.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        drop(l);
        let url = format!("http://{addr}/v1/chat/completions");
        let client = OpenAiClient::new(&url).with_provider_label("ollama");
        let start = std::time::Instant::now();
        let err = client
            .stream_reply(None, "m", None, &[ChatTurn::user("x")], |_| {})
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, ProviderError::Unreachable { .. }), "{msg}");
        assert!(msg.contains(&addr.to_string()), "{msg}");
        assert!(msg.contains("Tailscale"), "{msg}");
        // One retry happened (the retry delay elapsed).
        assert!(start.elapsed() >= http::RETRY_DELAY, "no retry delay observed");
    }

    #[tokio::test]
    async fn non_2xx_is_an_api_error_labelled_with_the_provider() {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|| async { (axum::http::StatusCode::NOT_FOUND, "model 'x' not found") }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let err = OpenAiClient::new(format!("http://{addr}/v1/chat/completions"))
            .with_provider_label("ollama")
            .stream_reply(None, "x", None, &[ChatTurn::user("x")], |_| {})
            .await
            .unwrap_err();
        match err {
            ProviderError::Api { provider, status, body } => {
                assert_eq!(provider, "ollama");
                assert_eq!(status, 404);
                assert!(body.contains("not found"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

/// Pure tests for the tool-call assembler, chunk parser and props helpers.
#[cfg(test)]
mod tool_parts {
    use super::*;

    #[test]
    fn assembles_streamed_fragments_by_index() {
        let mut a = ToolCallAssembler::default();
        // llama.cpp / OpenAI shape: id + name first, arguments in pieces, two
        // calls interleaved.
        a.push(&[json!({"index": 0, "id": "call_a", "type": "function", "function": {"name": "search", "arguments": ""}})]);
        a.push(&[json!({"index": 0, "function": {"arguments": "{\"q\":"}})]);
        a.push(&[json!({"index": 1, "id": "call_b", "function": {"name": "fetch", "arguments": "{\"url\""}})]);
        a.push(&[json!({"index": 0, "function": {"arguments": "\"rust\"}"}})]);
        a.push(&[json!({"index": 1, "function": {"arguments": ":\"x\"}"}})]);
        let calls = a.finish();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], AssembledCall { id: Some("call_a".into()), name: "search".into(), arguments: r#"{"q":"rust"}"#.into() });
        assert_eq!(calls[1].name, "fetch");
        assert_eq!(calls[1].parsed_arguments().unwrap(), json!({"url": "x"}));
    }

    #[test]
    fn tolerates_missing_index_repeated_names_and_object_arguments() {
        let mut a = ToolCallAssembler::default();
        // Whole calls without an index (some gateways) — each is a new call.
        a.push(&[json!({"id": "1", "function": {"name": "a", "arguments": {"x": 1}}})]);
        a.push(&[json!({"id": "2", "function": {"name": "b", "arguments": "{}"}})]);
        // A nameless, id-less fragment continues the last call.
        a.push(&[json!({"function": {"arguments": ""}})]);
        // A repeated name on a later fragment doesn't double it.
        a.push(&[json!({"index": 1, "function": {"name": "b"}})]);
        let calls = a.finish();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].parsed_arguments().unwrap(), json!({"x": 1}));
        assert_eq!(calls[1].name, "b");
        // Fragments that never named a function are dropped.
        let mut a = ToolCallAssembler::default();
        a.push(&[json!({"index": 0, "function": {"arguments": "{}"}})]);
        assert!(a.finish().is_empty());
    }

    #[test]
    fn a_whole_message_replaces_streamed_fragments() {
        let mut a = ToolCallAssembler::default();
        a.push(&[json!({"index": 0, "id": "c", "function": {"name": "search", "arguments": "{\"q\":\"x\"}"}})]);
        a.replace(&[json!({"id": "c", "type": "function", "function": {"name": "search", "arguments": "{\"q\":\"x\"}"}})]);
        let calls = a.finish();
        assert_eq!(calls.len(), 1, "no duplicate from the final full message");
        assert_eq!(calls[0].arguments, r#"{"q":"x"}"#);
    }

    #[test]
    fn argument_parsing_accepts_objects_and_rejects_the_rest() {
        let c = |s: &str| AssembledCall { id: None, name: "t".into(), arguments: s.into() };
        assert_eq!(c("").parsed_arguments().unwrap(), json!({}));
        assert_eq!(c(" {\"a\":1} ").parsed_arguments().unwrap(), json!({"a": 1}));
        // Double-encoded.
        assert_eq!(c(r#""{\"a\":1}""#).parsed_arguments().unwrap(), json!({"a": 1}));
        assert!(c("{\"a\":").parsed_arguments().unwrap_err().contains("not valid JSON"));
        assert!(c("[1,2]").parsed_arguments().unwrap_err().contains("JSON object"));
        assert!(c("42").parsed_arguments().is_err());
    }

    #[test]
    fn parses_delta_message_and_error_chunks() {
        let p = parse_chunk(&json!({"choices": [{"delta": {"content": "hi", "reasoning_content": "hm"}}]}), "endpoint").unwrap();
        assert_eq!((p.content.as_str(), p.reasoning.as_str(), p.whole_message), ("hi", "hm", false));
        let p = parse_chunk(
            &json!({"choices": [{"message": {"content": "", "tool_calls": [{"id": "x", "function": {"name": "f", "arguments": "{}"}}]}}]}),
            "endpoint",
        )
        .unwrap();
        assert!(p.whole_message);
        assert_eq!(p.whole_tool_calls.unwrap().len(), 1);
        let err = parse_chunk(&json!({"error": {"message": "context overflow"}}), "endpoint").unwrap_err();
        assert!(matches!(err, ProviderError::Api { status: 0, ref body, .. } if body == "context overflow"), "{err:?}");
        // A usage-only / empty-choices chunk carries nothing.
        assert_eq!(parse_chunk(&json!({"choices": [], "usage": {}}), "endpoint").unwrap(), ChunkParts::default());
    }

    #[test]
    fn rejects_tools_only_on_400_or_422_naming_tools() {
        assert!(rejects_tools(400, r#"{"error":{"message":"tools param requires --jinja flag"}}"#));
        assert!(rejects_tools(422, "Tool calling is not supported for this model"));
        assert!(!rejects_tools(400, "model not found"));
        assert!(!rejects_tools(500, "tool crashed"));
    }

    #[test]
    fn server_base_and_props_parsing() {
        assert_eq!(server_base("http://100.100.10.20:8080/v1/chat/completions"), "http://100.100.10.20:8080");
        assert_eq!(server_base("http://h:8081/v1/"), "http://h:8081");
        assert_eq!(server_base("http://h:8081/chat/completions"), "http://h:8081");
        assert_eq!(server_base("http://h:8081"), "http://h:8081");
        assert_eq!(n_ctx_from_props(&json!({"default_generation_settings": {"n_ctx": 32768}})), Some(32_768));
        assert_eq!(n_ctx_from_props(&json!({"n_ctx": 8192})), Some(8192));
        assert_eq!(n_ctx_from_props(&json!({"default_generation_settings": {"n_ctx": 0}})), None);
        assert_eq!(n_ctx_from_props(&json!({"model": "x"})), None);
    }
}

/// Wire tests for the OpenAI-wire tool loop and the `/props` probe against an
/// in-process fake server that answers each request from a script.
#[cfg(test)]
mod tool_wire {
    use super::*;
    use axum::body::Body;
    use axum::extract::State;
    use axum::routing::{get, post};
    use axum::Router;
    use std::sync::Arc;

    #[derive(Clone, Default)]
    struct Fake {
        bodies: Arc<Mutex<Vec<Value>>>,
        /// One response per request, in order (the last repeats). Each is SSE
        /// lines, or a whole JSON body when it is a single line starting `{`.
        rounds: Arc<Vec<Vec<String>>>,
        /// 400 any request that carries `tools`.
        reject_tools: bool,
        /// 400 any request that carries `chat_template_kwargs`.
        reject_extras: bool,
        /// `/props` body; `None` ⇒ 404 (a gateway without the route).
        props: Option<Value>,
        props_hits: Arc<Mutex<usize>>,
    }

    fn delta(d: Value) -> String {
        format!("data: {}", json!({"choices": [{"index": 0, "delta": d}]}))
    }

    fn text(s: &str) -> String {
        delta(json!({"content": s}))
    }

    /// A call streamed llama.cpp-style: header fragment, then the arguments
    /// split in two.
    fn streamed_call(index: u64, id: &str, name: &str, args: &str) -> Vec<String> {
        let (a, b) = args.split_at(args.len() / 2);
        vec![
            delta(json!({"tool_calls": [{"index": index, "id": id, "type": "function", "function": {"name": name, "arguments": ""}}]})),
            delta(json!({"tool_calls": [{"index": index, "function": {"arguments": a}}]})),
            delta(json!({"tool_calls": [{"index": index, "function": {"arguments": b}}]})),
        ]
    }

    async fn chat(State(f): State<Fake>, body: String) -> axum::response::Response {
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let n = {
            let mut b = f.bodies.lock().unwrap();
            b.push(v.clone());
            b.len() - 1
        };
        if f.reject_extras && v.get("chat_template_kwargs").is_some() {
            return axum::response::Response::builder()
                .status(400)
                .body(Body::from(r#"{"error":{"code":400,"message":"Unrecognized request argument supplied: chat_template_kwargs"}}"#))
                .unwrap();
        }
        if f.reject_tools && v.get("tools").is_some() {
            return axum::response::Response::builder()
                .status(400)
                .body(Body::from(r#"{"error":{"code":400,"message":"tools param requires --jinja flag","type":"invalid_request_error"}}"#))
                .unwrap();
        }
        let lines = f.rounds.get(n).or_else(|| f.rounds.last()).cloned().unwrap_or_default();
        if lines.len() == 1 && lines[0].starts_with('{') {
            return axum::response::Response::builder()
                .header("content-type", "application/json")
                .body(Body::from(lines[0].clone()))
                .unwrap();
        }
        let mut out: String = lines.iter().map(|l| format!("{l}\n\n")).collect();
        out.push_str("data: [DONE]\n\n");
        axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(out))
            .unwrap()
    }

    async fn props(State(f): State<Fake>) -> axum::response::Response {
        *f.props_hits.lock().unwrap() += 1;
        match f.props {
            Some(p) => axum::response::Response::builder()
                .header("content-type", "application/json")
                .body(Body::from(p.to_string()))
                .unwrap(),
            None => axum::response::Response::builder().status(404).body(Body::from("File Not Found")).unwrap(),
        }
    }

    async fn serve(f: Fake) -> String {
        let app = Router::new()
            .route("/v1/chat/completions", post(chat))
            .route("/props", get(props))
            .with_state(f);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/v1/chat/completions")
    }

    struct Recorder {
        calls: Mutex<Vec<(String, Value)>>,
    }
    impl ToolExecutor for Recorder {
        async fn call(&self, name: &str, input: &Value) -> (String, bool) {
            self.calls.lock().unwrap().push((name.to_string(), input.clone()));
            (format!("result for {name}: 42"), false)
        }
    }
    fn recorder() -> Recorder {
        Recorder { calls: Mutex::new(vec![]) }
    }

    fn search_tool() -> Vec<Value> {
        vec![json!({
            "name": "search",
            "description": "Search the index",
            "input_schema": {"type": "object", "properties": {"q": {"type": "string"}}, "required": ["q"]}
        })]
    }

    #[tokio::test]
    async fn streamed_tool_call_round_trips_through_the_executor_and_back() {
        let mut first = vec![delta(json!({"reasoning_content": "need to search"})), text("Let me check.")];
        first.extend(streamed_call(0, "call_abc", "search", r#"{"q":"rust"}"#));
        let f = Fake { rounds: Arc::new(vec![first, vec![text("final "), text("answer")]]), ..Default::default() };
        let bodies = f.bodies.clone();
        let url = serve(f).await;
        let exec = recorder();
        let (mut deltas, mut acts) = (Vec::new(), Vec::new());
        let out = OpenAiClient::new(&url)
            .stream_chat_with_tools(
                Some("sk"),
                "gemma4:12b",
                Some("sys"),
                &[ChatTurn::user("find rust")],
                &search_tool(),
                &exec,
                4,
                |d| deltas.push(d),
                |a| acts.push(a),
            )
            .await
            .unwrap();
        assert_eq!(out.text, "Let me check.\n\nfinal answer");
        assert_eq!(deltas.concat(), out.text, "the body is exactly what was streamed");
        assert_eq!(out.rounds, 2);
        assert!(!out.tools_unsupported);
        assert_eq!(exec.calls.lock().unwrap().as_slice(), &[("search".to_string(), json!({"q": "rust"}))]);
        assert!(matches!(&acts[0], StreamActivity::Thinking { text } if text == "need to search"), "{acts:?}");
        assert!(matches!(&acts[1], StreamActivity::Tool { id, name, input_json } if id == "call_abc" && name == "search" && input_json == r#"{"q":"rust"}"#), "{acts:?}");
        assert!(matches!(&acts[2], StreamActivity::ToolResult { call_id, is_error: false, content } if call_id == "call_abc" && content == "result for search: 42"), "{acts:?}");
        assert_eq!(acts.len(), 3);

        let b = bodies.lock().unwrap();
        assert_eq!(b.len(), 2);
        let tools = &b[0]["tools"];
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "search");
        assert_eq!(tools[0]["function"]["parameters"]["required"][0], "q");
        assert_eq!(b[0]["stream"], true);
        let msgs = b[1]["messages"].as_array().unwrap();
        let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["system", "user", "assistant", "tool"]);
        assert_eq!(msgs[2]["content"], "Let me check.");
        assert_eq!(msgs[2]["tool_calls"][0]["id"], "call_abc");
        assert_eq!(msgs[2]["tool_calls"][0]["type"], "function");
        assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "search");
        // Arguments go back as a JSON *string* on this wire.
        assert_eq!(msgs[2]["tool_calls"][0]["function"]["arguments"], r#"{"q":"rust"}"#);
        assert_eq!(msgs[3]["tool_call_id"], "call_abc");
        assert_eq!(msgs[3]["content"], "result for search: 42");
        assert!(b[1].get("tools").is_some(), "tools stay on offer until the last round");
    }

    #[tokio::test]
    async fn multi_round_with_parallel_calls_and_synthesized_ids() {
        // Round 1: two calls in one turn, no ids from the server.
        let r1 = vec![
            delta(json!({"tool_calls": [{"index": 0, "function": {"name": "search", "arguments": "{\"q\":\"a\"}"}}]})),
            delta(json!({"tool_calls": [{"index": 1, "function": {"name": "search", "arguments": "{\"q\":\"b\"}"}}]})),
        ];
        // Round 2: one more call. Round 3: the answer.
        let r2 = streamed_call(0, "srv_1", "search", r#"{"q":"c"}"#);
        let r3 = vec![text("done")];
        let f = Fake { rounds: Arc::new(vec![r1, r2, r3]), ..Default::default() };
        let bodies = f.bodies.clone();
        let url = serve(f).await;
        let exec = recorder();
        let mut acts = Vec::new();
        let out = OpenAiClient::new(&url)
            .stream_chat_with_tools(None, "m", None, &[ChatTurn::user("go")], &search_tool(), &exec, 6, |_| {}, |a| acts.push(a))
            .await
            .unwrap();
        assert_eq!(out.text, "done");
        assert_eq!(out.rounds, 3);
        let qs: Vec<Value> = exec.calls.lock().unwrap().iter().map(|(_, v)| v["q"].clone()).collect();
        assert_eq!(qs, [json!("a"), json!("b"), json!("c")]);
        let ids: Vec<String> = acts
            .iter()
            .filter_map(|a| match a {
                StreamActivity::Tool { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["call_0_0", "call_0_1", "srv_1"]);
        let b = bodies.lock().unwrap();
        let msgs = b[2]["messages"].as_array().unwrap();
        let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "assistant", "tool", "tool", "assistant", "tool"]);
        // Synthesized ids are used consistently in the call and its answer.
        assert_eq!(msgs[1]["tool_calls"][1]["id"], "call_0_1");
        assert_eq!(msgs[3]["tool_call_id"], "call_0_1");
        assert_eq!(msgs[5]["tool_call_id"], "srv_1");
    }

    #[tokio::test]
    async fn last_round_goes_out_without_tools_so_a_looping_model_still_answers() {
        let f = Fake {
            rounds: Arc::new(vec![
                streamed_call(0, "a", "search", r#"{"q":"1"}"#),
                streamed_call(0, "b", "search", r#"{"q":"2"}"#),
                vec![text("giving up: 42")],
            ]),
            ..Default::default()
        };
        let bodies = f.bodies.clone();
        let url = serve(f).await;
        let exec = recorder();
        let out = OpenAiClient::new(&url)
            .stream_chat_with_tools(None, "m", None, &[ChatTurn::user("go")], &search_tool(), &exec, 3, |_| {}, |_| {})
            .await
            .unwrap();
        assert_eq!(out.text, "giving up: 42");
        assert_eq!(exec.calls.lock().unwrap().len(), 2);
        let b = bodies.lock().unwrap();
        assert_eq!(b.len(), 3);
        assert!(b[0].get("tools").is_some());
        assert!(b[1].get("tools").is_some());
        assert!(b[2].get("tools").is_none(), "the final round forces a text answer");
    }

    fn extras() -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("chat_template_kwargs".into(), json!({"enable_thinking": false}));
        m.insert("reasoning_effort".into(), json!("low"));
        m
    }

    #[tokio::test]
    async fn extra_fields_ride_every_round() {
        let f = Fake {
            rounds: Arc::new(vec![streamed_call(0, "a", "search", r#"{"q":"1"}"#), vec![text("ok")]]),
            ..Default::default()
        };
        let bodies = f.bodies.clone();
        let url = serve(f).await;
        let exec = recorder();
        let out = OpenAiClient::new(&url)
            .with_extra_body(extras())
            .stream_chat_with_tools(None, "m", None, &[ChatTurn::user("go")], &search_tool(), &exec, 4, |_| {}, |_| {})
            .await
            .unwrap();
        assert_eq!(out.text, "ok");
        let b = bodies.lock().unwrap();
        assert_eq!(b.len(), 2);
        for body in b.iter() {
            assert_eq!(body["chat_template_kwargs"], json!({"enable_thinking": false}));
            assert_eq!(body["reasoning_effort"], "low");
            assert!(body.get("max_tokens").is_none(), "no output cap on the OpenAI wire");
        }
    }

    #[tokio::test]
    async fn rejected_extra_fields_are_dropped_for_the_rest_of_the_turn() {
        let f = Fake {
            // Request 0 is the rejected one; its script slot is never served.
            rounds: Arc::new(vec![vec![], streamed_call(0, "a", "search", r#"{"q":"1"}"#), vec![text("ok")]]),
            reject_extras: true,
            ..Default::default()
        };
        let bodies = f.bodies.clone();
        let url = serve(f).await;
        let exec = recorder();
        let out = OpenAiClient::new(&url)
            .with_extra_body(extras())
            .stream_chat_with_tools(None, "m", None, &[ChatTurn::user("go")], &search_tool(), &exec, 4, |_| {}, |_| {})
            .await
            .unwrap();
        assert_eq!(out.text, "ok");
        assert_eq!(exec.calls.lock().unwrap().len(), 1);
        let b = bodies.lock().unwrap();
        // Rejected once, retried without, and never sent again.
        assert_eq!(b.len(), 3);
        assert!(b[0].get("chat_template_kwargs").is_some());
        assert!(b[1].get("chat_template_kwargs").is_none() && b[1].get("reasoning_effort").is_none());
        assert!(b[1].get("tools").is_some(), "tools survive an extras rejection");
        assert!(b[2].get("chat_template_kwargs").is_none());
    }

    /// An executor whose results are large — fills the window fast.
    struct Bulky;
    impl ToolExecutor for Bulky {
        async fn call(&self, _name: &str, _input: &Value) -> (String, bool) {
            ("x".repeat(12_000), false)
        }
    }

    #[tokio::test]
    async fn tools_stop_being_offered_when_the_context_nears_the_window() {
        let f = Fake {
            rounds: Arc::new(vec![streamed_call(0, "a", "search", r#"{"q":"1"}"#), vec![text("answer from what I have")]]),
            ..Default::default()
        };
        let bodies = f.bodies.clone();
        let url = serve(f).await;
        let out = OpenAiClient::new(&url)
            .with_context_window(4096)
            .stream_chat_with_tools(None, "m", None, &[ChatTurn::user("go")], &search_tool(), &Bulky, 24, |_| {}, |_| {})
            .await
            .unwrap();
        assert_eq!(out.text, "answer from what I have");
        assert_eq!(out.rounds, 2, "the guard made round 2 the last");
        let b = bodies.lock().unwrap();
        assert!(b[0].get("tools").is_some());
        assert!(b[1].get("tools").is_none(), "near the window the round goes out tool-less");
    }

    #[tokio::test]
    async fn calls_on_a_tool_less_final_round_are_not_executed() {
        // A server that emits a call even with no tools offered.
        let f = Fake { rounds: Arc::new(vec![streamed_call(0, "a", "search", r#"{"q":"1"}"#)]), ..Default::default() };
        let url = serve(f).await;
        let exec = recorder();
        let out = OpenAiClient::new(&url)
            .stream_chat_with_tools(None, "m", None, &[ChatTurn::user("go")], &search_tool(), &exec, 1, |_| {}, |_| {})
            .await
            .unwrap();
        assert_eq!(out.rounds, 1);
        assert!(exec.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn tools_rejected_by_the_server_fall_back_to_a_plain_chat() {
        let f = Fake { rounds: Arc::new(vec![vec![text("plain")]]), reject_tools: true, ..Default::default() };
        let bodies = f.bodies.clone();
        let url = serve(f).await;
        let exec = recorder();
        let out = OpenAiClient::new(&url)
            .stream_chat_with_tools(None, "m", None, &[ChatTurn::user("go")], &search_tool(), &exec, 4, |_| {}, |_| {})
            .await
            .unwrap();
        assert_eq!(out.text, "plain");
        assert!(out.tools_unsupported);
        assert!(exec.calls.lock().unwrap().is_empty());
        let b = bodies.lock().unwrap();
        assert_eq!(b.len(), 2, "exactly one retry");
        assert!(b[0].get("tools").is_some());
        assert!(b[1].get("tools").is_none());
    }

    #[tokio::test]
    async fn other_400s_are_not_retried() {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|| async { (axum::http::StatusCode::BAD_REQUEST, "model 'x' not found") }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let err = OpenAiClient::new(format!("http://{addr}/v1/chat/completions"))
            .with_provider_label("endpoint")
            .stream_chat_with_tools(None, "x", None, &[ChatTurn::user("x")], &search_tool(), &recorder(), 4, |_| {}, |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::Api { provider: "endpoint", status: 400, .. }), "{err:?}");
    }

    #[tokio::test]
    async fn malformed_arguments_get_an_error_result_not_a_crash() {
        let f = Fake {
            rounds: Arc::new(vec![
                vec![delta(json!({"tool_calls": [{"index": 0, "id": "bad", "function": {"name": "search", "arguments": "{\"q\": \"unterminated"}}]}))],
                vec![text("sorry")],
            ]),
            ..Default::default()
        };
        let bodies = f.bodies.clone();
        let url = serve(f).await;
        let exec = recorder();
        let mut acts = Vec::new();
        let out = OpenAiClient::new(&url)
            .stream_chat_with_tools(None, "m", None, &[ChatTurn::user("go")], &search_tool(), &exec, 4, |_| {}, |a| acts.push(a))
            .await
            .unwrap();
        assert_eq!(out.text, "sorry");
        assert!(exec.calls.lock().unwrap().is_empty(), "the executor never sees bad arguments");
        assert!(matches!(&acts[1], StreamActivity::ToolResult { call_id, is_error: true, content } if call_id == "bad" && content.contains("not valid JSON")), "{acts:?}");
        let b = bodies.lock().unwrap();
        let msgs = b[1]["messages"].as_array().unwrap();
        // History stays parseable: the echoed call carries `{}`, the result the raw text.
        assert_eq!(msgs[1]["tool_calls"][0]["function"]["arguments"], "{}");
        assert_eq!(msgs[2]["tool_call_id"], "bad");
        assert!(msgs[2]["content"].as_str().unwrap().contains("unterminated"));
    }

    #[tokio::test]
    async fn tool_calls_in_a_final_whole_message_chunk_are_used_once() {
        // Streamed fragments, then a final chunk with the complete message (as
        // some servers send) — one call, not two.
        let mut r1 = streamed_call(0, "c1", "search", r#"{"q":"z"}"#);
        r1.push(format!(
            "data: {}",
            json!({"choices": [{"index": 0, "finish_reason": "tool_calls", "message": {"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "search", "arguments": "{\"q\":\"z\"}"}}]}}]})
        ));
        let f = Fake { rounds: Arc::new(vec![r1, vec![text("ok")]]), ..Default::default() };
        let url = serve(f).await;
        let exec = recorder();
        OpenAiClient::new(&url)
            .stream_chat_with_tools(None, "m", None, &[ChatTurn::user("go")], &search_tool(), &exec, 4, |_| {}, |_| {})
            .await
            .unwrap();
        assert_eq!(exec.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn non_streamed_json_completion_with_tool_calls_is_handled() {
        let whole = json!({"choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
            "role": "assistant", "content": "",
            "tool_calls": [{"id": "w1", "type": "function", "function": {"name": "search", "arguments": "{\"q\":\"w\"}"}}]
        }}]})
        .to_string();
        let answer = json!({"choices": [{"index": 0, "message": {"role": "assistant", "content": "<think>hm</think>whole answer"}}]}).to_string();
        let f = Fake { rounds: Arc::new(vec![vec![whole], vec![answer]]), ..Default::default() };
        let url = serve(f).await;
        let exec = recorder();
        let mut thinking = String::new();
        let out = OpenAiClient::new(&url)
            .stream_chat_with_tools(None, "m", None, &[ChatTurn::user("go")], &search_tool(), &exec, 4, |_| {}, |a| {
                if let StreamActivity::Thinking { text } = a {
                    thinking.push_str(&text)
                }
            })
            .await
            .unwrap();
        assert_eq!(out.text, "whole answer");
        assert_eq!(thinking, "hm");
        assert_eq!(exec.calls.lock().unwrap().as_slice(), &[("search".to_string(), json!({"q": "w"}))]);
    }

    #[tokio::test]
    async fn props_probe_reads_n_ctx_and_caches_it() {
        let f = Fake { props: Some(json!({"default_generation_settings": {"n_ctx": 32768}, "total_slots": 1})), ..Default::default() };
        let hits = f.props_hits.clone();
        let url = serve(f).await;
        let client = OpenAiClient::new(&url);
        assert_eq!(cached_context_window(&url, "gemma4:12b"), None);
        assert_eq!(client.probe_context_window(None, "gemma4:12b").await, Some(32_768));
        assert_eq!(client.probe_context_window(None, "gemma4:12b").await, Some(32_768));
        assert_eq!(*hits.lock().unwrap(), 1, "second probe served from the cache");
        assert_eq!(cached_context_window(&url, "gemma4:12b"), Some(32_768));
    }

    #[tokio::test]
    async fn props_probe_fails_quietly_and_is_not_repeated_every_turn() {
        // A gateway without /props.
        let f = Fake::default();
        let hits = f.props_hits.clone();
        let url = serve(f).await;
        let client = OpenAiClient::new(&url);
        assert_eq!(client.probe_context_window(None, "m").await, None);
        assert_eq!(client.probe_context_window(None, "m").await, None);
        assert_eq!(*hits.lock().unwrap(), 1, "a failed probe is remembered for a while");
        assert_eq!(cached_context_window(&url, "m"), None);
        // An unreachable server is also just `None`.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        drop(l);
        let dead = OpenAiClient::new(format!("http://{addr}/v1/chat/completions"));
        assert_eq!(dead.probe_context_window(None, "m").await, None);
    }
}
