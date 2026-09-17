//! Streaming separation of a model's *reasoning* from its *reply*.
//!
//! Local reasoning models (qwen3, deepseek-r1, …) served through an
//! OpenAI-compatible endpoint often emit their chain of thought inline as
//! `<think>…</think>` before the answer. Left alone, that text lands in the
//! transcript as the reply, gets scanned for `@mentions`, and can trip the
//! `[[workflow:]]` / `[[propose:]]` directive parser. [`ThinkFilter`] is a small
//! stateful splitter that is fed the raw streamed deltas and hands back, per
//! push, the part that is reply text and the part that is reasoning — with tag
//! boundaries handled even when a tag arrives split across chunks.

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

/// The two halves of one streamed delta after filtering.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Split {
    /// Reply text (goes to the transcript / on_delta).
    pub content: String,
    /// Reasoning text (goes to the thinking sink; never the transcript).
    pub thinking: String,
}

/// Stateful `<think>` / `</think>` splitter for a token stream.
#[derive(Debug, Default)]
pub struct ThinkFilter {
    in_think: bool,
    /// Bytes held back because they *might* be the start of a tag that hasn't
    /// fully arrived yet (e.g. a chunk ending in `<thi`).
    held: String,
    /// True right after a think block closes, so the leading blank lines models
    /// put between reasoning and answer don't become the reply's first bytes.
    trim_leading: bool,
}

impl ThinkFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one raw delta. Returns the portion to treat as reply text and the
    /// portion to treat as reasoning. Either may be empty.
    pub fn push(&mut self, delta: &str) -> Split {
        let mut out = Split::default();
        let mut buf = std::mem::take(&mut self.held);
        buf.push_str(delta);

        loop {
            let tag = if self.in_think { CLOSE } else { OPEN };
            match buf.find(tag) {
                Some(idx) => {
                    let before = &buf[..idx];
                    self.emit(&mut out, before);
                    buf.drain(..idx + tag.len());
                    self.in_think = !self.in_think;
                    if !self.in_think {
                        self.trim_leading = true;
                    }
                }
                None => {
                    // No full tag. Hold back any suffix that is a proper prefix
                    // of the tag we're looking for (a split tag in flight);
                    // everything before it is safe to emit.
                    let keep = longest_tag_prefix_suffix(&buf, tag);
                    let safe_len = buf.len() - keep;
                    let (safe, rest) = buf.split_at(safe_len);
                    self.emit(&mut out, safe);
                    self.held = rest.to_string();
                    break;
                }
            }
        }
        out
    }

    /// Flush anything still held at end of stream. A held partial tag is by
    /// definition not a tag, so it is plain text in whichever mode we're in.
    pub fn finish(&mut self) -> Split {
        let mut out = Split::default();
        let held = std::mem::take(&mut self.held);
        self.emit(&mut out, &held);
        out
    }

    /// Whether the stream is currently inside a think block.
    pub fn in_think(&self) -> bool {
        self.in_think
    }

    fn emit(&mut self, out: &mut Split, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.in_think {
            out.thinking.push_str(text);
        } else {
            let text = if self.trim_leading {
                let t = text.trim_start();
                if !t.is_empty() {
                    self.trim_leading = false;
                }
                t
            } else {
                text
            };
            out.content.push_str(text);
        }
    }
}

/// Length of the longest suffix of `buf` that is a proper prefix of `tag`
/// (0 when none). Used to hold back a possibly-split tag.
fn longest_tag_prefix_suffix(buf: &str, tag: &str) -> usize {
    let max = tag.len().saturating_sub(1).min(buf.len());
    for n in (1..=max).rev() {
        if !buf.is_char_boundary(buf.len() - n) {
            continue;
        }
        if buf.ends_with(&tag[..n]) {
            return n;
        }
    }
    0
}

/// One-shot convenience: split a fully-assembled reply.
pub fn split_reply(text: &str) -> Split {
    let mut f = ThinkFilter::new();
    let mut s = f.push(text);
    let tail = f.finish();
    s.content.push_str(&tail.content);
    s.thinking.push_str(&tail.thinking);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&str]) -> Split {
        let mut f = ThinkFilter::new();
        let mut acc = Split::default();
        for c in chunks {
            let s = f.push(c);
            acc.content.push_str(&s.content);
            acc.thinking.push_str(&s.thinking);
        }
        let s = f.finish();
        acc.content.push_str(&s.content);
        acc.thinking.push_str(&s.thinking);
        acc
    }

    #[test]
    fn plain_text_passes_through_untouched() {
        let s = run(&["Hello, ", "world"]);
        assert_eq!(s.content, "Hello, world");
        assert_eq!(s.thinking, "");
    }

    #[test]
    fn whole_block_in_one_chunk() {
        let s = run(&["<think>plan it</think>\n\nAnswer."]);
        assert_eq!(s.thinking, "plan it");
        assert_eq!(s.content, "Answer.");
    }

    #[test]
    fn tag_split_across_chunks() {
        let s = run(&["<th", "ink>rea", "soning</th", "ink>", "\nReply"]);
        assert_eq!(s.thinking, "reasoning");
        assert_eq!(s.content, "Reply");
    }

    #[test]
    fn lone_angle_bracket_is_not_swallowed() {
        // A `<` that never becomes a tag must still be delivered.
        let s = run(&["a < b", " and c"]);
        assert_eq!(s.content, "a < b and c");
        // A dangling partial tag at end of stream is flushed as text.
        let s = run(&["x <thi"]);
        assert_eq!(s.content, "x <thi");
    }

    #[test]
    fn reasoning_only_reply_yields_empty_content() {
        let s = run(&["<think>still thinking"]);
        assert_eq!(s.thinking, "still thinking");
        assert_eq!(s.content, "");
    }

    #[test]
    fn text_before_think_is_content() {
        let s = run(&["Sure. <think>hmm</think> Done."]);
        assert_eq!(s.content, "Sure. Done.");
        assert_eq!(s.thinking, "hmm");
    }

    #[test]
    fn multibyte_text_near_a_partial_tag_is_safe() {
        let s = run(&["héllo <", "think>ü</think>ö"]);
        assert_eq!(s.thinking, "ü");
        assert_eq!(s.content, "héllo ö");
    }

    #[test]
    fn split_reply_one_shot() {
        let s = split_reply("<think>a</think>b");
        assert_eq!(s.thinking, "a");
        assert_eq!(s.content, "b");
    }
}
