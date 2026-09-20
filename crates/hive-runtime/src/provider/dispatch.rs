//! Provider dispatch — resolves which client executes a turn based on the
//! responding participant's runtime, then streams the reply. This is the seam
//! that makes BYO runtimes real: an `@agent` bound to an OpenAI/Ollama runtime
//! actually runs there, and aider/pi/claude-code runtimes run as subprocesses.

use hive_core::ModelProviderKind;

use super::anthropic::{AnthropicClient, ChatTurn, ProviderError};
use super::ollama::{ChatOptions, OllamaClient};
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
    /// Explicit context-window override from the runtime's settings (tokens).
    /// `None` ⇒ infer from the model name. Local servers (Ollama) default to a
    /// small window regardless of what the model *could* take, so the user's
    /// setting must win over the name-based guess.
    pub context_window_tokens: Option<u32>,
    /// Ollama `keep_alive`: how long the model stays loaded after a request
    /// ("5m", "-1" = forever). `None` ⇒ server default.
    pub keep_alive: Option<String>,
    /// Ollama `think`: ask a reasoning model to think (`Some(true)`) or not
    /// (`Some(false)`); `None` ⇒ Hive's default, which is off.
    pub think: Option<bool>,
}

/// Env override forcing Ollama runtimes through the OpenAI-compatible `/v1`
/// shim instead of the native API (`HIVE_OLLAMA_WIRE=openai`). Escape hatch
/// for a proxy that only speaks OpenAI; loses num_ctx/keep_alive/think.
pub fn ollama_uses_openai_shim() -> bool {
    std::env::var("HIVE_OLLAMA_WIRE")
        .map(|v| v.trim().eq_ignore_ascii_case("openai"))
        .unwrap_or(false)
}

/// The native-Ollama request options for a runtime, given what the capability
/// probe reported (if anything). The server is told to allocate the window the
/// planner budgets against (override or model-name guess), capped by the
/// model's maximum; otherwise Ollama serves a small default and truncates the
/// *start* of the prompt — the system prompt — while the planner believes it
/// fits. Thinking is off unless the runtime opts in; when the probe says the
/// model can't think, the field is omitted entirely.
pub fn ollama_chat_options(rt: &ResolvedRuntime, caps: Option<&super::ollama::ModelCapabilities>) -> ChatOptions {
    let mut num_ctx = rt.context_window();
    if let Some(max) = caps.and_then(|c| c.context_length) {
        num_ctx = num_ctx.min(max);
    }
    let think = match (rt.think, caps) {
        (_, Some(c)) if !c.thinking => None,
        (Some(t), _) => Some(t),
        (None, _) => Some(false),
    };
    ChatOptions { num_ctx: Some(num_ctx), keep_alive: rt.keep_alive.clone().filter(|s| !s.trim().is_empty()), think }
}

impl ResolvedRuntime {
    /// Effective context window: the explicit override, else the model-name
    /// table in `hive_core::context_budget`.
    pub fn context_window(&self) -> u32 {
        match self.context_window_tokens {
            Some(n) if n > 0 => n,
            _ => hive_core::context_budget::model_context_window::tokens_for_model(&self.model),
        }
    }

    /// True for runtimes reached over an HTTP model endpoint with no tool loop
    /// (OpenAI-compatible wire, or Ollama's native API). Subprocess agents and
    /// Anthropic are excluded.
    pub fn is_openai_wire(&self) -> bool {
        matches!(
            self.provider,
            ModelProviderKind::OpenAI
                | ModelProviderKind::OpenRouter
                | ModelProviderKind::Ollama
                | ModelProviderKind::Custom
                | ModelProviderKind::HiveDaemon
                | ModelProviderKind::Azure
        )
    }

    /// True for runtimes executed by spawning an external CLI.
    pub fn is_subprocess(&self) -> bool {
        matches!(
            self.provider,
            ModelProviderKind::Aider | ModelProviderKind::Pi | ModelProviderKind::ClaudeCode
        )
    }
}

/// A live "background processing" signal from a subprocess agent (Claude Code):
/// the tool it's calling, a tool's result, or a thinking marker. Surfaced to the
/// UI while the turn runs; ephemeral (never persisted). Only providers whose
/// stream carries structured tool events emit these — HTTP providers don't.
#[derive(Debug, Clone)]
pub enum StreamActivity {
    /// A tool the agent invoked (e.g. Read/Bash/Edit). `input_json` is the raw
    /// arguments object.
    Tool {
        id: String,
        name: String,
        input_json: String,
    },
    /// The result of a prior [`StreamActivity::Tool`], matched by `call_id`.
    ToolResult {
        call_id: String,
        is_error: bool,
        content: String,
    },
    /// The agent produced reasoning this step. `text` is the fragment when
    /// the provider streams it (OpenAI-wire `reasoning` deltas / `<think>`
    /// blocks); empty for providers that only signal a marker (Claude Code).
    Thinking { text: String },
}

/// Error-message label for an OpenAI-wire provider kind.
pub fn provider_label(provider: ModelProviderKind) -> &'static str {
    match provider {
        ModelProviderKind::Anthropic => "anthropic",
        ModelProviderKind::OpenAI => "openai",
        ModelProviderKind::OpenRouter => "openrouter",
        ModelProviderKind::Ollama => "ollama",
        ModelProviderKind::Azure => "azure",
        ModelProviderKind::Custom => "endpoint",
        ModelProviderKind::HiveDaemon => "hive-daemon",
        ModelProviderKind::Aider => "aider",
        ModelProviderKind::Pi => "pi",
        ModelProviderKind::ClaudeCode => "claude-code",
        ModelProviderKind::Codex => "codex",
        ModelProviderKind::Hermes => "hermes",
    }
}

/// Stream a reply against `rt`, invoking `on_delta` for each fragment and
/// returning the assembled body. A reasoning model's chain of thought (Ollama /
/// OpenRouter `reasoning` deltas, or inline `<think>` blocks) is delivered as
/// [`StreamActivity::Thinking`] and is never part of the returned body.
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
    // Live tool/thinking activity: subprocess agents emit tool calls/results;
    // OpenAI-wire providers emit reasoning fragments.
    mut on_activity: impl FnMut(StreamActivity),
) -> Result<String, ProviderError> {
    match rt.provider {
        ModelProviderKind::Anthropic => {
            let key = rt.api_key.as_deref().unwrap_or_default();
            AnthropicClient::new()
                .stream_reply(key, &rt.model, system, turns, max_tokens, on_delta)
                .await
        }
        ModelProviderKind::Ollama if !ollama_uses_openai_shim() => {
            // Native API: carries num_ctx / keep_alive / think, which the `/v1`
            // shim drops. The endpoint may be stored in either spelling.
            let client = OllamaClient::new(&rt.endpoint);
            let caps = client.capabilities_cached(&rt.model).await;
            let opts = ollama_chat_options(rt, caps.as_ref());
            client
                .stream_chat(&rt.model, system, turns, &opts, on_delta, |text| {
                    on_activity(StreamActivity::Thinking { text })
                })
                .await
                .map(|out| out.text)
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
                .with_provider_label(provider_label(rt.provider))
                .stream_reply_with_thinking(
                    rt.api_key.as_deref(),
                    &rt.model,
                    system,
                    turns,
                    on_delta,
                    |text| on_activity(StreamActivity::Thinking { text }),
                )
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
                on_activity,
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
        ModelProviderKind::Codex => {
            // OpenAI Codex CLI, headless: `codex exec --json … <prompt>` in a
            // workspace-write sandbox (it edits files in the working dir, which
            // is an isolated worktree upstream). The bridge parses codex's JSON
            // events and surfaces the agent's reply. endpoint = the binary
            // (default "codex"); model → -m; runtime args are appended.
            super::codex::stream_reply(
                &rt.endpoint,
                &rt.model,
                &rt.args,
                working_dir,
                extra_env,
                system,
                turns,
                on_delta,
            )
            .await
        }
        ModelProviderKind::Hermes => {
            // Generic stdin-driven CLI agent: feed the rendered prompt on stdin
            // and stream stdout, with the binary (`endpoint`, default "hermes")
            // and any flags (`args`) configured on the runtime. This is the same
            // shape as aider, so any prompt-on-stdin agent works with no new code.
            let input = subprocess::render_prompt(system, turns);
            let program = if rt.endpoint.is_empty() { "hermes" } else { &rt.endpoint };
            subprocess::run_with(
                program,
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
    use super::super::ollama::ModelCapabilities;

    fn ollama_rt() -> ResolvedRuntime {
        ResolvedRuntime {
            provider: ModelProviderKind::Ollama,
            model: "qwen3.5".into(),
            endpoint: "http://100.64.0.5:11434/v1/chat/completions".into(),
            api_key: None,
            args: vec![],
            model_provider_id: None,
            model_base_url: None,
            context_window_tokens: None,
            keep_alive: None,
            think: None,
        }
    }

    #[test]
    fn ollama_options_default_to_planner_window_and_thinking_off() {
        let rt = ollama_rt();
        let o = ollama_chat_options(&rt, None);
        // No override → the model-name guess the planner budgets against.
        assert_eq!(o.num_ctx, Some(rt.context_window()));
        assert_eq!(o.think, Some(false), "thinking is off by default");
        assert_eq!(o.keep_alive, None);
    }

    #[test]
    fn ollama_options_honor_override_capped_by_model_max_and_opt_in() {
        let mut rt = ollama_rt();
        rt.context_window_tokens = Some(131_072);
        rt.think = Some(true);
        rt.keep_alive = Some("-1".into());
        let caps = ModelCapabilities { thinking: true, context_length: Some(40_960), ..Default::default() };
        let o = ollama_chat_options(&rt, Some(&caps));
        assert_eq!(o.num_ctx, Some(40_960), "override capped at the model's max");
        assert_eq!(o.think, Some(true));
        assert_eq!(o.keep_alive.as_deref(), Some("-1"));
        // Blank keep-alive is dropped.
        rt.keep_alive = Some("  ".into());
        assert_eq!(ollama_chat_options(&rt, None).keep_alive, None);
    }

    #[test]
    fn ollama_options_omit_think_when_the_model_cannot() {
        let mut rt = ollama_rt();
        rt.think = Some(true);
        let caps = ModelCapabilities { thinking: false, ..Default::default() };
        assert_eq!(ollama_chat_options(&rt, Some(&caps)).think, None);
    }

    #[test]
    fn openai_shim_is_opt_in_via_env() {
        // Default: native. (The env var is process-global; only assert the
        // default here rather than mutating it under parallel tests.)
        if std::env::var("HIVE_OLLAMA_WIRE").is_err() {
            assert!(!ollama_uses_openai_shim());
        }
    }

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
            context_window_tokens: None,
            keep_alive: None,
            think: None,
        };
        assert!(rt.is_subprocess());
        rt.provider = ModelProviderKind::Anthropic;
        assert!(!rt.is_subprocess());
        assert!(!rt.is_openai_wire());
        rt.provider = ModelProviderKind::Ollama;
        assert!(rt.is_openai_wire());
    }

    #[test]
    fn context_window_override_beats_model_name_guess() {
        let mut rt = ResolvedRuntime {
            provider: ModelProviderKind::Ollama,
            model: "qwen3.5".into(),
            endpoint: "http://100.64.0.5:11434/v1/chat/completions".into(),
            api_key: None,
            args: vec![],
            model_provider_id: None,
            model_base_url: None,
            context_window_tokens: None,
            keep_alive: None,
            think: None,
        };
        // Name-based guess for a qwen model.
        assert_eq!(rt.context_window(), 32_768);
        // The runtime's explicit setting wins (Ollama's real default is 4k).
        rt.context_window_tokens = Some(4096);
        assert_eq!(rt.context_window(), 4096);
        // Zero is "unset".
        rt.context_window_tokens = Some(0);
        assert_eq!(rt.context_window(), 32_768);
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
