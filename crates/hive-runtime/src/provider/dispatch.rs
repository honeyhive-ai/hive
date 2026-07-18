//! Provider dispatch — resolves which client executes a turn based on the
//! responding participant's runtime, then streams the reply. This is the seam
//! that makes BYO runtimes real: an `@agent` bound to an OpenAI/Ollama runtime
//! actually runs there, and aider/pi/claude-code runtimes run as subprocesses.

use hive_core::ModelProviderKind;

use super::anthropic::{AnthropicClient, ChatTurn, ProviderError};
use super::openai::OpenAiClient;
use super::subprocess;

/// A runtime resolved to everything needed to execute against it.
#[derive(Debug, Clone)]
pub struct ResolvedRuntime {
    pub provider: ModelProviderKind,
    pub model: String,
    /// OpenAI-compatible chat-completions URL, or the subprocess program path.
    pub endpoint: String,
    pub api_key: Option<String>,
    /// Subprocess args (aider/pi/claude-code).
    pub args: Vec<String>,
    /// For subprocess agents that can target an OpenAI-compatible backend
    /// (e.g. `pi` → a local Ollama): the provider id and base URL. When set,
    /// the pi bridge bootstraps a provider config pointing here.
    pub model_provider_id: Option<String>,
    pub model_base_url: Option<String>,
}

impl ResolvedRuntime {
    /// True for runtimes executed by spawning an external CLI.
    pub fn is_subprocess(&self) -> bool {
        matches!(
            self.provider,
            ModelProviderKind::Aider | ModelProviderKind::Pi | ModelProviderKind::ClaudeCode
        )
    }
}

/// Stream a reply against `rt`, invoking `on_delta` for each fragment and
/// returning the assembled body.
pub async fn stream(
    rt: &ResolvedRuntime,
    system: Option<&str>,
    turns: &[ChatTurn],
    working_dir: Option<&str>,
    // Extra process env for subprocess agents (e.g. GIT_AUTHOR_* for commit
    // attribution); ignored by HTTP providers (Anthropic/OpenAI).
    extra_env: &[(String, String)],
    max_tokens: u32,
    on_delta: impl FnMut(String),
) -> Result<String, ProviderError> {
    match rt.provider {
        ModelProviderKind::Anthropic => {
            let key = rt.api_key.as_deref().unwrap_or_default();
            AnthropicClient::new()
                .stream_reply(key, &rt.model, system, turns, max_tokens, on_delta)
                .await
        }
        ModelProviderKind::OpenAI
        | ModelProviderKind::OpenRouter
        | ModelProviderKind::Ollama
        | ModelProviderKind::Custom
        | ModelProviderKind::HiveDaemon
        | ModelProviderKind::Azure => {
            // Azure OpenAI speaks the same wire format but authenticates with an
            // `api-key` header instead of a bearer token.
            OpenAiClient::new(&rt.endpoint)
                .with_api_key_header(rt.provider == ModelProviderKind::Azure)
                .stream_reply(rt.api_key.as_deref(), &rt.model, system, turns, on_delta)
                .await
        }
        ModelProviderKind::ClaudeCode => {
            // BYO subscription via the `claude` CLI in stream-json mode (no API
            // key). endpoint = the binary (default "claude").
            super::claude_code::stream_reply(
                &rt.endpoint,
                &rt.args,
                working_dir,
                extra_env,
                system,
                turns,
                on_delta,
            )
            .await
        }
        ModelProviderKind::Pi => {
            // `pi` is interactive by default; `-p` runs one-shot and takes the
            // prompt as a positional argument (not stdin). When the runtime
            // carries an OpenAI-compatible base URL (e.g. a local Ollama), we
            // bootstrap a temp provider config and point PI_CODING_AGENT_DIR at
            // it — the parity feature from the Swift bridge.
            let prompt = subprocess::render_prompt(system, turns);
            let provider_id = rt
                .model_provider_id
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "ollama".to_string());

            // Match the Swift static args (ephemeral, offline, plain text).
            let mut args: Vec<String> = vec![
                "-p".into(),
                "--no-session".into(),
                "--offline".into(),
                "--mode".into(),
                "text".into(),
            ];
            if !rt.model.is_empty() {
                // `--provider` only when the model isn't already "provider/id".
                if !rt.model.contains('/') {
                    args.push("--provider".into());
                    args.push(provider_id.clone());
                }
                args.push("--model".into());
                args.push(rt.model.clone());
            }
            args.extend(rt.args.iter().cloned());
            args.push(prompt);

            // Bootstrap a provider config dir if a base URL is configured.
            let config_dir = rt.model_base_url.as_deref().filter(|s| !s.is_empty()).and_then(
                |base_url| {
                    subprocess::bootstrap_pi_provider(&provider_id, base_url, &[rt.model.clone()])
                        .map_err(|e| eprintln!("pi: failed to write provider config: {e}"))
                        .ok()
                },
            );
            let mut envs: Vec<(String, String)> = config_dir
                .as_ref()
                .map(|d| vec![("PI_CODING_AGENT_DIR".to_string(), d.display().to_string())])
                .unwrap_or_default();
            envs.extend(extra_env.iter().cloned());

            let result = subprocess::run_with(
                &rt.endpoint,
                &args,
                working_dir,
                &envs,
                subprocess::PromptInput::InArgs,
                on_delta,
            )
            .await;

            if let Some(dir) = config_dir {
                let _ = std::fs::remove_dir_all(dir);
            }
            result
        }
        ModelProviderKind::Aider => {
            let input = subprocess::render_prompt(system, turns);
            subprocess::run_with(
                &rt.endpoint,
                &rt.args,
                working_dir,
                extra_env,
                subprocess::PromptInput::Stdin(&input),
                on_delta,
            )
            .await
        }
    }
}

/// Default OpenAI-compatible endpoint for a provider kind when the runtime
/// config doesn't specify one.
pub fn default_endpoint(provider: ModelProviderKind) -> &'static str {
    match provider {
        ModelProviderKind::OpenAI => "https://api.openai.com/v1/chat/completions",
        ModelProviderKind::OpenRouter => "https://openrouter.ai/api/v1/chat/completions",
        ModelProviderKind::Ollama => "http://localhost:11434/v1/chat/completions",
        _ => "",
    }
}

/// One known OpenAI-compatible backend, for one-click provider setup in the UI.
pub struct ProviderPreset {
    /// Display label, e.g. "Google Gemini".
    pub label: &'static str,
    /// The provider kind to store (most are `Custom`; Azure uses its own kind
    /// because it authenticates differently).
    pub provider: ModelProviderKind,
    /// Full chat-completions URL; empty when it's deployment-specific (Azure).
    pub endpoint: &'static str,
    /// Whether an API key is expected (false for purely-local servers).
    pub needs_key: bool,
}

/// Known OpenAI-compatible endpoints offered as presets. They all route through
/// the OpenAI-compatible client; Gemini/LM Studio/Groq/Together need only a base
/// URL + key, while Azure additionally swaps bearer auth for an `api-key` header.
pub fn provider_presets() -> Vec<ProviderPreset> {
    use ModelProviderKind::*;
    vec![
        ProviderPreset { label: "OpenAI", provider: OpenAI, endpoint: "https://api.openai.com/v1/chat/completions", needs_key: true },
        ProviderPreset { label: "OpenRouter", provider: OpenRouter, endpoint: "https://openrouter.ai/api/v1/chat/completions", needs_key: true },
        ProviderPreset { label: "Ollama (local)", provider: Ollama, endpoint: "http://localhost:11434/v1/chat/completions", needs_key: false },
        ProviderPreset { label: "LM Studio (local)", provider: Custom, endpoint: "http://localhost:1234/v1/chat/completions", needs_key: false },
        ProviderPreset { label: "Google Gemini", provider: Custom, endpoint: "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", needs_key: true },
        ProviderPreset { label: "Groq", provider: Custom, endpoint: "https://api.groq.com/openai/v1/chat/completions", needs_key: true },
        ProviderPreset { label: "Together", provider: Custom, endpoint: "https://api.together.xyz/v1/chat/completions", needs_key: true },
        ProviderPreset { label: "Azure OpenAI", provider: Azure, endpoint: "", needs_key: true },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_subprocess_providers() {
        let mut rt = ResolvedRuntime {
            provider: ModelProviderKind::Pi,
            model: "pi".into(),
            endpoint: "pi".into(),
            api_key: None,
            args: vec![],
            model_provider_id: None,
            model_base_url: None,
        };
        assert!(rt.is_subprocess());
        rt.provider = ModelProviderKind::Anthropic;
        assert!(!rt.is_subprocess());
    }

    #[test]
    fn default_endpoints_known() {
        assert!(default_endpoint(ModelProviderKind::OpenAI).contains("openai.com"));
        assert!(default_endpoint(ModelProviderKind::OpenRouter).contains("openrouter"));
    }

    #[test]
    fn presets_cover_gemini_lmstudio_azure() {
        let ps = provider_presets();
        let gemini = ps.iter().find(|p| p.label == "Google Gemini").unwrap();
        assert!(gemini.endpoint.contains("generativelanguage.googleapis.com"));
        assert_eq!(gemini.provider, ModelProviderKind::Custom);

        let azure = ps.iter().find(|p| p.label == "Azure OpenAI").unwrap();
        assert_eq!(azure.provider, ModelProviderKind::Azure);
        assert!(azure.endpoint.is_empty()); // deployment-specific; user fills it

        let lm = ps.iter().find(|p| p.label.starts_with("LM Studio")).unwrap();
        assert!(!lm.needs_key); // local server
    }
}
