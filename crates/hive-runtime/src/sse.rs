//! Server-Sent Events (SSE) push stream for the relay's live event nudge.
//!
//! The relay exposes `GET /v1/workspaces/{id}/events` as `text/event-stream`.
//! It is a **"wake up and pull" nudge**, not full event delivery: on connect it
//! sends `: connected`, then `data: {"seq":<u64>}` whenever a new envelope is
//! appended, and `: keep-alive` comments periodically. The client reacts to each
//! `data:` frame by triggering an immediate sync pull through the *unchanged*
//! fetch/decode/ingest path — so the transport here stays content-blind.
//!
//! This module holds the **pure, unit-testable** pieces: the SSE line-framing
//! decoder ([`SseDecoder`]), the `{"seq":N}` payload parser ([`parse_seq`]), and
//! the reconnect backoff schedule ([`next_backoff`]). The actual network open
//! lives on [`crate::RelayClient::open_event_stream`], which drives an
//! [`SseDecoder`] over `reqwest`'s `bytes_stream()` and forwards nudges over an
//! [`SseStream`] channel the sync loop can `select!` on.

use std::time::Duration;

/// Incremental decoder for the SSE wire format, robust to chunk boundaries that
/// split a line (or even a multi-byte UTF-8 sequence) across two network reads.
///
/// Feed raw byte chunks via [`SseDecoder::feed`]; it returns the `data` payload
/// of every event whose terminating blank line has arrived. Only complete lines
/// (those ending in `\n`, optionally preceded by `\r`) are consumed, so bytes
/// straddling a chunk boundary are buffered until the rest arrives.
#[derive(Default)]
pub struct SseDecoder {
    /// Bytes received but not yet forming a complete (newline-terminated) line.
    buf: Vec<u8>,
    /// Accumulated `data:` field(s) for the event currently being parsed.
    /// Multiple `data:` lines in one event are joined with `\n` per the spec.
    data: String,
    /// Whether the current event carried at least one `data:` field. Lets a
    /// legitimately empty `data:` payload still dispatch, while a bare
    /// comment-then-blank-line (the keep-alive/connected frames) dispatches
    /// nothing.
    has_data: bool,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of bytes; returns the `data` payload of each event completed
    /// by this chunk (in order). Comment lines (`:` prefix) and non-`data`
    /// fields are ignored; a blank line dispatches the buffered event.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut events = Vec::new();
        // Consume every complete line currently in the buffer. A line is only
        // extracted once its `\n` is present, so a partial line (or a multi-byte
        // char split across chunks) stays buffered until the rest arrives.
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // drop the '\n'
            if line.last() == Some(&b'\r') {
                line.pop(); // CRLF → drop the '\r' too
            }
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                // Blank line: dispatch the buffered event (if it had data).
                if self.has_data {
                    events.push(std::mem::take(&mut self.data));
                }
                self.data.clear();
                self.has_data = false;
            } else if line.starts_with(':') {
                // Comment (`: connected`, `: keep-alive`) — ignore.
            } else if let Some(rest) = line.strip_prefix("data:") {
                // A single optional leading space after the colon is stripped.
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(rest);
                self.has_data = true;
            } else {
                // Other fields (`event:`, `id:`, `retry:`) — irrelevant to a
                // pure nudge; ignore.
            }
        }
        events
    }
}

/// Parse a nudge payload (`{"seq":5}`) to its sequence. Returns `None` for a
/// payload without a numeric `seq` — the caller still treats any `data:` frame
/// as a "something changed" nudge, so an unparseable seq never drops the wake-up.
pub fn parse_seq(data: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(data.trim()).ok()?;
    v.get("seq").and_then(serde_json::Value::as_u64)
}

/// Exponential reconnect backoff for a dropped/failed SSE stream. Pure so the
/// schedule is unit-testable. `attempt` 0 → 1s, then doubling (2s, 4s, 8s, 16s)
/// and capped at 30s so a persistently-down relay is retried at most twice a
/// minute instead of hammered. The old sync loop had *no* backoff (the audit
/// flagged the busy 3s reconnect); this replaces it.
pub fn next_backoff(attempt: u32) -> Duration {
    // 1 << attempt, saturating; cap the exponent so the shift can't overflow.
    let secs = 1u64.checked_shl(attempt.min(5)).unwrap_or(30).min(30);
    Duration::from_secs(secs)
}

/// Why opening the SSE stream failed, so the sync loop can react: `Unsupported`
/// (older relay without the endpoint) → degrade to polling permanently for this
/// config; the rest → back off and keep polling as the safety net.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseConnectError {
    /// The endpoint returned 404 — an older relay that predates SSE push. The
    /// caller should fall back to polling and stop re-attempting the stream.
    Unsupported,
    /// The relay rejected the bearer/entitlement (401/403).
    Unauthorized,
    /// Any other non-2xx status.
    Status(u16),
    /// A transport-level failure (DNS/connect/TLS) before a response arrived.
    Transport(String),
}

impl std::fmt::Display for SseConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SseConnectError::Unsupported => write!(f, "relay has no SSE push endpoint (404)"),
            SseConnectError::Unauthorized => write!(f, "relay rejected the access token"),
            SseConnectError::Status(c) => write!(f, "relay returned HTTP {c}"),
            SseConnectError::Transport(e) => write!(f, "could not reach the relay: {e}"),
        }
    }
}

/// A live SSE nudge stream. Each [`recv`](SseStream::recv) yields the `seq` of a
/// newly-appended envelope (or `0` when the frame lacked a parseable seq — still
/// a valid "pull now" nudge). `None` means the stream ended or errored and the
/// caller should reconnect (with [`next_backoff`]). Dropping the `SseStream`
/// tears down the background reader task and closes the HTTP connection.
pub struct SseStream {
    rx: tokio::sync::mpsc::Receiver<u64>,
}

impl SseStream {
    pub(crate) fn new(rx: tokio::sync::mpsc::Receiver<u64>) -> Self {
        Self { rx }
    }

    /// Await the next nudge. `None` = stream closed/errored → reconnect.
    pub async fn recv(&mut self) -> Option<u64> {
        self.rx.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_single_data_frame() {
        let mut d = SseDecoder::new();
        let events = d.feed(b"data: {\"seq\":5}\n\n");
        assert_eq!(events, vec!["{\"seq\":5}".to_string()]);
        assert_eq!(parse_seq(&events[0]), Some(5));
    }

    #[test]
    fn ignores_comment_frames() {
        let mut d = SseDecoder::new();
        // The connect + keep-alive comments must dispatch no events.
        assert!(d.feed(b": connected\n\n").is_empty());
        assert!(d.feed(b": keep-alive\n\n").is_empty());
        // A comment immediately followed by a real data frame still works.
        let events = d.feed(b": keep-alive\ndata: {\"seq\":9}\n\n");
        assert_eq!(events, vec!["{\"seq\":9}".to_string()]);
    }

    #[test]
    fn reassembles_a_frame_split_across_two_chunks() {
        let mut d = SseDecoder::new();
        // First chunk cuts the line mid-payload (and mid-JSON) — nothing yet.
        assert!(d.feed(b"data: {\"se").is_empty());
        // The rest of the line + terminator arrives in a later chunk.
        let events = d.feed(b"q\":42}\n\n");
        assert_eq!(events, vec!["{\"seq\":42}".to_string()]);
        assert_eq!(parse_seq(&events[0]), Some(42));
    }

    #[test]
    fn handles_multiple_events_in_one_chunk() {
        let mut d = SseDecoder::new();
        let events = d.feed(b"data: {\"seq\":1}\n\ndata: {\"seq\":2}\n\ndata: {\"seq\":3}\n\n");
        assert_eq!(
            events,
            vec![
                "{\"seq\":1}".to_string(),
                "{\"seq\":2}".to_string(),
                "{\"seq\":3}".to_string()
            ]
        );
        let seqs: Vec<u64> = events.iter().filter_map(|e| parse_seq(e)).collect();
        assert_eq!(seqs, vec![1, 2, 3]);
    }

    #[test]
    fn tolerates_crlf_line_endings() {
        let mut d = SseDecoder::new();
        let events = d.feed(b": connected\r\ndata: {\"seq\":7}\r\n\r\n");
        assert_eq!(events, vec!["{\"seq\":7}".to_string()]);
    }

    #[test]
    fn joins_multiple_data_lines_per_event() {
        // Per the SSE spec, consecutive `data:` lines join with '\n'.
        let mut d = SseDecoder::new();
        let events = d.feed(b"data: a\ndata: b\n\n");
        assert_eq!(events, vec!["a\nb".to_string()]);
    }

    #[test]
    fn nudge_without_seq_is_none_but_not_a_panic() {
        assert_eq!(parse_seq("{}"), None);
        assert_eq!(parse_seq("not json"), None);
        assert_eq!(parse_seq("{\"seq\":\"x\"}"), None);
    }

    #[test]
    fn backoff_schedule_is_exponential_and_capped() {
        assert_eq!(next_backoff(0), Duration::from_secs(1));
        assert_eq!(next_backoff(1), Duration::from_secs(2));
        assert_eq!(next_backoff(2), Duration::from_secs(4));
        assert_eq!(next_backoff(3), Duration::from_secs(8));
        assert_eq!(next_backoff(4), Duration::from_secs(16));
        // Capped at 30s from here on — no overflow, no unbounded growth.
        assert_eq!(next_backoff(5), Duration::from_secs(30));
        assert_eq!(next_backoff(6), Duration::from_secs(30));
        assert_eq!(next_backoff(1000), Duration::from_secs(30));
    }
}
