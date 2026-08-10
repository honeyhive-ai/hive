// Colour maths for the derived token layer.
//
// Why this exists at all, when CSS can do the same sums:
//
// The shell used to build its derived tokens with `oklch(from var(--x) L c h)`
// and let the engine finish the job. `oklch()` with the lightness replaced and
// chroma held routinely lands *outside* sRGB — that substitution is exactly the
// move that leaves the gamut — and engines disagree about what to do next.
// Chromium clips each channel independently. WebKit runs the CSS Color 4 §13.2
// chroma-reduction algorithm. Hive's desktop is Tauri: WebView2 (Chromium) on
// Windows, WKWebView on macOS. So the same palette rendered two different
// colours depending on which desktop you installed — dark `--hive-danger-ink`
// came out rgb(255,157,155) on Windows and rgb(255,173,170) on macOS, a
// saturated red against a pink.
//
// Doing the maths here instead removes the engine from the design system's
// critical path: every platform gets the same bytes because nothing downstream
// of `applyTheme()` has a choice left to make. Full account, and the rules that
// keep it that way: docs/hive-color-tokens.md.
//
// Ported from the mobile companion's `src/theme/color.ts`, which needed the
// same functions because React Native has neither custom properties nor those
// colour functions. The two copies must agree; the shared ground truth is the
// Chromium-measured fixture both test suites assert against.
//
// Two rules this file exists to keep honest:
//
//  1. `in srgb` means the *gamma-encoded* sRGB coordinates, not linear-light.
//     Interpolating in linear-light is a different (and visibly lighter) result
//     for the turn fills, so `mixSrgb` deliberately does not linearise.
//  2. The gamut-mapping strategy is a *measured* fact, not a spec question —
//     see `GamutStrategy` below.

export interface Rgba {
  /** 0–255, not necessarily integral until formatted. */
  r: number;
  g: number;
  b: number;
  /** 0–1. */
  a: number;
}

const TRANSPARENT: Rgba = { r: 0, g: 0, b: 0, a: 0 };

/// Parse the CSS colour notations the palettes actually use: `rgb()`, `rgba()`,
/// `#rgb` / `#rrggbb` / `#rrggbbaa`, and the `transparent` keyword. Throws on
/// anything else rather than silently returning black — a palette typo (or a
/// stored avatar colour in some unexpected notation) should fail loudly in a
/// test, not render as a plausible dark colour.
export function parseColor(css: string): Rgba {
  const s = css.trim().toLowerCase();
  if (s === "transparent") return { ...TRANSPARENT };

  if (s.startsWith("#")) {
    const hex = s.slice(1);
    const expand = (h: string) => parseInt(h.length === 1 ? h + h : h, 16);
    if (hex.length === 3 || hex.length === 4) {
      return {
        r: expand(hex[0]),
        g: expand(hex[1]),
        b: expand(hex[2]),
        a: hex.length === 4 ? expand(hex[3]) / 255 : 1,
      };
    }
    if (hex.length === 6 || hex.length === 8) {
      return {
        r: parseInt(hex.slice(0, 2), 16),
        g: parseInt(hex.slice(2, 4), 16),
        b: parseInt(hex.slice(4, 6), 16),
        a: hex.length === 8 ? parseInt(hex.slice(6, 8), 16) / 255 : 1,
      };
    }
    throw new Error(`unparseable hex colour: ${css}`);
  }

  const fn = s.match(/^rgba?\(([^)]*)\)$/);
  if (!fn) throw new Error(`unparseable colour: ${css}`);
  // Accept both the legacy comma form and the modern `r g b / a` form.
  const parts = fn[1].replace(/\//g, " ").split(/[\s,]+/).filter(Boolean);
  if (parts.length < 3) throw new Error(`unparseable colour: ${css}`);
  const chan = (t: string) => (t.endsWith("%") ? (parseFloat(t) / 100) * 255 : parseFloat(t));
  const alpha = (t: string | undefined) =>
    t === undefined ? 1 : t.endsWith("%") ? parseFloat(t) / 100 : parseFloat(t);
  return { r: chan(parts[0]), g: chan(parts[1]), b: chan(parts[2]), a: alpha(parts[3]) };
}

/// Serialise back to CSS. Channels round to integers; alpha keeps three
/// decimals (enough that a 6% hover tint survives the round-trip).
export function formatColor(c: Rgba): string {
  const ch = (n: number) => Math.round(clamp(n, 0, 255));
  const a = clamp(c.a, 0, 1);
  if (a >= 1) return `rgb(${ch(c.r)}, ${ch(c.g)}, ${ch(c.b)})`;
  return `rgba(${ch(c.r)}, ${ch(c.g)}, ${ch(c.b)}, ${round(a, 3)})`;
}

/// `color-mix(in srgb, a <pct>%, b)`.
///
/// Follows CSS Color 4 §12: both colours are premultiplied by alpha, mixed
/// componentwise in the gamma-encoded sRGB coordinates, then un-premultiplied.
/// The premultiply step is what makes `mix(ink 55%, transparent)` come out as
/// ink at 55% alpha instead of ink darkened toward black.
///
/// Engines do agree on this one — the stylesheet keeps its `color-mix()` calls
/// and this is here for the handful of mixes that feed an OKLCH derivation,
/// where the intermediate must stay unrounded.
export function mixSrgb(a: string | Rgba, pct: number, b: string | Rgba): Rgba {
  const ca = typeof a === "string" ? parseColor(a) : a;
  const cb = typeof b === "string" ? parseColor(b) : b;
  const wa = clamp(pct, 0, 100) / 100;
  const wb = 1 - wa;

  const alpha = ca.a * wa + cb.a * wb;
  if (alpha === 0) return { ...TRANSPARENT };
  const prem = (x: number, y: number) => (x * ca.a * wa + y * cb.a * wb) / alpha;
  return { r: prem(ca.r, cb.r), g: prem(ca.g, cb.g), b: prem(ca.b, cb.b), a: alpha };
}

// ---------------------------------------------------------------------------
// Oklab / Oklch
// ---------------------------------------------------------------------------

export interface Oklch {
  /** 0–1. */
  L: number;
  C: number;
  /** degrees, 0–360. */
  h: number;
  a: number;
}

interface Oklab {
  L: number;
  a: number;
  b: number;
}

// Sign-preserving sRGB transfer functions, so out-of-gamut coordinates stay
// ordered (a negative channel must remain negative through the round trip or
// the in-gamut test below silently passes).
function toLinear(c: number): number {
  const s = Math.sign(c);
  const x = Math.abs(c);
  return s * (x <= 0.04045 ? x / 12.92 : Math.pow((x + 0.055) / 1.055, 2.4));
}

function toGamma(c: number): number {
  const s = Math.sign(c);
  const x = Math.abs(c);
  return s * (x <= 0.0031308 ? x * 12.92 : 1.055 * Math.pow(x, 1 / 2.4) - 0.055);
}

function srgbToOklab(c: Rgba): Oklab {
  const r = toLinear(c.r / 255);
  const g = toLinear(c.g / 255);
  const b = toLinear(c.b / 255);

  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);

  return {
    L: 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    a: 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    b: 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  };
}

/// Oklab → sRGB coordinates, *unclamped*: channels may fall outside 0–255 and
/// that is the signal the gamut mapper reads.
function oklabToSrgb(lab: Oklab, alpha: number): Rgba {
  const l_ = lab.L + 0.3963377774 * lab.a + 0.2158037573 * lab.b;
  const m_ = lab.L - 0.1055613458 * lab.a - 0.0638541728 * lab.b;
  const s_ = lab.L - 0.0894841775 * lab.a - 1.291485548 * lab.b;

  const l = l_ * l_ * l_;
  const m = m_ * m_ * m_;
  const s = s_ * s_ * s_;

  return {
    r: toGamma(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s) * 255,
    g: toGamma(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s) * 255,
    b: toGamma(-0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s) * 255,
    a: alpha,
  };
}

export function srgbToOklch(c: Rgba): Oklch {
  const lab = srgbToOklab(c);
  const C = Math.sqrt(lab.a * lab.a + lab.b * lab.b);
  // Hue is meaningless at zero chroma; pin it to 0 so achromatic accents
  // (studio's greys) round-trip deterministically instead of picking up noise.
  const h = C < 1e-7 ? 0 : ((Math.atan2(lab.b, lab.a) * 180) / Math.PI + 360) % 360;
  return { L: lab.L, C, h, a: c.a };
}

function oklchToOklab(c: Oklch): Oklab {
  const rad = (c.h * Math.PI) / 180;
  return { L: c.L, a: c.C * Math.cos(rad), b: c.C * Math.sin(rad) };
}

// A hair of tolerance: the round trip is float maths, and an exactly-in-gamut
// colour must not be mapped just because it lands at 255.0000001.
const GAMUT_EPS = 1e-5;

function inGamut(c: Rgba): boolean {
  const lo = -GAMUT_EPS * 255;
  const hi = 255 + GAMUT_EPS * 255;
  return c.r >= lo && c.r <= hi && c.g >= lo && c.g <= hi && c.b >= lo && c.b <= hi;
}

function clipToGamut(c: Rgba): Rgba {
  return { r: clamp(c.r, 0, 255), g: clamp(c.g, 0, 255), b: clamp(c.b, 0, 255), a: c.a };
}

function deltaEOK(a: Rgba, b: Rgba): number {
  const la = srgbToOklab(a);
  const lb = srgbToOklab(b);
  const dL = la.L - lb.L;
  const da = la.a - lb.a;
  const db = la.b - lb.b;
  return Math.sqrt(dL * dL + da * da + db * db);
}

/// How to bring an out-of-gamut OKLCH colour back into sRGB.
///
/// `"clip"` — clamp each channel independently. Holds chroma, shifts hue a
/// little. **This is what Chromium does**, and therefore what every Windows and
/// Linux desktop install has been rendering since the tokens shipped.
///
/// `"css4"` — the CSS Color 4 §13.2 algorithm: binary-search chroma downward
/// until the clipped result is within one JND (deltaEOK 0.02). Gives up chroma
/// to hold hue. WebKit does this, so it is what a macOS build rendered.
///
/// The default is `"clip"`, and the reason is worth stating plainly because it
/// is the opposite of what the spec would suggest. The choice here is not
/// "which is correct" but "which is already shipped": clipping is what the
/// majority of installs see today, so pinning to it makes macOS agree with
/// Windows without restyling the product for everyone else. Every value in the
/// test fixture was measured out of real rendered Chromium pixels, so `"clip"`
/// is the one that reproduces them. `"css4"` is kept, and tested, because
/// flipping this constant is the whole implementation of the other option if
/// design ever decides the spec colours are the ones they want.
export type GamutStrategy = "clip" | "css4";

export function oklchToSrgb(c: Oklch, strategy: GamutStrategy = "clip"): Rgba {
  if (c.L >= 1) return { r: 255, g: 255, b: 255, a: c.a };
  if (c.L <= 0) return { r: 0, g: 0, b: 0, a: c.a };

  const direct = oklabToSrgb(oklchToOklab(c), c.a);
  if (strategy === "clip" || inGamut(direct)) return clipToGamut(direct);

  const JND = 0.02;
  const EPSILON = 0.0001;
  let lo = 0;
  let hi = c.C;
  let loInGamut = true;
  let current = direct;

  while (hi - lo > EPSILON) {
    const chroma = (lo + hi) / 2;
    current = oklabToSrgb(oklchToOklab({ ...c, C: chroma }), c.a);
    if (loInGamut && inGamut(current)) {
      lo = chroma;
      continue;
    }
    const clipped = clipToGamut(current);
    const e = deltaEOK(clipped, current);
    if (e < JND) {
      if (JND - e < EPSILON) return clipped;
      loInGamut = false;
      lo = chroma;
    } else {
      hi = chroma;
    }
  }
  return clipToGamut(current);
}

/// `oklch(from <src> <L> c h)` — replace lightness, hold chroma and hue.
/// `l` is the 0–1 lightness (the stylesheet used to spell it as a percentage).
export function oklchReplaceL(
  src: string | Rgba,
  l: number,
  strategy: GamutStrategy = "clip",
): Rgba {
  const c = typeof src === "string" ? parseColor(src) : src;
  const lch = srgbToOklch(c);
  return oklchToSrgb({ ...lch, L: l }, strategy);
}

/// Perceptual distance in Oklab. Exported for the gamut tests, and useful for
/// any future "is this pair distinguishable" check.
export function deltaEOKColors(a: string | Rgba, b: string | Rgba): number {
  return deltaEOK(
    typeof a === "string" ? parseColor(a) : a,
    typeof b === "string" ? parseColor(b) : b,
  );
}

function clamp(n: number, lo: number, hi: number): number {
  return n < lo ? lo : n > hi ? hi : n;
}

function round(n: number, places: number): number {
  const f = Math.pow(10, places);
  return Math.round(n * f) / f;
}
