//! Anthropic Messages API streaming client — ported from the Anthropic backend
//! in `Providers.swift`. Streams Server-Sent Events and surfaces incremental
//! text deltas through a callback, returning the fully-assembled reply.
//!
//! The SSE parsing (`extract_text_delta`) is pure and unit-tested; the network
//! call is exercised in integration only (needs a real API key).

use futures_util::StreamExt;
use serde::Serialize;
use thiserror::Error;

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("anthropic API error ({status}): {body}")]
    Api { status: u16, body: String },
    #[error("stream decode error: {0}")]
    Decode(String),
    #[error("subprocess error: {0}")]
    Subprocess(String),
}

/// One conversation turn in provider wire shape.
#[derive(Debug, Clone, Serialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

impl ChatTurn {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }
}

/// Anthropic image media type for a path, if it's a supported image.
fn image_media_type(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        Some("image/png")
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if lower.ends_with(".gif") {
        Some("image/gif")
    } else if lower.ends_with(".webp") {
        Some("image/webp")
    } else {
        None
    }
}

/// Image attachment paths (+ media type) referenced by `[Attached: ...]`.
fn image_attachments(text: &str) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(s) = rest.find("[Attached: ") {
        let after = &rest[s + "[Attached: ".len()..];
        let Some(e) = after.find(']') else { break };
        let path = after[..e].trim();
        if let Some(mt) = image_media_type(path) {
            out.push((path.to_string(), mt));
        }
        rest = &after[e + 1..];
    }
    out
}

/// Build the Anthropic `content` for a turn. Plain string unless the text
/// references readable image attachments, in which case a content-block array
/// with the images inlined as base64 (vision). `read` loads file bytes
/// (injected for tests).
pub fn content_value_with(
    text: &str,
    read: impl Fn(&str) -> Option<Vec<u8>>,
) -> serde_json::Value {
    use base64::Engine;
    let images = image_attachments(text);
    if images.is_empty() {
        return serde_json::Value::String(text.to_string());
    }
    let mut blocks = vec![serde_json::json!({ "type": "text", "text": text })];
    for (path, media_type) in images {
        if let Some(bytes) = read(&path) {
            let data = base64::engine::general_purpose::STANDARD.encode(bytes);
            blocks.push(serde_json::json!({
                "type": "image",
                "source": { "type": "base64", "media_type": media_type, "data": data },
            }));
        }
    }
    serde_json::Value::Array(blocks)
}

/// `content_value_with` reading real files from disk.
pub fn content_value(text: &str) -> serde_json::Value {
    content_value_with(text, |p| std::fs::read(p).ok())
}

#[derive(Debug, Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    /// `{role, content}` per turn, where content is a string or — when the turn
    /// references image attachments — a block array with inlined images.
    messages: Vec<serde_json::Value>,
}

/// Anthropic streaming client. Cheap to clone (wraps a `reqwest::Client`).
#[derive(Debug, Clone)]
pub struct AnthropicClient {
    http: reqwest::Client,
    endpoint: String,
}

impl Default for AnthropicClient {
    fn default() -> Self {
        Self {
            http: reqwest::Client::new(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
        }
    }
}

impl AnthropicClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the endpoint (custom gateway / test server).
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Stream a reply, invoking `on_delta` for each incremental text fragment
    /// and returning the assembled message body.
    pub async fn stream_reply(
        &self,
        api_key: &str,
        model: &str,
        system: Option<&str>,
        turns: &[ChatTurn],
        max_tokens: u32,
        mut on_delta: impl FnMut(String),
    ) -> Result<String, ProviderError> {
        let messages = turns
            .iter()
            .map(|t| serde_json::json!({ "role": t.role, "content": content_value(&t.content) }))
            .collect();
        let body = MessagesRequest {
            model,
            max_tokens,
            stream: true,
            system,
            messages,
        };

        let resp = self
            .http
            .post(&self.endpoint)
            .header("x-api-key", api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

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
            // SSE events are newline-delimited; process complete lines and keep
            // any trailing partial line in the buffer.
            while let Some(idx) = buffer.find('\n') {
                let line = buffer[..idx].trim_end_matches('\r').to_string();
                buffer.drain(..=idx);
                if let Some(text) = extract_text_delta(&line) {
                    assembled.push_str(&text);
                    on_delta(text);
                }
            }
        }
        Ok(assembled)
    }
}

// ---------------------------------------------------------------------------
// Non-streaming tool-use round (for the MCP tool loop)
// ---------------------------------------------------------------------------

use serde_json::{json, Value};

/// A tool the model asked to invoke.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// Parsed non-streaming `messages` response.
#[derive(Debug, Clone)]
pub struct AnthropicResponse {
    /// The raw `content` block array (echoed back as the assistant turn).
    pub content: Value,
    /// Concatenated text blocks.
    pub text: String,
    /// Any `tool_use` blocks.
    pub tool_uses: Vec<ToolUse>,
    pub stop_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct MessagesToolRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: &'a [Value],
    #[serde(skip_serializing_if = "<[Value]>::is_empty")]
    tools: &'a [Value],
}

impl AnthropicClient {
    /// One non-streaming turn with tools. `messages` is the Anthropic-format
    /// message array; `tools` are tool definitions (name/description/input_schema).
    pub async fn run_messages(
        &self,
        api_key: &str,
        model: &str,
        system: Option<&str>,
        messages: &[Value],
        tools: &[Value],
        max_tokens: u32,
    ) -> Result<AnthropicResponse, ProviderError> {
        let body = MessagesToolRequest {
            model,
            max_tokens,
            system,
            messages,
            tools,
        };
        let resp = self
            .http
            .post(&self.endpoint)
            .header("x-api-key", api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api { status, body });
        }
        let value: Value = resp.json().await.map_err(ProviderError::Http)?;
        Ok(parse_messages_response(&value))
    }
}

/// Parse a non-streaming Messages response into text + tool_use blocks.
pub fn parse_messages_response(value: &Value) -> AnthropicResponse {
    let content = value.get("content").cloned().unwrap_or_else(|| json!([]));
    let mut text = String::new();
    let mut tool_uses = Vec::new();
    if let Some(blocks) = content.as_array() {
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(Value::as_str) {
                        text.push_str(t);
                    }
                }
                Some("tool_use") => {
                    if let (Some(id), Some(name)) = (
                        block.get("id").and_then(Value::as_str),
                        block.get("name").and_then(Value::as_str),
                    ) {
                        tool_uses.push(ToolUse {
                            id: id.to_string(),
                            name: name.to_string(),
                            input: block.get("input").cloned().unwrap_or(Value::Null),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    AnthropicResponse {
        content,
        text,
        tool_uses,
        stop_reason: value
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

/// Extract the incremental text from a single SSE `data:` line, if it carries a
/// `content_block_delta` of type `text_delta`. Returns `None` for control
/// frames (`message_start`, `ping`, `content_block_stop`, `message_stop`, …)
/// and non-data lines (`event:` / blank).
pub fn extract_text_delta(line: &str) -> Option<String> {
    let data = line.strip_prefix("data:")?.trim();
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(data).ok()?;
    if value.get("type")?.as_str()? != "content_block_delta" {
        return None;
    }
    let delta = value.get("delta")?;
    if delta.get("type")?.as_str()? != "text_delta" {
        return None;
    }
    Some(delta.get("text")?.as_str()?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_from_content_block_delta() {
        let line = r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        assert_eq!(extract_text_delta(line).as_deref(), Some("Hello"));
    }

    #[test]
    fn plain_text_stays_a_string() {
        assert_eq!(
            content_value_with("just text", |_| None),
            serde_json::Value::String("just text".into())
        );
    }

    #[test]
    fn image_attachment_becomes_a_vision_block() {
        let text = "look [Attached: /tmp/a.png]";
        let v = content_value_with(text, |p| {
            assert_eq!(p, "/tmp/a.png");
            Some(vec![1, 2, 3])
        });
        let arr = v.as_array().expect("array");
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], text);
        assert_eq!(arr[1]["type"], "image");
        assert_eq!(arr[1]["source"]["media_type"], "image/png");
        assert_eq!(arr[1]["source"]["data"], "AQID"); // base64 of [1,2,3]
    }

    #[test]
    fn non_image_attachment_does_not_inline() {
        // A .txt attachment is left for the agent to read by path, not inlined.
        assert!(content_value_with("see [Attached: /tmp/a.txt]", |_| Some(vec![9]))
            .is_string());
    }

    #[test]
    fn unreadable_image_is_skipped() {
        // Marker present but file unreadable → text block only, no image block.
        let v = content_value_with("x [Attached: /tmp/missing.png]", |_| None);
        let arr = v.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "text");
    }

    #[test]
    fn ignores_control_and_non_data_frames() {
        assert!(extract_text_delta("event: message_start").is_none());
        assert!(extract_text_delta("").is_none());
        assert!(extract_text_delta("data: [DONE]").is_none());
        assert!(extract_text_delta(
            r#"data: {"type":"message_start","message":{"id":"x"}}"#
        )
        .is_none());
        assert!(extract_text_delta(
            r#"data: {"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{"}}"#
        )
        .is_none());
    }

    #[test]
    fn parses_tool_use_and_text_from_messages_response() {
        let value = serde_json::json!({
            "content": [
                { "type": "text", "text": "Let me check." },
                { "type": "tool_use", "id": "tu_1", "name": "search", "input": { "q": "rust" } }
            ],
            "stop_reason": "tool_use"
        });
        let parsed = parse_messages_response(&value);
        assert_eq!(parsed.text, "Let me check.");
        assert_eq!(parsed.tool_uses.len(), 1);
        assert_eq!(parsed.tool_uses[0].name, "search");
        assert_eq!(parsed.stop_reason.as_deref(), Some("tool_use"));
    }

    #[test]
    fn assembling_a_sequence_of_deltas_reconstructs_message() {
        let lines = [
            r#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}"#,
            r#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"lo, "}}"#,
            r#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"world"}}"#,
        ];
        let assembled: String = lines.iter().filter_map(|l| extract_text_delta(l)).collect();
        assert_eq!(assembled, "Hello, world");
    }
}
