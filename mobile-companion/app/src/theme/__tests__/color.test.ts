// Colour-maths tests.
//
// These use the globals (`describe` / `test` / `expect`) that both jest-expo
// and `bun test` provide, so the same file runs in CI on the phone toolchain
// and on a machine that only has bun.
//
// The anchors below are published reference values, not values this
// implementation produced — the whole risk with a colour port is that a wrong
// conversion degrades silently into plausible-but-off colour, so the test has
// to be able to disagree with the code.

import {
  formatColor,
  mixSrgb,
  oklchReplaceL,
  oklchToSrgb,
  parseColor,
  srgbToOklch,
} from "../color";

const near = (a: number, b: number, tol: number) => Math.abs(a - b) <= tol;

describe("parseColor", () => {
  test("reads the notations the desktop palettes use", () => {
    expect(parseColor("rgb(38,29,16)")).toEqual({ r: 38, g: 29, b: 16, a: 1 });
    expect(parseColor("rgba(120,84,24,0.12)")).toEqual({ r: 120, g: 84, b: 24, a: 0.12 });
    expect(parseColor("#3f72a8")).toEqual({ r: 63, g: 114, b: 168, a: 1 });
    expect(parseColor("rgb(20 40 60 / 0.5)")).toEqual({ r: 20, g: 40, b: 60, a: 0.5 });
    expect(parseColor("transparent")).toEqual({ r: 0, g: 0, b: 0, a: 0 });
  });

  test("throws rather than yielding a plausible black", () => {
    expect(() => parseColor("hsl(200 50% 50%)")).toThrow();
    expect(() => parseColor("rebeccapurple")).toThrow();
  });
});

describe("mixSrgb", () => {
  // The premultiplied-alpha rule in CSS Color 4 §12 is what makes mixing with
  // `transparent` set alpha instead of darkening toward black. Getting this
  // wrong would make every muted label in the app dirty rather than faint.
  test("mixing with transparent only lowers alpha", () => {
    const t = mixSrgb("rgb(38,29,16)", 55, "transparent");
    expect(t.r).toBeCloseTo(38, 6);
    expect(t.g).toBeCloseTo(29, 6);
    expect(t.b).toBeCloseTo(16, 6);
    expect(t.a).toBeCloseTo(0.55, 6);
    expect(formatColor(t)).toBe("rgba(38, 29, 16, 0.55)");
  });

  test("interpolates gamma-encoded sRGB, not linear-light", () => {
    // Linear-light would give ~188 here; `in srgb` gives the midpoint of the
    // encoded values. If this test ever reads 188, someone linearised.
    const mid = mixSrgb("rgb(255,255,255)", 50, "rgb(0,0,0)");
    expect(mid.r).toBeCloseTo(127.5, 6);
  });

  test("the turn fill is a whisper, not a highlight", () => {
    // 92% panel + 8% accent over pollen dark. The accent's own chroma must
    // barely register — this is the sRGB-vs-OKLCH decision from styles.css.
    const fill = mixSrgb("rgb(32,24,13)", 92, "rgb(240,194,90)");
    expect(fill.r).toBeCloseTo(48.64, 2);
    expect(fill.g).toBeCloseTo(37.6, 2);
    expect(fill.b).toBeCloseTo(19.16, 2);
  });

  test("weights clamp and endpoints are exact", () => {
    expect(formatColor(mixSrgb("rgb(10,20,30)", 100, "rgb(200,200,200)"))).toBe("rgb(10, 20, 30)");
    expect(formatColor(mixSrgb("rgb(10,20,30)", 0, "rgb(200,200,200)"))).toBe(
      "rgb(200, 200, 200)",
    );
  });
});

describe("srgbToOklch", () => {
  // Published reference values for the sRGB primaries (Ottosson / CSS Color 4).
  const cases: Array<[string, number, number, number]> = [
    ["rgb(255,0,0)", 0.62796, 0.25768, 29.234],
    ["rgb(0,255,0)", 0.86644, 0.29483, 142.495],
    ["rgb(0,0,255)", 0.45201, 0.31321, 264.052],
    ["rgb(255,255,255)", 1.0, 0.0, 0],
  ];

  for (const [css, L, C, h] of cases) {
    test(`${css} converts to its published Oklch`, () => {
      const got = srgbToOklch(parseColor(css));
      expect(near(got.L, L, 0.0005)).toBe(true);
      expect(near(got.C, C, 0.0005)).toBe(true);
      if (C > 0.001) expect(near(got.h, h, 0.05)).toBe(true);
    });
  }

  test("achromatic colours get a deterministic hue", () => {
    expect(srgbToOklch(parseColor("rgb(128,128,128)")).h).toBe(0);
  });
});

describe("oklchReplaceL", () => {
  test("replacing L with its own L is the identity", () => {
    for (const css of ["rgb(214,130,56)", "rgb(20,120,180)", "rgb(104,180,100)"]) {
      const c = parseColor(css);
      const back = oklchReplaceL(c, srgbToOklch(c).L);
      expect(near(back.r, c.r, 0.6)).toBe(true);
      expect(near(back.g, c.g, 0.6)).toBe(true);
      expect(near(back.b, c.b, 0.6)).toBe(true);
    }
  });

  test("results are always inside sRGB", () => {
    for (const css of ["rgb(240,194,90)", "rgb(64,168,224)", "rgb(90,155,234)"]) {
      for (const L of [0.4, 0.42, 0.45, 0.54, 0.7, 0.82, 0.84]) {
        const out = oklchReplaceL(css, L);
        for (const ch of [out.r, out.g, out.b]) {
          expect(ch).toBeGreaterThanOrEqual(0);
          expect(ch).toBeLessThanOrEqual(255);
        }
      }
    }
  });

  // The two strategies genuinely disagree, and which one is *right* is a
  // measured question, not a spec question — see the `GamutStrategy` note in
  // color.ts. The default has to be the one Chromium does, because that is
  // what the desktop app renders and parity is the requirement.
  test("the default clips, matching Chromium and therefore the desktop", () => {
    // Dark `dangerInk`: rgb(240,112,112) lifted to L=0.84. Measured out of
    // rendered Chromium pixels. This is the single most visible disagreement
    // between the strategies in the shipped palettes — saturated red vs pink.
    expect(formatColor(oklchReplaceL("rgb(240,112,112)", 0.84))).toBe("rgb(255, 157, 155)");
    expect(formatColor(oklchReplaceL("rgb(240,112,112)", 0.84, "css4"))).toBe("rgb(255, 173, 170)");
  });

  test("css4 gives up chroma to hold hue; clipping does the opposite", () => {
    // harbor dark accentCool lifted to the dark turn-name lightness (82%) is
    // out of gamut: a saturated blue cannot stay that saturated that light.
    const src = "rgb(64,168,224)";
    const before = srgbToOklch(parseColor(src));
    const clipped = srgbToOklch(oklchReplaceL(src, 0.82));
    const mapped = srgbToOklch(oklchReplaceL(src, 0.82, "css4"));

    expect(mapped.C).toBeLessThan(clipped.C);
    expect(Math.abs(mapped.h - before.h)).toBeLessThan(Math.abs(clipped.h - before.h) + 1e-9);
    // Both stay recognisably blue rather than collapsing to a clipped cyan.
    for (const out of [oklchReplaceL(src, 0.82), oklchReplaceL(src, 0.82, "css4")]) {
      expect(out.b).toBeGreaterThan(out.r);
    }
  });

  test("both strategies agree when the colour is already in gamut", () => {
    // Low-chroma sources: studio's slate and warm grey, plus a neutral. These
    // stay inside sRGB at every lightness in the token set, so the mapper must
    // be a no-op and the two strategies must be indistinguishable. (Saturated
    // accents are a different story — pollen's warm orange is already outside
    // the gamut at L=0.42, which is why it is not in this list.)
    for (const src of ["rgb(82,90,102)", "rgb(150,140,128)", "rgb(128,128,128)"]) {
      for (const L of [0.42, 0.45, 0.54, 0.7, 0.82, 0.84]) {
        expect(formatColor(oklchReplaceL(src, L))).toBe(
          formatColor(oklchReplaceL(src, L, "css4")),
        );
      }
    }
  });

  test("in-gamut requests are left alone by the mapper", () => {
    // studio's accents are near-achromatic, so no lightness in the token set
    // pushes them out; the mapper must be a no-op rather than nudging chroma.
    const src = "rgb(82,90,102)";
    const before = srgbToOklch(parseColor(src));
    const after = srgbToOklch(oklchReplaceL(src, 0.45));
    expect(near(after.C, before.C, 0.002)).toBe(true);
  });

  test("degenerate lightness short-circuits to black and white", () => {
    expect(formatColor(oklchToSrgb({ L: 1, C: 0.3, h: 200, a: 1 }))).toBe("rgb(255, 255, 255)");
    expect(formatColor(oklchToSrgb({ L: 0, C: 0.3, h: 200, a: 1 }))).toBe("rgb(0, 0, 0)");
  });

  test("alpha rides through the conversion", () => {
    expect(oklchReplaceL("rgba(64,168,224,0.4)", 0.7).a).toBeCloseTo(0.4, 6);
  });
});
