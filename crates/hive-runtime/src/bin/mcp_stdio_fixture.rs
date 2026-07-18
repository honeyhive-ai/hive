//! Minimal MCP stdio server used by the `mcp_stdio` integration test — the
//! only thing in the workspace that exercises the *real* subprocess spawn +
//! line-framed JSON-RPC round trip (`initialize` → `notifications/initialized`
//! → `tools/list` / `tools/call`). Advertises one tool, `echo`, which returns
//! `echo: <msg>`. Std-only; exits when stdin closes.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id").cloned();

        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "mcp-stdio-fixture", "version": "0.0.0" }
            }),
            // Notification — no id, no response.
            "notifications/initialized" => continue,
            "tools/list" => json!({
                "tools": [{
                    "name": "echo",
                    "description": "Echo a message back",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "msg": { "type": "string" } },
                        "required": ["msg"]
                    }
                }]
            }),
            "tools/call" => {
                let msg_arg = msg
                    .pointer("/params/arguments/msg")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                json!({ "content": [{ "type": "text", "text": format!("echo: {msg_arg}") }] })
            }
            _ => continue,
        };

        let Some(id) = id else { continue };
        let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        if writeln!(out, "{reply}").is_err() {
            break;
        }
        let _ = out.flush();
    }
}
