//! Generating an agent's reply — with MCP tools when configured.
//!
//! Both `hive agent` and `hive worker` turn a chat transcript into one reply.
//! When `HIVE_MCP_CONFIG` names one or more enabled MCP servers *and* the
//! runtime is Anthropic with a key, the reply is produced by the agentic tool
//! loop (model ↔ tool_use ↔ tool_result), so the agent can reach external
//! boards (Linear/GitHub) via MCP tools. Otherwise it falls back to a single
//! streamed completion — identical to the pre-MCP behavior — so nothing
//! regresses when no MCP config is present.
//!
//! Mirrors the app's `try_tool_loop`: an Anthropic `MessagesApi` adapter + an
//! `McpRegistry`-backed `ToolExecutor`, driven by `run_with_messages`. Runs
//! entirely within the caller's task (the CLI's current-thread runtime), so the
//! non-`Send` registry/executor never cross tasks.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use hive_core::{ModelProviderKind, WorkspaceRole};
use hive_runtime::provider::anthropic::{content_value, AnthropicResponse};
use hive_runtime::tool_loop::{self, MessagesApi, ToolExecutor};
use hive_runtime::{dispatch, AnthropicClient, ChatTurn, McpRegistry, ProviderError, ResolvedRuntime};
use serde_json::{json, Value};

use crate::mcp_config;

const MAX_TOKENS: u32 = 1024;
const MAX_TOOL_ITERS: usize = 6;

/// Env var that raises (or lowers) the minimum author role required before a
/// headless reply is allowed to execute MCP tools. See [`parse_min_role`].
pub const MIN_ROLE_ENV: &str = "HIVE_MCP_MIN_ROLE";

/// Parse `HIVE_MCP_MIN_ROLE` (viewer|contributor|admin|owner, case-insensitive)
/// into the minimum author role that unlocks the MCP tool loop. Unknown or
/// absent → `Contributor` (the safe default: a Viewer can never drive tools).
pub fn parse_min_role(v: Option<&str>) -> WorkspaceRole {
    match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("viewer") => WorkspaceRole::Viewer,
        Some("contributor") => WorkspaceRole::Contributor,
        Some("admin") => WorkspaceRole::Admin,
        Some("owner") => WorkspaceRole::Owner,
        _ => WorkspaceRole::Contributor,
    }
}

/// The configured minimum author role, read from `HIVE_MCP_MIN_ROLE`.
pub fn mcp_min_role() -> WorkspaceRole {
    parse_min_role(std::env::var(MIN_ROLE_ENV).ok().as_deref())
}

/// Whether the triggering message's author is privileged enough to have MCP
/// tools executed on their behalf. `None` = author could not be resolved (no
/// `actor_identity`) → fail closed (denied). This is the H1 gate: a low-
/// privilege member must not be able to drive the worker's tools on the
/// victim's key.
pub fn mcp_allowed_for(author_role: Option<WorkspaceRole>, min_role: WorkspaceRole) -> bool {
    match author_role {
        Some(role) => role.rank() >= min_role.rank(),
        None => false,
    }
}

/// Produce one reply for `turns` under `system`. Runs the MCP tool loop when an
/// Anthropic runtime has a key, `HIVE_MCP_CONFIG` yields ≥1 enabled tool, **and**
/// `allow_mcp_tools` is set (the triggering author cleared the role gate);
/// otherwise streams a single completion (the original, tool-free behavior).
pub async fn generate_reply(
    rt: &ResolvedRuntime,
    system: &str,
    turns: &[ChatTurn],
    allow_mcp_tools: bool,
) -> Result<String> {
    if allow_mcp_tools {
        if let Some(text) = try_tool_loop(rt, system, turns).await? {
            return Ok(text);
        }
    }
    dispatch::stream(rt, Some(system), turns, None, &[], MAX_TOKENS, |_| {})
        .await
        .map_err(|e| anyhow!("provider: {e}"))
}

/// Attempt the MCP tool loop. `Ok(None)` = no tools / unsupported runtime →
/// caller falls back to streaming. Mirrors the app's `try_tool_loop`.
async fn try_tool_loop(rt: &ResolvedRuntime, system: &str, turns: &[ChatTurn]) -> Result<Option<String>> {
    // The tool-loop adapter is Anthropic-only (as in the app), and needs a key.
    if rt.provider != ModelProviderKind::Anthropic {
        return Ok(None);
    }
    let Some(api_key) = rt.api_key.clone() else {
        return Ok(None);
    };
    // No config / no enabled servers → empty registry → fall back to streaming.
    let registry = McpRegistry::new(mcp_config::load_from_env()?);
    if registry.enabled().next().is_none() {
        return Ok(None);
    }
    let tagged = registry.list_all_tools().await;
    if tagged.is_empty() {
        return Ok(None);
    }

    let mut tool_defs = Vec::with_capacity(tagged.len());
    let mut names = HashMap::new();
    for (server, tool) in &tagged {
        let tname = sanitize_tool_name(&format!("{server}__{}", tool.name));
        tool_defs.push(json!({
            "name": tname,
            "description": tool.description,
            "input_schema": tool.input_schema,
        }));
        names.insert(tname, (server.clone(), tool.name.clone()));
    }

    let model = AnthropicMessages {
        client: AnthropicClient::new(),
        api_key,
        model: rt.model.clone(),
        system: Some(system.to_string()),
    };
    let executor = McpToolExecutor { registry, names };
    let initial = turns_to_messages(turns);
    let text = tool_loop::run_with_messages(&model, &executor, initial, tool_defs, MAX_TOOL_ITERS)
        .await
        .map_err(|e| anyhow!("tool loop: {e}"))?;
    Ok(Some(text))
}

/// Adapter: one non-streaming Anthropic turn for the tool loop.
struct AnthropicMessages {
    client: AnthropicClient,
    api_key: String,
    model: String,
    system: Option<String>,
}

impl MessagesApi for AnthropicMessages {
    async fn run(&self, messages: &[Value], tools: &[Value]) -> Result<AnthropicResponse, ProviderError> {
        self.client
            .run_messages(&self.api_key, &self.model, self.system.as_deref(), messages, tools, MAX_TOKENS)
            .await
    }
}

/// Routes a (sanitized) tool name back to its MCP server, honoring the gate.
struct McpToolExecutor {
    registry: McpRegistry,
    /// sanitized anthropic tool name → (server id, real tool name)
    names: HashMap<String, (String, String)>,
}

impl ToolExecutor for McpToolExecutor {
    async fn call(&self, name: &str, input: &Value) -> (String, bool) {
        match self.names.get(name) {
            Some((server, tool)) => match self.registry.call_tool(server, tool, input).await {
                Ok(text) => (text, false),
                Err(e) => (format!("tool error: {e}"), true),
            },
            None => (format!("unknown tool {name}"), true),
        }
    }
}

/// Anthropic tool names must match `^[a-zA-Z0-9_-]{1,64}$`.
fn sanitize_tool_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    cleaned.chars().take(64).collect()
}

/// Convert provider turns into the Anthropic message array the loop seeds.
fn turns_to_messages(turns: &[ChatTurn]) -> Vec<Value> {
    turns
        .iter()
        .map(|t| json!({ "role": t.role, "content": content_value(&t.content) }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::WorkspaceRole::*;

    #[test]
    fn viewer_denied_at_default() {
        let min = parse_min_role(None);
        assert_eq!(min, Contributor);
        assert!(!mcp_allowed_for(Some(Viewer), min));
    }

    #[test]
    fn contributor_allowed_at_default() {
        assert!(mcp_allowed_for(Some(Contributor), parse_min_role(None)));
        // and everyone above the floor
        assert!(mcp_allowed_for(Some(Admin), parse_min_role(None)));
        assert!(mcp_allowed_for(Some(Owner), parse_min_role(None)));
    }

    #[test]
    fn raised_min_role_denies_contributor() {
        let min = parse_min_role(Some("admin"));
        assert_eq!(min, Admin);
        assert!(!mcp_allowed_for(Some(Contributor), min));
        assert!(mcp_allowed_for(Some(Admin), min));
    }

    #[test]
    fn env_parse_is_case_insensitive_and_trims() {
        assert_eq!(parse_min_role(Some("  Owner ")), Owner);
        assert_eq!(parse_min_role(Some("VIEWER")), Viewer);
        assert_eq!(parse_min_role(Some("Contributor")), Contributor);
    }

    #[test]
    fn unknown_or_absent_env_defaults_to_contributor() {
        assert_eq!(parse_min_role(None), Contributor);
        assert_eq!(parse_min_role(Some("")), Contributor);
        assert_eq!(parse_min_role(Some("superuser")), Contributor);
    }

    #[test]
    fn unresolved_author_is_denied() {
        // No actor_identity → None → fail closed at any threshold.
        assert!(!mcp_allowed_for(None, parse_min_role(None)));
        assert!(!mcp_allowed_for(None, parse_min_role(Some("viewer"))));
    }
}
