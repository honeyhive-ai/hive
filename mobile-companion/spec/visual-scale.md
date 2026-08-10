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

The composer is the same story and the easiest one to get backwards. `.cmp-card` writes
`border-radius: 12px` literally, so it is **12** — not `radius.xl`'s 10.5. It is the one card in the
product whose radius is off the ramp, and rounding it *onto* the ramp is the same class of error as
MC-003 running in the opposite direction. Same for `.hint`, a literal 12px sitting a quarter-pixel
below `text-sm`'s 12.25. These live in `scale.ts` under `composer`.

### A third case: lengths in inline JS

Not every desktop length is a CSS rule or a utility class. `web/src/components/WorkspaceRail.tsx:183`
sets `borderRadius: w.active ? 12 : 18` as an inline `style={{…}}` object on a tile whose *other*
dimensions come from `h-10 w-10`. Inline JS px is never rem-scaled, so the 12 is real — but the
`h-10 w-10` beside it is `2.5rem` = **35**, and the first port of that tile wrote 40.

One component, one line apart, two opposite rules. These are collected in `scale.ts` under
`inlineDesktop` so the next person does not have to rediscover which half scales.

## Enforcement

`app/src/theme/scale.ts` is the single source: `rem(n) = n * 14`, plus `space()`, `radius` and
`text` for the ramp, and `chat`, `composer` and `inlineDesktop` for the literal values.

Raw numeric literals are **forbidden** in style props for anything that maps to a Tailwind utility
on desktop. Two test files, deliberately separate:

- `app/src/theme/__tests__/scale.test.ts` pins the *numbers* against the desktop source.
- `app/src/theme/__tests__/no-raw-px.test.ts` pins the *rule*. It scans `src/` and fails on a
  numeric literal assigned to a radius, padding, margin, gap, font-size or box-dimension prop
  outside `scale.ts`.

The rule exists because the failure mode is gradual: the 0.875 factor is not dropped all at once,
it is dropped one component at a time by whoever types `borderRadius: 12` from memory. A per-file
guard catches that on the commit that introduces it, which is the only point at which it is cheap
to see.

Box dimensions (`width`/`height`/`min*`/`max*`) are guarded too, and were added after they let the
`h-10 w-10` → 40 slip through. The pattern only matches the `prop: value` form, so a JSX prop
(`<Svg width={16} …>`) is untouched — an icon's intrinsic size is not a layout length.

Comments are stripped before matching. Most file headers in this port quote the desktop rule they
came from, and a guard that cannot tell a quoted `.tt-system .ln { max-width: 120px }` from a live
style prop punishes exactly the comments that make the port auditable. `no-raw-px.test.ts` exports
`findOffences()` and unit-tests itself against known-bad and known-good input, so the guard is not
merely a test that has never been seen to fail.

Escape hatch: a trailing `// px-ok: <reason>` on the same line — the reason is required, so an
exception stays reviewable. Reserved for values that genuinely have no desktop equivalent. Hairline
borders are simply unguarded (Tailwind's `border` is a literal 1px, not rem), and native shell
metrics live in `scale.ts` under `touch`, chosen for the finger rather than ported.

## What is deliberately not ported

The desktop's 1280×832 default window and 880px minimum (`app/tauri.conf.json`) describe a
four-column layout. They do not fold onto a phone and are not a source for any number here. Touch
targets are the platform minimum — 44pt iOS / 48dp Android, take the larger — not a scaled-down
desktop control height.
