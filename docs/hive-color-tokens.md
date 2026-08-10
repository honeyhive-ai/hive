# Derived colour tokens

How the desktop shell builds the colours that aren't in a palette, and why one
particular class of them is computed in JavaScript rather than by CSS.

## The layers

A theme (`web/src/lib/theme.ts`) ships eleven flat colours per accent family per
scheme — canvas, ink, panel, mist, line, the two accents, the sidebar four.
Everything else is *derived* from those, so adding a palette means picking
eleven colours and nothing else.

Derivation happens in three places:

| Layer | Where | Example |
| --- | --- | --- |
| sRGB mixes | CSS, `color-mix(in srgb, …)` | `--hive-ink-soft`, `--tf` (turn fill) |
| OKLCH lightness swaps | **JS**, `applyTheme()` | `--hive-danger-ink`, `--tr` (turn rail) |
| Per-turn accents | **JS**, `ChatView` | a personal agent's own avatar colour |

The sRGB mixes stay in CSS. Every engine agrees on `color-mix(in srgb, …)`, it
tracks palette changes with no wiring, and there is no reason to move it.

## Why the OKLCH swaps moved into JS

The `*-ink` and `*-fill` tokens take a semantic accent and replace its lightness
while holding chroma and hue, so the same red reads as text in a light theme and
in a dark one. That used to be written as relative colour and left to the engine:

```css
--hive-danger-ink: oklch(from var(--hive-danger) var(--hive-accent-ink-l) c h);
```

Replacing lightness while holding chroma is exactly the operation that leaves the
sRGB gamut, and engines do not agree about what to do next:

- **Chromium** clips each channel independently. Holds chroma, shifts hue a little.
- **WebKit** runs the CSS Color 4 §13.2 chroma-reduction algorithm: binary-search
  chroma down until the clipped result is within one JND. Holds hue, drops chroma.

Hive's desktop is Tauri, which renders on WebView2 (Chromium) on Windows and
WKWebView on macOS. So the same palette produced different colours on the two
desktop builds, for every OKLCH-derived token. The gap is visible, not a rounding
artefact — dark `--hive-danger-ink` was `rgb(255,157,155)` on Windows against
`rgb(255,173,170)` on macOS, a saturated red against a pink.

`applyTheme()` now computes these in JS (`web/src/lib/color.ts`) and sets finished
`rgb()` values on `:root`, so nothing downstream of it has a choice left to make.

**The target is the Chromium result, deliberately.** That is not a claim that
clipping is more correct — the spec says otherwise. It is that clipping is what
the majority of installs have been rendering since the tokens shipped, so pinning
to it makes macOS agree with Windows without restyling the product for everyone
else. `GamutStrategy` in `color.ts` implements both; switching the default to
`"css4"` is the entire implementation of the other choice, if design ever wants
the spec colours instead.

## Rules

- **No `oklch()` in `web/src/styles.css`.** Derive it in `theme.ts` and publish
  the resolved value. `theme.token-parity.test.ts` fails if one appears.
- **The lightness knobs are gone.** `--hive-turn-rail-l`, `--hive-turn-name-l`,
  `--hive-accent-ink-l` and `--hive-fill-l` are no longer published, because
  publishing them is what would let the next `oklch(from … c h)` back in. The
  numbers live in `CHAT` in `theme.ts`.
- **Overriding `--turn-accent` means overriding `--tr` and `--tn` too.** The turn
  fill still derives from the accent in CSS (an sRGB mix), but the rail and the
  tinted handle no longer do. `ChatView` sets all three for a personal agent.
- **Intermediates stay unrounded.** `--hive-accent-muted` is a `color-mix` that
  CSS still evaluates, but the *muted* turn rail derives from it in OKLCH.
  `deriveTokens()` recomputes the mix at full precision rather than reading back
  an 8-bit value, because the browser kept full precision through the chain.

## Testing

`web/src/lib/theme.token-parity.test.ts` asserts every derived token, for all
five palettes in both schemes, against values **measured out of real rendered
Chromium pixels** — not against a spec implementation and not against the code
under test. Computed style is useless for this: it serialises an out-of-gamut
`oklch()` straight back as `oklch()`, telling you what you asked for rather than
what the user saw. Only the pixels answer the question.

The fixture is shared with the mobile companion, which needs the same numbers for
the same reason (React Native has neither custom properties nor these colour
functions) and regenerates it with `bun run tools/extract-desktop-tokens.ts`. The
two implementations of `color.ts` must agree; the fixture is what holds them
together.

If a value in that fixture changes, the desktop's appearance changed. That should
be a deliberate design call, not a side effect of a refactor.

## Not covered here

macOS was measured indirectly: the WebKit-side numbers come from a spec
implementation cross-checked against culori, not from a Mac. That does not affect
what shipped — pinning to Chromium leaves Windows and Linux byte-identical either
way, and if WKWebView turned out to clip after all, this change is simply a no-op
there. But the *size* of the divergence it closed is a computed figure until
somebody confirms it on real hardware.
