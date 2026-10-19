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
        // Open-weight models served locally (llama.cpp / Ollama): the family
        // profile's per-slot window. These models can take far more, but a
        // local server allocates a fixed per-slot window and 32k is the
        // conservative slot size; a runtime override or the server's own
        // `/props` report wins over this guess. Checked before the hosted-model
        // rules so e.g. "gpt-oss" never matches a GPT entry.
        if let Some(profile) = crate::model_profile::profile_for(model_id) {
            return profile.context_window;
        }
        name_table_window(model_id)
    }

    /// The model-name table alone, without the open-model family profiles —
    /// what native Ollama runtimes size `num_ctx` from. The profiles' 32k is a
    /// llama.cpp gateway's slot; asking Ollama for it would silently
    /// quadruple the KV cache of an existing 8k gemma/gpt-oss runtime (spill
    /// to CPU, or fail to load) where 8k worked before.
    pub fn name_table_window(model_id: &str) -> u32 {
        let m = model_id.to_lowercase();
        if m.contains("claude") || m.contains("sonnet") || m.contains("opus") || m.contains("haiku")
        {
            return 200_000;
        }
        if m.contains("gemini") {
            return 1_000_000;
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

/// Default high watermark for [`plan_with_hysteresis`]: compact once the kept
/// history grows past 85% of the history budget.
pub const DEFAULT_HIGH_WATERMARK: f64 = 0.85;
/// Default low watermark for [`plan_with_hysteresis`]: when compacting, move
/// the boundary far enough that the kept history fits 50% of the budget.
pub const DEFAULT_LOW_WATERMARK: f64 = 0.50;

/// A windowing plan that remembers where it cut. `boundary` is the id of the
/// first kept message once compaction has started (`None` while the whole
/// history still fits); callers persist it and pass it back next turn.
#[derive(Debug, Clone, PartialEq)]
pub struct HysteresisPlan {
    pub plan: ContextWindowPlan,
    pub boundary: Option<uuid::Uuid>,
    /// True when this call moved the boundary (a compaction batch happened);
    /// false when the prior boundary — or "no compaction" — was kept as is.
    pub moved: bool,
}

/// Batch compaction with hysteresis, for prefix-cache-friendly prompts.
///
/// [`plan`] slides the window every turn, so the oldest kept message (and the
/// summary covering everything before it) changes on nearly every turn and a
/// server-side prefix cache is never reused past the system prompt. Instead:
///
/// - With no boundary yet, the whole history is kept until it exceeds `high`
///   × budget.
/// - With a boundary, everything from it onward is kept unchanged until that
///   exceeds `high` × budget.
/// - On exceeding, the boundary jumps forward in one batch so the kept history
///   fits `low` × budget (the newest message is always kept, as in [`plan`]).
///
/// So the summary is regenerated once per batch rather than once per turn, and
/// the prompt prefix (summary + kept turns) stays byte-stable between batches.
/// A prior boundary that is no longer in the history (deleted, or `/compact`
/// rewrote the transcript) is treated as no boundary. Watermarks are clamped to
/// `0 < low <= high <= 1`.
pub fn plan_with_hysteresis(
    history: &[ChatMessage],
    budget_tokens: i64,
    prior_boundary: Option<uuid::Uuid>,
    high: f64,
    low: f64,
) -> HysteresisPlan {
    let unchanged = |start: usize, boundary: Option<uuid::Uuid>| HysteresisPlan {
        plan: ContextWindowPlan {
            kept: history[start..].to_vec(),
            overflow: history[..start].to_vec(),
        },
        boundary,
        moved: false,
    };
    if history.is_empty() {
        return unchanged(0, None);
    }
    let high = if high.is_finite() { high.clamp(f64::MIN_POSITIVE, 1.0) } else { 1.0 };
    let low = if low.is_finite() { low.clamp(f64::MIN_POSITIVE, high) } else { high };
    let budget = budget_tokens.max(0) as f64;
    let high_limit = (budget * high).floor() as i64;
    let low_limit = (budget * low).floor() as i64;

    // The whole history fits this budget: nothing to compact, whatever a
    // prior boundary says (it may have been set under a smaller window — a
    // different responder in the same chat, or before the window grew).
    if token_estimator::estimate_messages(history) as i64 <= high_limit {
        return unchanged(0, None);
    }

    // Where the kept history currently starts, if a known boundary survives.
    // A boundary this budget would never have produced — even the message
    // before it still fits the low watermark — came from a smaller window;
    // re-plan rather than keep a cut this budget doesn't need.
    let start = prior_boundary
        .and_then(|id| history.iter().position(|m| m.id == id))
        .filter(|&i| {
            i == 0 || token_estimator::estimate_messages(&history[i - 1..]) as i64 > low_limit
        });
    let from = start.unwrap_or(0);
    let kept_tokens = token_estimator::estimate_messages(&history[from..]) as i64;
    if kept_tokens <= high_limit {
        return match start {
            // A boundary at index 0 has nothing before it — no compaction.
            Some(i) if i > 0 => unchanged(i, prior_boundary),
            _ => unchanged(0, None),
        };
    }

    // Over the high watermark: move the boundary in one batch to the low one.
    let fitted = plan(history, low_limit);
    if fitted.overflow.is_empty() {
        // Only possible for a lone (huge) message: nothing to summarize.
        return unchanged(0, None);
    }
    let boundary = fitted.kept.first().map(|m| m.id);
    let moved = boundary != prior_boundary;
    HysteresisPlan {
        plan: fitted,
        boundary,
        moved,
    }
}

/// A stable digest of the messages a compaction summary covers (id, author
/// and body of each, in order). Stored with the summary so a later turn —
/// or a later app run — reuses it only while those messages are unchanged:
/// an edited or removed message changes the digest and forces a re-summary.
pub fn messages_fingerprint(messages: &[ChatMessage]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for m in messages {
        for part in [m.id.as_bytes().as_slice(), m.author.as_bytes(), m.body.as_bytes()] {
            h.update((part.len() as u64).to_le_bytes());
            h.update(part);
        }
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Backoff for a failing summarizer. A summary on a local model takes tens of
/// seconds; when it fails (server down, timeout, empty reply), retrying on the
/// very next turn makes every turn pay that cost again for nothing. After the
/// n-th consecutive failure the next attempt waits until either `2^n` turns
/// have gone by (capped at [`SummaryBackoff::MAX_SKIPPED_TURNS`]) or
/// `2^(n-1)` × [`SummaryBackoff::BASE_DELAY`] has passed (capped at
/// [`SummaryBackoff::MAX_DELAY`]), whichever comes first — so a fast chat
/// retries by turn count and a slow one by time. Meanwhile turns use the
/// safe fallback (the previous summary, or an "earlier messages omitted"
/// note) and the conversation itself is untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryBackoff {
    failures: u32,
    skipped: u32,
    since: std::time::Instant,
}

impl SummaryBackoff {
    pub const BASE_DELAY: std::time::Duration = std::time::Duration::from_secs(120);
    pub const MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(30 * 60);
    pub const MAX_SKIPPED_TURNS: u32 = 16;

    /// Record a failure at `now`, after `prior` consecutive ones (if any).
    pub fn after_failure(prior: Option<&SummaryBackoff>, now: std::time::Instant) -> Self {
        Self { failures: prior.map_or(0, |p| p.failures).saturating_add(1), skipped: 0, since: now }
    }

    /// Consecutive failures so far.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Turns to skip before the next attempt.
    pub fn skip_turns(&self) -> u32 {
        1u32.checked_shl(self.failures.min(31)).unwrap_or(u32::MAX).min(Self::MAX_SKIPPED_TURNS)
    }

    /// Time to wait before the next attempt.
    pub fn delay(&self) -> std::time::Duration {
        let factor = 1u32.checked_shl(self.failures.saturating_sub(1).min(31)).unwrap_or(u32::MAX);
        Self::BASE_DELAY.saturating_mul(factor).min(Self::MAX_DELAY)
    }

    /// Whether this turn may call the summarizer again. When it may not, the
    /// turn counts as skipped.
    pub fn try_attempt(&mut self, now: std::time::Instant) -> bool {
        if self.skipped >= self.skip_turns() || now.saturating_duration_since(self.since) >= self.delay() {
            return true;
        }
        self.skipped += 1;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{ChatMessage, MessageRole};

    #[test]
    fn summary_backoff_skips_two_turns_or_two_minutes_then_grows_to_a_cap() {
        use std::time::Duration;
        let t0 = std::time::Instant::now();
        let mut b = SummaryBackoff::after_failure(None, t0);
        assert_eq!((b.failures(), b.skip_turns(), b.delay()), (1, 2, Duration::from_secs(120)));
        // Fast chat: two skipped turns, then a retry.
        assert!(!b.try_attempt(t0 + Duration::from_secs(5)));
        assert!(!b.try_attempt(t0 + Duration::from_secs(10)));
        assert!(b.try_attempt(t0 + Duration::from_secs(15)));
        // Slow chat: the very next turn after two minutes retries.
        let mut slow = SummaryBackoff::after_failure(None, t0);
        assert!(slow.try_attempt(t0 + Duration::from_secs(121)));

        // Consecutive failures double both, up to the caps.
        let b2 = SummaryBackoff::after_failure(Some(&b), t0);
        assert_eq!((b2.failures(), b2.skip_turns(), b2.delay()), (2, 4, Duration::from_secs(240)));
        let mut many = b2.clone();
        for _ in 0..40 {
            many = SummaryBackoff::after_failure(Some(&many), t0);
        }
        assert_eq!(many.skip_turns(), SummaryBackoff::MAX_SKIPPED_TURNS);
        assert_eq!(many.delay(), SummaryBackoff::MAX_DELAY);
        let mut m = many.clone();
        for i in 0..SummaryBackoff::MAX_SKIPPED_TURNS {
            assert!(!m.try_attempt(t0 + Duration::from_secs(u64::from(i))), "turn {i}");
        }
        assert!(m.try_attempt(t0 + Duration::from_secs(60)));
        let mut m = many;
        assert!(m.try_attempt(t0 + SummaryBackoff::MAX_DELAY));
    }

    #[test]
    fn fingerprint_tracks_ids_authors_and_bodies() {
        let a = msg("alpha");
        let b = msg("beta");
        let fp = messages_fingerprint(&[a.clone(), b.clone()]);
        assert_eq!(fp.len(), 64);
        assert_eq!(fp, messages_fingerprint(&[a.clone(), b.clone()]), "stable");
        assert_ne!(fp, messages_fingerprint(&[b.clone(), a.clone()]), "order matters");
        let mut edited = b.clone();
        edited.body = "beta, edited".into();
        assert_ne!(fp, messages_fingerprint(&[a.clone(), edited]));
        assert_ne!(fp, messages_fingerprint(&[a.clone(), msg("beta")]), "a different message with the same text");
        assert_ne!(fp, messages_fingerprint(&[a]));
    }

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
        // Native Ollama sizes num_ctx from the name table alone: the windows
        // these families had before the profiles existed.
        for (id, want) in [("gemma3:27b", 8_192), ("gemma4:12b", 8_192), ("gpt-oss:20b", model_context_window::FALLBACK), ("lfm2:8b", model_context_window::FALLBACK), ("qwen3.5", 32_768)] {
            assert_eq!(model_context_window::name_table_window(id), want, "{id}");
        }
        assert_eq!(model_context_window::name_table_window("gpt-4o"), 128_000);
        // Retrieval models plan against their own (small) windows, not a slot.
        assert_eq!(model_context_window::tokens_for_model("embeddinggemma-300m"), 2_048);
        assert_eq!(model_context_window::tokens_for_model("bge-reranker-v2-m3"), 8_192);
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

    /// A message whose estimate is exactly `tokens` (body chars/4 + overhead).
    fn sized(tokens: usize) -> ChatMessage {
        msg(&"x".repeat((tokens - MESSAGE_OVERHEAD) * 4))
    }

    fn ids(messages: &[ChatMessage]) -> Vec<uuid::Uuid> {
        messages.iter().map(|m| m.id).collect()
    }

    const H: f64 = DEFAULT_HIGH_WATERMARK;
    const L: f64 = DEFAULT_LOW_WATERMARK;

    #[test]
    fn hysteresis_empty_history() {
        let p = plan_with_hysteresis(&[], 1000, None, H, L);
        assert!(p.plan.kept.is_empty() && p.plan.overflow.is_empty());
        assert_eq!(p.boundary, None);
        assert!(!p.moved);
        // A stale boundary against an empty history is simply forgotten.
        let p = plan_with_hysteresis(&[], 1000, Some(uuid::Uuid::new_v4()), H, L);
        assert_eq!(p.boundary, None);
    }

    #[test]
    fn hysteresis_keeps_everything_until_high_watermark() {
        // 8 × 10 = 80 tokens ≤ 85 (85% of 100): no compaction yet.
        let history: Vec<_> = (0..8).map(|_| sized(10)).collect();
        let p = plan_with_hysteresis(&history, 100, None, H, L);
        assert_eq!(p.plan.kept.len(), 8);
        assert!(p.plan.overflow.is_empty());
        assert_eq!(p.boundary, None);
        assert!(!p.moved);
    }

    #[test]
    fn hysteresis_moves_in_one_batch_to_low_watermark() {
        // 9 × 10 = 90 > 85 → compact to ≤ 50 tokens: the newest 5 messages.
        let history: Vec<_> = (0..9).map(|_| sized(10)).collect();
        let p = plan_with_hysteresis(&history, 100, None, H, L);
        assert!(p.moved);
        assert_eq!(p.plan.kept.len(), 5);
        assert_eq!(p.boundary, Some(history[4].id));
        assert_eq!(ids(&p.plan.overflow), ids(&history[..4]));
    }

    #[test]
    fn hysteresis_boundary_stays_fixed_across_turns() {
        let mut history: Vec<_> = (0..9).map(|_| sized(10)).collect();
        let first = plan_with_hysteresis(&history, 100, None, H, L);
        let boundary = first.boundary;
        // Kept = 50 tokens; three more turns (80 total) stay under 85.
        for turn in 0..3 {
            history.push(sized(10));
            let p = plan_with_hysteresis(&history, 100, boundary, H, L);
            assert!(!p.moved, "turn {turn} must not move the boundary");
            assert_eq!(p.boundary, boundary);
            // Same overflow → the cached summary is reused verbatim.
            assert_eq!(ids(&p.plan.overflow), ids(&first.plan.overflow));
            assert_eq!(p.plan.kept.first().map(|m| m.id), boundary);
        }
        // The next turn crosses 85 → one batch moves forward, and the old
        // overflow is a prefix of the new one (incremental summary).
        history.push(sized(10));
        let p = plan_with_hysteresis(&history, 100, boundary, H, L);
        assert!(p.moved);
        assert_ne!(p.boundary, boundary);
        assert_eq!(p.plan.kept.len(), 5);
        assert!(ids(&p.plan.overflow).starts_with(&ids(&first.plan.overflow)));
        assert!(p.plan.overflow.len() > first.plan.overflow.len());
    }

    #[test]
    fn hysteresis_single_huge_message() {
        // One message larger than the whole budget is still kept (a turn is
        // never empty), with no boundary since nothing precedes it.
        let history = vec![sized(500)];
        let p = plan_with_hysteresis(&history, 100, None, H, L);
        assert_eq!(p.plan.kept.len(), 1);
        assert!(p.plan.overflow.is_empty());
        assert_eq!(p.boundary, None);
        // A huge newest message after normal ones pushes everything else out.
        let history = vec![sized(10), sized(10), sized(500)];
        let p = plan_with_hysteresis(&history, 100, None, H, L);
        assert_eq!(ids(&p.plan.kept), vec![history[2].id]);
        assert_eq!(p.plan.overflow.len(), 2);
        assert_eq!(p.boundary, Some(history[2].id));
        // Re-planning the same history keeps the boundary (no churn).
        let same = plan_with_hysteresis(&history, 100, p.boundary, H, L);
        assert!(!same.moved);
        assert_eq!(same.boundary, p.boundary);
        // Next turn: huge + new is still over → the batch moves past it.
        let mut next = history.clone();
        next.push(sized(10));
        let q = plan_with_hysteresis(&next, 100, p.boundary, H, L);
        assert!(q.moved);
        assert_eq!(ids(&q.plan.kept), vec![next[3].id]);
    }

    #[test]
    fn hysteresis_boundary_message_deleted_replans() {
        let history: Vec<_> = (0..9).map(|_| sized(10)).collect();
        let p = plan_with_hysteresis(&history, 100, None, H, L);
        let boundary = p.boundary.unwrap();
        let without: Vec<_> = history.iter().filter(|m| m.id != boundary).cloned().collect();
        // What remains still exceeds the high watermark → a fresh batch plan.
        let mut long = without.clone();
        long.extend((0..2).map(|_| sized(10)));
        let r = plan_with_hysteresis(&long, 100, Some(boundary), H, L);
        assert!(r.moved);
        assert_eq!(r.plan.kept.len(), 5);
        assert_ne!(r.boundary, Some(boundary));
        // What remains fits → compaction is dropped altogether.
        let short = without[4..].to_vec();
        let q = plan_with_hysteresis(&short, 100, Some(boundary), H, L);
        assert_eq!(q.boundary, None);
        assert!(q.plan.overflow.is_empty());
        assert!(!q.moved);
    }

    #[test]
    fn hysteresis_boundary_from_a_smaller_window_is_not_inherited() {
        // 60 messages, ~30k tokens: a 25k-budget responder compacts…
        let history: Vec<_> = (0..60).map(|_| sized(504)).collect();
        let small = plan_with_hysteresis(&history, 25_000, None, H, L);
        assert!(small.moved && small.plan.overflow.len() > 30, "{}", small.plan.overflow.len());
        // …a 120k-budget responder handed that boundary keeps everything.
        let big = plan_with_hysteresis(&history, 120_000, small.boundary, H, L);
        assert!(big.plan.overflow.is_empty(), "inherited {} overflow", big.plan.overflow.len());
        assert_eq!(big.boundary, None);
        // A window that grew but still can't hold everything re-plans to its
        // own low watermark instead of keeping the small window's cut.
        let long: Vec<_> = (0..200).map(|_| sized(504)).collect();
        let small = plan_with_hysteresis(&long, 25_000, None, H, L);
        let grown = plan_with_hysteresis(&long, 60_000, small.boundary, H, L);
        let fresh = plan_with_hysteresis(&long, 60_000, None, H, L);
        assert_eq!(grown.boundary, fresh.boundary);
        assert!(grown.moved);
        assert!(grown.plan.kept.len() > small.plan.kept.len());
        // And the larger window's own boundary is stable on the next turn.
        let again = plan_with_hysteresis(&long, 60_000, grown.boundary, H, L);
        assert!(!again.moved);
        assert_eq!(again.boundary, grown.boundary);
    }

    #[test]
    fn hysteresis_boundary_at_head_means_no_compaction() {
        // e.g. /compact removed everything before the old boundary.
        let history: Vec<_> = (0..3).map(|_| sized(10)).collect();
        let p = plan_with_hysteresis(&history, 100, Some(history[0].id), H, L);
        assert_eq!(p.boundary, None);
        assert!(p.plan.overflow.is_empty());
        assert!(!p.moved);
    }

    #[test]
    fn hysteresis_clamps_bad_watermarks() {
        let history: Vec<_> = (0..9).map(|_| sized(10)).collect();
        // low > high is clamped down to high (50 tokens → 5 kept).
        let p = plan_with_hysteresis(&history, 100, None, 0.5, 0.9);
        assert_eq!(p.plan.kept.len(), 5);
        // A NaN high falls back to 1.0: 90 ≤ 100, nothing to compact.
        let p = plan_with_hysteresis(&history, 100, None, f64::NAN, 0.5);
        assert!(p.plan.overflow.is_empty());
    }
}
