import { avColor, initials } from "@/lib/avatar";

// One avatar for every render site. Shows the uploaded image when present,
// else initials on a color (an explicit accent, or a deterministic one from the
// name). `kind` picks the shape: human = circle, agent = rounded square.
export function Avatar({
  name,
  url,
  colorHex,
  kind = "human",
  size = 30,
}: {
  name: string;
  url?: string | null;
  colorHex?: string | null;
  kind?: "human" | "agent";
  size?: number;
}) {
  const radius = kind === "agent" ? "30%" : "50%";
  if (url) {
    return (
      <img
        src={url}
        alt=""
        aria-hidden
        width={size}
        height={size}
        style={{ width: size, height: size, borderRadius: radius, objectFit: "cover", flex: "none" }}
      />
    );
  }
  return (
    <div
      aria-hidden
      style={{
        width: size,
        height: size,
        borderRadius: radius,
        background: colorHex || avColor(name),
        color: "#fff",
        fontSize: Math.round(size * 0.4),
        fontWeight: 600,
        display: "grid",
        placeItems: "center",
        flex: "none",
      }}
    >
      {initials(name)}
    </div>
  );
}
