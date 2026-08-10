// The source guard for MC-003.
//
// `scale.test.ts` pins the numbers. This pins the *rule*, and the rule is the
// half that has to survive contact with a growing codebase.
//
// The 0.875 factor is not lost in one commit. It is lost one component at a
// time, by whoever types `borderRadius: 12` from memory because that is what
// `rounded-xl` has meant everywhere else they have worked. Each slip is a
// pixel and a half; none of them is visible alone; the sum is an app that is
// uniformly too round and too large and reads as a near-miss of the desktop.
// The only place that comparison can be made is side by side, which is exactly
// what a phone build never gets.
//
// So: a numeric literal on a style prop that maps to a rem-based Tailwind
// utility is a test failure. See ../../../spec/visual-scale.md.

import fs from "node:fs";
import path from "node:path";

const SRC = path.resolve(__dirname, "..", "..");

/// Style props whose desktop counterpart is a rem-based Tailwind utility, so a
/// numeric literal here is a dropped 0.875.
///
/// Box dimensions are included: `h-10 w-10` on the desktop workspace tile is
/// 2.5rem = 35px, and the port had 40 — the exact MC-003 slip, and one that
/// stayed invisible until this list grew. Note the pattern only matches the
/// `prop: value` form, so a JSX prop (`<Svg width={16} …>`) is untouched; an
/// icon's intrinsic size is not a layout length.
///
/// Deliberately absent: `borderWidth` and friends (Tailwind's `border` is a
/// literal 1px, not rem), `flex`, `opacity`, `zIndex`, and `lineHeight`
/// (legitimately derived from a fontSize already on the ramp).
const GUARDED_PROPS = [
  "width",
  "height",
  "minWidth",
  "minHeight",
  "maxWidth",
  "maxHeight",
  "borderRadius",
  "borderTopLeftRadius",
  "borderTopRightRadius",
  "borderBottomLeftRadius",
  "borderBottomRightRadius",
  "borderStartStartRadius",
  "borderStartEndRadius",
  "borderEndStartRadius",
  "borderEndEndRadius",
  "padding",
  "paddingTop",
  "paddingBottom",
  "paddingLeft",
  "paddingRight",
  "paddingStart",
  "paddingEnd",
  "paddingHorizontal",
  "paddingVertical",
  "margin",
  "marginTop",
  "marginBottom",
  "marginLeft",
  "marginRight",
  "marginStart",
  "marginEnd",
  "marginHorizontal",
  "marginVertical",
  "gap",
  "rowGap",
  "columnGap",
  "fontSize",
];

/// Matches `<prop>: <number>` — a bare numeric literal, not `radius.xl`,
/// `space(4)` or `chat.textSize`.
///
/// The lookbehind rejects a preceding hyphen as well as a word character. Without
/// it, a CSS name quoted in a comment (`max-width: 120px`) matches on its tail
/// and gets reported as the prop it happens to end with.
const LITERAL = new RegExp(
  `(?<![\\w-])(${GUARDED_PROPS.join("|")})\\s*:\\s*(-?\\d*\\.?\\d+)`,
  "g",
);

/// Per-line opt-out. Requires a reason, so the exceptions stay readable and a
/// reviewer can see at a glance whether the excuse is a real one.
const ESCAPE = /\/\/\s*px-ok:\s*\S/;

/// `scale.ts` is where the literals are supposed to live, and the value tests
/// necessarily quote them.
const EXEMPT_FILES = new Set([
  path.join(SRC, "theme", "scale.ts"),
  path.join(SRC, "theme", "__tests__", "scale.test.ts"),
  path.join(SRC, "theme", "__tests__", "no-raw-px.test.ts"),
]);

/// Comments come out before matching. Most file headers in this port quote the
/// desktop rule they were ported from, and a guard that cannot tell a quoted
/// `.tt-system .ln { max-width: 120px }` from a live style prop punishes
/// precisely the comments that make the port auditable.
///
/// Newlines inside block comments are preserved so reported line numbers stay
/// true. A `//` inside a string literal (a URL) over-strips, which can only
/// cause a miss, never a false alarm.
function stripComments(source: string): string {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "))
    .replace(/\/\/[^\n]*/g, "");
}

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "node_modules") continue;
      out.push(...sourceFiles(full));
    } else if (/\.tsx?$/.test(entry.name) && !EXEMPT_FILES.has(full)) {
      out.push(full);
    }
  }
  return out;
}

/// Exported so the guard can be exercised against known-bad input below rather
/// than only against a tree that currently passes. A guard that has never been
/// seen to fail is a guard nobody has tested.
export function findOffences(source: string): Array<{ line: number; prop: string; value: string }> {
  const raw = source.split(/\r?\n/);
  const clean = stripComments(source).split(/\r?\n/);
  const out: Array<{ line: number; prop: string; value: string }> = [];

  clean.forEach((line, i) => {
    if (ESCAPE.test(raw[i] ?? "")) return;
    for (const m of line.matchAll(LITERAL)) {
      // `0` is the same zero at any root.
      if (Number(m[2]) === 0) continue;
      out.push({ line: i + 1, prop: m[1], value: m[2] });
    }
  });
  return out;
}

describe("no raw px literals in style props", () => {
  test("every guarded prop goes through the scale module", () => {
    const offences: string[] = [];

    for (const file of sourceFiles(SRC)) {
      const source = fs.readFileSync(file, "utf8");
      for (const o of findOffences(source)) {
        offences.push(`${path.relative(SRC, file)}:${o.line}  ${o.prop}: ${o.value}`);
      }
    }

    // Thrown rather than `expect(x, msg)` — the message argument is a Vitest and
    // Bun extension that jest-expo does not have, and this file has to run under
    // both. The fix is never "update an expected list".
    if (offences.length > 0) {
      throw new Error(
        `Raw px literals found in style props. Desktop lengths resolve against a ` +
          `14px root (MC-003), so a literal here is ~14% too large. Use radius.* / ` +
          `space(n) / text.* / chat.* / composer.* from src/theme/scale.ts, or append ` +
          `\`// px-ok: <reason>\` if the value genuinely has no desktop equivalent:\n\n  ` +
          offences.join("\n  ") +
          "\n",
      );
    }
    expect(offences).toEqual([]);
  });
});

describe("the guard itself", () => {
  test("catches the literal that started all this", () => {
    const found = findOffences(`const s = { borderRadius: 12, padding: 16 };`);
    expect(found.map((o) => `${o.prop}:${o.value}`)).toEqual([
      "borderRadius:12",
      "padding:16",
    ]);
  });

  test("leaves scale references, zero, and unguarded props alone", () => {
    const found = findOffences(
      [
        `borderRadius: radius.xl,`,
        `padding: space(4),`,
        `fontSize: chat.textSize,`,
        `borderWidth: 1,`,
        `flex: 1,`,
        `margin: 0,`,
      ].join("\n"),
    );
    expect(found).toEqual([]);
  });

  test("does not fire inside a comment that quotes the desktop rule", () => {
    // The bug this replaced: `max-width` ends in `width`, and the line is a
    // comment, so it was reported twice over as a real offence.
    const found = findOffences(
      [
        `// matching \`.tt-system .ln { flex: 1; max-width: 120px }\`.`,
        `/* .cmp-card { border-radius: 12px; padding: 9px 10px } */`,
        `rule: { maxWidth: chat.systemRuleMaxWidth },`,
      ].join("\n"),
    );
    expect(found).toEqual([]);
  });

  test("a block comment does not shift the reported line number", () => {
    const found = findOffences(
      [`/* two`, `   lines */`, `const s = { gap: 9 };`].join("\n"),
    );
    expect(found).toEqual([{ line: 3, prop: "gap", value: "9" }]);
  });

  test("honours `px-ok` only when a reason follows it", () => {
    expect(findOffences(`padding: 3, // px-ok: mobile-only control`)).toEqual([]);
    expect(findOffences(`padding: 3, // px-ok:`)).toHaveLength(1);
    expect(findOffences(`padding: 3, // whatever`)).toHaveLength(1);
  });
});
