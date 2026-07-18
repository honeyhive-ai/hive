//! Model Context Protocol client.
//!
//! Hand-rolled JSON-RPC rather than a heavy SDK dependency: `initialize` →
//! `notifications/initialized` → `tools/list` / `tools/call`, over stdio
//! (spawned server) or streamable HTTP.
//!
//! **Security gate:** an installed MCP server is
//! *inert until explicitly enabled*. Enabling is what launches the command /
//! opens the connection — [`McpRegistry`] never connects a disabled server.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Error)]
pub enum McpError {
    #[error("server is disabled (enable it to connect)")]
    Disabled,
    #[error("transport error: {0}")]
    Transport(String),
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// How to reach an MCP server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum McpTransport {
    Stdio { command: String, args: Vec<String> },
    Http { url: String },
}

/// A configured MCP server. `enabled` defaults to `false` — the gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerSpec {
    pub id: String,
    pub transport: McpTransport,
    #[serde(default)]
    pub enabled: bool,
    /// Optional bearer token for an authenticated HTTP server (e.g. a remote
    /// MCP server behind OAuth). Sent as `Authorization: Bearer`. `None` for
    /// open/stdio servers. Populated by the OAuth flow (see task #147).
    #[serde(default)]
    pub auth: Option<String>,
}

/// A tool advertised by a server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input_schema: Value,
}

// ---------------------------------------------------------------------------
// JSON-RPC helpers (pure)
// ---------------------------------------------------------------------------

/// Build a JSON-RPC 2.0 request object.
pub fn jsonrpc_request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "hive", "version": env!("CARGO_PKG_VERSION") }
    })
}

/// Parse the `result` of a `tools/list` response into [`McpTool`]s.
pub fn parse_tools(result: &Value) -> Vec<McpTool> {
    result
        .get("tools")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    Some(McpTool {
                        name: t.get("name")?.as_str()?.to_string(),
                        description: t
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        input_schema: t.get("inputSchema").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Registry + gate
// ---------------------------------------------------------------------------

/// The set of configured MCP servers. Only enabled servers may be connected.
#[derive(Debug, Default, Clone)]
pub struct McpRegistry {
    pub servers: Vec<McpServerSpec>,
}

impl McpRegistry {
    pub fn new(servers: Vec<McpServerSpec>) -> Self {
        Self { servers }
    }

    /// Servers the user has explicitly enabled — the only ones we ever connect.
    pub fn enabled(&self) -> impl Iterator<Item = &McpServerSpec> {
        self.servers.iter().filter(|s| s.enabled)
    }

    /// List tools for a server. Enforces the gate: a disabled server is never
    /// launched/connected.
    pub async fn list_tools(&self, id: &str) -> Result<Vec<McpTool>, McpError> {
        let spec = self.enabled_spec(id)?;
        match &spec.transport {
            McpTransport::Http { url } => list_tools_http(url, spec.auth.as_deref()).await,
            McpTransport::Stdio { command, args } => list_tools_stdio(command, args).await,
        }
    }

    /// List every tool across all enabled servers, tagged with their server id
    /// (so a tool call can be routed back). Errors from one server are skipped
    /// so a single bad server doesn't break the rest.
    pub async fn list_all_tools(&self) -> Vec<(String, McpTool)> {
        let mut out = Vec::new();
        for spec in self.enabled() {
            if let Ok(tools) = self.list_tools(&spec.id).await {
                for tool in tools {
                    out.push((spec.id.clone(), tool));
                }
            }
        }
        out
    }

    /// Call a tool on an enabled server (gate-enforced) and return its text
    /// result.
    pub async fn call_tool(
        &self,
        server_id: &str,
        tool: &str,
        arguments: &Value,
    ) -> Result<String, McpError> {
        let spec = self.enabled_spec(server_id)?;
        match &spec.transport {
            McpTransport::Http { url } => {
                call_tool_http(url, spec.auth.as_deref(), tool, arguments).await
            }
            McpTransport::Stdio { command, args } => {
                call_tool_stdio(command, args, tool, arguments).await
            }
        }
    }

    fn enabled_spec(&self, id: &str) -> Result<&McpServerSpec, McpError> {
        let spec = self
            .servers
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| McpError::Transport(format!("unknown server {id}")))?;
        if !spec.enabled {
            return Err(McpError::Disabled);
        }
        Ok(spec)
    }
}

/// Extract the text from a `tools/call` result (`result.content[].text`).
pub fn parse_tool_result(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn tools_call_params(tool: &str, arguments: &Value) -> Value {
    json!({ "name": tool, "arguments": arguments })
}

// ---------------------------------------------------------------------------
// Transports (integration-only; the JSON-RPC handshake mirrors the spec)
// ---------------------------------------------------------------------------

/// One JSON-RPC round-trip over Streamable HTTP. Sends the request with bearer
/// auth + session id (when present), accepts either a plain JSON body or an SSE
/// (`text/event-stream`) reply, and returns the JSON-RPC response plus any
/// session id the server assigned (`Mcp-Session-Id` response header, e.g. on
/// `initialize`).
async fn http_send(
    http: &reqwest::Client,
    url: &str,
    auth: Option<&str>,
    session: Option<&str>,
    request: &Value,
) -> Result<(Value, Option<String>), McpError> {
    let mut req = http
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
        .json(request);
    if let Some(token) = auth {
        req = req.bearer_auth(token);
    }
    if let Some(sid) = session {
        req = req.header("Mcp-Session-Id", sid);
    }
    let resp = req.send().await.map_err(|e| McpError::Transport(e.to_string()))?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        // OAuth hook (task #147): a 401 here is where we'd run the auth flow.
        return Err(McpError::Transport(
            "unauthorized (401) — this server requires authentication".into(),
        ));
    }
    if !resp.status().is_success() {
        return Err(McpError::Transport(format!("http {}", resp.status())));
    }
    let new_session = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let is_sse = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|c| c.contains("text/event-stream"))
        .unwrap_or(false);
    let want_id = request.get("id").and_then(Value::as_u64);
    let body = resp.text().await.map_err(|e| McpError::Transport(e.to_string()))?;
    let value = if is_sse {
        parse_sse_jsonrpc(&body, want_id)
            .ok_or_else(|| McpError::Protocol("no matching JSON-RPC response in SSE stream".into()))?
    } else {
        serde_json::from_str(&body).map_err(|e| McpError::Protocol(e.to_string()))?
    };
    Ok((value, new_session))
}

/// Extract the JSON-RPC response with `id` from an SSE (`text/event-stream`)
/// body: split into events on blank lines, concatenate each event's `data:`
/// lines, parse as JSON, and return the first object whose `id` matches (or the
/// first JSON object when `id` is `None`). Pure — unit-tested.
pub fn parse_sse_jsonrpc(body: &str, id: Option<u64>) -> Option<Value> {
    let mut events: Vec<String> = Vec::new();
    let mut data = String::new();
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        } else if line.trim().is_empty() && !data.is_empty() {
            events.push(std::mem::take(&mut data));
        }
    }
    if !data.is_empty() {
        events.push(data);
    }
    for ev in events {
        let Ok(v) = serde_json::from_str::<Value>(&ev) else { continue };
        match id {
            None => return Some(v),
            Some(want) if v.get("id").and_then(Value::as_u64) == Some(want) => return Some(v),
            _ => {}
        }
    }
    None
}

async fn list_tools_http(url: &str, auth: Option<&str>) -> Result<Vec<McpTool>, McpError> {
    let http = reqwest::Client::new();
    let (_, session) =
        http_send(&http, url, auth, None, &jsonrpc_request(1, "initialize", initialize_params())).await?;
    let (value, _) =
        http_send(&http, url, auth, session.as_deref(), &jsonrpc_request(2, "tools/list", json!({}))).await?;
    let result = value
        .get("result")
        .ok_or_else(|| McpError::Protocol("missing result".into()))?;
    Ok(parse_tools(result))
}

async fn call_tool_http(
    url: &str,
    auth: Option<&str>,
    tool: &str,
    arguments: &Value,
) -> Result<String, McpError> {
    let http = reqwest::Client::new();
    let (_, session) =
        http_send(&http, url, auth, None, &jsonrpc_request(1, "initialize", initialize_params())).await?;
    let (value, _) = http_send(
        &http,
        url,
        auth,
        session.as_deref(),
        &jsonrpc_request(2, "tools/call", tools_call_params(tool, arguments)),
    )
    .await?;
    let result = value
        .get("result")
        .ok_or_else(|| McpError::Protocol("missing result".into()))?;
    Ok(parse_tool_result(result))
}

async fn call_tool_stdio(
    command: &str,
    args: &[String],
    tool: &str,
    arguments: &Value,
) -> Result<String, McpError> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::process::Command;

    let mut child = Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| McpError::Transport(format!("spawn {command}: {e}")))?;

    let mut stdin = child.stdin.take().ok_or_else(|| McpError::Transport("no stdin".into()))?;
    for msg in [
        jsonrpc_request(1, "initialize", initialize_params()),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }),
        jsonrpc_request(2, "tools/call", tools_call_params(tool, arguments)),
    ] {
        let line = format!("{}\n", serde_json::to_string(&msg).unwrap());
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
    }
    stdin.flush().await.ok();

    let stdout = child.stdout.take().ok_or_else(|| McpError::Transport("no stdout".into()))?;
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| McpError::Transport(e.to_string()))?
    {
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            if value.get("id").and_then(Value::as_u64) == Some(2) {
                if let Some(result) = value.get("result") {
                    let text = parse_tool_result(result);
                    let _ = child.kill().await;
                    return Ok(text);
                }
            }
        }
    }
    let _ = child.kill().await;
    Err(McpError::Protocol("no tools/call response".into()))
}

async fn list_tools_stdio(command: &str, args: &[String]) -> Result<Vec<McpTool>, McpError> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::process::Command;

    let mut child = Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| McpError::Transport(format!("spawn {command}: {e}")))?;

    let mut stdin = child.stdin.take().ok_or_else(|| McpError::Transport("no stdin".into()))?;
    for msg in [
        jsonrpc_request(1, "initialize", initialize_params()),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": {} }),
        jsonrpc_request(2, "tools/list", json!({})),
    ] {
        let line = format!("{}\n", serde_json::to_string(&msg).unwrap());
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
    }
    stdin.flush().await.ok();

    let stdout = child.stdout.take().ok_or_else(|| McpError::Transport("no stdout".into()))?;
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| McpError::Transport(e.to_string()))?
    {
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            if value.get("id").and_then(Value::as_u64) == Some(2) {
                if let Some(result) = value.get("result") {
                    let tools = parse_tools(result);
                    let _ = child.kill().await;
                    return Ok(tools);
                }
            }
        }
    }
    let _ = child.kill().await;
    Err(McpError::Protocol("no tools/list response".into()))
}

/// Convert MCP tools into the JSON tool definitions a provider's tool API
/// expects (Anthropic/OpenAI both accept name + description + JSON schema).
/// The actual tool-call execution loop is a Phase 6 follow-up.
pub fn tools_as_provider_json(tools: &[McpTool]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonrpc_request_shape() {
        let r = jsonrpc_request(7, "tools/list", json!({}));
        assert_eq!(r["jsonrpc"], "2.0");
        assert_eq!(r["id"], 7);
        assert_eq!(r["method"], "tools/list");
    }

    #[test]
    fn parses_tools_list_result() {
        let result = json!({
            "tools": [
                { "name": "search", "description": "web search",
                  "inputSchema": { "type": "object" } },
                { "name": "noop" }
            ]
        });
        let tools = parse_tools(&result);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "search");
        assert_eq!(tools[0].description, "web search");
        assert_eq!(tools[1].description, "");
    }

    #[tokio::test]
    async fn disabled_server_is_never_connected() {
        let reg = McpRegistry::new(vec![McpServerSpec {
            id: "fs".into(),
            transport: McpTransport::Stdio {
                command: "definitely-not-a-real-binary-xyz".into(),
                args: vec![],
            },
            enabled: false,
            auth: None,
        }]);
        // The gate must trip before any spawn attempt.
        assert!(matches!(reg.list_tools("fs").await, Err(McpError::Disabled)));
        assert_eq!(reg.enabled().count(), 0);
    }

    #[test]
    fn parses_tool_call_result_text() {
        let result = json!({
            "content": [
                { "type": "text", "text": "line one" },
                { "type": "text", "text": "line two" }
            ]
        });
        assert_eq!(parse_tool_result(&result), "line one\nline two");
        assert_eq!(parse_tool_result(&json!({})), "");
    }

    #[tokio::test]
    async fn call_tool_gate_blocks_disabled_server() {
        let reg = McpRegistry::new(vec![McpServerSpec {
            id: "fs".into(),
            transport: McpTransport::Http { url: "http://x".into() },
            enabled: false,
            auth: None,
        }]);
        assert!(matches!(
            reg.call_tool("fs", "read", &json!({})).await,
            Err(McpError::Disabled)
        ));
    }

    #[test]
    fn sse_parser_extracts_jsonrpc_by_id() {
        // A Streamable-HTTP SSE reply: an unrelated notification event, then the
        // response for id 2. Each event is `data:` line(s) separated by a blank.
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\n\n";
        let v = parse_sse_jsonrpc(body, Some(2)).expect("should find id 2");
        assert_eq!(v["id"], 2);
        assert!(v["result"]["tools"].is_array());
        // Multi-line data is concatenated with newlines into one JSON doc.
        let multi = "data: {\"jsonrpc\":\"2.0\",\ndata: \"id\":5,\"result\":1}\n\n";
        assert_eq!(parse_sse_jsonrpc(multi, Some(5)).unwrap()["result"], 1);
        // No matching id → None; id=None returns the first JSON object.
        assert!(parse_sse_jsonrpc(body, Some(99)).is_none());
        assert!(parse_sse_jsonrpc(body, None).is_some());
    }

    #[test]
    fn enabled_filter_respects_the_gate() {
        let reg = McpRegistry::new(vec![
            McpServerSpec {
                id: "a".into(),
                transport: McpTransport::Http { url: "http://x".into() },
                enabled: true,
                auth: None,
            },
            McpServerSpec {
                id: "b".into(),
                transport: McpTransport::Http { url: "http://y".into() },
                enabled: false,
                auth: None,
            },
        ]);
        let ids: Vec<_> = reg.enabled().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["a"]);
    }
}
