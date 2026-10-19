//! The tool set an HTTP model runtime (native Ollama or the OpenAI wire) gets
//! for one turn: Hive's [`BuiltinTools`] plus the enabled MCP tools, behind a
//! single [`ToolExecutor`].
//!
//! - Names never collide: a built-in name wins and the MCP tool of the same
//!   name is dropped (and logged).
//! - At most [`MAX_OFFERED_TOOLS`] are offered, built-ins first — small open
//!   models pick worse from a long list, and every definition costs prompt
//!   tokens on every round. What's dropped is logged.
//! - A call is routed to whichever source offered that name. A built-in name
//!   the policy left out still goes to [`BuiltinTools`], which refuses it with
//!   a clear message; anything else unknown is an error result.
//!
//! [`builtin_policy`] decides what the built-ins may do for a turn, and
//! [`tool_round_budget`] / [`tools_fit_context`] bound the loop.

use std::collections::HashMap;
use std::path::PathBuf;

use hive_core::ModelProviderKind;
use serde_json::Value;

use crate::builtin_tools::{BuiltinTools, Policy};
use crate::tool_loop::ToolExecutor;

/// Tools offered per turn, built-ins first.
pub const MAX_OFFERED_TOOLS: usize = 16;

/// Model rounds per tool-using turn on the Anthropic loop (unchanged).
pub const ANTHROPIC_TOOL_ROUNDS: usize = 6;
/// Default model rounds per turn for local/HTTP runtimes: enough for a
/// read → search → edit → re-read → verify cycle on a real bug.
pub const DEFAULT_HTTP_TOOL_ROUNDS: usize = 24;
/// Upper bound on a configured round budget.
pub const MAX_TOOL_ROUNDS_LIMIT: usize = 100;

/// Tokens kept free for the model's answer when deciding whether another
/// tool round still fits. Reasoning models need at least this much (at 160
/// they ran out mid-thought).
pub const ANSWER_RESERVE_TOKENS: u32 = 1024;
/// Stop offering tools once the running request passes this share of the
/// context window — the next round is the final, tool-less one.
pub const CONTEXT_GUARD_FRACTION: f64 = 0.85;

/// Where a tool call goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Builtin,
    Mcp,
}

/// Built-in tools plus MCP tools behind one executor. See the module docs.
pub struct CompositeTools<M: ToolExecutor> {
    builtin: Option<BuiltinTools>,
    mcp: Option<M>,
    defs: Vec<Value>,
    routes: HashMap<String, Source>,
    /// Names dropped for a collision or the cap — for logs and tests.
    dropped: Vec<String>,
}

fn def_name(def: &Value) -> &str {
    def.get("name").and_then(Value::as_str).unwrap_or("")
}

impl<M: ToolExecutor> CompositeTools<M> {
    /// Merge `builtin` (its policy-allowed defs) with `mcp` (`defs` + the
    /// executor that runs them), capped at `max_tools`.
    pub fn new(builtin: Option<BuiltinTools>, mcp: Option<(Vec<Value>, M)>, max_tools: usize) -> Self {
        let mut defs = Vec::new();
        let mut routes = HashMap::new();
        let mut dropped = Vec::new();
        let mut offer = |def: Value, source: Source, defs: &mut Vec<Value>, dropped: &mut Vec<String>| {
            let name = def_name(&def).to_string();
            if name.is_empty() {
                return;
            }
            if routes.contains_key(&name) || (source == Source::Mcp && BuiltinTools::is_builtin(&name)) {
                tracing::warn!(target: "dispatch", tool = %name, "MCP tool name collides with a built-in tool; the built-in wins");
                dropped.push(name);
                return;
            }
            if defs.len() >= max_tools {
                dropped.push(name);
                return;
            }
            routes.insert(name, source);
            defs.push(def);
        };
        if let Some(b) = &builtin {
            for def in b.defs() {
                offer(def, Source::Builtin, &mut defs, &mut dropped);
            }
        }
        let mcp = mcp.map(|(mcp_defs, exec)| {
            for def in mcp_defs {
                offer(def, Source::Mcp, &mut defs, &mut dropped);
            }
            exec
        });
        let over_cap: Vec<&String> = dropped.iter().filter(|n| !BuiltinTools::is_builtin(n)).collect();
        if !over_cap.is_empty() {
            tracing::info!(
                target: "dispatch",
                offered = defs.len(),
                cap = max_tools,
                dropped = ?over_cap,
                "tool set trimmed for this turn"
            );
        }
        Self { builtin, mcp, defs, routes, dropped }
    }

    /// The definitions to offer, built-ins first.
    pub fn defs(&self) -> &[Value] {
        &self.defs
    }

    /// Tool names left out (collision or cap).
    pub fn dropped(&self) -> &[String] {
        &self.dropped
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

impl<M: ToolExecutor> ToolExecutor for CompositeTools<M> {
    async fn call(&self, name: &str, input: &Value) -> (String, bool) {
        let route = self.routes.get(name).copied().or_else(|| {
            // Not offered, but a built-in name: let BuiltinTools refuse it with
            // its own "not allowed" message rather than a bare "unknown".
            (BuiltinTools::is_builtin(name) && self.builtin.is_some()).then_some(Source::Builtin)
        });
        match (route, &self.builtin, &self.mcp) {
            (Some(Source::Builtin), Some(b), _) => b.call(name, input).await,
            (Some(Source::Mcp), _, Some(m)) => m.call(name, input).await,
            _ => (format!("Error: unknown tool {name:?}. Use only the tools you were given."), true),
        }
    }
}

/// What a turn's built-in tools may do.
///
/// - Reads, listing and search: always.
/// - Writes: when the turn runs in an isolated per-turn git worktree (its
///   changes come back as a review-gated proposal), or when the user allowed
///   in-place writes.
/// - `run_command`: only when the user turned it on AND writes are allowed —
///   a shell can write files, so it never widens a read-only turn.
pub fn builtin_policy(root: impl Into<PathBuf>, isolated: bool, allow_in_place_writes: bool, allow_commands: bool) -> Policy {
    let mut p = Policy::new(root);
    p.allow_write = isolated || allow_in_place_writes;
    p.allow_commands = allow_commands && p.allow_write;
    p
}

/// Model rounds for a tool-using turn. Anthropic keeps its fixed bound; HTTP
/// runtimes use the runtime's setting, else [`DEFAULT_HTTP_TOOL_ROUNDS`],
/// clamped to `1..=`[`MAX_TOOL_ROUNDS_LIMIT`].
pub fn tool_round_budget(provider: ModelProviderKind, configured: Option<u32>) -> usize {
    if provider == ModelProviderKind::Anthropic {
        return ANTHROPIC_TOOL_ROUNDS;
    }
    match configured {
        Some(n) if n > 0 => (n as usize).min(MAX_TOOL_ROUNDS_LIMIT),
        _ => DEFAULT_HTTP_TOOL_ROUNDS,
    }
}

/// Rough token count of a request: ~4 characters per token over the JSON the
/// server receives. Overestimates (escapes, keys), which is the safe side.
pub fn estimate_request_tokens(messages: &[Value], tools: &[Value]) -> u32 {
    let chars: usize = messages.iter().chain(tools).map(|v| v.to_string().len()).sum();
    u32::try_from(chars.div_ceil(4)).unwrap_or(u32::MAX)
}

/// Whether another round may still carry tools: the running request plus the
/// answer reserve stays under [`CONTEXT_GUARD_FRACTION`] of `window`. When it
/// doesn't, the loop sends the next round without tools so the model answers
/// with what it has instead of overflowing the slot. `window == 0` = unknown,
/// never guards.
pub fn tools_fit_context(messages: &[Value], tools: &[Value], window: u32) -> bool {
    if window == 0 {
        return true;
    }
    let used = estimate_request_tokens(messages, tools).saturating_add(ANSWER_RESERVE_TOKENS);
    f64::from(used) <= f64::from(window) * CONTEXT_GUARD_FRACTION
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    struct FakeMcp {
        calls: Mutex<Vec<String>>,
    }
    impl ToolExecutor for FakeMcp {
        async fn call(&self, name: &str, _input: &Value) -> (String, bool) {
            self.calls.lock().unwrap().push(name.to_string());
            (format!("mcp ran {name}"), false)
        }
    }

    fn mcp_def(name: &str) -> Value {
        json!({ "name": name, "description": "", "input_schema": { "type": "object" } })
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hive-agent-tools-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        dir
    }

    fn names(c: &CompositeTools<FakeMcp>) -> Vec<String> {
        c.defs().iter().map(|d| def_name(d).to_string()).collect()
    }

    #[tokio::test]
    async fn builtins_come_first_and_win_name_collisions() {
        let root = temp_root("collide");
        let b = BuiltinTools::new(builtin_policy(&root, true, false, false)).unwrap();
        let mcp = FakeMcp { calls: Mutex::new(vec![]) };
        let c = CompositeTools::new(Some(b), Some((vec![mcp_def("read_file"), mcp_def("rdna__search_docs")], mcp)), 16);
        let n = names(&c);
        assert_eq!(&n[..5], &["read_file", "list_dir", "search", "write_file", "edit_file"]);
        assert_eq!(n.last().unwrap(), "rdna__search_docs");
        assert_eq!(n.iter().filter(|x| *x == "read_file").count(), 1);
        assert_eq!(c.dropped(), &["read_file".to_string()]);

        // read_file goes to the built-in (it really reads the file)…
        let (out, err) = c.call("read_file", &json!({ "path": "a.txt" })).await;
        assert!(!err, "{out}");
        assert!(out.contains("hello"), "{out}");
        // …the MCP tool to MCP.
        let (out, err) = c.call("rdna__search_docs", &json!({})).await;
        assert!(!err);
        assert_eq!(out, "mcp ran rdna__search_docs");
        assert_eq!(c.mcp.as_ref().unwrap().calls.lock().unwrap().as_slice(), &["rdna__search_docs".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn cap_keeps_builtins_and_drops_the_mcp_tail() {
        let root = temp_root("cap");
        let b = BuiltinTools::new(builtin_policy(&root, false, false, false)).unwrap();
        let mcp_defs: Vec<Value> = (0..20).map(|i| mcp_def(&format!("srv__t{i}"))).collect();
        let c = CompositeTools::new(Some(b), Some((mcp_defs, FakeMcp { calls: Mutex::new(vec![]) })), 16);
        assert_eq!(c.defs().len(), 16);
        let n = names(&c);
        assert_eq!(&n[..3], &["read_file", "list_dir", "search"], "read-only built-ins lead");
        assert_eq!(c.dropped().len(), 7, "3 built-ins + 13 MCP offered; 7 dropped");
        // A dropped tool is not callable.
        let (out, err) = c.call("srv__t19", &json!({})).await;
        assert!(err, "{out}");
        assert!(c.mcp.as_ref().unwrap().calls.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn disallowed_builtin_is_refused_not_routed_to_mcp() {
        let root = temp_root("refuse");
        let b = BuiltinTools::new(builtin_policy(&root, false, false, false)).unwrap();
        let c = CompositeTools::new(Some(b), Some((vec![], FakeMcp { calls: Mutex::new(vec![]) })), 16);
        assert!(!names(&c).contains(&"write_file".to_string()));
        let (out, err) = c.call("write_file", &json!({ "path": "x.txt", "content": "x" })).await;
        assert!(err);
        assert!(out.contains("not allowed"), "{out}");
        assert!(!root.join("x.txt").exists());
        let (out, err) = c.call("nope", &json!({})).await;
        assert!(err && out.contains("unknown tool"), "{out}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn mcp_only_and_builtin_only_sets() {
        let c = CompositeTools::new(None, Some((vec![mcp_def("a__b")], FakeMcp { calls: Mutex::new(vec![]) })), 16);
        assert_eq!(names(&c), vec!["a__b"]);
        // A built-in name with no built-ins configured is just unknown.
        assert!(c.call("read_file", &json!({})).await.1);
        let root = temp_root("bonly");
        let b = BuiltinTools::new(builtin_policy(&root, false, false, false)).unwrap();
        let c: CompositeTools<FakeMcp> = CompositeTools::new(Some(b), None, 16);
        assert_eq!(names(&c), vec!["read_file", "list_dir", "search"]);
        let empty: CompositeTools<FakeMcp> = CompositeTools::new(None, None, 16);
        assert!(empty.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn policy_gates_writes_on_isolation_or_opt_in_and_commands_on_both() {
        let p = builtin_policy("/tmp", false, false, true);
        assert!(!p.allow_write && !p.allow_commands, "in place, nothing opted in: read-only even with commands on");
        let p = builtin_policy("/tmp", true, false, false);
        assert!(p.allow_write && !p.allow_commands, "isolated: writes, no commands by default");
        let p = builtin_policy("/tmp", true, false, true);
        assert!(p.allow_write && p.allow_commands);
        let p = builtin_policy("/tmp", false, true, false);
        assert!(p.allow_write && !p.allow_commands, "in-place writes opted in");
        let p = builtin_policy("/tmp", false, true, true);
        assert!(p.allow_commands);
    }

    #[test]
    fn round_budget_defaults_to_24_for_http_and_keeps_anthropic() {
        assert_eq!(tool_round_budget(ModelProviderKind::Custom, None), 24);
        assert_eq!(tool_round_budget(ModelProviderKind::Ollama, Some(0)), 24);
        assert_eq!(tool_round_budget(ModelProviderKind::OpenAI, Some(10)), 10);
        assert_eq!(tool_round_budget(ModelProviderKind::Custom, Some(10_000)), MAX_TOOL_ROUNDS_LIMIT);
        assert_eq!(tool_round_budget(ModelProviderKind::Anthropic, Some(40)), ANTHROPIC_TOOL_ROUNDS);
        assert_eq!(ANTHROPIC_TOOL_ROUNDS, 6);
    }

    #[test]
    fn context_guard_trips_near_the_window() {
        let small = vec![json!({ "role": "user", "content": "hi" })];
        assert!(tools_fit_context(&small, &[], 8192));
        assert!(tools_fit_context(&small, &[], 0), "unknown window never guards");
        // ~6000 tokens of history + 1024 reserve > 85% of 8192.
        let big = vec![json!({ "role": "tool", "content": "x".repeat(24_000) })];
        assert!(!tools_fit_context(&big, &[], 8192));
        assert!(tools_fit_context(&big, &[], 32_768));
        // Tool definitions count too.
        let defs: Vec<Value> = (0..40).map(|i| json!({ "name": format!("t{i}"), "description": "d".repeat(400) })).collect();
        assert!(!tools_fit_context(&small, &defs, 4096));
    }
}
