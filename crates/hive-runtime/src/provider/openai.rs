//! OpenAI-compatible streaming client — covers OpenAI, OpenRouter, custom
//! gateways, and Ollama's `/v1` compatibility endpoint (ported from the
//! corresponding backends in `Providers.swift`). Shares `ChatTurn` with the
//! Anthropic client.
//!
//! The SSE delta parser (`extract_delta`) is pure and unit-tested; the network
//! call is integration-only.

use futures_util::StreamExt;
use serde::Serialize;

use super::anthropic::{ChatTurn, ProviderError};

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
}

impl OpenAiClient {
    /// `endpoint` is the full chat-completions URL, e.g.
    /// `https://api.openai.com/v1/chat/completions` or
    /// `http://localhost:11434/v1/chat/completions`.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            endpoint: endpoint.into(),
            api_key_header: false,
        }
    }

    /// Authenticate with an `api-key` header instead of a bearer token (Azure
    /// OpenAI). The endpoint should already include `?api-version=...`.
    pub fn with_api_key_header(mut self, yes: bool) -> Self {
        self.api_key_header = yes;
        self
    }

    pub async fn stream_reply(
        &self,
        api_key: Option<&str>,
        model: &str,
        system: Option<&str>,
        turns: &[ChatTurn],
        mut on_delta: impl FnMut(String),
    ) -> Result<String, ProviderError> {
        let mut messages = Vec::with_capacity(turns.len() + 1);
        if let Some(sys) = system {
            messages.push(Message {
                role: "system",
                content: sys,
            });
        }
        for t in turns {
            messages.push(Message {
                role: &t.role,
                content: &t.content,
            });
        }
        let body = CompletionsRequest {
            model,
            stream: true,
            messages,
        };

        let mut req = self
            .http
            .post(&self.endpoint)
            .header("content-type", "application/json")
            .json(&body);
        if let Some(key) = api_key {
            req = if self.api_key_header {
                req.header("api-key", key)
            } else {
                req.header("authorization", format!("Bearer {key}"))
            };
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api { status, body });
        }

        let mut assembled = String::new();
        let mut buffer = String::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let bytes = chunk?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(idx) = buffer.find('\n') {
                let line = buffer[..idx].trim_end_matches('\r').to_string();
                buffer.drain(..=idx);
                if let Some(text) = extract_delta(&line) {
                    assembled.push_str(&text);
                    on_delta(text);
                }
            }
        }
        Ok(assembled)
    }
}

/// Extract incremental text from an OpenAI-style SSE `data:` line
/// (`choices[0].delta.content`). Returns `None` for `[DONE]`, role-only frames,
/// and non-data lines.
pub fn extract_delta(line: &str) -> Option<String> {
    let data = line.strip_prefix("data:")?.trim();
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    let content = value
        .get("choices")?
        .get(0)?
        .get("delta")?
        .get("content")?
        .as_str()?;
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
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
}
