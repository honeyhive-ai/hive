// The derived token layer.
//
// On desktop these live in `:root` as CSS custom properties built with
// `color-mix()` and `oklch(from …)`. Here they are computed once per
// theme-resolve and handed to components as flat colour strings.
//
// Scope note, because it is easy to assume this file absorbs everything: the
// desktop also makes 52 further `color-mix` calls inline across 17 component
// files. Those are per-component and stay per-component — port each with
// `mixSrgb` at its call site. What is centralised here is exactly the `:root`
// set plus the per-turn derivations, because those are the ones every screen
// shares.

import { formatColor, mixSrgb, oklchReplaceL, type Rgba } from "./color";
import {
  CHAT,
  STATUS,
  resolvePalette,
  type Palette,
  type Scheme,
  type ThemeName,
} from "./palettes";

export interface Tokens {
  name: ThemeName;
  scheme: Scheme;

  // --- Base palette, straight through. ---
  canvas: string;
  ink: string;
  panel: string;
  mist: string;
  line: string;
  accentWarm: string;
  accentCool: string;
  sidebarTop: string;
  sidebarBottom: string;
  sidebarInk: string;
  sidebarInkMuted: string;

  // --- Scheme-derived status set. ---
  success: string;
  danger: string;
  warn: string;
  overlay: string;
  onAccent: string;

  // --- Derived in sRGB. ---
  inkSoft: string;
  inkFaint: string;
  hover: string;
  tintCool: string;
  tintCoolLine: string;
  tintWarm: string;
  tintWarmLine: string;
  /// The shared-workspace / system identity accent.
  accentMuted: string;

  // --- Derived in OKLCH (lightness replaced, chroma and hue held). ---
  successInk: string;
  dangerInk: string;
  warnInk: string;
  /// Defined on desktop but currently unused there; kept for parity.
  warmInk: string;
  accentFill: string;
  /// Defined on desktop but currently unused there; kept for parity.
  warmFill: string;

  // --- Raw numbers the per-turn derivation still needs at render time. ---
  turnRailL: number;
  turnNameL: number;

  /// Unrounded intermediates. `accentMuted` is itself a mix and is then fed
  /// into an OKLCH derivation for shared turns; rounding it to 8-bit in
  /// between costs a count or two against what the browser (which keeps full
  /// precision through the chain) produces. Kept out of the public colour
  /// fields so nothing renders from it directly.
  raw: { accentMuted: Rgba };
}

/// Build the full resolved token set for a family + scheme.
export function resolveTokens(name: ThemeName, scheme: Scheme): Tokens {
  return tokensFromPalette(name, resolvePalette(name, scheme));
}

function tokensFromPalette(name: ThemeName, p: Palette): Tokens {
  const status = STATUS[p.scheme];
  const chat = CHAT[p.scheme];
  const mix = (a: string, pct: number, b: string) => formatColor(mixSrgb(a, pct, b));
  const lite = (src: string, l: number) => formatColor(oklchReplaceL(src, l));
  const accentMuted = mixSrgb(p.ink, 34, p.panel);

  return {
    name,
    scheme: p.scheme,

    canvas: p.canvas,
    ink: p.ink,
    panel: p.panel,
    mist: p.mist,
    line: p.line,
    accentWarm: p.accentWarm,
    accentCool: p.accentCool,
    sidebarTop: p.sidebarTop,
    sidebarBottom: p.sidebarBottom,
    sidebarInk: p.sidebarInk,
    sidebarInkMuted: p.sidebarInkMuted,

    success: status.success,
    danger: status.danger,
    warn: status.warn,
    overlay: status.overlay,
    onAccent: chat.onAccent,

    inkSoft: mix(p.ink, 55, "transparent"),
    inkFaint: mix(p.ink, 42, "transparent"),
    hover: mix(p.ink, 6, "transparent"),
    tintCool: mix(p.accentCool, 14, "transparent"),
    tintCoolLine: mix(p.accentCool, 38, "transparent"),
    tintWarm: mix(p.accentWarm, 13, "transparent"),
    tintWarmLine: mix(p.accentWarm, 40, "transparent"),
    accentMuted: formatColor(accentMuted),

    successInk: lite(status.success, chat.accentInkL),
    dangerInk: lite(status.danger, chat.accentInkL),
    warnInk: lite(status.warn, chat.accentInkL),
    warmInk: lite(p.accentWarm, chat.accentInkL),
    accentFill: lite(p.accentCool, chat.fillL),
    warmFill: lite(p.accentWarm, chat.fillL),

    turnRailL: chat.turnRailL,
    turnNameL: chat.turnNameL,
    raw: { accentMuted },
  };
}

// ---------------------------------------------------------------------------
// Per-turn derivation
// ---------------------------------------------------------------------------

export type TurnKind = "human" | "agent" | "shared";

export interface TurnColors {
  /// The accent the turn is tinted by (`--turn-accent`).
  accent: string;
  /// Row background (`--tf`).
  fill: string;
  /// Row background while pressed.
  fillPressed: string;
  /// The 2px leading rail actually drawn. `transparent` on shared turns.
  rail: string;
  /// The derived rail colour (`--tr`) regardless of whether the row draws it —
  /// the caret, focus ring and live-tag border read this even on shared turns.
  railAccent: string;
  /// The author label colour.
  author: string;
  /// Body text colour.
  text: string;
  /// Timestamps, model name, attribution line.
  meta: string;
  /// `--tn`, the tinted accent used by the live tag inside the turn.
  tagAccent: string;
}

/// Resolve a turn's colours from one accent input, mirroring the `.tt` rules.
///
/// Three things here are not what a summary of the design would lead you to
/// write, and all three come from reading web/src/styles.css and ChatView.tsx:
///
///  - `shared` is a *fourth branch*, not a third accent. It overrides the fill
///    to `mist`, makes the rail transparent, and drops author and body text to
///    `inkSoft`. Running the muted accent through the human/agent derivation
///    instead draws a rail and a tint the desktop does not draw.
///  - The author label is tinted for *agents only*. ChatView renders
///    `.tt-name` for humans (plain `ink`) and `.tt-handle` for agents (`--tn`).
///    Despite its name, `--hive-turn-name-l` does not colour `.tt-name`.
///  - An agent may override `--turn-accent` with its own stored avatar colour;
///    the theme still owns the fill and rail lightness. Hence `accentOverride`.
export function turnColors(
  t: Tokens,
  kind: TurnKind,
  accentOverride?: string | null,
): TurnColors {
  // Shared turns derive from the *unrounded* muted accent — see `Tokens.raw`.
  const accent: string | Rgba =
    kind === "human"
      ? t.accentWarm
      : kind === "shared"
        ? t.raw.accentMuted
        : (accentOverride ?? t.accentCool);

  const railAccent = formatColor(oklchReplaceL(accent, t.turnRailL));
  const tagAccent = formatColor(oklchReplaceL(accent, t.turnNameL));

  if (kind === "shared") {
    return {
      accent: t.accentMuted,
      fill: t.mist,
      fillPressed: formatColor(mixSrgb(t.mist, 90, t.ink)),
      rail: "transparent",
      railAccent,
      author: t.inkSoft,
      text: t.inkSoft,
      meta: t.inkSoft,
      tagAccent,
    };
  }

  return {
    accent: typeof accent === "string" ? accent : formatColor(accent),
    // A *whisper* of tint — 92% panel. Mixed in sRGB deliberately: OKLCH
    // preserves chroma, so an 8% amber over a dark panel would read as a
    // saturated 30-40% card. Wrong space, wrong design.
    fill: formatColor(mixSrgb(t.panel, 92, accent)),
    fillPressed: formatColor(mixSrgb(t.panel, 88, accent)),
    rail: railAccent,
    railAccent,
    author: kind === "human" ? t.ink : tagAccent,
    text: t.ink,
    meta: t.inkSoft,
    tagAccent,
  };
}
