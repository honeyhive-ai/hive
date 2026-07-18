//! Context compaction: fit a growing transcript into a model's context window.
//!
//! Hybrid windowing + cached incremental summarization: keep the most-recent
//! messages that fit the budget (via `hive_core::context_budget`), and condense
//! the overflow into a cached summary that is refreshed incrementally as the
//! thread grows (only newly-aged-out turns are re-summarized).

use hive_core::context_budget::{self};
use hive_core::ChatMessage;
use uuid::Uuid;

/// Produces a prose summary of overflowed turns. In production this is an LLM
/// call; tests use a deterministic fake.
pub trait Summarizer {
    /// Summarize `messages`. When `prior` is `Some`, it's an existing summary to
    /// extend with only the new `messages` (incremental update).
    fn summarize(&self, prior: Option<&str>, messages: &[ChatMessage]) -> String;
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompactionResult {
    /// Recent messages that fit the budget (sent verbatim).
    pub kept: Vec<ChatMessage>,
    /// Condensed summary of the overflow, if any aged out.
    pub summary: Option<String>,
    /// How many messages were condensed (drives the transcript marker).
    pub overflow_count: usize,
}

fn is_prefix(prefix: &[Uuid], full: &[Uuid]) -> bool {
    prefix.len() < full.len() && full.starts_with(prefix)
}

/// Per-chat compactor holding the incremental summary cache.
#[derive(Default)]
pub struct Compactor {
    /// (ids covered by the cached summary, the summary text)
    cache: Option<(Vec<Uuid>, String)>,
}

impl Compactor {
    /// Plan the window for `history` under `budget_tokens` (already net of the
    /// system prompt + output reserve) and summarize any overflow, reusing or
    /// incrementally extending the cached summary where possible.
    pub fn compact(
        &mut self,
        history: &[ChatMessage],
        budget_tokens: i64,
        summarizer: &dyn Summarizer,
    ) -> CompactionResult {
        let plan = context_budget::plan(history, budget_tokens);
        if plan.overflow.is_empty() {
            self.cache = None;
            return CompactionResult {
                kept: plan.kept,
                summary: None,
                overflow_count: 0,
            };
        }

        let overflow_ids: Vec<Uuid> = plan.overflow.iter().map(|m| m.id).collect();
        let summary = match &self.cache {
            // exact same overflow → reuse, no LLM call
            Some((covered, sum)) if *covered == overflow_ids => sum.clone(),
            // cache covers a prefix → summarize only the newly-aged-out delta
            Some((covered, sum)) if is_prefix(covered, &overflow_ids) => {
                let delta = &plan.overflow[covered.len()..];
                summarizer.summarize(Some(sum), delta)
            }
            // otherwise → full re-summarize
            _ => summarizer.summarize(None, &plan.overflow),
        };
        self.cache = Some((overflow_ids, summary.clone()));

        CompactionResult {
            kept: plan.kept,
            summary: Some(summary),
            overflow_count: plan.overflow.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::{ChatMessage, MessageRole};
    use std::cell::Cell;

    /// Counts calls and distinguishes full vs incremental summaries.
    struct FakeSummarizer {
        full_calls: Cell<usize>,
        incremental_calls: Cell<usize>,
    }
    impl FakeSummarizer {
        fn new() -> Self {
            Self {
                full_calls: Cell::new(0),
                incremental_calls: Cell::new(0),
            }
        }
    }
    impl Summarizer for FakeSummarizer {
        fn summarize(&self, prior: Option<&str>, messages: &[ChatMessage]) -> String {
            if prior.is_some() {
                self.incremental_calls.set(self.incremental_calls.get() + 1);
                format!("{}+{}", prior.unwrap(), messages.len())
            } else {
                self.full_calls.set(self.full_calls.get() + 1);
                format!("sum({})", messages.len())
            }
        }
    }

    fn msg(body: &str) -> ChatMessage {
        ChatMessage::new(MessageRole::User, "u", body)
    }

    #[test]
    fn no_overflow_yields_no_summary() {
        let mut c = Compactor::default();
        let fake = FakeSummarizer::new();
        let history = vec![msg("hi")];
        let r = c.compact(&history, 1000, &fake);
        assert!(r.summary.is_none());
        assert_eq!(r.overflow_count, 0);
        assert_eq!(fake.full_calls.get(), 0);
    }

    #[test]
    fn overflow_summarized_then_reused_then_incrementally_extended() {
        let mut c = Compactor::default();
        let fake = FakeSummarizer::new();
        // each 4-char body = 5 tokens; budget 5 keeps only the newest.
        let h1 = vec![msg("aaaa"), msg("bbbb"), msg("cccc")];

        let r1 = c.compact(&h1, 5, &fake);
        assert_eq!(r1.kept.len(), 1);
        assert_eq!(r1.overflow_count, 2);
        assert_eq!(fake.full_calls.get(), 1);

        // identical overflow → reuse (no new call)
        let r2 = c.compact(&h1, 5, &fake);
        assert_eq!(r2.summary, r1.summary);
        assert_eq!(fake.full_calls.get(), 1);
        assert_eq!(fake.incremental_calls.get(), 0);

        // append a newer message: overflow grows by one (prefix match) →
        // incremental summary of just the delta
        let mut h2 = h1.clone();
        h2.push(msg("dddd"));
        let r3 = c.compact(&h2, 5, &fake);
        assert_eq!(r3.overflow_count, 3);
        assert_eq!(fake.full_calls.get(), 1);
        assert_eq!(fake.incremental_calls.get(), 1);
    }
}
