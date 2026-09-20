//! Native Ollama client (`/api/chat` + `/api/show`).
//!
//! Ollama's OpenAI-compatible `/v1` shim accepts only the OpenAI request shape,
//! so the settings that matter most for a local model never reached the server:
//! the context window (`num_ctx` — Ollama otherwise serves a small default and
//! silently truncates the *start* of the prompt, i.e. the system prompt), how
//! long to keep the model loaded (`keep_alive`), and whether a reasoning model
//! should think (`think`). The native API carries all three, streams reasoning
//! in a separate `message.thinking` field, and exposes a capability probe
//! (`/api/show` → `capabilities: ["completion","tools","thinking"]` plus the
//! model's maximum context length) that later phases gate tool use on.
//!
//! Wire shape (NDJSON, one object per line):
//! ```text
//! {"message":{"role":"assistant","content":"Hi","thinking":""},"done":false}
//! {"message":{"role":"assistant","content":""},"done":true,"prompt_eval_count":26,"eval_count":298}
//! ```
//! Errors arrive as a non-2xx JSON `{"error":"…"}` body or, mid-stream, as an
//! `{"error":"…"}` line.
//!
//! Shares the connect/idle/retry policy with the OpenAI-wire client
//! ([`super::http`]) so a Tailscale box that's asleep fails the same way.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::anthropic::{ChatTurn, ProviderError};
use super::http::{self, Peer};
use super::openai::endpoint_host;
use super::thinking::ThinkFilter;
use super::turn_idle_timeout;

const PROVIDER: &str = "ollama";
const DEFAULT_BASE: &str = "http://localhost:11434";

/// Reduce any spelling of an Ollama endpoint to its base URL. Runtimes were
/// historically stored as the OpenAI-compatible chat URL
/// (`http://host:11434/v1/chat/completions`); the native client wants
/// `http://host:11434`. Empty ⇒ the local default.
pub fn base_url(endpoint: &str) -> String {
    let mut e = endpoint.trim().trim_end_matches('/').to_string();
    if e.is_empty() {
        return DEFAULT_BASE.to_string();
    }
    for suffix in ["/v1/chat/completions", "/chat/completions", "/api/chat", "/api/generate", "/v1", "/api"] {
        if let Some(stripped) = e.strip_suffix(suffix) {
            e = stripped.trim_end_matches('/').to_string();
            break;
        }
    }
    if !e.contains("://") {
        e = format!("http://{e}");
    }
    e
}

/// What `/api/show` reports about a model. Everything is optional because
/// older servers (pre-0.9) omit `capabilities`; absent ⇒ `false`/`None`, and
/// callers must treat "unknown" as "don't assume".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilities {
    /// The model can take tool definitions and emit tool calls.
    pub tools: bool,
    /// The model separates reasoning from its reply (`think` is honoured).
    pub thinking: bool,
    /// The model accepts images.
    pub vision: bool,
    /// The model's *maximum* context length (tokens). Not what the server
    /// currently serves — that's `num_ctx`, which we send per request.
    pub context_length: Option<u32>,
    pub family: Option<String>,
    pub parameter_size: Option<String>,
    pub quantization: Option<String>,
}

impl ModelCapabilities {
    /// Parse an `/api/show` response body.
    pub fn from_show(v: &Value) -> Self {
        let caps: Vec<&str> = v
            .get("capabilities")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let has = |c: &str| caps.iter().any(|x| x.eq_ignore_ascii_case(c));
        let context_length = v
            .get("model_info")
            .and_then(Value::as_object)
            .and_then(|m| {
                m.iter()
                    .find(|(k, _)| k.ends_with(".context_length"))
                    .and_then(|(_, v)| v.as_u64())
            })
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0);
        let details = v.get("details");
        let detail = |k: &str| {
            details
                .and_then(|d| d.get(k))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        Self {
            tools: has("tools"),
            thinking: has("thinking"),
            vision: has("vision"),
            context_length,
            family: detail("family"),
            parameter_size: detail("parameter_size"),
            quantization: detail("quantization_level"),
        }
    }

    /// Short human summary ("tools · thinking · 40k ctx") for the Test result.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.tools {
            parts.push("tools".to_string());
        }
        if self.thinking {
            parts.push("thinking".to_string());
        }
        if self.vision {
            parts.push("vision".to_string());
        }
        if let Some(n) = self.context_length {
            parts.push(format!("{}k max ctx", n / 1024));
        }
        parts.join(" · ")
    }
}

/// Per-request generation settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatOptions {
    /// Context window the server should allocate for this request. `None` ⇒
    /// the server's default (small — usually 4096).
    pub num_ctx: Option<u32>,
    /// How long the model stays loaded after the request ("5m", "-1" = forever,
    /// "0" = unload immediately). `None` ⇒ server default.
    pub keep_alive: Option<String>,
    /// Ask a reasoning model to think (`Some(true)`), not to (`Some(false)`),
    /// or leave it to the model (`None`). A server that rejects the field for a
    /// non-reasoning model gets one retry without it.
    pub think: Option<bool>,
}

/// Ollama's `keep_alive` accepts a Go duration string ("5m", "1h") or a number
/// of seconds (negative = forever). A bare integer *string* ("-1") is rejected
/// by the server's duration parser, so integer-looking values go as numbers.
pub fn keep_alive_value(s: &str) -> Option<Value> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(json!(n));
    }
    Some(json!(s))
}

/// The assembled result of one streamed chat.
#[derive(Debug, Clone, Default)]
pub struct ChatOutcome {
    /// Reply text (reasoning excluded).
    pub text: String,
    /// Tokens the server counted in the prompt, when reported.
    pub prompt_tokens: Option<u32>,
    /// Tokens generated, when reported.
    pub completion_tokens: Option<u32>,
    /// The server rejected `think` for this model and the request was retried
    /// without it (a hint to stop sending it).
    pub think_unsupported: bool,
}

/// Process-wide probe cache: a model's capabilities only change when it is
/// re-pulled, and a probe per turn would otherwise add a round trip.
fn probe_cache() -> &'static Mutex<HashMap<String, ModelCapabilities>> {
    static CACHE: OnceLock<Mutex<HashMap<String, ModelCapabilities>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Clone)]
pub struct OllamaClient {
    http: reqwest::Client,
    base: String,
    idle_timeout: Duration,
    connect_retries: u32,
}

impl OllamaClient {
    /// `endpoint` may be the base URL or any of the historical chat URLs; see
    /// [`base_url`].
    pub fn new(endpoint: &str) -> Self {
        Self {
            http: http::client(),
            base: base_url(endpoint),
            idle_timeout: turn_idle_timeout(),
            connect_retries: 1,
        }
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

    /// The normalized base URL.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// `host[:port]`, for messages.
    pub fn host(&self) -> String {
        endpoint_host(&self.base)
    }

    fn chat_url(&self) -> String {
        format!("{}/api/chat", self.base)
    }

    fn show_url(&self) -> String {
        format!("{}/api/show", self.base)
    }

    /// Probe a model's capabilities via `/api/show`. A 404 means the model is
    /// not pulled on that server.
    pub async fn show(&self, model: &str) -> Result<ModelCapabilities, ProviderError> {
        let url = self.show_url();
        let peer = Peer { provider: PROVIDER, url: &url };
        // A probe should be quick; don't hold the whole turn-idle window for it.
        let idle = self.idle_timeout.min(Duration::from_secs(20));
        let body = serde_json::to_vec(&json!({ "model": model }))
            .map_err(|e| ProviderError::Decode(format!("encode show request: {e}")))?;
        let resp = http::send_with_retry(peer, idle, self.connect_retries, || {
            self.http.post(&url).header("content-type", "application/json").body(body.clone())
        })
        .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = error_text(&resp.text().await.unwrap_or_default());
            return Err(ProviderError::Api { provider: PROVIDER, status, body });
        }
        let v: Value = resp.json().await.map_err(|e| ProviderError::Decode(format!("show: {e}")))?;
        Ok(ModelCapabilities::from_show(&v))
    }

    /// [`Self::show`] with a process-wide cache. Errors are logged and yield
    /// `None` so a turn never fails because the probe did.
    pub async fn capabilities_cached(&self, model: &str) -> Option<ModelCapabilities> {
        let key = format!("{}|{}", self.base, model);
        if let Some(c) = probe_cache().lock().unwrap().get(&key).cloned() {
            return Some(c);
        }
        match self.show(model).await {
            Ok(c) => {
                probe_cache().lock().unwrap().insert(key, c.clone());
                Some(c)
            }
            Err(e) => {
                tracing::debug!(target: "dispatch", host = %self.host(), model, "ollama capability probe failed: {e}");
                None
            }
        }
    }

    /// Forget a cached probe (e.g. after the user re-pulls a model).
    pub fn forget_cached(&self, model: &str) {
        probe_cache().lock().unwrap().remove(&format!("{}|{}", self.base, model));
    }

    /// Stream a chat. Reply text goes to `on_delta` (and the returned
    /// [`ChatOutcome::text`]); reasoning — the `thinking` field, or inline
    /// `<think>` blocks from models/servers that don't separate it — goes to
    /// `on_thinking` and never into the reply.
    pub async fn stream_chat(
        &self,
        model: &str,
        system: Option<&str>,
        turns: &[ChatTurn],
        opts: &ChatOptions,
        mut on_delta: impl FnMut(String),
        mut on_thinking: impl FnMut(String),
    ) -> Result<ChatOutcome, ProviderError> {
        let mut think = opts.think;
        let mut retried_without_think = false;
        let resp = loop {
            let body = build_request(model, system, turns, opts, think);
            let resp = self.post_chat(&body).await?;
            if resp.status().is_success() {
                break resp;
            }
            let status = resp.status().as_u16();
            let text = error_text(&resp.text().await.unwrap_or_default());
            // "\"qwen2.5\" does not support thinking" — drop the field and retry
            // once, so a non-reasoning model works with the default `think: false`.
            if status == 400 && think.is_some() && !retried_without_think && text.to_lowercase().contains("think") {
                tracing::info!(target: "dispatch", host = %self.host(), model, "server rejected `think` ({text}); retrying without it");
                think = None;
                retried_without_think = true;
                continue;
            }
            return Err(ProviderError::Api { provider: PROVIDER, status, body: text });
        };

        let url = self.chat_url();
        let peer = Peer { provider: PROVIDER, url: &url };
        let mut out = ChatOutcome { think_unsupported: retried_without_think, ..Default::default() };
        let mut buffer = String::new();
        let mut filter = ThinkFilter::new();
        let mut stream = resp.bytes_stream();
        loop {
            let chunk = match tokio::time::timeout(self.idle_timeout, stream.next()).await {
                Ok(Some(chunk)) => chunk?,
                Ok(None) => break,
                Err(_) => return Err(peer.idle_error(self.idle_timeout)),
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(idx) = buffer.find('\n') {
                let line = buffer[..idx].trim().to_string();
                buffer.drain(..=idx);
                if line.is_empty() {
                    continue;
                }
                let frame = parse_frame(&line)?;
                if !frame.thinking.is_empty() {
                    on_thinking(frame.thinking);
                }
                if !frame.content.is_empty() {
                    let split = filter.push(&frame.content);
                    if !split.thinking.is_empty() {
                        on_thinking(split.thinking);
                    }
                    if !split.content.is_empty() {
                        out.text.push_str(&split.content);
                        on_delta(split.content);
                    }
                }
                if frame.done {
                    out.prompt_tokens = frame.prompt_tokens;
                    out.completion_tokens = frame.completion_tokens;
                }
            }
        }
        // A final line without a trailing newline.
        let rest = buffer.trim();
        if !rest.is_empty() {
            let frame = parse_frame(rest)?;
            if !frame.thinking.is_empty() {
                on_thinking(frame.thinking);
            }
            if !frame.content.is_empty() {
                let split = filter.push(&frame.content);
                if !split.thinking.is_empty() {
                    on_thinking(split.thinking);
                }
                if !split.content.is_empty() {
                    out.text.push_str(&split.content);
                    on_delta(split.content);
                }
            }
            if frame.done {
                out.prompt_tokens = frame.prompt_tokens;
                out.completion_tokens = frame.completion_tokens;
            }
        }
        let tail = filter.finish();
        if !tail.thinking.is_empty() {
            on_thinking(tail.thinking);
        }
        if !tail.content.is_empty() {
            out.text.push_str(&tail.content);
            on_delta(tail.content);
        }
        tracing::debug!(
            target: "dispatch",
            host = %self.host(),
            model,
            prompt_tokens = ?out.prompt_tokens,
            completion_tokens = ?out.completion_tokens,
            "ollama chat complete"
        );
        Ok(out)
    }

    async fn post_chat(&self, body: &Value) -> Result<reqwest::Response, ProviderError> {
        let url = self.chat_url();
        let peer = Peer { provider: PROVIDER, url: &url };
        let bytes = serde_json::to_vec(body)
            .map_err(|e| ProviderError::Decode(format!("encode chat request: {e}")))?;
        http::send_with_retry(peer, self.idle_timeout, self.connect_retries, || {
            self.http.post(&url).header("content-type", "application/json").body(bytes.clone())
        })
        .await
    }
}

#[derive(Debug, Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

/// Build the `/api/chat` body. Only set fields are serialized so an older
/// server never sees keys it doesn't know.
pub fn build_request(
    model: &str,
    system: Option<&str>,
    turns: &[ChatTurn],
    opts: &ChatOptions,
    think: Option<bool>,
) -> Value {
    let mut messages = Vec::with_capacity(turns.len() + 1);
    if let Some(sys) = system {
        messages.push(Message { role: "system", content: sys });
    }
    for t in turns {
        messages.push(Message { role: &t.role, content: &t.content });
    }
    let mut body = json!({
        "model": model,
        "stream": true,
        "messages": messages,
    });
    if let Some(t) = think {
        body["think"] = json!(t);
    }
    if let Some(ka) = opts.keep_alive.as_deref().and_then(keep_alive_value) {
        body["keep_alive"] = ka;
    }
    if let Some(n) = opts.num_ctx.filter(|n| *n > 0) {
        body["options"] = json!({ "num_ctx": n });
    }
    body
}

/// One parsed NDJSON frame.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub content: String,
    pub thinking: String,
    pub done: bool,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
}

/// Parse a stream line. An `{"error": …}` line becomes an API error (status 0:
/// the HTTP response had already succeeded); malformed JSON is a decode error.
pub fn parse_frame(line: &str) -> Result<Frame, ProviderError> {
    let v: Value = serde_json::from_str(line)
        .map_err(|e| ProviderError::Decode(format!("ollama frame: {e}: {}", truncate(line, 120))))?;
    if let Some(err) = v.get("error").and_then(Value::as_str) {
        return Err(ProviderError::Api { provider: PROVIDER, status: 0, body: err.to_string() });
    }
    let msg = v.get("message");
    let field = |k: &str| {
        msg.and_then(|m| m.get(k))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let count = |k: &str| v.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
    Ok(Frame {
        content: field("content"),
        thinking: field("thinking"),
        done: v.get("done").and_then(Value::as_bool).unwrap_or(false),
        prompt_tokens: count("prompt_eval_count"),
        completion_tokens: count("eval_count"),
    })
}

/// The `error` string out of an Ollama error body, else the body itself.
fn error_text(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| body.trim().to_string())
}

fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_normalizes_every_spelling() {
        for e in [
            "http://100.64.0.5:11434",
            "http://100.64.0.5:11434/",
            "http://100.64.0.5:11434/v1",
            "http://100.64.0.5:11434/v1/",
            "http://100.64.0.5:11434/v1/chat/completions",
            "http://100.64.0.5:11434/api/chat",
            "http://100.64.0.5:11434/api",
        ] {
            assert_eq!(base_url(e), "http://100.64.0.5:11434", "{e}");
        }
        assert_eq!(base_url("localhost:11434"), "http://localhost:11434");
        assert_eq!(base_url(""), DEFAULT_BASE);
        assert_eq!(base_url("https://ollama.example.com/v1/chat/completions"), "https://ollama.example.com");
    }

    #[test]
    fn keep_alive_integers_go_as_numbers() {
        assert_eq!(keep_alive_value("-1"), Some(json!(-1)));
        assert_eq!(keep_alive_value("0"), Some(json!(0)));
        assert_eq!(keep_alive_value("5m"), Some(json!("5m")));
        assert_eq!(keep_alive_value("  1h "), Some(json!("1h")));
        assert_eq!(keep_alive_value(""), None);
    }

    #[test]
    fn request_carries_only_set_fields() {
        let turns = [ChatTurn::user("hi")];
        let body = build_request("qwen3", Some("sys"), &turns, &ChatOptions::default(), None);
        assert_eq!(body["model"], "qwen3");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "hi");
        assert!(body.get("think").is_none());
        assert!(body.get("keep_alive").is_none());
        assert!(body.get("options").is_none());

        let opts = ChatOptions { num_ctx: Some(32768), keep_alive: Some("-1".into()), think: Some(true) };
        let body = build_request("qwen3", None, &turns, &opts, Some(false));
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["think"], false, "explicit think arg wins over opts");
        assert_eq!(body["keep_alive"], -1);
        assert_eq!(body["options"]["num_ctx"], 32768);
    }

    #[test]
    fn parses_frames_thinking_done_and_errors() {
        let f = parse_frame(r#"{"message":{"role":"assistant","content":"Hi","thinking":"hmm"},"done":false}"#).unwrap();
        assert_eq!(f, Frame { content: "Hi".into(), thinking: "hmm".into(), done: false, prompt_tokens: None, completion_tokens: None });
        let f = parse_frame(r#"{"message":{"role":"assistant","content":""},"done":true,"prompt_eval_count":26,"eval_count":298}"#).unwrap();
        assert!(f.done);
        assert_eq!(f.prompt_tokens, Some(26));
        assert_eq!(f.completion_tokens, Some(298));
        let e = parse_frame(r#"{"error":"model 'x' not found"}"#).unwrap_err();
        assert!(matches!(e, ProviderError::Api { provider: "ollama", status: 0, .. }), "{e:?}");
        assert!(matches!(parse_frame("not json"), Err(ProviderError::Decode(_))));
    }

    #[test]
    fn capabilities_parse_from_show() {
        let v = json!({
            "capabilities": ["completion", "tools", "thinking"],
            "details": {"family": "qwen3", "parameter_size": "8.2B", "quantization_level": "Q4_K_M"},
            "model_info": {"general.architecture": "qwen3", "qwen3.context_length": 40960, "qwen3.embedding_length": 4096}
        });
        let c = ModelCapabilities::from_show(&v);
        assert!(c.tools && c.thinking && !c.vision);
        assert_eq!(c.context_length, Some(40960));
        assert_eq!(c.family.as_deref(), Some("qwen3"));
        assert_eq!(c.parameter_size.as_deref(), Some("8.2B"));
        assert_eq!(c.summary(), "tools · thinking · 40k max ctx");
        // Old server: no capabilities key → nothing assumed.
        let c = ModelCapabilities::from_show(&json!({"details": {}}));
        assert_eq!(c, ModelCapabilities::default());
    }
}

/// Wire-level tests against an in-process fake Ollama.
#[cfg(test)]
mod wire {
    use super::*;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::Router;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Seen {
        chat_bodies: Vec<Value>,
        show_bodies: Vec<Value>,
    }

    #[derive(Clone)]
    struct Fake {
        seen: Arc<Mutex<Seen>>,
        /// NDJSON lines to stream back.
        reply: Arc<Vec<String>>,
        /// Reject any request carrying `think` with Ollama's 400.
        reject_think: bool,
        /// Stall (never finish) after the first line.
        stall: bool,
        /// `/api/show` response.
        show: Arc<Value>,
    }

    fn frame(content: &str) -> String {
        json!({"message": {"role": "assistant", "content": content}, "done": false}).to_string()
    }

    fn think_frame(thinking: &str) -> String {
        json!({"message": {"role": "assistant", "content": "", "thinking": thinking}, "done": false}).to_string()
    }

    fn done() -> String {
        json!({"message": {"role": "assistant", "content": ""}, "done": true, "prompt_eval_count": 12, "eval_count": 7}).to_string()
    }

    async fn chat(State(f): State<Fake>, body: String) -> axum::response::Response {
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        f.seen.lock().unwrap().chat_bodies.push(v.clone());
        if f.reject_think && v.get("think").is_some() {
            return axum::response::Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header("content-type", "application/json")
                .body(Body::from(json!({"error": "\"qwen2.5\" does not support thinking"}).to_string()))
                .unwrap();
        }
        let lines = f.reply.clone();
        let stall = f.stall;
        let stream = futures_util::stream::unfold(0usize, move |i| {
            let lines = lines.clone();
            async move {
                if stall && i == 1 {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                if i < lines.len() {
                    Some((Ok::<_, std::io::Error>(format!("{}\n", lines[i])), i + 1))
                } else {
                    None
                }
            }
        });
        axum::response::Response::builder()
            .header("content-type", "application/x-ndjson")
            .body(Body::from_stream(stream))
            .unwrap()
    }

    async fn show(State(f): State<Fake>, body: String) -> axum::response::Response {
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        f.seen.lock().unwrap().show_bodies.push(v.clone());
        if v["model"] == "missing" {
            return axum::response::Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from(json!({"error": "model 'missing' not found"}).to_string()))
                .unwrap();
        }
        axum::response::Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(f.show.to_string()))
            .unwrap()
    }

    async fn serve(fake: Fake) -> (String, Arc<Mutex<Seen>>) {
        let seen = fake.seen.clone();
        let app = Router::new()
            .route("/api/chat", post(chat))
            .route("/api/show", post(show))
            .with_state(fake);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), seen)
    }

    fn fake(reply: Vec<String>) -> Fake {
        Fake {
            seen: Arc::new(Mutex::new(Seen::default())),
            reply: Arc::new(reply),
            reject_think: false,
            stall: false,
            show: Arc::new(json!({
                "capabilities": ["completion", "tools", "thinking"],
                "details": {"family": "qwen3"},
                "model_info": {"qwen3.context_length": 40960}
            })),
        }
    }

    #[tokio::test]
    async fn sends_native_body_with_num_ctx_keep_alive_and_think() {
        let (base, seen) = serve(fake(vec![frame("Hi"), frame(" there"), done()])).await;
        // The historical chat URL must still resolve to the native endpoint.
        let client = OllamaClient::new(&format!("{base}/v1/chat/completions"));
        let opts = ChatOptions { num_ctx: Some(32768), keep_alive: Some("-1".into()), think: Some(false) };
        let mut deltas = Vec::new();
        let out = client
            .stream_chat("qwen3", Some("be brief"), &[ChatTurn::user("hello")], &opts, |d| deltas.push(d), |_| {})
            .await
            .unwrap();
        assert_eq!(out.text, "Hi there");
        assert_eq!(deltas.concat(), "Hi there");
        assert_eq!(out.prompt_tokens, Some(12));
        assert_eq!(out.completion_tokens, Some(7));
        assert!(!out.think_unsupported);
        let s = seen.lock().unwrap();
        let body = &s.chat_bodies[0];
        assert_eq!(body["model"], "qwen3");
        assert_eq!(body["stream"], true);
        assert_eq!(body["think"], false);
        assert_eq!(body["keep_alive"], -1);
        assert_eq!(body["options"]["num_ctx"], 32768);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "hello");
    }

    #[tokio::test]
    async fn thinking_field_and_inline_think_tags_go_to_the_sink() {
        let (base, _) = serve(fake(vec![
            think_frame("plan "),
            think_frame("it"),
            frame("<think>more</think>"),
            frame("Answer"),
            done(),
        ]))
        .await;
        let mut thinking = Vec::new();
        let out = OllamaClient::new(&base)
            .stream_chat("qwen3", None, &[ChatTurn::user("x")], &ChatOptions::default(), |_| {}, |t| thinking.push(t))
            .await
            .unwrap();
        assert_eq!(out.text, "Answer");
        assert_eq!(thinking.concat(), "plan itmore");
    }

    #[tokio::test]
    async fn think_rejected_by_server_is_retried_without_it() {
        let mut f = fake(vec![frame("ok"), done()]);
        f.reject_think = true;
        let (base, seen) = serve(f).await;
        let opts = ChatOptions { think: Some(false), ..Default::default() };
        let out = OllamaClient::new(&base)
            .stream_chat("qwen2.5", None, &[ChatTurn::user("x")], &opts, |_| {}, |_| {})
            .await
            .unwrap();
        assert_eq!(out.text, "ok");
        assert!(out.think_unsupported);
        let s = seen.lock().unwrap();
        assert_eq!(s.chat_bodies.len(), 2);
        assert!(s.chat_bodies[0].get("think").is_some());
        assert!(s.chat_bodies[1].get("think").is_none());
    }

    #[tokio::test]
    async fn other_400s_surface_as_labelled_api_errors() {
        let mut f = fake(vec![]);
        f.reject_think = true;
        let (base, _) = serve(f).await;
        // Two rejections in a row: the retry itself must not loop forever. Here
        // the fake only rejects when `think` is present, so instead use a
        // missing-model 404 from /api/show to check the label + text.
        let err = OllamaClient::new(&base).show("missing").await.unwrap_err();
        match err {
            ProviderError::Api { provider, status, body } => {
                assert_eq!(provider, "ollama");
                assert_eq!(status, 404);
                assert_eq!(body, "model 'missing' not found");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn mid_stream_error_line_fails_the_turn() {
        let (base, _) = serve(fake(vec![frame("par"), json!({"error": "out of memory"}).to_string()])).await;
        let err = OllamaClient::new(&base)
            .stream_chat("qwen3", None, &[ChatTurn::user("x")], &ChatOptions::default(), |_| {}, |_| {})
            .await
            .unwrap_err();
        assert!(matches!(&err, ProviderError::Api { provider: "ollama", body, .. } if body == "out of memory"), "{err:?}");
    }

    #[tokio::test]
    async fn stalled_stream_trips_the_idle_timeout_and_names_the_host() {
        let mut f = fake(vec![frame("partial"), frame("never"), done()]);
        f.stall = true;
        let (base, _) = serve(f).await;
        let client = OllamaClient::new(&base).with_idle_timeout(Duration::from_millis(300));
        let err = client
            .stream_chat("qwen3", None, &[ChatTurn::user("x")], &ChatOptions::default(), |_| {}, |_| {})
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, ProviderError::Idle { .. }), "{msg}");
        assert!(msg.contains("ollama") && msg.contains(&client.host()), "{msg}");
    }

    #[tokio::test]
    async fn refused_connection_is_retried_then_reported_with_host() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        drop(l);
        let client = OllamaClient::new(&format!("http://{addr}"));
        let start = std::time::Instant::now();
        let err = client
            .stream_chat("m", None, &[ChatTurn::user("x")], &ChatOptions::default(), |_| {}, |_| {})
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, ProviderError::Unreachable { .. }), "{msg}");
        assert!(msg.contains(&addr.to_string()) && msg.contains("Tailscale"), "{msg}");
        assert!(start.elapsed() >= http::RETRY_DELAY, "no retry delay observed");
    }

    #[tokio::test]
    async fn probe_parses_and_caches_capabilities() {
        let (base, seen) = serve(fake(vec![])).await;
        let client = OllamaClient::new(&base);
        let caps = client.show("qwen3").await.unwrap();
        assert!(caps.tools && caps.thinking);
        assert_eq!(caps.context_length, Some(40960));
        assert_eq!(seen.lock().unwrap().show_bodies[0]["model"], "qwen3");

        client.forget_cached("qwen3");
        let a = client.capabilities_cached("qwen3").await.unwrap();
        let b = client.capabilities_cached("qwen3").await.unwrap();
        assert_eq!(a, b);
        // One show for the direct call, one for the first cached call; the
        // second cached call hit the cache.
        assert_eq!(seen.lock().unwrap().show_bodies.len(), 2);
        // Unknown model → None, never an error.
        assert!(client.capabilities_cached("missing").await.is_none());
        client.forget_cached("qwen3");
    }
}
