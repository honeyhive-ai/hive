//! Context budgeting — direct port of `ContextBudget.swift` (CTX.1/CTX.2):
//! deterministic token estimation, the per-model context-window table, and the
//! newest-first windowing planner. No per-provider tokenizer; the budget is
//! approximate by design, which is why callers keep generous reserve headroom.

use crate::chat::ChatMessage;
use crate::runtime::RuntimeTarget;

/// Per-message structural overhead (role markers, separators).
pub const MESSAGE_OVERHEAD: usize = 4;

/// Cheap, deterministic ~4-characters-per-token estimate.
pub mod token_estimator {
    use super::*;

    pub fn estimate_text(text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        // ceil(chars / 4), counting Unicode scalar values like Swift's
        // `String.count` (grapheme clusters differ, but this matches the
        // intent: a coarse character count).
        let chars = text.chars().count();
        (chars + 3) / 4
    }

    pub fn estimate_message(message: &ChatMessage) -> usize {
        estimate_text(&message.body) + MESSAGE_OVERHEAD
    }

    pub fn estimate_messages(messages: &[ChatMessage]) -> usize {
        messages.iter().map(estimate_message).sum()
    }
}

/// Built-in context-window sizes (tokens) keyed by model-id substrings.
pub mod model_context_window {
    use super::*;

    pub const FALLBACK: u32 = 8_192;

    pub fn tokens_for_model(model_id: &str) -> u32 {
        let m = model_id.to_lowercase();
        if m.contains("claude") || m.contains("sonnet") || m.contains("opus") || m.contains("haiku")
        {
            return 200_000;
        }
        if m.contains("gemini") {
            return 1_000_000;
        }
        // Open-weight models served locally (llama.cpp / Ollama). These can
        // take far more, but a local server allocates a fixed per-slot window
        // and 32k is the conservative slot size; a runtime override or the
        // server's own `/props` report wins over this guess. Checked before
        // the hosted-model rules so e.g. "gpt-oss" never matches a GPT entry.
        if m.contains("gpt-oss")
            || m.contains("lfm2")
            || ["gemma3", "gemma-3", "gemma4", "gemma-4"].iter().any(|g| m.contains(g))
        {
            return 32_768;
        }
        if m.contains("gpt-4o")
            || m.contains("gpt-4.1")
            || m.contains("o1")
            || m.contains("o3")
            || m.contains("gpt-5")
        {
            return 128_000;
        }
        if m.contains("gpt-3.5") {
            return 16_385;
        }
        if m.contains("gpt-4") {
            return 8_192;
        }
        if m.contains("qwen") {
            return 32_768;
        }
        if m.contains("llama")
            || m.contains("mistral")
            || m.contains("mixtral")
            || m.contains("gemma")
            || m.contains("phi")
        {
            return 8_192;
        }
        FALLBACK
    }

    /// Effective window for a runtime: explicit override first, then the
    /// model-id default table.
    pub fn tokens_for_runtime(runtime: &RuntimeTarget) -> u32 {
        match runtime.capabilities.context_window_tokens {
            Some(override_tokens) if override_tokens > 0 => override_tokens,
            _ => tokens_for_model(&runtime.model_id),
        }
    }
}

/// The result of planning which history fits a budget: `kept` (most-recent
/// messages that fit, in original order) and `overflow` (older messages that
/// don't, to be summarized or dropped).
#[derive(Debug, Clone, PartialEq)]
pub struct ContextWindowPlan {
    pub kept: Vec<ChatMessage>,
    pub overflow: Vec<ChatMessage>,
}

/// Fill the budget from the newest message backward; everything older that
/// doesn't fit becomes overflow. The most recent message is always kept (even
/// if it alone exceeds the budget) so a turn is never empty.
pub fn plan(history: &[ChatMessage], budget_tokens: i64) -> ContextWindowPlan {
    if history.is_empty() {
        return ContextWindowPlan {
            kept: Vec::new(),
            overflow: Vec::new(),
        };
    }
    let budget = budget_tokens.max(0) as usize;
    let mut kept_reversed: Vec<ChatMessage> = Vec::new();
    let mut used = 0usize;
    for message in history.iter().rev() {
        let cost = token_estimator::estimate_message(message);
        if kept_reversed.is_empty() || used + cost <= budget {
            kept_reversed.push(message.clone());
            used += cost;
        } else {
            break;
        }
    }
    kept_reversed.reverse();
    let kept = kept_reversed;
    let overflow = history[..history.len() - kept.len()].to_vec();
    ContextWindowPlan { kept, overflow }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{ChatMessage, MessageRole};

    fn msg(body: &str) -> ChatMessage {
        ChatMessage::new(MessageRole::User, "u", body)
    }

    #[test]
    fn estimate_is_four_chars_per_token_plus_overhead() {
        assert_eq!(token_estimator::estimate_text(""), 0);
        assert_eq!(token_estimator::estimate_text("abcd"), 1);
        assert_eq!(token_estimator::estimate_text("abcde"), 2);
        // body "abcd" => 1 token + 4 overhead
        assert_eq!(token_estimator::estimate_message(&msg("abcd")), 5);
    }

    #[test]
    fn model_window_table_matches_swift() {
        assert_eq!(model_context_window::tokens_for_model("claude-opus-4"), 200_000);
        assert_eq!(model_context_window::tokens_for_model("gemini-2.0"), 1_000_000);
        assert_eq!(model_context_window::tokens_for_model("gpt-4o-mini"), 128_000);
        assert_eq!(model_context_window::tokens_for_model("qwen2.5"), 32_768);
        assert_eq!(model_context_window::tokens_for_model("something-else"), 8_192);
    }

    #[test]
    fn local_open_models_get_a_32k_slot() {
        for id in [
            "gemma4:12b",
            "gemma3:27b",
            "google/gemma-3-12b-it",
            "gemma-4-12b-it-Q4_K_M",
            "gpt-oss:20b",
            "openai/gpt-oss-120b",
            "lfm2:8b",
            "LFM2-8B-A1B",
            "lfm2.5:1.2b",
        ] {
            assert_eq!(model_context_window::tokens_for_model(id), 32_768, "{id}");
        }
        // Older Gemma generations and other local families keep their entries.
        assert_eq!(model_context_window::tokens_for_model("gemma2:9b"), 8_192);
        assert_eq!(model_context_window::tokens_for_model("llama3.1:8b"), 8_192);
        assert_eq!(model_context_window::tokens_for_model("qwen3.6:35b-a3b"), 32_768);
        // Hosted GPT entries are untouched.
        assert_eq!(model_context_window::tokens_for_model("gpt-4o"), 128_000);
        assert_eq!(model_context_window::tokens_for_model("gpt-4"), 8_192);
    }

    #[test]
    fn newest_message_always_kept_even_when_over_budget() {
        let history = vec![msg("aaaaaaaa"), msg("this is a long message body")];
        let p = plan(&history, 0);
        assert_eq!(p.kept.len(), 1);
        assert_eq!(p.kept[0].body, history[1].body);
        assert_eq!(p.overflow.len(), 1);
    }

    #[test]
    fn fills_from_newest_backward() {
        // three 4-char bodies => 5 tokens each; budget for two = 10.
        let history = vec![msg("aaaa"), msg("bbbb"), msg("cccc")];
        let p = plan(&history, 10);
        assert_eq!(p.kept.len(), 2);
        assert_eq!(p.kept[0].body, "bbbb");
        assert_eq!(p.kept[1].body, "cccc");
        assert_eq!(p.overflow.len(), 1);
        assert_eq!(p.overflow[0].body, "aaaa");
    }
}
