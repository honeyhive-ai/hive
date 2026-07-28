import type { CSSProperties } from "react";
import { avColor, initials } from "@/lib/avatar";

// One avatar for every render site. Shows the uploaded image when present,
// else initials on a color (an explicit accent, or a deterministic one from the
// name). `kind` picks the shape: human = circle, agent = rounded square — so
// role is carried by geometry, not hue alone (survives greyscale / colour-blind
// / before the convention is learned). An agent may also carry a `host` badge
// (laptop = local, cloud = remote) so the avatar says *where* it runs.
export function Avatar({
  name,
  url,
  colorHex,
  kind = "human",
  host,
  size = 30,
}: {
  name: string;
  url?: string | null;
  colorHex?: string | null;
  kind?: "human" | "agent";
  host?: "local" | "remote" | null;
  size?: number;
}) {
  const radius = kind === "agent" ? "30%" : "50%";
  const cls = `av ${kind}`;
  const base: CSSProperties = { width: size, height: size, borderRadius: radius, flex: "none" };
  const face = url ? (
    <img src={url} alt="" aria-hidden width={size} height={size} className={cls} style={{ ...base, objectFit: "cover" }} />
  ) : (
    <div
      aria-hidden
      className={cls}
      style={{
        ...base,
        background: colorHex || avColor(name),
        color: "#fff",
        fontSize: Math.round(size * 0.4),
        fontWeight: 600,
        display: "grid",
        placeItems: "center",
      }}
    >
      {initials(name)}
    </div>
  );

  // No host badge → return the bare face (keeps every existing call site pixel-
  // identical). With one, wrap in a relative box and pin the badge bottom-right.
  if (kind !== "agent" || !host) return face;
  const badge = Math.max(11, Math.round(size * 0.42));
  return (
    <span style={{ position: "relative", display: "inline-flex", flex: "none" }}>
      {face}
      <span
        title={host === "remote" ? "Runs on a remote host" : "Runs on this device"}
        style={{
          position: "absolute",
          right: -2,
          bottom: -2,
          width: badge,
          height: badge,
          display: "grid",
          placeItems: "center",
          borderRadius: "50%",
          background: "var(--hive-panel)",
          color: "var(--hive-ink-soft)",
          boxShadow: "0 0 0 1.5px var(--hive-panel)",
        }}
      >
        {host === "remote" ? <CloudGlyph size={badge - 4} /> : <LaptopGlyph size={badge - 4} />}
      </span>
    </span>
  );
}

function LaptopGlyph({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <rect x="4" y="5" width="16" height="11" rx="1.5" />
      <path d="M2 20h20" />
    </svg>
  );
}

function CloudGlyph({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="M6 18a4 4 0 0 1-.4-7.98A5.5 5.5 0 0 1 16.5 9 4.5 4.5 0 0 1 17 18Z" />
    </svg>
  );
}
