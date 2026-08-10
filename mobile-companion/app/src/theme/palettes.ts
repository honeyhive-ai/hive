// Verbatim port of the desktop palettes — web/src/lib/theme.ts.
//
// The RGB triples below are copied, not re-derived. If the desktop palette
// changes, re-copy; do not "improve" a value here, because the two shells are
// meant to be the same product and a drifted accent is the most visible way to
// look like two products.
//
// Each theme is an accent *family* with a light and a dark variant; the
// appearance mode (auto/light/dark) picks the variant. `pollen` (Hive's warm
// honey-gold identity) is the launch default.
//
//  pollen   — honey / gold (brand)
//  studio   — neutral graphite (achromatic, professional)
//  harbor   — cool ocean blue
//  midnight — neutral blue-grey dark
//  meadow   — botanical green

export type ThemeName = "pollen" | "studio" | "harbor" | "meadow" | "midnight";
export type AppearanceMode = "auto" | "light" | "dark";
export type Scheme = "light" | "dark";

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
  scheme: Scheme;
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

  // Neutral graphite — achromatic; a restrained slate accent, no colour cast.
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

export const THEME_NAMES = Object.keys(THEMES) as ThemeName[];
export const DEFAULT_THEME: ThemeName = "pollen";

/// Storage keys match the desktop shell's localStorage keys, so a future
/// settings sync has nothing to translate.
export const STORAGE_KEY = "hive.theme";
export const MODE_KEY = "hive.appearance";

export interface StatusColors {
  success: string;
  danger: string;
  warn: string;
  overlay: string;
}

/// Status + overlay tokens are scheme-derived (identical across accent
/// families) — one legible green/red/amber per light/dark rather than ten
/// hand-tuned variants. Keying these to a theme name would be wrong.
export const STATUS: Record<Scheme, StatusColors> = {
  dark: {
    success: "rgb(92,205,134)",
    danger: "rgb(240,112,112)",
    warn: "rgb(228,180,80)",
    overlay: "rgba(255,255,255,0.06)",
  },
  light: {
    success: "rgb(34,140,80)",
    danger: "rgb(198,55,55)",
    warn: "rgb(170,116,24)",
    overlay: "rgba(0,0,0,0.05)",
  },
};

export interface ChatNumbers {
  /// Present for parity with the desktop token set, but unused there and here:
  /// `.tt` hardcodes the 92% fill literal rather than reading --hive-turn-mix.
  /// Kept so the port stays a legible one-to-one against theme.ts.
  turnMix: number;
  turnRailL: number;
  turnNameL: number;
  accentInkL: number;
  fillL: number;
  onAccent: string;
}

/// Chat-layer scheme-derived numbers. Like the status tokens they depend on
/// light/dark, never on the accent family. Percentages in the CSS; 0–1 here.
export const CHAT: Record<Scheme, ChatNumbers> = {
  dark: {
    turnMix: 0.87,
    turnRailL: 0.7,
    turnNameL: 0.82,
    accentInkL: 0.84,
    fillL: 0.82,
    onAccent: "rgb(18,20,24)",
  },
  light: {
    turnMix: 0.91,
    turnRailL: 0.54,
    turnNameL: 0.4,
    accentInkL: 0.42,
    fillL: 0.45,
    onAccent: "rgb(255,255,255)",
  },
};

/// Resolve the concrete palette to render given the chosen family + scheme.
export function resolvePalette(name: ThemeName, scheme: Scheme): Palette {
  const family = THEMES[name] ?? THEMES[DEFAULT_THEME];
  return scheme === "dark" ? family.dark : family.light;
}
