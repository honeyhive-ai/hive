//! Provider adapters — ported from `Providers.swift` / `RuntimeChatClient.swift`.
//!
//! - `anthropic` — Anthropic Messages API streaming (Phase 3)
//! - `openai` — OpenAI-compatible streaming: OpenAI/OpenRouter/custom/Ollama (Phase 5)
//! - `subprocess` — external CLI agents: aider/pi/claude-code (Phase 5 follow-up)
//! - `thinking` — splits a reasoning model's `<think>` output from its reply
//! - `dispatch` — resolves + routes a turn to the right client by runtime

use std::time::Duration;

pub mod anthropic;
pub mod claude_code;
pub mod codex;
pub mod dispatch;
pub mod openai;
pub mod subprocess;
pub mod thinking;

pub use anthropic::{AnthropicClient, ChatTurn, ProviderError};
pub use dispatch::{default_endpoint, stream, ResolvedRuntime, StreamActivity};
pub use openai::{endpoint_host, OpenAiClient};

/// TCP connect timeout for HTTP providers. A remote box that is off, or a
/// Tailscale peer whose tunnel is down, should surface as an error in seconds —
/// not sit on the OS default (minutes) while the UI shows "thinking".
pub const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Env var overriding the per-turn *idle* timeout (seconds).
pub const IDLE_TIMEOUT_ENV: &str = "HIVE_TURN_IDLE_TIMEOUT_SECS";

/// Idle timeout for a turn: how long a provider may produce *nothing* before
/// the turn is declared wedged. Any activity (a byte, a tool event) resets it,
/// so a long-but-active turn never trips it. Shared by the Claude Code bridge
/// and the HTTP providers so one knob (`HIVE_TURN_IDLE_TIMEOUT_SECS`, default
/// 300) governs both. (#100)
pub fn turn_idle_timeout() -> Duration {
    Duration::from_secs(
        std::env::var(IDLE_TIMEOUT_ENV)
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&s| s > 0)
            .unwrap_or(300),
    )
}
