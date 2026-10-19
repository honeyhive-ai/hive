//! Provider dispatch — resolves which client executes a turn based on the
//! responding participant's runtime, then streams the reply. This is the seam
//! that makes BYO runtimes real: an `@agent` bound to an OpenAI/Ollama runtime
//! actually runs there, and aider/pi/claude-code runtimes run as subprocesses.

use hive_core::ModelProviderKind;
use serde_json::{json, Map, Value};

use super::anthropic::{AnthropicClient, ChatTurn, ProviderError};
use super::ollama::{ChatOptions, OllamaClient};
use super::openai::OpenAiClient;
use super::subprocess;
use crate::tool_loop::ToolExecutor;

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
    /// The runtime's "Supports tools" switch: opts an HTTP model runtime into
    /// the MCP tool loop. Native Ollama additionally needs the capability
    /// probe to say the model can call tools; on the OpenAI wire the switch is
    /// the gate (a server that rejects `tools` falls back to plain chat).
    pub tools: bool,
    /// Model rounds per tool-using turn (the runtime's setting). `None` ⇒
    /// [`crate::agent_tools::tool_round_budget`]'s default for the provider.
    pub max_tool_rounds: Option<u32>,
    /// The runtime's "Allow workspace file tools for this endpoint" setting:
    /// offer Hive's built-in file tools even when the endpoint's host isn't
    /// local or private (they send file contents to it).
    pub allow_remote_file_tools: bool,
}

/// Tools to offer a turn: neutral definitions (`{name, description,
/// input_schema}`) and the executor that runs them. Consumed by
/// [`stream_with_tools`] for native Ollama and every OpenAI-wire provider;
/// Anthropic has its own loop in the app.
pub struct ToolSet<'a, E: ToolExecutor> {
    pub defs: &'a [Value],
    pub executor: &'a E,
    /// Model rounds per turn, including the final tool-less one.
    pub max_rounds: usize,
}

/// The executor type for a turn that offers no tools.
pub struct NoTools;
impl ToolExecutor for NoTools {
    async fn call(&self, name: &str, _input: &Value) -> (String, bool) {
        (format!("no tools are available (asked for {name})"), true)
    }
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

/// Whether a native-Ollama turn may carry tool definitions: the runtime opted
/// in AND the probe says the model can call tools. No probe (server
/// unreachable at probe time, or an old server without `capabilities`) means
/// no tools — a model that can't parse them would answer with junk.
pub fn ollama_tools_allowed(rt: &ResolvedRuntime, caps: Option<&super::ollama::ModelCapabilities>) -> bool {
    rt.tools && caps.map(|c| c.tools).unwrap_or(false)
}

/// Whether an OpenAI-wire runtime may carry tool definitions: the runtime
/// opted in AND it may front a local server (see
/// [`ResolvedRuntime::runs_agent_tools`]). There is no capability probe on
/// this wire; a server that can't take `tools` says so with a 400 and the
/// client retries without them.
pub fn openai_tools_allowed(rt: &ResolvedRuntime) -> bool {
    rt.runs_agent_tools() && rt.is_openai_wire()
}

/// Per-model request fields for an OpenAI-wire runtime, where they're safe:
/// only runtimes that may front a local server (Custom, HiveDaemon) get them —
/// hosted APIs reject unknown fields. A server that still refuses one gets the
/// request again without them (see [`OpenAiClient::with_extra_body`]).
///
/// The model's family profile (`hive_core::model_profile`) picks the fields:
/// - `chat_template_kwargs.enable_thinking` follows the runtime's think
///   setting (Hive's default is off). llama.cpp hands it to the model's chat
///   template: gemma4 / qwen3 templates honour it, others ignore it. Skipped
///   for families profiled as never reasoning (LFM2, embedders, rerankers).
/// - Families that always reason with an effort knob (gpt-oss) get
///   `reasoning_effort: "low"` with thinking off (its template ignores
///   `enable_thinking`).
///
/// No output cap is set: the OpenAI wire sends no `max_tokens`, so a
/// reasoning model's answer is bounded only by the server's slot — never by
/// the 1024 the Anthropic path passes.
pub fn openai_request_extras(rt: &ResolvedRuntime) -> Map<String, Value> {
    let mut extra = Map::new();
    if !matches!(rt.provider, ModelProviderKind::Custom | ModelProviderKind::HiveDaemon) {
        return extra;
    }
    use hive_core::model_profile::{profile_for, ReasoningStyle};
    let think = rt.think.unwrap_or(false);
    // Unknown models keep the kwargs: a template that doesn't know the flag
    // ignores it.
    let reasoning = profile_for(&rt.model).map(|p| p.reasoning);
    if reasoning != Some(ReasoningStyle::None) {
        extra.insert("chat_template_kwargs".into(), json!({ "enable_thinking": think }));
    }
    if !think && reasoning == Some(ReasoningStyle::AlwaysWithEffort) {
        extra.insert("reasoning_effort".into(), json!("low"));
    }
    extra
}

/// The window an HTTP tool loop guards against (0 = don't guard): the
/// planner's window for runtimes that may be a local server, where the slot
/// is small and overflowing it fails the request. Hosted APIs aren't guarded —
/// the name table's fallback would understate their windows.
pub fn tool_loop_context_window(rt: &ResolvedRuntime) -> u32 {
    if rt.probes_server_props() || rt.provider == ModelProviderKind::Ollama {
        rt.context_window()
    } else {
        0
    }
}

/// Ask a local OpenAI-compatible server (`llama-server`) for its per-slot
/// context window and cache the answer, so [`ResolvedRuntime::context_window`]
/// can use it. Only for runtimes that may front a local server (Custom,
/// HiveDaemon) and have no explicit override; a no-op otherwise. Never fails.
pub async fn warm_context_probe(rt: &ResolvedRuntime) {
    if !rt.probes_server_props() || rt.context_window_tokens.is_some_and(|n| n > 0) {
        return;
    }
    OpenAiClient::new(&rt.endpoint)
        .with_provider_label(provider_label(rt.provider))
        .probe_context_window(rt.api_key.as_deref(), &rt.model)
        .await;
}

impl ResolvedRuntime {
    /// Effective context window: the explicit override, else what a local
    /// server reported (`/props`, cached by [`warm_context_probe`]), else the
    /// model-name table in `hive_core::context_budget`. Native Ollama uses
    /// the name table without the open-model family profiles: this value is
    /// the `num_ctx` Ollama allocates, and the profiles' 32k (a llama.cpp
    /// gateway slot) would quadruple an existing 8k runtime's KV cache.
    pub fn context_window(&self) -> u32 {
        use hive_core::context_budget::model_context_window::{name_table_window, tokens_for_model};
        match self.context_window_tokens {
            Some(n) if n > 0 => n,
            _ if self.provider == ModelProviderKind::Ollama => name_table_window(&self.model),
            _ => self
                .probes_server_props()
                .then(|| super::openai::cached_context_window(&self.endpoint, &self.model))
                .flatten()
                .unwrap_or_else(|| tokens_for_model(&self.model)),
        }
    }

    /// Runtimes whose endpoint may be a local `llama-server` worth asking
    /// for its window. Hosted providers (OpenAI, OpenRouter, Azure) and
    /// Ollama (native probe) are excluded.
    pub fn probes_server_props(&self) -> bool {
        matches!(self.provider, ModelProviderKind::Custom | ModelProviderKind::HiveDaemon) && !self.endpoint.is_empty()
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

    /// Whether this runtime's "Supports tools" switch turns on Hive's agent
    /// tool loop (MCP + built-in file tools): only runtimes that may front a
    /// local or self-hosted server — Custom, HiveDaemon and Ollama. Hosted
    /// APIs (OpenAI, OpenRouter, Azure) never get it: the switch defaulted on
    /// for them back when it did nothing on this wire, so it is not consent to
    /// send workspace files to a third-party cloud.
    pub fn runs_agent_tools(&self) -> bool {
        self.tools
            && matches!(
                self.provider,
                ModelProviderKind::Custom | ModelProviderKind::HiveDaemon | ModelProviderKind::Ollama
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
    on_activity: impl FnMut(StreamActivity),
) -> Result<String, ProviderError> {
    stream_with_tools(rt, system, turns, working_dir, extra_env, max_tokens, None::<ToolSet<'_, NoTools>>, on_delta, on_activity)
        .await
}

/// [`stream`], with `tools` on offer. A native-Ollama runtime runs the tool
/// loop when the runtime opts in (`tools` switch), the probe reports tool
/// support, and `tools` is non-empty; an OpenAI-wire runtime (OpenAI,
/// OpenRouter, Azure, Custom/llama.cpp, HiveDaemon, the Ollama shim) runs it
/// when it opts in and `tools` is non-empty. Tool calls and results are
/// surfaced as [`StreamActivity`] like a subprocess agent's. Otherwise `tools`
/// is ignored and the turn streams as plain chat.
#[allow(clippy::too_many_arguments)]
pub async fn stream_with_tools<E: ToolExecutor>(
    rt: &ResolvedRuntime,
    system: Option<&str>,
    turns: &[ChatTurn],
    working_dir: Option<&str>,
    extra_env: &[(String, String)],
    max_tokens: u32,
    tools: Option<ToolSet<'_, E>>,
    on_delta: impl FnMut(String),
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
            match tools.filter(|t| !t.defs.is_empty()) {
                Some(t) if ollama_tools_allowed(rt, caps.as_ref()) => {
                    tracing::info!(target: "dispatch", host = %client.host(), model = %rt.model, tools = t.defs.len(), "ollama turn with tools");
                    // The identity block told the model it has no tools; it does now.
                    let system = system.map(|s| crate::prompt::with_tools_offered_for(s, t.defs));
                    client
                        .stream_chat_with_tools(
                            &rt.model,
                            system.as_deref(),
                            turns,
                            &opts,
                            t.defs,
                            t.executor,
                            t.max_rounds,
                            on_delta,
                            on_activity,
                        )
                        .await
                        .map(|out| out.text)
                }
                offered => {
                    if offered.is_some() {
                        tracing::info!(
                            target: "dispatch",
                            host = %client.host(),
                            model = %rt.model,
                            opted_in = rt.tools,
                            probe = ?caps.as_ref().map(|c| c.tools),
                            "ollama turn: tools requested but not offered"
                        );
                    }
                    client
                        .stream_chat(&rt.model, system, turns, &opts, on_delta, |text| {
                            on_activity(StreamActivity::Thinking { text })
                        })
                        .await
                        .map(|out| out.text)
                }
            }
        }
        ModelProviderKind::OpenAI
        | ModelProviderKind::OpenRouter
        | ModelProviderKind::Ollama
        | ModelProviderKind::Custom
        | ModelProviderKind::HiveDaemon
        | ModelProviderKind::Azure => {
            // Azure OpenAI speaks the same wire format but authenticates with an
            // `api-key` header instead of a bearer token.
            let client = OpenAiClient::new(&rt.endpoint)
                .with_api_key_header(rt.provider == ModelProviderKind::Azure)
                .with_provider_label(provider_label(rt.provider))
                .with_extra_body(openai_request_extras(rt))
                .with_context_window(tool_loop_context_window(rt));
            match tools.filter(|t| !t.defs.is_empty()) {
                Some(t) if openai_tools_allowed(rt) => {
                    tracing::info!(target: "dispatch", provider = provider_label(rt.provider), host = %client.host(), model = %rt.model, tools = t.defs.len(), "openai-wire turn with tools");
                    // The identity block told the model it has no tools; it does now.
                    let system = system.map(|s| crate::prompt::with_tools_offered_for(s, t.defs));
                    client
                        .stream_chat_with_tools(
                            rt.api_key.as_deref(),
                            &rt.model,
                            system.as_deref(),
                            turns,
                            t.defs,
                            t.executor,
                            t.max_rounds,
                            on_delta,
                            on_activity,
                        )
                        .await
                        .map(|out| out.text)
                }
                _ => {
                    client
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
            }
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
    /// Whether the new runtime should offer tools (MCP + Hive's built-ins).
    pub tools: bool,
}

/// Endpoint template for the local llama.cpp gateway preset; the user
/// replaces `<host>` with the gateway's address.
pub const LOCAL_GATEWAY_ENDPOINT: &str = "http://<host>:8080/v1/chat/completions";

/// Known OpenAI-compatible endpoints offered as presets. They all route through
/// the OpenAI-compatible client; Gemini/LM Studio/Groq/Together need only a base
/// URL + key, while Azure additionally swaps bearer auth for an `api-key` header.
pub fn provider_presets() -> Vec<ProviderPreset> {
    use ModelProviderKind::*;
    vec![
        ProviderPreset { label: "OpenAI", provider: OpenAI, endpoint: "https://api.openai.com/v1/chat/completions", needs_key: true, tools: true },
        ProviderPreset { label: "OpenRouter", provider: OpenRouter, endpoint: "https://openrouter.ai/api/v1/chat/completions", needs_key: true, tools: true },
        ProviderPreset { label: "Ollama (local)", provider: Ollama, endpoint: "http://localhost:11434/v1/chat/completions", needs_key: false, tools: true },
        // llama.cpp behind an OpenAI-compatible gateway: the fast local path
        // (5.8x Ollama's prefill on ROCm in Hive's lab), tool calls verified.
        ProviderPreset { label: "Local gateway (llama.cpp, OpenAI-compatible)", provider: Custom, endpoint: LOCAL_GATEWAY_ENDPOINT, needs_key: false, tools: true },
        ProviderPreset { label: "LM Studio (local)", provider: Custom, endpoint: "http://localhost:1234/v1/chat/completions", needs_key: false, tools: true },
        ProviderPreset { label: "Google Gemini", provider: Custom, endpoint: "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", needs_key: true, tools: true },
        ProviderPreset { label: "Groq", provider: Custom, endpoint: "https://api.groq.com/openai/v1/chat/completions", needs_key: true, tools: true },
        ProviderPreset { label: "Together", provider: Custom, endpoint: "https://api.together.xyz/v1/chat/completions", needs_key: true, tools: true },
        ProviderPreset { label: "Azure OpenAI", provider: Azure, endpoint: "", needs_key: true, tools: true },
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
            tools: false,
            max_tool_rounds: None,
            allow_remote_file_tools: false,
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
    fn native_ollama_num_ctx_keeps_the_pre_profile_window() {
        // Existing native-Ollama runtimes with no override: num_ctx stays what
        // it was (8k for gemma, the fallback for gpt-oss/lfm2), not the
        // llama.cpp slot size the model profiles carry.
        let caps = ModelCapabilities { context_length: Some(131_072), ..Default::default() };
        for model in ["gemma3:27b", "gemma4:12b", "gpt-oss:20b", "lfm2:8b"] {
            let rt = ResolvedRuntime { model: model.into(), ..ollama_rt() };
            assert_eq!(ollama_chat_options(&rt, Some(&caps)).num_ctx, Some(8_192), "{model}");
            assert_eq!(rt.context_window(), 8_192, "{model}: the planner budgets the same window");
            // The same model behind a llama.cpp gateway gets its slot size.
            let gw = ResolvedRuntime { provider: ModelProviderKind::Custom, endpoint: "http://10.11.12.13:9/v1/chat/completions".into(), ..rt };
            assert_eq!(gw.context_window(), 32_768, "{model}");
        }
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
            tools: false,
            max_tool_rounds: None,
            allow_remote_file_tools: false,
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
            tools: false,
            max_tool_rounds: None,
            allow_remote_file_tools: false,
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
    fn context_window_precedence_is_override_then_server_probe_then_name() {
        let endpoint = "http://10.9.8.7:8081/v1/chat/completions";
        let mut rt = ResolvedRuntime {
            provider: ModelProviderKind::Custom,
            model: "some-local-gguf".into(),
            endpoint: endpoint.into(),
            ..ollama_rt()
        };
        // Nothing probed yet: the name table (fallback for an unknown name).
        assert_eq!(rt.context_window(), hive_core::context_budget::model_context_window::FALLBACK);
        // A probe that found nothing changes nothing.
        super::super::openai::seed_context_probe(endpoint, "some-local-gguf", None);
        assert_eq!(rt.context_window(), hive_core::context_budget::model_context_window::FALLBACK);
        // The server's per-slot window beats the name guess…
        super::super::openai::seed_context_probe(endpoint, "some-local-gguf", Some(16_384));
        assert_eq!(rt.context_window(), 16_384);
        // …and the runtime's explicit setting beats the probe.
        rt.context_window_tokens = Some(4096);
        assert_eq!(rt.context_window(), 4096);
        // Hosted providers never consult the probe cache.
        rt.context_window_tokens = None;
        rt.provider = ModelProviderKind::OpenAI;
        assert_eq!(rt.context_window(), hive_core::context_budget::model_context_window::FALLBACK);
    }

    #[test]
    fn openai_wire_tools_follow_the_runtime_switch() {
        let mut rt = ResolvedRuntime { provider: ModelProviderKind::Custom, ..ollama_rt() };
        assert!(!openai_tools_allowed(&rt), "off unless opted in");
        rt.tools = true;
        assert!(openai_tools_allowed(&rt));
        for p in [ModelProviderKind::HiveDaemon, ModelProviderKind::Ollama] {
            rt.provider = p;
            assert!(openai_tools_allowed(&rt), "{p:?}");
        }
        // Hosted APIs never run the agent loop, even with the (old-default)
        // switch on: it isn't consent to upload workspace files.
        for p in [
            ModelProviderKind::OpenAI,
            ModelProviderKind::OpenRouter,
            ModelProviderKind::Azure,
            ModelProviderKind::Anthropic,
            ModelProviderKind::ClaudeCode,
            ModelProviderKind::Pi,
        ] {
            rt.provider = p;
            assert!(!openai_tools_allowed(&rt), "{p:?}");
        }
    }

    #[test]
    fn request_extras_map_think_for_local_servers_only() {
        let mut rt = ResolvedRuntime { provider: ModelProviderKind::Custom, model: "gemma4:12b".into(), ..ollama_rt() };
        // Hive's default (unset) is thinking off.
        let e = openai_request_extras(&rt);
        assert_eq!(e["chat_template_kwargs"], json!({ "enable_thinking": false }));
        assert!(!e.contains_key("reasoning_effort"), "only gpt-oss gets reasoning_effort");
        rt.think = Some(true);
        assert_eq!(openai_request_extras(&rt)["chat_template_kwargs"], json!({ "enable_thinking": true }));
        // gpt-oss always reasons: low effort when thinking is off, its default otherwise.
        rt.model = "gpt-oss:20b".into();
        assert!(!openai_request_extras(&rt).contains_key("reasoning_effort"));
        rt.think = None;
        assert_eq!(openai_request_extras(&rt)["reasoning_effort"], "low");
        rt.provider = ModelProviderKind::HiveDaemon;
        assert_eq!(openai_request_extras(&rt)["reasoning_effort"], "low");
        // Hosted APIs (and the Ollama shim) get nothing they might reject.
        for p in [ModelProviderKind::OpenAI, ModelProviderKind::OpenRouter, ModelProviderKind::Azure, ModelProviderKind::Ollama] {
            rt.provider = p;
            assert!(openai_request_extras(&rt).is_empty(), "{p:?}");
        }
    }

    #[test]
    fn tool_loop_guards_local_windows_only() {
        let mut rt = ResolvedRuntime { provider: ModelProviderKind::Custom, model: "unknown-gguf".into(), ..ollama_rt() };
        rt.context_window_tokens = Some(32_768);
        assert_eq!(tool_loop_context_window(&rt), 32_768);
        rt.provider = ModelProviderKind::Ollama;
        assert_eq!(tool_loop_context_window(&rt), 32_768);
        rt.provider = ModelProviderKind::OpenAI;
        assert_eq!(tool_loop_context_window(&rt), 0, "hosted windows aren't guessed at");
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

    #[test]
    fn local_gateway_preset_is_a_keyless_custom_runtime_with_tools() {
        let ps = provider_presets();
        let gw = ps.iter().find(|p| p.label.starts_with("Local gateway")).unwrap();
        assert_eq!(gw.provider, ModelProviderKind::Custom);
        assert_eq!(gw.endpoint, "http://<host>:8080/v1/chat/completions");
        assert!(!gw.needs_key);
        assert!(gw.tools);
        // Labels are the UI's lookup key.
        let labels: std::collections::HashSet<_> = ps.iter().map(|p| p.label).collect();
        assert_eq!(labels.len(), ps.len());
    }

    #[test]
    fn request_extras_follow_the_model_profile() {
        let mut rt = ResolvedRuntime { provider: ModelProviderKind::Custom, model: "lfm2:8b".into(), ..ollama_rt() };
        // A family that never reasons gets no thinking fields at all.
        assert!(openai_request_extras(&rt).is_empty());
        rt.model = "bge-reranker-v2-m3".into();
        assert!(openai_request_extras(&rt).is_empty());
        // LFM2.5 always reasons with no effort knob: kwargs only.
        rt.model = "lfm2.5:1.2b".into();
        let e = openai_request_extras(&rt);
        assert_eq!(e["chat_template_kwargs"], json!({ "enable_thinking": false }));
        assert!(!e.contains_key("reasoning_effort"));
        // Unknown models keep the kwargs (templates ignore unknown flags).
        rt.model = "mystery-7b".into();
        assert!(openai_request_extras(&rt).contains_key("chat_template_kwargs"));
        // gpt-oss under any naming gets low effort.
        rt.model = "GPT_OSS-20B-Q4_K_M".into();
        assert_eq!(openai_request_extras(&rt)["reasoning_effort"], "low");
    }
}
