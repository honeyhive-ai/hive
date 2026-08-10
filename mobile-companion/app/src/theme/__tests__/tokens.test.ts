// The derived token layer, checked against colours measured out of a real
// browser (see tools/extract-desktop-tokens.ts).
//
// This is the test that matters. The palettes are copied constants and the
// layout numbers are arithmetic, but the derived layer is a reimplementation
// of CSS in JS — and a wrong colour conversion does not throw, it just ships a
// slightly different product. The fixture is measured, not asserted from our
// own output, so this test is able to disagree with the code.

import fixture from "./fixtures/derived-tokens.json";
import { parseColor } from "../color";
import { resolveTokens, turnColors, type TurnKind } from "../tokens";
import { THEME_NAMES, type Scheme } from "../palettes";

const REF: Record<string, string> = fixture.tokens;

/// The fixture's two measurement paths have different precision floors, so the
/// tolerance is not uniform:
///
///  - `getComputedStyle` serialises sRGB channels to six decimal places, which
///    is a hair under half a count. A mix landing exactly on x.5 can therefore
///    read one lower than the true value. One count of slack, and no more.
///  - Rendered pixels are exact 8-bit, so OKLCH tokens must match exactly.
const MIX_TOLERANCE = 1;
const RENDERED_TOLERANCE = 0;

function expectColor(key: string, mine: string, tolerance: number) {
  const ref = REF[key];
  if (!ref) throw new Error(`fixture is missing ${key} — re-run the extractor`);
  const a = parseColor(ref);
  const b = parseColor(mine);
  const delta = Math.max(Math.abs(a.r - b.r), Math.abs(a.g - b.g), Math.abs(a.b - b.b));
  if (delta > tolerance || Math.abs(a.a - b.a) > 0.001) {
    throw new Error(`${key}: browser ${ref}, ours ${mine} (delta ${delta})`);
  }
}

const MIX_TOKENS = [
  "inkSoft",
  "inkFaint",
  "hover",
  "tintCool",
  "tintCoolLine",
  "tintWarm",
  "tintWarmLine",
  "accentMuted",
] as const;

const OKLCH_TOKENS = [
  "successInk",
  "dangerInk",
  "warnInk",
  "warmInk",
  "accentFill",
  "warmFill",
] as const;

const SCHEMES: Scheme[] = ["light", "dark"];
const KINDS: TurnKind[] = ["human", "agent", "shared"];

describe("resolved tokens match the browser", () => {
  for (const name of THEME_NAMES) {
    for (const scheme of SCHEMES) {
      const prefix = `${name}.${scheme}`;

      test(`${prefix} — sRGB mixes`, () => {
        const t = resolveTokens(name, scheme);
        for (const key of MIX_TOKENS) {
          expectColor(`${prefix}.${key}`, t[key], MIX_TOLERANCE);
        }
      });

      test(`${prefix} — OKLCH derivations`, () => {
        const t = resolveTokens(name, scheme);
        for (const key of OKLCH_TOKENS) {
          expectColor(`${prefix}.${key}`, t[key], RENDERED_TOLERANCE);
        }
      });

      test(`${prefix} — turn colours`, () => {
        const t = resolveTokens(name, scheme);
        for (const kind of KINDS) {
          const c = turnColors(t, kind);
          expectColor(`${prefix}.${kind}.rail`, c.railAccent, RENDERED_TOLERANCE);
          expectColor(`${prefix}.${kind}.tag`, c.tagAccent, RENDERED_TOLERANCE);
          expectColor(`${prefix}.${kind}.fillPressed`, c.fillPressed, MIX_TOLERANCE);
          if (kind !== "shared") {
            expectColor(`${prefix}.${kind}.fill`, c.fill, MIX_TOLERANCE);
          }
        }
      });
    }
  }

  test("the fixture covers every theme and scheme", () => {
    expect(Object.keys(REF).length).toBe(THEME_NAMES.length * SCHEMES.length * 25);
  });
});

// The rules below are structural rather than numeric: they encode decisions
// that reading web/src/styles.css and ChatView.tsx settles, and that a summary
// of the design gets wrong in a way no colour comparison would catch.
describe("turn structure", () => {
  const t = resolveTokens("pollen", "dark");

  test("shared turns are a fourth branch, not a third accent", () => {
    const shared = turnColors(t, "shared");
    // `.tt.shared` overrides the fill to mist and blanks the rail. Running the
    // muted accent through the human/agent derivation instead would paint a
    // tint and a rail the desktop does not draw.
    expect(shared.fill).toBe(t.mist);
    expect(shared.rail).toBe("transparent");
    expect(shared.text).toBe(t.inkSoft);
    expect(shared.author).toBe(t.inkSoft);
  });

  test("shared turns still derive a rail accent for their children", () => {
    // The row draws no rail, but the caret and the live tag inside it read
    // `--tr` / `--tn`, which `.tt.shared` still sets via --turn-accent.
    const shared = turnColors(t, "shared");
    expect(shared.railAccent).not.toBe("transparent");
    expect(shared.tagAccent).not.toBe(shared.author);
  });

  test("only agent author labels are tinted", () => {
    // ChatView renders `.tt-name` (plain ink) for humans and `.tt-handle`
    // (--tn) for agents. Despite the token's name, --hive-turn-name-l does not
    // colour `.tt-name`.
    expect(turnColors(t, "human").author).toBe(t.ink);
    expect(turnColors(t, "agent").author).toBe(turnColors(t, "agent").tagAccent);
    expect(turnColors(t, "human").author).not.toBe(turnColors(t, "human").tagAccent);
  });

  test("an agent can override the accent but not the lightness", () => {
    const custom = turnColors(t, "agent", "#8a5a9e");
    const plain = turnColors(t, "agent");
    expect(custom.fill).not.toBe(plain.fill);
    expect(custom.rail).not.toBe(plain.rail);
    // Still a whisper: the override changes hue, not how loud the fill is.
    const fill = parseColor(custom.fill);
    const panel = parseColor(t.panel);
    expect(Math.abs(fill.r - panel.r)).toBeLessThan(12);
  });

  test("human and agent turns are distinguishable from each other", () => {
    expect(turnColors(t, "human").fill).not.toBe(turnColors(t, "agent").fill);
  });
});
