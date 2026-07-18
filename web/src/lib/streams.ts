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
