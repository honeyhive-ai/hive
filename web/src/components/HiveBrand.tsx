import { useId } from "react";

function HiveRosette({
  gradientPrefix,
  markOnly = false,
  rosetteScale = 0.4173913,
}: {
  gradientPrefix: string;
  markOnly?: boolean;
  rosetteScale?: number;
}) {
  return (
    <>
      <defs>
        <linearGradient id={`${gradientPrefix}-warm`} x1="0" y1="0" x2="0.4" y2="1">
          <stop offset="0" stopColor="#F0B878" />
          <stop offset="1" stopColor="#D98A44" />
        </linearGradient>
        {!markOnly && (
          <linearGradient id={`${gradientPrefix}-tile`} x1="0" y1="0" x2="0.5" y2="1">
            <stop offset="0" stopColor="#9A6620" />
            <stop offset="1" stopColor="#4A2E12" />
          </linearGradient>
        )}
      </defs>
      {!markOnly && <rect x="2" y="2" width="60" height="60" rx="14" fill={`url(#${gradientPrefix}-tile)`} />}
      {/* Canonical honeycomb rosette (assets/branding/HiveLogo.html): a lit core
          plus a 6-cell ring; active amber cells at the right (0°) and up-left
          (240°), inactive cells alternating cream. R=14, 0.9 gap. */}
      <g transform={`translate(32 32) scale(${rosetteScale})`}>
        <path d="M0.00 -12.60 L 10.91 -6.30 L 10.91 6.30 L 0.00 12.60 L -10.91 6.30 L -10.91 -6.30 Z" fill={`url(#${gradientPrefix}-warm)`} />
        <path d="M24.25 -12.60 L 35.16 -6.30 L 35.16 6.30 L 24.25 12.60 L 13.34 6.30 L 13.34 -6.30 Z" fill={`url(#${gradientPrefix}-warm)`} />
        <path d="M12.12 8.40 L 23.04 14.70 L 23.04 27.30 L 12.12 33.60 L 1.21 27.30 L 1.21 14.70 Z" fill="#F0E0B8" />
        <path d="M-12.12 8.40 L -1.21 14.70 L -1.21 27.30 L -12.12 33.60 L -23.04 27.30 L -23.04 14.70 Z" fill="#FBF1D8" />
        <path d="M-24.25 -12.60 L -13.34 -6.30 L -13.34 6.30 L -24.25 12.60 L -35.16 6.30 L -35.16 -6.30 Z" fill="#F0E0B8" />
        <path d="M-12.12 -33.60 L -1.21 -27.30 L -1.21 -14.70 L -12.12 -8.40 L -23.04 -14.70 L -23.04 -27.30 Z" fill={`url(#${gradientPrefix}-warm)`} />
        <path d="M12.12 -33.60 L 23.04 -27.30 L 23.04 -14.70 L 12.12 -8.40 L 1.21 -14.70 L 1.21 -27.30 Z" fill="#F0E0B8" />
      </g>
    </>
  );
}

export function HiveBrandIcon({
  size = 40,
  className,
  title = "Hive",
}: {
  size?: number;
  className?: string;
  title?: string;
}) {
  const reactId = useId().replace(/:/g, "");
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 64 64"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      className={className}
      role="img"
      aria-label={title}
    >
      <title>{title}</title>
      <HiveRosette gradientPrefix={`hive-icon-${reactId}`} />
    </svg>
  );
}

export function HiveBrandMark({
  size = 40,
  className,
  title = "Hive",
}: {
  size?: number;
  className?: string;
  title?: string;
}) {
  const reactId = useId().replace(/:/g, "");
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 64 64"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      className={className}
      role="img"
      aria-label={title}
      style={{ display: "block" }}
    >
      <title>{title}</title>
      {/* Fill ~84% of the 64 box (half-extent 35.16): 32*0.84/35.16 ≈ 0.764. */}
      <HiveRosette gradientPrefix={`hive-mark-${reactId}`} markOnly rosetteScale={0.764} />
    </svg>
  );
}
