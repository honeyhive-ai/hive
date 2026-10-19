//! Model profiles — per-family defaults for open-weight models served locally
//! (llama.cpp / Ollama), keyed by model-id substrings. They feed the context
//! planner's window guess, the reasoning fields an OpenAI-wire request carries,
//! and the "recommended role" hint in Settings → Models.
//!
//! The defaults come from lab runs against a llama.cpp gateway (4 × 32k slots):
//! - gpt-oss 20B ran agent loops at 2.2× gemma4's speed and kept every fact
//!   through compaction; it always reasons (`reasoning_effort: "low"` keeps
//!   that short).
//! - gemma4 12B is the chat model; with `web_search` + `fetch_url` it went from
//!   0/11 to 9/11 on live-version questions. Thinking toggles via
//!   `chat_template_kwargs.enable_thinking`.
//! - LFM2 8B-A1B answers quick jobs (titles, classification) in 0.2–0.4 s on
//!   CPU. LFM2.5 always reasons, which makes it slow for quick jobs, and it lost
//!   a third of the facts when summarizing — don't use it for compaction.
//! - Qwen3.6 35B-A3B is the quality pick.
//!
//! Unknown models get no profile; callers fall back to their own defaults.

/// What a model is best used for in Hive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRole {
    /// Tool-using agent loops (many rounds, compaction).
    Agent,
    /// Everyday conversation.
    Chat,
    /// Quick jobs: chat titles, classification (the "Quick tasks model").
    Utility,
    /// Slower, best answers.
    Quality,
    /// Text embeddings for retrieval (not a chat model).
    Embedding,
    /// Passage reranking for retrieval (not a chat model).
    Rerank,
}

impl ModelRole {
    /// Stable lowercase id (what the UI receives).
    pub fn as_str(self) -> &'static str {
        match self {
            ModelRole::Agent => "agent",
            ModelRole::Chat => "chat",
            ModelRole::Utility => "utility",
            ModelRole::Quality => "quality",
            ModelRole::Embedding => "embedding",
            ModelRole::Rerank => "rerank",
        }
    }

    /// Short human label.
    pub fn label(self) -> &'static str {
        match self {
            ModelRole::Agent => "Agent loops",
            ModelRole::Chat => "Chat",
            ModelRole::Utility => "Quick jobs (titles, classification)",
            ModelRole::Quality => "Quality answers",
            ModelRole::Embedding => "Embeddings (retrieval)",
            ModelRole::Rerank => "Reranking (retrieval)",
        }
    }
}

/// How a model reasons, which decides the request fields that control it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningStyle {
    /// No reasoning phase.
    None,
    /// Thinking toggles via `chat_template_kwargs.enable_thinking`; Hive's
    /// default is off.
    Toggle,
    /// Always reasons; `reasoning_effort` bounds it (gpt-oss: send "low"
    /// when thinking is off).
    AlwaysWithEffort,
    /// Always reasons and can't be turned down (LFM2.5). Give it an answer
    /// budget of at least 1024 tokens.
    Always,
}

impl ReasoningStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            ReasoningStyle::None => "none",
            ReasoningStyle::Toggle => "toggle",
            ReasoningStyle::AlwaysWithEffort => "always-effort",
            ReasoningStyle::Always => "always",
        }
    }

    /// Whether the model spends output tokens thinking even with thinking off.
    pub fn always_reasons(self) -> bool {
        matches!(self, ReasoningStyle::AlwaysWithEffort | ReasoningStyle::Always)
    }
}

/// Defaults for one model family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelProfile {
    /// Family name, e.g. "gpt-oss".
    pub family: &'static str,
    /// Context window to plan against when neither the runtime nor the server
    /// says otherwise — the per-slot window a local server allocates, not the
    /// model's trained maximum.
    pub context_window: u32,
    /// Whether the family calls tools well enough to offer them by default.
    pub tools: bool,
    pub reasoning: ReasoningStyle,
    /// Recommended role; `None` when the family isn't a good default for any.
    pub role: Option<ModelRole>,
    /// One-line hint for the add-runtime form.
    pub note: &'static str,
}

/// Minimum answer budget for a model that reasons: at 160 tokens reasoning
/// models ran out mid-thought.
pub const REASONING_ANSWER_BUDGET: u32 = 1024;

/// Per-slot window of the reference llama.cpp gateway.
const SLOT: u32 = 32_768;

/// Ordered table: the first entry whose pattern matches wins, so more
/// specific families (embeddinggemma, lfm2.5, qwen3.6) precede the general
/// ones (gemma, lfm2, qwen3).
const PROFILES: &[(&[&str], ModelProfile)] = &[
    (
        &["embeddinggemma"],
        ModelProfile {
            family: "embeddinggemma",
            context_window: 2_048,
            tools: false,
            reasoning: ReasoningStyle::None,
            role: Some(ModelRole::Embedding),
            note: "Embedding model for retrieval (pair with bge-reranker-v2-m3); not a chat model.",
        },
    ),
    (
        &["reranker", "rerank"],
        ModelProfile {
            family: "reranker",
            context_window: 8_192,
            tools: false,
            reasoning: ReasoningStyle::None,
            role: Some(ModelRole::Rerank),
            note: "Reranker for retrieval (rerank the top 20, keep 5); not a chat model.",
        },
    ),
    (
        &["gptoss"],
        ModelProfile {
            family: "gpt-oss",
            context_window: SLOT,
            tools: true,
            reasoning: ReasoningStyle::AlwaysWithEffort,
            role: Some(ModelRole::Agent),
            note: "Best for agent loops: 2.2x gemma4's speed, kept every fact through compaction. Always reasons; Hive sends reasoning_effort low when thinking is off.",
        },
    ),
    (
        &["lfm2.5"],
        ModelProfile {
            family: "lfm2.5",
            context_window: SLOT,
            tools: true,
            reasoning: ReasoningStyle::Always,
            role: None,
            note: "Always reasons, so it's slow for quick jobs, and it lost a third of the facts when summarizing. Prefer LFM2 for quick jobs and a stronger model for summaries.",
        },
    ),
    (
        &["lfm2"],
        ModelProfile {
            family: "lfm2",
            context_window: SLOT,
            tools: true,
            reasoning: ReasoningStyle::None,
            role: Some(ModelRole::Utility),
            note: "Quick jobs: LFM2 8B-A1B answers titles and classification in 0.2-0.4 s on CPU. Use it as the Quick tasks model, not for summaries.",
        },
    ),
    (
        &["gemma4"],
        ModelProfile {
            family: "gemma4",
            context_window: SLOT,
            tools: true,
            reasoning: ReasoningStyle::Toggle,
            role: Some(ModelRole::Chat),
            note: "Chat. Turn on web tools: with web_search and fetch_url it went from 0/11 to 9/11 on live-version questions. Thinking is off unless you enable it.",
        },
    ),
    (
        &["gemma3"],
        ModelProfile {
            family: "gemma3",
            context_window: SLOT,
            tools: false,
            reasoning: ReasoningStyle::None,
            role: Some(ModelRole::Chat),
            note: "Chat. No native tool calling.",
        },
    ),
    (
        &["qwen3.6", "qwen335ba3b", "qwen330ba3b"],
        ModelProfile {
            family: "qwen3-moe",
            context_window: SLOT,
            tools: true,
            reasoning: ReasoningStyle::Toggle,
            role: Some(ModelRole::Quality),
            note: "Quality answers (Qwen3.6 35B-A3B). Thinking is off unless you enable it.",
        },
    ),
    (
        &["qwen3"],
        ModelProfile {
            family: "qwen3",
            context_window: SLOT,
            tools: true,
            reasoning: ReasoningStyle::Toggle,
            role: Some(ModelRole::Chat),
            note: "Chat. Thinking is off unless you enable it.",
        },
    ),
];

/// Lowercase and drop separators so "gpt-oss", "GPT_OSS" and "gpt oss" (or
/// "gemma-4" and "gemma4") match the same pattern. Dots and colons stay: they
/// separate versions and tags ("lfm2.5", "qwen3.6:35b").
fn normalize(model_id: &str) -> String {
    model_id
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

/// The profile for a model id, matching on family substrings anywhere in the
/// id (so registry prefixes like "hf.co/unsloth/" or tags like ":20b" don't
/// matter). `None` for unknown models.
pub fn profile_for(model_id: &str) -> Option<&'static ModelProfile> {
    let m = normalize(model_id);
    if m.is_empty() {
        return None;
    }
    PROFILES
        .iter()
        .find(|(patterns, _)| patterns.iter().any(|p| m.contains(p)))
        .map(|(_, profile)| profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family(id: &str) -> Option<&'static str> {
        profile_for(id).map(|p| p.family)
    }

    #[test]
    fn matches_families_across_naming_styles() {
        assert_eq!(family("gpt-oss:20b"), Some("gpt-oss"));
        assert_eq!(family("openai/gpt-oss-20b"), Some("gpt-oss"));
        assert_eq!(family("GPT_OSS-20B-Q4_K_M.gguf"), Some("gpt-oss"));
        assert_eq!(family("gemma4:12b"), Some("gemma4"));
        assert_eq!(family("google/gemma-4-12b-it"), Some("gemma4"));
        assert_eq!(family("gemma-3-27b-it"), Some("gemma3"));
        assert_eq!(family("lfm2:8b"), Some("lfm2"));
        assert_eq!(family("LFM2-8B-A1B"), Some("lfm2"));
        assert_eq!(family("qwen3.5"), Some("qwen3"));
        assert_eq!(family("Qwen3-8B"), Some("qwen3"));
        assert_eq!(family("bge-reranker-v2-m3"), Some("reranker"));
    }

    #[test]
    fn specific_families_win_over_general_ones() {
        // lfm2.5 contains "lfm2"; embeddinggemma contains "gemma"; qwen3.6
        // contains "qwen3" — the table order must pick the specific entry.
        assert_eq!(family("LFM2.5-1.2B-Thinking"), Some("lfm2.5"));
        assert_eq!(family("lfm2.5:1.2b"), Some("lfm2.5"));
        assert_eq!(family("embeddinggemma-300m"), Some("embeddinggemma"));
        assert_eq!(family("qwen3.6:35b-a3b"), Some("qwen3-moe"));
        assert_eq!(family("Qwen3-30B-A3B"), Some("qwen3-moe"));
    }

    #[test]
    fn unknown_and_hosted_models_have_no_profile() {
        for id in ["", "claude-sonnet-4", "gpt-4o", "gpt-5", "llama3.1:8b", "mistral", "qwen2.5-coder"] {
            assert_eq!(profile_for(id), None, "{id}");
        }
    }

    #[test]
    fn roles_follow_the_measured_picks() {
        let role = |id| profile_for(id).and_then(|p| p.role);
        assert_eq!(role("gpt-oss:20b"), Some(ModelRole::Agent));
        assert_eq!(role("gemma4:12b"), Some(ModelRole::Chat));
        assert_eq!(role("lfm2:8b"), Some(ModelRole::Utility));
        assert_eq!(role("qwen3.6:35b-a3b"), Some(ModelRole::Quality));
        assert_eq!(role("embeddinggemma-300m"), Some(ModelRole::Embedding));
        assert_eq!(role("bge-reranker-v2-m3"), Some(ModelRole::Rerank));
        // LFM2.5 always reasons and dropped facts in summaries: no default role.
        assert_eq!(role("lfm2.5"), None);
    }

    #[test]
    fn reasoning_styles() {
        let r = |id| profile_for(id).unwrap().reasoning;
        assert_eq!(r("gpt-oss:20b"), ReasoningStyle::AlwaysWithEffort);
        assert_eq!(r("lfm2.5"), ReasoningStyle::Always);
        assert_eq!(r("gemma4:12b"), ReasoningStyle::Toggle);
        assert_eq!(r("qwen3:8b"), ReasoningStyle::Toggle);
        assert_eq!(r("lfm2:8b"), ReasoningStyle::None);
        assert!(r("gpt-oss").always_reasons() && r("lfm2.5").always_reasons());
        assert!(!r("gemma4").always_reasons() && !r("lfm2").always_reasons());
    }

    #[test]
    fn chat_models_plan_against_a_slot_and_retrieval_models_are_not_tools() {
        for id in ["gpt-oss:20b", "gemma4:12b", "lfm2:8b", "lfm2.5", "qwen3.6:35b-a3b", "qwen3"] {
            assert_eq!(profile_for(id).unwrap().context_window, 32_768, "{id}");
        }
        for id in ["embeddinggemma-300m", "bge-reranker-v2-m3"] {
            let p = profile_for(id).unwrap();
            assert!(!p.tools, "{id}");
            assert!(p.context_window < 32_768, "{id}");
        }
        assert!(profile_for("gpt-oss").unwrap().tools && profile_for("gemma4").unwrap().tools);
    }

    #[test]
    fn every_entry_is_reachable_and_has_a_note() {
        // A pattern shadowed by an earlier entry would silently never match.
        for (patterns, profile) in PROFILES {
            assert!(!profile.note.is_empty(), "{}", profile.family);
            for p in *patterns {
                assert_eq!(profile_for(p).map(|x| x.family), Some(profile.family), "{p}");
            }
        }
    }

    #[test]
    fn role_ids_and_labels_are_distinct() {
        let all = [
            ModelRole::Agent,
            ModelRole::Chat,
            ModelRole::Utility,
            ModelRole::Quality,
            ModelRole::Embedding,
            ModelRole::Rerank,
        ];
        let ids: std::collections::HashSet<_> = all.iter().map(|r| r.as_str()).collect();
        let labels: std::collections::HashSet<_> = all.iter().map(|r| r.label()).collect();
        assert_eq!(ids.len(), all.len());
        assert_eq!(labels.len(), all.len());
    }
}
