# Visual scale — normative

Status: **approved, binding** (MC-003, 2026-08-10). Applies to every screen in the React Native
port. Changing a number here requires a new approved proposal.

## The rule

The desktop shell sets `:root { font-size: 14px }` — `web/src/styles.css:8` — and uses Tailwind v4
(`@import "tailwindcss"` at `web/src/styles.css:1`; `tailwindcss ^4.0.0` in `web/package.json`)
with **no `@theme` override anywhere in the tree**. Tailwind's length utilities are defined in
`rem`, so every one of them resolves against a 14px root rather than the 16px its name implies.

Every Tailwind length in the desktop app therefore renders at **0.875× its nominal value**.

React Native has no `rem`. A port that writes the nominal numbers is uniformly ~14% oversized and
over-rounded. It looks entirely plausible in isolation and is only detectable side-by-side with the
desktop — which is precisely the comparison a phone build cannot make.

## The numbers

Radius — Tailwind v4's default ramp, resolved at the 14px root. Usage counts are over `web/src`.

| utility | uses | nominal | **normative** |
|---|---|---|---|
| `rounded-sm` | — | 4px | 3.5 |
| `rounded-md` | 17 | 6px | 5.25 |
| `rounded-lg` | 62 | 8px | 7 |
| `rounded-xl` | **122** | 12px | **10.5** |
| `rounded-2xl` | 42 | 16px | 14 |
| `rounded-3xl` | 4 | 24px | 21 |
| `rounded-full` | 36 | — | 9999 |

`rounded-xl` is the dominant radius in the product by a factor of two. If exactly one number is
carried over correctly, it is 10.5.

Spacing — Tailwind v4's spacing unit is `--spacing: 0.25rem`, so step *n* is `0.25n rem`:

| utility | normative |
|---|---|
| `gap-1` | 3.5 |
| `gap-2` | 7 |
| `p-3` | 10.5 |
| `p-4` | 14 |
| `p-6` | 21 |

Type — step × 14px:

| utility | nominal | normative |
|---|---|---|
| `text-xs` | 12px | 10.5 |
| `text-sm` | 14px | 12.25 |
| `text-base` | 16px | 14 |
| `text-lg` | 18px | 15.75 |

## The second ramp, which is not rem-scaled

`web/src/styles.css` writes the chat layer's own type and box scale in **literal px**, while the
components around it use rem-scaled Tailwind utilities:

| rule | value | line |
|---|---|---|
| `.tt-name` / `.tt-handle` | 14px | 100–101 |
| `.tt-model` | 11px | 102 |
| `.tt-time` / `.tt-attr` | 12px | 104–105 |
| `.tt-text` | 14px, line-height 1.5 | 106 |
| `.tt-badge` / `.tt-tag` | 10px | 112–113 |
| `.tcall-n` | 11.5px | 138 |

These are **not** rem-scaled and port unchanged.

The two ramps do not line up on the desktop either — `.tt-text` is 14px where a `text-sm` beside it
is 12.25px. That is the shipped design, and it is what a user comparing their phone to their desktop
sees. **Do not reconcile them.** A port that "fixes" the inconsistency has changed the product.

## Enforcement

`app/src/theme/scale.ts` is the single source: `rem(n) = n * 14`, plus `space()`, `radius`, `text`,
and `chat` for the literal ramp.

Raw numeric literals are **forbidden** in style props for anything that maps to a Tailwind utility
on desktop. This is enforced by `app/src/theme/__tests__/scale.test.ts`, which fails on a numeric
literal assigned to a radius, padding, margin, gap, or font-size prop outside `scale.ts`.

The rule exists because the failure mode is gradual: the 0.875 factor is not dropped all at once,
it is dropped one component at a time by whoever types `borderRadius: 12` from memory. A per-file
guard catches that on the commit that introduces it, which is the only point at which it is cheap
to see.

Escape hatch: a trailing `// px-ok: <reason>` on the same line. Reserved for values that genuinely
have no desktop Tailwind equivalent — hairline borders (Tailwind's `border` is a literal 1px, not
rem) and native touch metrics, which live in `scale.ts` under `touch` and are chosen for the finger
rather than ported.

## What is deliberately not ported

The desktop's 1280×832 default window and 880px minimum (`app/tauri.conf.json`) describe a
four-column layout. They do not fold onto a phone and are not a source for any number here. Touch
targets are the platform minimum — 44pt iOS / 48dp Android, take the larger — not a scaled-down
desktop control height.
