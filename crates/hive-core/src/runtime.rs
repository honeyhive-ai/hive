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
            estimated_performance_score: 0.0,
            estimated_cost_per_1m_input_tokens_usd: 0.0,
            capabilities: RuntimeCapabilities::default(),
        };
        assert_eq!(rt.display_label(), "Local · qwen2.5");
    }
}
