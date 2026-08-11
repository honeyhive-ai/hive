// Holding a chat transcript pinned to its newest turn, factored out of ChatView
// so the settling logic is unit-testable without a real layout engine.
//
// A single `scrollTop = scrollHeight` is not enough here, and that is why the
// view lands mid-conversation when you navigate away and back. Turn rows carry
// `content-visibility: auto`, so a row the engine hasn't laid out reports its
// `contain-intrinsic-size` estimate (80px) instead of its real height. On a
// fresh mount nothing is painted yet, so `scrollHeight` for a long thread is a
// large underestimate — scrolling to it puts you somewhere in the middle. The
// rows around that landing spot then get laid out for real, grow past the
// estimate, and strand the viewport above the newest turn with no further event
// to correct it (content growing below the scroll offset fires no scroll event).
//
// So we re-pin across frames until `scrollHeight` stops moving. This converges
// because the rows keep their measured height once seen (`contain-intrinsic-
// size: auto`); with the non-auto form they revert to the estimate on scrolling
// past, and the measurement oscillates instead of settling.

/// The slice of a scroll container this needs — an element satisfies it, and so
/// can a fake in a test.
export interface Pinnable {
  scrollTop: number;
  readonly scrollHeight: number;
}

/// How close to the end counts as "at the bottom".
export const NEAR_BOTTOM_PX = 80;

/// The slice of a scroll container needed to judge the pin state.
export interface ScrollMetrics {
  readonly scrollTop: number;
  readonly scrollHeight: number;
  readonly clientHeight: number;
}

/// Whether the transcript should stay pinned after a scroll event.
///
/// The subtlety is that "far from the bottom" does not imply the reader moved.
/// A pin that lands on a still-settling layout grows scrollHeight *below* the
/// viewport as the rows there are laid out for real, so the next scroll event
/// reports a large distance to the end while the reader has done nothing.
/// Un-pinning on that reading would cancel the settle loop mid-flight and
/// strand the view exactly where it strands today. So leaving the bottom takes
/// an actual upward scroll; growth underneath never does it.
export function pinAfterScroll(
  prev: { top: number; pinned: boolean },
  now: ScrollMetrics,
): boolean {
  if (now.scrollHeight - now.scrollTop - now.clientHeight < NEAR_BOTTOM_PX) return true;
  return prev.pinned && now.scrollTop >= prev.top;
}

/// What the scroll-to-newest affordance should show, or `null` to hide it.
///
/// Two different situations want the same button, and only the first was
/// handled: content arrived while the reader was scrolled up ("New messages").
/// The second — the reader scrolled up to re-read something and now just wants
/// back to the live end — surfaced nothing, so the only way down was to drag the
/// whole way by hand. Both mean "put me at the newest turn", so both get the
/// button; they differ only in label and emphasis, since unread content is worth
/// advertising and a plain jump should stay quiet.
export type JumpHint = "new" | "jump";

export function jumpHint(state: { atBottom: boolean; hasNew: boolean }): JumpHint | null {
  if (state.atBottom) return null;
  return state.hasNew ? "new" : "jump";
}

/// Frames of unchanged scrollHeight before the layout counts as settled.
const STABLE_FRAMES = 2;
/// Hard cap, so a live stream (which grows the transcript every frame) can't
/// keep one loop alive indefinitely. Callers re-arm on the next update anyway.
const MAX_FRAMES = 30;

export function pinToBottom(
  el: () => Pinnable | null,
  opts: {
    /// Checked before every re-pin: false means the reader scrolled away and we
    /// must stop, rather than yanking them back down.
    pinned?: () => boolean;
    raf?: (cb: () => void) => number;
    cancelRaf?: (handle: number) => void;
  } = {},
): () => void {
  const pinned = opts.pinned ?? (() => true);
  const raf =
    opts.raf ?? (typeof requestAnimationFrame === "function" ? requestAnimationFrame : null);
  const cancelRaf =
    opts.cancelRaf ?? (typeof cancelAnimationFrame === "function" ? cancelAnimationFrame : null);

  let handle: number | null = null;
  let frames = 0;
  let stable = 0;
  let last = -1;
  let cancelled = false;

  const step = () => {
    handle = null;
    const node = el();
    if (cancelled || !node || !pinned()) return;
    node.scrollTop = node.scrollHeight;
    stable = node.scrollHeight === last ? stable + 1 : 0;
    last = node.scrollHeight;
    frames += 1;
    if (stable >= STABLE_FRAMES || frames >= MAX_FRAMES || !raf) return;
    handle = raf(step);
  };

  // The first pin is synchronous: the common case (already-measured layout) is
  // then indistinguishable from the old one-shot scroll, with no flash.
  step();

  return () => {
    cancelled = true;
    if (handle !== null && cancelRaf) cancelRaf(handle);
    handle = null;
  };
}
