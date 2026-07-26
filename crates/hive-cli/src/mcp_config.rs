//! Headless MCP config — the CLI's equivalent of the app's device-local
//! `mcp_servers` table.
//!
//! The desktop app configures MCP servers through a UI that writes an
//! `mcp_servers` table into `hive.config.toml` and gates each one behind an
//! explicit *enable* toggle. A headless worker has no UI, so it reads the same
//! table shape from a file named by `HIVE_MCP_CONFIG`. Presence in that file
//! **is** the opt-in (an operator wrote and provisioned it), so servers default
//! to `enabled = true` here — set `enabled = false` to keep one inert.
//!
//! Secrets never live in the TOML: an HTTP/SSE server's bearer token is named
//! by `token_env` and resolved from the worker's environment (the same
//! out-of-band convention as `HIVE_WS_SECRET_*`). A stdio server's child process
//! inherits the worker's environment, so its provider token (e.g.
//! `GITHUB_PERSONAL_ACCESS_TOKEN`) is provisioned there.

use anyhow::{Context, Result};
use hive_runtime::{McpServerSpec, McpTransport};
use serde::Deserialize;

/// The env var naming the headless MCP config TOML file.
pub const CONFIG_ENV: &str = "HIVE_MCP_CONFIG";

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    mcp_servers: Vec<RawMcpServer>,
}

/// Mirrors the app's `mcp_servers` entry (`id` + `transport` + stdio
/// `command`/`args` or http `url`), plus a headless-only `token_env` for
/// resolving a bearer token from the environment.
#[derive(Debug, Default, Deserialize)]
struct RawMcpServer {
    id: String,
    #[serde(default = "default_transport_stdio")]
    transport: String,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    url: Option<String>,
    /// Name of the env var holding this server's bearer token (http/sse). The
    /// token itself is never written to the TOML.
    token_env: Option<String>,
    /// Servers default to enabled in a headless config — the file is the opt-in.
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_transport_stdio() -> String {
    "stdio".to_string()
}

fn default_true() -> bool {
    true
}

/// Load the MCP servers named by `HIVE_MCP_CONFIG`. Unset or empty → no servers
/// (an empty vec), so the caller behaves exactly as it did before MCP wiring.
pub fn load_from_env() -> Result<Vec<McpServerSpec>> {
    let Some(path) = std::env::var(CONFIG_ENV).ok().filter(|s| !s.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {CONFIG_ENV}={path}"))?;
    specs_from_str(&text).with_context(|| format!("parsing {CONFIG_ENV}={path}"))
}

/// Parse an MCP config TOML into [`McpServerSpec`]s, resolving each http/sse
/// server's bearer token from its `token_env` environment variable.
pub fn specs_from_str(text: &str) -> Result<Vec<McpServerSpec>> {
    let raw: RawConfig = toml::from_str(text)?;
    Ok(raw.mcp_servers.into_iter().map(spec_from_raw).collect())
}

fn spec_from_raw(m: RawMcpServer) -> McpServerSpec {
    let is_http = matches!(m.transport.to_lowercase().as_str(), "http" | "sse");
    let transport = if is_http {
        McpTransport::Http { url: m.url.unwrap_or_default() }
    } else {
        McpTransport::Stdio {
            command: m.command.unwrap_or_default(),
            args: m.args,
        }
    };
    // Resolve a bearer token from the environment (never from the file).
    let auth = m.token_env.as_deref().and_then(|var| {
        match std::env::var(var).ok().filter(|s| !s.trim().is_empty()) {
            Some(tok) => Some(tok),
            None => {
                eprintln!("mcp: server '{}' token_env '{var}' is not set — connecting without auth", m.id);
                None
            }
        }
    });
    McpServerSpec {
        id: m.id,
        transport,
        enabled: m.enabled,
        auth,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_missing_config_yields_no_servers() {
        // No `mcp_servers` table at all → empty.
        assert!(specs_from_str("").unwrap().is_empty());
        assert!(specs_from_str("# just a comment\n").unwrap().is_empty());
    }

    #[test]
    fn parses_stdio_and_http_servers() {
        std::env::set_var("HIVE_MCP_TEST_TOKEN", "secret-abc");
        let toml = r#"
[[mcp_servers]]
id = "fs"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/data"]

[[mcp_servers]]
id = "linear"
transport = "http"
url = "https://mcp.linear.app/mcp"
token_env = "HIVE_MCP_TEST_TOKEN"
"#;
        let specs = specs_from_str(toml).unwrap();
        assert_eq!(specs.len(), 2);

        // stdio server: command + args, no auth, enabled by default (headless).
        assert_eq!(specs[0].id, "fs");
        assert!(specs[0].enabled, "headless servers default to enabled");
        assert_eq!(specs[0].auth, None);
        assert_eq!(
            specs[0].transport,
            McpTransport::Stdio {
                command: "npx".into(),
                args: vec![
                    "-y".into(),
                    "@modelcontextprotocol/server-filesystem".into(),
                    "/data".into(),
                ],
            }
        );

        // http server: url + bearer token resolved from token_env.
        assert_eq!(specs[1].id, "linear");
        assert_eq!(
            specs[1].transport,
            McpTransport::Http { url: "https://mcp.linear.app/mcp".into() }
        );
        assert_eq!(specs[1].auth.as_deref(), Some("secret-abc"));

        std::env::remove_var("HIVE_MCP_TEST_TOKEN");
    }

    #[test]
    fn sse_maps_to_http_and_disabled_is_respected() {
        let toml = r#"
[[mcp_servers]]
id = "remote"
transport = "sse"
url = "https://example.com/sse"
enabled = false
"#;
        let specs = specs_from_str(toml).unwrap();
        assert_eq!(specs.len(), 1);
        assert!(!specs[0].enabled, "explicit enabled=false is honored");
        assert!(matches!(specs[0].transport, McpTransport::Http { .. }));
    }
}
