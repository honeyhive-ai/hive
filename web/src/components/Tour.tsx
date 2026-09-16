import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";

/// One stop on the guided tour. Anchors to a `[data-tour="<anchor>"]` element in
/// the live shell (omit `anchor` for a centered intro/outro card). `before` drives
/// the UI into the state the step describes (switch canvas mode, reveal a pane)
/// and runs just before the anchor is measured, so the spotlight lands on what
/// the copy is talking about.
export interface TourStep {
  anchor?: string;
  title: string;
  body: ReactNode;
  placement?: Side;
  before?: () => void;
}

type Side = "top" | "bottom" | "left" | "right";
type Rect = { top: number; left: number; width: number; height: number };

const PAD = 8; // spotlight breathing room around the anchor
const GAP = 14; // bubble distance from the spotlight
const BUBBLE_W = 340;

const reduceMotion =
  typeof window !== "undefined" &&
  typeof window.matchMedia === "function" &&
  window.matchMedia("(prefers-reduced-motion: reduce)").matches;

/// A skippable, spotlight-style product tour. Dims the whole app and cuts a hole
/// around one region at a time while a bubble explains it; the host supplies the
/// steps (and their `before` UI-driving callbacks). `onClose(true)` = finished
/// the last step, `onClose(false)` = skipped / Esc.
export function Tour({ steps, onClose }: { steps: TourStep[]; onClose: (completed: boolean) => void }) {
  const [i, setI] = useState(0);
  const [rect, setRect] = useState<Rect | null>(null);
  const [vp, setVp] = useState(() => ({ w: window.innerWidth, h: window.innerHeight }));
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);
  const bubbleRef = useRef<HTMLDivElement>(null);
  const step = steps[i];
  const atEnd = i >= steps.length - 1;

  const measure = useCallback(() => {
    setVp({ w: window.innerWidth, h: window.innerHeight });
    const a = steps[i]?.anchor;
    if (!a) {
      setRect(null);
      return;
    }
    const el = document.querySelector<HTMLElement>(`[data-tour="${a}"]`);
    if (!el) {
      setRect(null);
      return;
    }
    const r = el.getBoundingClientRect();
    setRect({ top: r.top, left: r.left, width: r.width, height: r.height });
  }, [i, steps]);

  // Run the step's UI side effect, then measure once React has flushed it and the
  // layout has settled. The late timeout catches lazily-mounted panes (the Code
  // and Diff views mount through Suspense, so their box isn't there on frame one).
  useLayoutEffect(() => {
    steps[i]?.before?.();
    let raf2 = 0;
    const raf1 = requestAnimationFrame(() => {
      measure();
      raf2 = requestAnimationFrame(measure);
    });
    const t = window.setTimeout(measure, 280);
    return () => {
      cancelAnimationFrame(raf1);
      cancelAnimationFrame(raf2);
      window.clearTimeout(t);
    };
    // Only re-run when the step changes — not on every parent re-render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [i]);

  useEffect(() => {
    const on = () => measure();
    window.addEventListener("resize", on);
    window.addEventListener("scroll", on, true);
    return () => {
      window.removeEventListener("resize", on);
      window.removeEventListener("scroll", on, true);
    };
  }, [measure]);

  const next = useCallback(() => (atEnd ? onClose(true) : setI((n) => n + 1)), [atEnd, onClose]);
  const back = useCallback(() => setI((n) => Math.max(0, n - 1)), []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose(false);
      } else if (e.key === "ArrowRight" || e.key === "Enter") {
        e.preventDefault();
        next();
      } else if (e.key === "ArrowLeft") {
        e.preventDefault();
        back();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [next, back, onClose]);

  // Position the bubble against the spotlight once we know both boxes. Prefer the
  // step's placement hint, fall back to whichever side has room, then clamp fully
  // into the viewport so it never runs off an edge.
  const hole = rect
    ? { x: rect.left - PAD, y: rect.top - PAD, w: rect.width + PAD * 2, h: rect.height + PAD * 2 }
    : null;
  useLayoutEffect(() => {
    const b = bubbleRef.current;
    if (!b) return;
    const bw = b.offsetWidth || BUBBLE_W;
    const bh = b.offsetHeight || 160;
    let top: number;
    let left: number;
    if (!hole) {
      top = (vp.h - bh) / 2;
      left = (vp.w - bw) / 2;
    } else {
      const side = step?.placement ?? autoSide(hole, bw, bh, vp);
      if (side === "right") {
        left = hole.x + hole.w + GAP;
        top = hole.y + hole.h / 2 - bh / 2;
      } else if (side === "left") {
        left = hole.x - GAP - bw;
        top = hole.y + hole.h / 2 - bh / 2;
      } else if (side === "top") {
        top = hole.y - GAP - bh;
        left = hole.x + hole.w / 2 - bw / 2;
      } else {
        top = hole.y + hole.h + GAP;
        left = hole.x + hole.w / 2 - bw / 2;
      }
    }
    left = Math.max(GAP, Math.min(left, vp.w - bw - GAP));
    top = Math.max(GAP, Math.min(top, vp.h - bh - GAP));
    setPos({ top, left });
  }, [rect, hole?.x, hole?.y, hole?.w, hole?.h, vp.w, vp.h, i, step?.placement]);

  if (!step) return null;
  const maskId = "hive-tour-hole";

  return (
    <div className="fixed inset-0 z-[1000]" role="dialog" aria-modal="true" aria-label="Product tour">
      {/* The dim + spotlight. Captures clicks so the guided flow can't be nudged
          out of place; navigation is via the bubble buttons or the keyboard. */}
      <svg
        width={vp.w}
        height={vp.h}
        className="absolute inset-0"
        onClick={() => {}}
        style={{ display: "block" }}
      >
        <defs>
          <mask id={maskId}>
            <rect x={0} y={0} width={vp.w} height={vp.h} fill="white" />
            {hole && <rect x={hole.x} y={hole.y} width={hole.w} height={hole.h} rx={16} fill="black" />}
          </mask>
        </defs>
        <rect x={0} y={0} width={vp.w} height={vp.h} fill="rgba(9,11,16,0.62)" mask={`url(#${maskId})`} />
        {hole && (
          <rect
            x={hole.x}
            y={hole.y}
            width={hole.w}
            height={hole.h}
            rx={16}
            fill="none"
            stroke="var(--hive-accent-cool)"
            strokeWidth={2}
          />
        )}
      </svg>

      <div
        ref={bubbleRef}
        className="rounded-2xl border p-4 shadow-2xl"
        style={{
          position: "absolute",
          width: BUBBLE_W,
          maxWidth: "calc(100vw - 28px)",
          top: pos?.top ?? -9999,
          left: pos?.left ?? -9999,
          background: "var(--hive-panel)",
          borderColor: "var(--hive-line)",
          color: "var(--hive-ink)",
          visibility: pos ? "visible" : "hidden",
          transition: reduceMotion ? undefined : "top .26s cubic-bezier(.4,0,.2,1), left .26s cubic-bezier(.4,0,.2,1)",
        }}
      >
        <div className="text-sm font-semibold">{step.title}</div>
        <div className="mt-1.5 text-[13px] leading-relaxed opacity-70">{step.body}</div>
        <div className="mt-4 flex items-center justify-between">
          <div className="flex items-center gap-1.5" aria-label={`Step ${i + 1} of ${steps.length}`}>
            {steps.map((_, n) => (
              <span
                key={n}
                className="h-1.5 rounded-full transition-all"
                style={{
                  width: n === i ? 16 : 6,
                  background: n === i ? "var(--hive-accent-cool)" : "var(--hive-line)",
                }}
              />
            ))}
          </div>
          <div className="flex items-center gap-2">
            {i > 0 && (
              <button
                type="button"
                onClick={back}
                className="rounded-lg px-2.5 py-1.5 text-xs font-medium opacity-60 hover:opacity-100"
              >
                Back
              </button>
            )}
            <button
              type="button"
              onClick={() => onClose(false)}
              className="rounded-lg px-2.5 py-1.5 text-xs font-medium opacity-50 hover:opacity-100"
            >
              {atEnd ? "Close" : "Skip"}
            </button>
            <button
              type="button"
              onClick={next}
              className="rounded-xl px-4 py-1.5 text-xs font-semibold hover:brightness-110"
              // `--hive-on-accent` is the scheme-derived legible foreground for the
              // accent fill (dark on Obsidian's silver, white elsewhere) — plain
              // `text-white` was invisible on Obsidian's near-white accent.
              style={{ background: "var(--hive-accent-cool)", color: "var(--hive-on-accent)" }}
            >
              {atEnd ? "Done" : i === 0 ? "Start" : "Next"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

function autoSide(hole: { x: number; y: number; w: number; h: number }, bw: number, bh: number, vp: { w: number; h: number }): Side {
  if (hole.x + hole.w + GAP + bw <= vp.w) return "right";
  if (hole.x - GAP - bw >= 0) return "left";
  if (hole.y + hole.h + GAP + bh <= vp.h) return "bottom";
  return "top";
}
