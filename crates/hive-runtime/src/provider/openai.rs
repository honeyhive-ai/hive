//! OpenAI-compatible streaming client — covers OpenAI, OpenRouter, custom
//! gateways, and Ollama's `/v1` compatibility endpoint (ported from the
//! corresponding backends in `Providers.swift`). Shares `ChatTurn` with the
//! Anthropic client.
//!
//! Hardened for local / remote-LAN models (Ollama over Tailscale, LM Studio):
//! a connect timeout, one retry on a refused/dropped connection, an *idle*
//! timeout on the response stream (any byte resets it, so a slow-but-alive
//! model isn't killed), errors that name the host, and separation of a
//! reasoning model's `<think>…</think>` (or `reasoning` delta field) from the
//! reply so chain-of-thought never lands in the transcript.
//!
//! The SSE delta parser (`extract_delta`) and the think splitter are pure and
//! unit-tested; the network path is exercised against an in-process fake
//! server in the `wire` tests below.

use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;

use super::anthropic::{ChatTurn, ProviderError};
use super::http::{self, Peer};
use super::thinking::ThinkFilter;
use super::turn_idle_timeout;

#[derive(Debug, Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Serialize)]
struct CompletionsRequest<'a> {
    model: &'a str,
    stream: bool,
    messages: Vec<Message<'a>>,
}

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
        }
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
        mut on_delta: impl FnMut(String),
        mut on_thinking: impl FnMut(String),
    ) -> Result<String, ProviderError> {
        let mut messages = Vec::with_capacity(turns.len() + 1);
        if let Some(sys) = system {
            messages.push(Message { role: "system", content: sys });
        }
        for t in turns {
            messages.push(Message { role: &t.role, content: &t.content });
        }
        let body = CompletionsRequest { model, stream: true, messages };
        let body = serde_json::to_vec(&body)
            .map_err(|e| ProviderError::Decode(format!("encode request: {e}")))?;

        let resp = self.send_with_retry(api_key, body).await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api { provider: self.provider_label, status, body });
        }

        let mut assembled = String::new();
        let mut buffer = String::new();
        let mut filter = ThinkFilter::new();
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
                if let Some(text) = extract_delta(&line) {
                    let split = filter.push(&text);
                    if !split.thinking.is_empty() {
                        on_thinking(split.thinking);
                    }
                    if !split.content.is_empty() {
                        assembled.push_str(&split.content);
                        on_delta(split.content);
                    }
                }
                if let Some(reasoning) = extract_reasoning_delta(&line) {
                    on_thinking(reasoning);
                }
            }
        }
        let tail = filter.finish();
        if !tail.thinking.is_empty() {
            on_thinking(tail.thinking);
        }
        if !tail.content.is_empty() {
            assembled.push_str(&tail.content);
            on_delta(tail.content);
        }
        Ok(assembled)
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
