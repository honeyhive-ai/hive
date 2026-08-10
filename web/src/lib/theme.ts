// Design-token palettes for the React shell, applied to :root as CSS custom
// properties. Each theme is an accent *family* with a light and a dark variant;
// the appearance mode (auto/light/dark) picks the variant. `pollen` (Hive's
// warm honey-gold identity) is the launch default.
//
//  pollen — honey / gold (brand)
//  studio — neutral graphite (achromatic, professional)
//  harbor — cool ocean blue
//  meadow — botanical green

import { useSyncExternalStore } from "react";
import { setTitlebarColor } from "@/lib/ipc";
import { formatColor, mixSrgb, oklchReplaceL, type Rgba } from "@/lib/color";

export type ThemeName = "pollen" | "studio" | "harbor" | "meadow" | "midnight";
export type AppearanceMode = "auto" | "light" | "dark";

export interface Palette {
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
  scheme: "light" | "dark";
}

interface ThemeVariants {
  light: Palette;
  dark: Palette;
}

export const THEMES: Record<ThemeName, ThemeVariants> = {
  // Honey / gold — warm cream in light, honey on near-black in dark.
  pollen: {
    light: {
      canvas: "rgb(249,243,228)",
      ink: "rgb(38,29,16)",
      panel: "rgb(255,252,244)",
      mist: "rgb(243,233,212)",
      line: "rgba(120,84,24,0.12)",
      accentWarm: "rgb(214,130,56)",
      accentCool: "rgb(216,150,32)",
      sidebarTop: "rgb(74,46,18)",
      sidebarBottom: "rgb(154,102,32)",
      sidebarInk: "rgb(252,242,216)",
      sidebarInkMuted: "rgb(222,196,150)",
      scheme: "light",
    },
    dark: {
      canvas: "rgb(20,15,8)",
      ink: "rgb(247,236,208)",
      panel: "rgb(32,24,13)",
      mist: "rgb(44,33,18)",
      line: "rgba(240,200,120,0.10)",
      accentWarm: "rgb(224,150,80)",
      accentCool: "rgb(240,194,90)",
      sidebarTop: "rgb(10,7,3)",
      sidebarBottom: "rgb(40,28,12)",
      sidebarInk: "rgb(251,241,216)",
      sidebarInkMuted: "rgb(201,171,120)",
      scheme: "dark",
    },
  },

  // Neutral graphite — achromatic; a restrained slate accent, no color cast.
  studio: {
    light: {
      canvas: "rgb(245,245,246)",
      ink: "rgb(38,40,43)",
      panel: "rgb(255,255,255)",
      mist: "rgb(232,233,236)",
      line: "rgba(20,22,26,0.10)",
      accentWarm: "rgb(150,140,128)",
      accentCool: "rgb(82,90,102)",
      sidebarTop: "rgb(38,41,46)",
      sidebarBottom: "rgb(58,62,69)",
      sidebarInk: "rgb(244,245,247)",
      sidebarInkMuted: "rgb(180,184,192)",
      scheme: "light",
    },
    dark: {
      canvas: "rgb(24,26,29)",
      ink: "rgb(232,234,238)",
      panel: "rgb(33,36,40)",
      mist: "rgb(42,45,50)",
      line: "rgba(255,255,255,0.08)",
      accentWarm: "rgb(168,158,146)",
      accentCool: "rgb(140,150,164)",
      sidebarTop: "rgb(16,18,20)",
      sidebarBottom: "rgb(30,33,37)",
      sidebarInk: "rgb(240,242,245)",
      sidebarInkMuted: "rgb(160,165,173)",
      scheme: "dark",
    },
  },

  // Cool ocean blue — distinctly blue in both variants.
  harbor: {
    light: {
      canvas: "rgb(236,243,248)",
      ink: "rgb(23,42,58)",
      panel: "rgb(248,252,255)",
      mist: "rgb(218,232,242)",
      line: "rgba(20,60,90,0.12)",
      accentWarm: "rgb(224,150,90)",
      accentCool: "rgb(20,120,180)",
      sidebarTop: "rgb(16,52,78)",
      sidebarBottom: "rgb(28,86,120)",
      sidebarInk: "rgb(232,244,251)",
      sidebarInkMuted: "rgb(168,198,218)",
      scheme: "light",
    },
    dark: {
      canvas: "rgb(14,22,32)",
      ink: "rgb(220,233,243)",
      panel: "rgb(22,33,46)",
      mist: "rgb(28,42,58)",
      line: "rgba(120,180,230,0.10)",
      accentWarm: "rgb(230,160,100)",
      accentCool: "rgb(64,168,224)",
      sidebarTop: "rgb(8,16,26)",
      sidebarBottom: "rgb(20,40,60)",
      sidebarInk: "rgb(228,242,251)",
      sidebarInkMuted: "rgb(150,180,205)",
      scheme: "dark",
    },
  },

  // Midnight — neutral blue-grey dark (the chat-redesign base). A genuinely
  // cool, low-chroma dark with a blue agent accent; light variant is a clean
  // cool-white for daytime.
  midnight: {
    light: {
      canvas: "rgb(243,246,249)",
      ink: "rgb(29,39,49)",
      panel: "rgb(255,255,255)",
      mist: "rgb(233,239,245)",
      line: "rgba(20,32,48,0.11)",
      accentWarm: "rgb(181,103,58)",
      accentCool: "rgb(47,111,196)",
      sidebarTop: "rgb(24,28,35)",
      sidebarBottom: "rgb(16,18,22)",
      sidebarInk: "rgb(233,238,244)",
      sidebarInkMuted: "rgb(150,160,172)",
      scheme: "light",
    },
    dark: {
      canvas: "rgb(21,23,28)",
      ink: "rgb(231,234,239)",
      panel: "rgb(35,40,48)",
      mist: "rgb(25,28,34)",
      line: "rgba(255,255,255,0.10)",
      accentWarm: "rgb(210,133,79)",
      accentCool: "rgb(90,155,234)",
      sidebarTop: "rgb(27,31,38)",
      sidebarBottom: "rgb(19,21,25)",
      sidebarInk: "rgb(231,234,239)",
      sidebarInkMuted: "rgb(139,148,162)",
      scheme: "dark",
    },
  },

  // Botanical green.
  meadow: {
    light: {
      canvas: "rgb(240,244,233)",
      ink: "rgb(30,43,28)",
      panel: "rgb(250,252,245)",
      mist: "rgb(224,234,214)",
      line: "rgba(40,70,30,0.12)",
      accentWarm: "rgb(206,140,80)",
      accentCool: "rgb(74,140,72)",
      sidebarTop: "rgb(30,56,32)",
      sidebarBottom: "rgb(54,92,54)",
      sidebarInk: "rgb(238,246,230)",
      sidebarInkMuted: "rgb(178,200,168)",
      scheme: "light",
    },
    dark: {
      canvas: "rgb(16,24,16)",
      ink: "rgb(224,236,216)",
      panel: "rgb(24,34,24)",
      mist: "rgb(32,46,32)",
      line: "rgba(150,200,140,0.10)",
      accentWarm: "rgb(214,150,86)",
      accentCool: "rgb(104,180,100)",
      sidebarTop: "rgb(10,18,10)",
      sidebarBottom: "rgb(26,44,26)",
      sidebarInk: "rgb(232,244,224)",
      sidebarInkMuted: "rgb(160,190,150)",
      scheme: "dark",
    },
  },
};

const STORAGE_KEY = "hive.theme";
const MODE_KEY = "hive.appearance";
const DEFAULT_THEME: ThemeName = "pollen";

export type Scheme = "light" | "dark";

/// Status + overlay tokens. Scheme-derived and identical across accent families
/// — one legible green/red/amber per light/dark rather than eight hand-tuned
/// variants. Components must use these instead of hardcoded #34c759-style
/// literals so light palettes stay readable.
export const STATUS: Record<Scheme, { success: string; danger: string; warn: string; overlay: string }> = {
  light: {
    success: "rgb(34,140,80)",
    danger: "rgb(198,55,55)",
    warn: "rgb(170,116,24)",
    overlay: "rgba(0,0,0,0.05)",
  },
  dark: {
    success: "rgb(92,205,134)",
    danger: "rgb(240,112,112)",
    warn: "rgb(228,180,80)",
    overlay: "rgba(255,255,255,0.06)",
  },
};

/// Chat-layer scheme-derived numbers (redesign). Like the status tokens they
/// depend on light/dark, never on the accent family — so keying them to a theme
/// name would be wrong. They set the target lightness for turn rails, tinted
/// author names, and filled controls, plus the on-accent text colour, so one
/// CSS rule set stays legible in every palette + scheme.
///
/// The lightnesses are 0–1 OKLCH lightness, not CSS percentage strings, because
/// they are no longer handed to the engine — `applyTheme()` resolves them here
/// and publishes finished colours. See `deriveTokens`.
export const CHAT: Record<
  Scheme,
  { turnMix: string; turnRailL: number; turnNameL: number; accentInkL: number; fillL: number; onAccent: string }
> = {
  light: {
    turnMix: "91%",
    turnRailL: 0.54,
    turnNameL: 0.4,
    accentInkL: 0.42,
    fillL: 0.45,
    onAccent: "rgb(255,255,255)",
  },
  dark: {
    turnMix: "87%",
    turnRailL: 0.7,
    turnNameL: 0.82,
    accentInkL: 0.84,
    fillL: 0.82,
    onAccent: "rgb(18,20,24)",
  },
};

/// The turn-identity accents. `.tt` tints one way per identity; a personal agent
/// substitutes its own stored avatar colour for `cool` at render time.
export type TurnIdentity = "warm" | "cool" | "muted";

/// The `:root` custom properties whose value is an OKLCH derivation.
///
/// These used to be `oklch(from var(--x) L c h)` declarations in styles.css and
/// the engine finished them. It no longer does — see lib/color.ts for why the
/// engine's answer was not the same answer on every desktop. `applyTheme()`
/// computes them and sets resolved `rgb()` strings, so all three platforms show
/// the same bytes.
export interface DerivedTokens {
  successInk: string;
  dangerInk: string;
  warnInk: string;
  warmInk: string;
  accentFill: string;
  warmFill: string;
  /// Per-identity turn rail (`--tr`) and tinted author name (`--tn`).
  rail: Record<TurnIdentity, string>;
  name: Record<TurnIdentity, string>;
}

/// The neutral "muted" accent — the shared-workspace / system identity.
///
/// styles.css still spells this as a `color-mix()`, which every engine agrees
/// on, so `--hive-accent-muted` itself is left to CSS. It is recomputed here
/// only because the muted turn rail derives *from* it in OKLCH, and that chain
/// has to stay unrounded: the browser kept full precision through the mix, so
/// snapping to 8-bit in between costs a count or two against what it rendered.
function accentMuted(p: Palette): Rgba {
  return mixSrgb(p.ink, 34, p.panel);
}

/// Resolve every OKLCH-derived token for a palette. Pure — no DOM — so the
/// parity tests can assert it against browser-measured pixels directly.
export function deriveTokens(p: Palette): DerivedTokens {
  const s = STATUS[p.scheme];
  const c = CHAT[p.scheme];
  const ink = (src: string) => formatColor(oklchReplaceL(src, c.accentInkL));
  const fill = (src: string) => formatColor(oklchReplaceL(src, c.fillL));

  const accents: Record<TurnIdentity, string | Rgba> = {
    warm: p.accentWarm,
    cool: p.accentCool,
    muted: accentMuted(p),
  };
  const rail = {} as Record<TurnIdentity, string>;
  const name = {} as Record<TurnIdentity, string>;
  for (const id of ["warm", "cool", "muted"] as const) {
    rail[id] = formatColor(oklchReplaceL(accents[id], c.turnRailL));
    name[id] = formatColor(oklchReplaceL(accents[id], c.turnNameL));
  }

  return {
    successInk: ink(s.success),
    dangerInk: ink(s.danger),
    warnInk: ink(s.warn),
    warmInk: ink(p.accentWarm),
    accentFill: fill(p.accentCool),
    warmFill: fill(p.accentWarm),
    rail,
    name,
  };
}

// A personal agent's turn accent is its stored avatar colour, which is not
// known until render. Resolving two OKLCH derivations per turn per render is
// wasteful when a thread shows the same handful of agents over and over, so
// cache by accent + scheme. Bounded by (agents × 2), which is small.
const turnCache = new Map<string, { rail: string; name: string }>();

/// The rail (`--tr`) and tinted-name (`--tn`) colours for an arbitrary turn
/// accent — the personal-agent case, where the accent is a stored avatar colour
/// rather than one of the three theme identities. Falls back to the accent
/// itself if it is in some notation we cannot parse, which is the same thing
/// the stylesheet's `oklch(from …)` did with an invalid source: leave it alone
/// rather than blank the rail.
export function turnAccentColors(accent: string, scheme: Scheme): { rail: string; name: string } {
  const key = `${scheme}|${accent}`;
  const hit = turnCache.get(key);
  if (hit) return hit;
  const c = CHAT[scheme];
  let out: { rail: string; name: string };
  try {
    out = {
      rail: formatColor(oklchReplaceL(accent, c.turnRailL)),
      name: formatColor(oklchReplaceL(accent, c.turnNameL)),
    };
  } catch {
    out = { rail: accent, name: accent };
  }
  turnCache.set(key, out);
  return out;
}

/// The user's preferred accent family (independent of light/dark mode).
export function loadTheme(): ThemeName {
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored && stored in THEMES) return stored as ThemeName;
  return DEFAULT_THEME;
}

export function savePalette(name: ThemeName) {
  localStorage.setItem(STORAGE_KEY, name);
}

export function loadMode(): AppearanceMode {
  const m = localStorage.getItem(MODE_KEY);
  return m === "light" || m === "dark" || m === "auto" ? m : "auto";
}

export function saveMode(mode: AppearanceMode) {
  localStorage.setItem(MODE_KEY, mode);
}

export function systemPrefersDark(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-color-scheme: dark)").matches
  );
}

/// Run `cb` whenever the OS light/dark preference flips. Returns an unsubscribe.
export function watchSystemScheme(cb: () => void): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return () => {};
  }
  const mq = window.matchMedia("(prefers-color-scheme: dark)");
  mq.addEventListener("change", cb);
  return () => mq.removeEventListener("change", cb);
}

/// Resolve the concrete palette to render given the chosen family + mode.
export function resolvePalette(name: ThemeName, mode: AppearanceMode): Palette {
  const wantsDark = mode === "dark" || (mode === "auto" && systemPrefersDark());
  const family = THEMES[name] ?? THEMES[DEFAULT_THEME];
  return wantsDark ? family.dark : family.light;
}

export function applyTheme(p: Palette) {
  const root = document.documentElement;
  const status = STATUS[p.scheme];
  root.style.setProperty("--hive-success", status.success);
  root.style.setProperty("--hive-danger", status.danger);
  root.style.setProperty("--hive-warn", status.warn);
  root.style.setProperty("--hive-overlay", status.overlay);
  root.style.setProperty("--hive-turn-mix", CHAT[p.scheme].turnMix);
  root.style.setProperty("--hive-on-accent", CHAT[p.scheme].onAccent);
  // The OKLCH-derived tokens, resolved here rather than by the engine. The
  // lightness knobs they used to be built from (--hive-turn-rail-l and friends)
  // are deliberately no longer published: leaving them would invite the next
  // `oklch(from … var(--hive-fill-l) c h)` and re-open the platform split this
  // closed. lib/color.ts has the full story.
  const d = deriveTokens(p);
  root.style.setProperty("--hive-success-ink", d.successInk);
  root.style.setProperty("--hive-danger-ink", d.dangerInk);
  root.style.setProperty("--hive-warn-ink", d.warnInk);
  root.style.setProperty("--hive-warm-ink", d.warmInk);
  root.style.setProperty("--hive-accent-fill", d.accentFill);
  root.style.setProperty("--hive-warm-fill", d.warmFill);
  for (const id of ["warm", "cool", "muted"] as const) {
    root.style.setProperty(`--hive-rail-${id}`, d.rail[id]);
    root.style.setProperty(`--hive-name-${id}`, d.name[id]);
  }
  root.style.setProperty("--hive-canvas", p.canvas);
  root.style.setProperty("--hive-ink", p.ink);
  root.style.setProperty("--hive-panel", p.panel);
  root.style.setProperty("--hive-mist", p.mist);
  root.style.setProperty("--hive-line", p.line);
  root.style.setProperty("--hive-accent-warm", p.accentWarm);
  root.style.setProperty("--hive-accent-cool", p.accentCool);
  root.style.setProperty("--hive-sidebar-top", p.sidebarTop);
  root.style.setProperty("--hive-sidebar-bottom", p.sidebarBottom);
  root.style.setProperty("--hive-sidebar-ink", p.sidebarInk);
  root.style.setProperty("--hive-sidebar-ink-muted", p.sidebarInkMuted);
  root.style.colorScheme = p.scheme;
  // Let scheme-aware embeds (Monaco diff, xterm logs) react to theme flips.
  window.dispatchEvent(new Event("hive:theme"));
  syncNativeTitlebar(p);
}

/// The concrete scheme currently applied to the document.
export function currentScheme(): "light" | "dark" {
  return document.documentElement.style.colorScheme === "dark" ? "dark" : "light";
}

/// React to the applied light/dark scheme — for embeds that can't consume the
/// CSS custom properties directly (Monaco themes, xterm themes).
export function useColorScheme(): "light" | "dark" {
  return useSyncExternalStore(
    (cb) => {
      window.addEventListener("hive:theme", cb);
      return () => window.removeEventListener("hive:theme", cb);
    },
    currentScheme,
    () => "light" as const,
  );
}

// Tint the native OS title bar to match the canvas (Windows 11; no-op elsewhere
// and outside Tauri). Best-effort — a failure here must never break theming.
function syncNativeTitlebar(p: Palette) {
  const m = p.canvas.match(/(\d+(?:\.\d+)?)/g);
  if (!m || m.length < 3) return;
  const [r, g, b] = m.slice(0, 3).map((n) => Math.round(Number(n)));
  void setTitlebarColor(r, g, b, p.scheme === "dark").catch(() => {});
}
