/// Lightweight loading placeholders. Pure CSS pulse (no JS), so they cost
/// nothing to render and disappear the instant real data arrives.

export function Skeleton({ className = "", style }: { className?: string; style?: React.CSSProperties }) {
  return (
    <div
      className={`animate-pulse rounded-md ${className}`}
      style={{ background: "var(--hive-line)", opacity: 0.5, ...style }}
    />
  );
}

/// A stack of fake list rows — used for the chat list / member list while the
/// first query is in flight.
export function SkeletonRows({ rows = 5, className = "" }: { rows?: number; className?: string }) {
  return (
    <div className={`space-y-2 ${className}`}>
      {Array.from({ length: rows }).map((_, i) => (
        <div key={i} className="rounded-2xl border px-3 py-3" style={{ borderColor: "var(--hive-line)" }}>
          <Skeleton className="h-3.5 w-2/3" />
          <Skeleton className="mt-2 h-2.5 w-2/5" />
        </div>
      ))}
    </div>
  );
}

/// Fake transcript bubbles for the chat loading state — mirrors the real
/// asymmetric layout (assistant left / user right, avatar outside, capped
/// width) so load → loaded doesn't visibly jump.
export function SkeletonBubbles({ count = 3 }: { count?: number }) {
  return (
    <div className="space-y-4">
      {Array.from({ length: count }).map((_, i) => {
        const isUser = i % 2 === 1;
        return (
          <div key={i} className={`flex gap-2.5 ${isUser ? "flex-row-reverse" : "flex-row"}`}>
            <Skeleton className="mt-6 h-8 w-8 shrink-0 rounded-full" />
            <div className={`flex max-w-[82%] flex-1 flex-col ${isUser ? "items-end" : "items-start"}`}>
              <Skeleton className="mb-1 h-2.5 w-20" />
              <div
                className={`w-full border px-4 py-3 ${isUser ? "rounded-2xl rounded-tr-sm" : "rounded-2xl rounded-tl-sm"}`}
                style={{
                  background: isUser ? "rgba(214,158,87,0.10)" : "rgba(87,161,168,0.08)",
                  borderColor: "var(--hive-line)",
                }}
              >
                <Skeleton className="h-3 w-full" />
                <Skeleton className="mt-2 h-3 w-5/6" />
                {!isUser && <Skeleton className="mt-2 h-3 w-3/4" />}
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}
