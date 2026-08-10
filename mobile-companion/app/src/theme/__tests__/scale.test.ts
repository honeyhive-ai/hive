// The dimension scale.
//
// These are arithmetic, but they are the arithmetic that is easiest to get
// wrong by writing down the number a Tailwind utility *says* instead of the
// number it renders. Pinning them means a future contributor who "corrects"
// rounded-xl back to 12 gets a failure instead of a 14%-too-round app.

import { ROOT_FONT_PX, chat, radius, rem, space, text } from "../scale";

describe("rem", () => {
  test("resolves against the desktop's 14px root, not 16", () => {
    expect(ROOT_FONT_PX).toBe(14);
    expect(rem(1)).toBe(14);
  });

  test("every Tailwind length is 0.875 of its nominal value", () => {
    // The nominal values are what the utility names imply at a 16px root.
    const nominal: Array<[number, number]> = [
      [radius.sm, 4],
      [radius.md, 6],
      [radius.lg, 8],
      [radius.xl, 12],
      [radius["2xl"], 16],
      [radius["3xl"], 24],
      [text.xs.fontSize, 12],
      [text.sm.fontSize, 14],
      [text.base.fontSize, 16],
      [space(4), 16],
    ];
    for (const [actual, nom] of nominal) {
      expect(actual).toBeCloseTo(nom * 0.875, 6);
    }
  });

  test("the most common radius in the product is 10.5, not 12", () => {
    // `rounded-xl` — 118 uses in web/src/components.
    expect(radius.xl).toBe(10.5);
  });

  test("radius.full stays a large constant, not a scaled value", () => {
    expect(radius.full).toBeGreaterThan(1000);
  });
});

describe("the chat ramp is literal px and does not scale", () => {
  test("turn body text is 14, where text-sm beside it is 12.25", () => {
    // Both are correct. styles.css writes the chat layer in literal px while
    // the surrounding components use rem-scaled Tailwind, and the two ramps do
    // not line up on the desktop either. Reconciling them here would be a
    // regression, not a tidy-up.
    expect(chat.textSize).toBe(14);
    expect(text.sm.fontSize).toBe(12.25);
  });

  test("the turn row geometry matches the .tt rule", () => {
    expect(chat.turnRadius).toBe(8);
    expect(chat.turnPaddingV).toBe(6);
    expect(chat.turnPaddingH).toBe(12);
    expect(chat.turnGap).toBe(10);
    expect(chat.turnRailWidth).toBe(2);
  });

  test("line heights follow the stylesheet's ratios", () => {
    expect(chat.textLineHeight).toBeCloseTo(21, 6); // 14px * 1.5
    expect(chat.toolOutputLineHeight).toBeCloseTo(18.15, 6); // 11px * 1.65
  });
});
