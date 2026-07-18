//! Provider adapters — ported from `Providers.swift` / `RuntimeChatClient.swift`.
//!
//! - `anthropic` — Anthropic Messages API streaming (Phase 3)
//! - `openai` — OpenAI-compatible streaming: OpenAI/OpenRouter/custom/Ollama (Phase 5)
//! - `subprocess` — external CLI agents: aider/pi/claude-code (Phase 5 follow-up)
//! - `dispatch` — resolves + routes a turn to the right client by runtime

pub mod anthropic;
pub mod claude_code;
pub mod dispatch;
pub mod openai;
pub mod subprocess;

pub use anthropic::{AnthropicClient, ChatTurn, ProviderError};
pub use dispatch::{default_endpoint, stream, ResolvedRuntime};
pub use openai::OpenAiClient;
