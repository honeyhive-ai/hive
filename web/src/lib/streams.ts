// Live-stream accumulation for chat bubbles: messageId → text so far.
// A Map rather than a single slot because workflow fan-out runs several
// assistant turns concurrently and their deltas interleave.

export function applyStreamDelta(
  prev: Map<string, string>,
  messageId: string,
  text: string,
): Map<string, string> {
  const next = new Map(prev);
  next.set(messageId, (prev.get(messageId) ?? "") + text);
  return next;
}

export function retireStream(prev: Map<string, string>, messageId: string): Map<string, string> {
  const next = new Map(prev);
  next.delete(messageId);
  return next;
}

export interface TerminalResult {
  streams: Map<string, string>;
  retired: Set<string>;
  /** The message was already retired — caller should no-op (duplicate terminal). */
  duplicate: boolean;
  /** No live streams remain — caller clears the shared sending/optimistic state. */
  cleared: boolean;
}

/// Handle a terminal (completed/error) stream event idempotently — the contract
/// behind the chat's "thinking" lifecycle:
///   - a DUPLICATE terminal for an already-retired message is a no-op, so it can't
///     collapse a sibling turn's state (P2-11);
///   - otherwise retire the message's live stream and report whether the LAST one
///     is now gone, i.e. whether the shared sending/optimistic state should clear
///     (a sibling parallel workflow stage may still be live → keep "thinking").
/// A completion with no prior delta (message never in `streams`) still clears when
/// it's the only in-flight turn — the zero-delta path.
export function handleTerminal(
  streams: Map<string, string>,
  retired: Set<string>,
  messageId: string,
): TerminalResult {
  if (retired.has(messageId)) {
    return { streams, retired, duplicate: true, cleared: false };
  }
  const nextRetired = new Set(retired);
  nextRetired.add(messageId);
  const nextStreams = retireStream(streams, messageId);
  return { streams: nextStreams, retired: nextRetired, duplicate: false, cleared: nextStreams.size === 0 };
}
