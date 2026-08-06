import { describe, expect, it } from "vitest";
import { pinToBottom, pinAfterScroll } from "./autoscroll";

// A stand-in for the transcript container under `content-visibility: auto`.
// A row reports an 80px estimate until it has been inside the viewport once,
// after which it reports its real height and keeps it (that is the `auto` in
// `contain-intrinsic-size: auto 80px`). So the container's scrollHeight is a
// large underestimate until you have actually scrolled through it — the exact
// condition the pin loop exists to survive.
class FakeTranscript {
  scrollTopValue = 0;
  seen: boolean[];
  constructor(
    readonly rows: number,
    readonly estimate: number,
    readonly real: number,
    readonly viewport: number,
  ) {
    this.seen = new Array(rows).fill(false);
  }
  private height(i: number) {
    return this.seen[i] ? this.real : this.estimate;
  }
  get scrollHeight() {
    let total = 0;
    for (let i = 0; i < this.rows; i++) total += this.height(i);
    return total;
  }
  get scrollTop() {
    return this.scrollTopValue;
  }
  set scrollTop(v: number) {
    const max = Math.max(0, this.scrollHeight - this.viewport);
    this.scrollTopValue = Math.min(Math.max(0, v), max);
    const tops: number[] = [];
    let off = 0;
    for (let i = 0; i < this.rows; i++) {
      tops.push(off);
      off += this.height(i);
    }
    for (let i = 0; i < this.rows; i++) {
      const bottom = tops[i] + this.height(i);
      if (bottom > this.scrollTopValue && tops[i] < this.scrollTopValue + this.viewport) {
        this.seen[i] = true;
      }
    }
  }
  get atBottom() {
    return this.scrollHeight - this.scrollTop - this.viewport < 1;
  }
}

// A hand-driven requestAnimationFrame so the settle loop runs deterministically.
function manualRaf() {
  const queue = new Map<number, () => void>();
  let id = 0;
  return {
    raf: (cb: () => void) => {
      queue.set(++id, cb);
      return id;
    },
    cancelRaf: (handle: number) => {
      queue.delete(handle);
    },
    get pending() {
      return queue.size;
    },
    drain(limit = 200) {
      let ran = 0;
      while (queue.size > 0 && ran < limit) {
        const key = [...queue.keys()][0];
        const cb = queue.get(key)!;
        queue.delete(key);
        cb();
        ran += 1;
      }
      return ran;
    },
  };
}

const longThread = () => new FakeTranscript(100, 80, 300, 600);

describe("pinToBottom", () => {
  it("negative control: one scroll to scrollHeight lands mid-conversation", () => {
    const el = longThread();
    el.scrollTop = el.scrollHeight;
    expect(el.atBottom).toBe(false);
  });

  it("reaches the newest turn even though off-screen rows under-report height", () => {
    const el = longThread();
    const clock = manualRaf();
    pinToBottom(() => el, { raf: clock.raf, cancelRaf: clock.cancelRaf });
    clock.drain();
    expect(el.atBottom).toBe(true);
  });

  it("settles instead of spinning: the loop ends on its own", () => {
    const el = longThread();
    const clock = manualRaf();
    pinToBottom(() => el, { raf: clock.raf, cancelRaf: clock.cancelRaf });
    expect(clock.drain()).toBeLessThan(200);
    expect(clock.pending).toBe(0);
  });

  it("pins a settled transcript without needing a scheduler", () => {
    const el = new FakeTranscript(10, 80, 80, 600);
    pinToBottom(() => el, { raf: undefined, cancelRaf: undefined, pinned: () => true });
    expect(el.atBottom).toBe(true);
  });

  it("stops when the reader scrolls away mid-settle", () => {
    const el = longThread();
    const clock = manualRaf();
    let pinned = true;
    pinToBottom(() => el, { raf: clock.raf, cancelRaf: clock.cancelRaf, pinned: () => pinned });
    pinned = false;
    const parked = el.scrollTop;
    clock.drain();
    expect(el.scrollTop).toBe(parked);
    expect(el.atBottom).toBe(false);
  });

  it("cancel stops a settle already in flight", () => {
    const el = longThread();
    const clock = manualRaf();
    const cancel = pinToBottom(() => el, { raf: clock.raf, cancelRaf: clock.cancelRaf });
    const parked = el.scrollTop;
    cancel();
    clock.drain();
    expect(el.scrollTop).toBe(parked);
  });

  it("tolerates the container being unmounted mid-settle", () => {
    let el: FakeTranscript | null = longThread();
    const clock = manualRaf();
    pinToBottom(() => el, { raf: clock.raf, cancelRaf: clock.cancelRaf });
    el = null;
    expect(() => clock.drain()).not.toThrow();
  });
});

describe("pinAfterScroll", () => {
  // The regression the pin loop cannot survive on its own. A pin lands at the
  // bottom of an under-measured transcript; the rows it revealed are then laid
  // out for real and scrollHeight grows below the viewport. The scroll event
  // that follows reports a huge distance to the end, but the reader never
  // touched anything — treating that as "scrolled away" cancels the settle.
  it("growth underneath a settled pin does not un-pin", () => {
    const pinned = pinAfterScroll(
      { top: 7400, pinned: true },
      { scrollTop: 7400, scrollHeight: 9760, clientHeight: 600 },
    );
    expect(pinned).toBe(true);
  });

  it("un-pins when the reader actually scrolls up", () => {
    const pinned = pinAfterScroll(
      { top: 7400, pinned: true },
      { scrollTop: 5000, scrollHeight: 8000, clientHeight: 600 },
    );
    expect(pinned).toBe(false);
  });

  it("re-pins on reaching the bottom again", () => {
    const pinned = pinAfterScroll(
      { top: 2000, pinned: false },
      { scrollTop: 7400, scrollHeight: 8000, clientHeight: 600 },
    );
    expect(pinned).toBe(true);
  });

  it("stays away while the reader scrolls down but not to the end", () => {
    const pinned = pinAfterScroll(
      { top: 2000, pinned: false },
      { scrollTop: 4000, scrollHeight: 8000, clientHeight: 600 },
    );
    expect(pinned).toBe(false);
  });

  // The whole loop, driven the way a browser drives it: pin, let the revealed
  // rows lay out, deliver the scroll event that reports the new height. The
  // reader stays pinned throughout, so the settle is never cancelled.
  it("survives a full settle without the reader touching anything", () => {
    const el = longThread();
    const clock = manualRaf();
    let pinned = true;
    let mark = { top: el.scrollTop, pinned };
    pinToBottom(() => el, {
      raf: (cb) =>
        clock.raf(() => {
          // The scroll event for the previous frame's pin arrives first — the
          // HTML rendering steps run scroll steps ahead of animation frames.
          pinned = pinAfterScroll(mark, {
            scrollTop: el.scrollTop,
            scrollHeight: el.scrollHeight,
            clientHeight: el.viewport,
          });
          mark = { top: el.scrollTop, pinned };
          cb();
        }),
      cancelRaf: clock.cancelRaf,
      pinned: () => pinned,
    });
    clock.drain();
    expect(pinned).toBe(true);
    expect(el.atBottom).toBe(true);
  });
});
