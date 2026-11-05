//! Runtime targets. A `RuntimeTarget` is one configured model endpoint
//! (provider + model + capabilities), as read by the config loader, context
//! budgeting, and chat.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelProviderKind {
    Anthropic,
    #[serde(rename = "openAI")]
    OpenAI,
    OpenRouter,
    Ollama,
    /// Azure OpenAI — OpenAI-compatible wire format, but authenticates with an
    /// `api-key` header (not a bearer token); the endpoint embeds the deployment
    /// + `api-version`.
    Azure,
    Custom,
    HiveDaemon,
    Aider,
    Pi,
    ClaudeCode,
    /// OpenAI's `codex` CLI coding agent (`codex exec …`), driven headless.
    Codex,
    /// A generic stdin-driven CLI agent (e.g. `hermes`): the prompt goes in on
    /// stdin, the reply comes back on stdout. Binary + flags are configured on
    /// the runtime, so any such agent can be wired without new code.
    Hermes,
    /// Google's Antigravity CLI (`agy -p …`), driven headless. This is the CLI
    /// that serves Google AI Pro / Ultra sign-ins since Gemini CLI stopped
    /// serving individual accounts (2026-06-18). Uses its own login.
    Antigravity,
    /// GitHub's Copilot CLI (`copilot -p …`), driven headless. Uses its own
    /// `copilot login` (or a token in its documented env vars).
    Copilot,
}

impl ModelProviderKind {
    /// True for CLI coding agents Hive runs as a subprocess, which bring their
    /// own tools and (for the subscription CLIs) their own login.
    pub fn is_cli_agent(self) -> bool {
        matches!(
            self,
            Self::Aider
                | Self::Pi
                | Self::ClaudeCode
                | Self::Codex
                | Self::Hermes
                | Self::Antigravity
                | Self::Copilot
        )
    }

    /// CLI agents whose permission tier follows Hive's Claude Code permission
    /// setting (read-only by default; edits / commands / bypass when raised).
    pub fn follows_permission_mode(self) -> bool {
        matches!(self, Self::ClaudeCode | Self::Antigravity | Self::Copilot)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeLocation {
    Local,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCapabilities {
    pub supports_embeddings: bool,
    pub supports_tools: bool,
    pub supports_streaming: bool,
    pub supports_agent_orchestration: bool,
    /// Optional context-window override (tokens). When unset, the
    /// `ModelContextWindow` default table is used. Set via `context_window`
    /// in a `[[runtimes]]` block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u32>,
}

impl Default for RuntimeCapabilities {
    fn default() -> Self {
        Self {
            supports_embeddings: false,
            supports_tools: false,
            supports_streaming: true,
            supports_agent_orchestration: false,
            context_window_tokens: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTarget {
    pub id: String,
    pub name: String,
    pub provider_kind: ModelProviderKind,
    pub location: RuntimeLocation,
    pub model_id: String,
    #[serde(default)]
    pub available_models: Vec<String>,
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_keep_alive: Option<String>,
    /// Ollama `think`: let a reasoning model think before answering. `None`
    /// ⇒ Hive's default (off). Set via `think` in a `[[runtimes]]` block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think: Option<bool>,
    /// Model rounds per tool-using turn for an HTTP runtime (native Ollama,
    /// OpenAI wire). `None` ⇒ Hive's default (24). Set via `max_tool_rounds`
    /// in a `[[runtimes]]` block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<u32>,
    /// Offer Hive's built-in workspace file tools even though the endpoint's
    /// host isn't local or private — the user accepts that file contents are
    /// sent to it. **Device-local**: never read from or written to any config
    /// or catalog (a shared `hive.config.toml` must not be able to turn it
    /// on). The app fills it in from this device's settings, bound to the
    /// runtime's endpoint origin.
    #[serde(skip)]
    pub allow_remote_file_tools: bool,
    #[serde(default)]
    pub estimated_performance_score: f64,
    #[serde(default)]
    pub estimated_cost_per_1m_input_tokens_usd: f64,
    #[serde(default)]
    pub capabilities: RuntimeCapabilities,
}

impl RuntimeTarget {
    pub fn display_label(&self) -> String {
        let model = self.model_id.trim();
        if model.is_empty() {
            self.name.clone()
        } else {
            format!("{} · {}", self.name, model)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_provider_kind_uses_swift_raw_value() {
        // Swift's ModelProviderKind.openAI raw value is "openAI".
        let json = serde_json::to_string(&ModelProviderKind::OpenAI).unwrap();
        assert_eq!(json, "\"openAI\"");
        let back: ModelProviderKind = serde_json::from_str("\"openAI\"").unwrap();
        assert_eq!(back, ModelProviderKind::OpenAI);
    }

    #[test]
    fn cli_runtime_kinds_round_trip() {
        for (kind, raw) in [(ModelProviderKind::Antigravity, "\"antigravity\""), (ModelProviderKind::Copilot, "\"copilot\"")] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), raw);
            assert_eq!(serde_json::from_str::<ModelProviderKind>(raw).unwrap(), kind);
            assert!(kind.is_cli_agent() && kind.follows_permission_mode());
        }
        assert!(!ModelProviderKind::Ollama.is_cli_agent());
        assert!(!ModelProviderKind::Codex.follows_permission_mode());
    }

    #[test]
    fn display_label_combines_name_and_model() {
        let rt = RuntimeTarget {
            id: "r1".into(),
            name: "Local".into(),
            provider_kind: ModelProviderKind::Ollama,
            location: RuntimeLocation::Local,
            model_id: "qwen2.5".into(),
            available_models: vec![],
            endpoint: "http://localhost:11434".into(),
            metrics_endpoint: None,
            model_provider_id: None,
            model_base_url: None,
            request_keep_alive: None,
            think: None,
            max_tool_rounds: None,
            allow_remote_file_tools: false,
            estimated_performance_score: 0.0,
            estimated_cost_per_1m_input_tokens_usd: 0.0,
            capabilities: RuntimeCapabilities::default(),
        };
        assert_eq!(rt.display_label(), "Local · qwen2.5");
    }
}
