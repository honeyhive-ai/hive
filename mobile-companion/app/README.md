# Hive mobile companion — base build (MC-001)

The app shell, the theme engine, navigation, and a static transcript rendered
from fixture data. **No pairing, no crypto, no transport** — MC-002 / MC-002-A
are a separate milestone, and mixing them in makes both harder to review.

## Running it

This is an **Expo dev client**, not Expo Go. Expo Go cannot hold the
Keychain/Keystore key the approval milestone needs, and the install path
differs enough that migrating later is worse than starting here.

```sh
npm install
npx expo run:ios      # or: npx expo run:android
npm start             # subsequent runs — serves to the installed dev client
```

Requires Node and a native toolchain (Xcode / Android SDK). The phone and the
dev server must share a LAN.

```sh
npm test          # jest — theme, tokens, avatar
npm run typecheck
```

## What mirrors the desktop, and what doesn't

The design source of truth is `web/` in this repo. Everything in `src/theme/`
is a port of `web/src/lib/theme.ts` and `web/src/styles.css`, not an
interpretation of them.

**Ported exactly**: the five accent families and their light/dark variants, the
scheme-derived status and chat-lightness numbers, avatar colour and initials,
the `.tt` turn treatment, the literal-px chat type ramp.

**Adapted deliberately**: layout. The desktop is four columns
(`WorkspaceRail` → `Sidebar` → `main` → `RightRail`) with an 880px minimum
width, which does not fold onto a phone. The rail and sidebar become the
drawer, `main` becomes the stack root, `RightRail` becomes a pushed screen. The
visual language mirrors; the column count does not.

## Three things that will bite you if you skim

**1. Every Tailwind length on the desktop is ×0.875 of its nominal value.**
`web/src/styles.css` sets `:root { font-size: 14px }` and there is no `@theme`
override, so `rounded-xl` renders at 10.5px, not 12px — and it is the most
common radius in the product (118 uses). `p-4` is 14px. `text-sm` is 12.25px.
React Native has no rem, so writing the nominal numbers makes the whole app
uniformly ~14% too large and too round, which is invisible without a
side-by-side. Go through `rem()` / `space()` / `radius` in `src/theme/scale.ts`.

The chat layer is the exception: `styles.css` writes it in literal px, and
those two ramps do not line up on the desktop either. They are ported as
literals in `scale.ts`'s `chat` and `composer` objects. Do not reconcile them.

**2. Chromium clips out-of-gamut OKLCH; it does not gamut-map.** The derived
tokens use `oklch(from var(--x) L c h)`, and replacing lightness while holding
chroma routinely leaves the sRGB gamut. CSS Color 4 §13.2 specifies a chroma-
reduction algorithm — and Chromium does not run it. It clips per channel.

This was measured, not assumed: `tools/extract-desktop-tokens.ts` renders every
derived token in headless Chromium and reads the pixels back. The two
strategies genuinely disagree on the shipped palettes (dark `dangerInk` is
`rgb(255,157,155)` clipped and `rgb(255,173,170)` mapped — saturated red versus
pink), so this is not a rounding question.

The desktop is Tauri, which renders on WebView2 (Chromium) on Windows, so
clipping is what the desktop ships and `"clip"` is the default here. The spec
algorithm is implemented and tested as `"css4"` because WKWebView does gamut-map
— **which means a macOS desktop build already disagrees with the Windows build
on these tokens.** That is a pre-existing desktop issue, not a mobile one.

**3. The author label is tinted for agents only.** `--hive-turn-name-l` does
not colour `.tt-name`, despite the name. `ChatView.tsx` renders `.tt-name`
(plain ink) for humans and `.tt-handle` (`--tn`) for agents. And `.tt.shared`
is a *fourth branch*, not a third accent: it overrides the fill to mist, blanks
the rail, and drops text to `ink-soft`. Running the muted accent through the
human/agent derivation instead paints a rail and a tint the desktop does not
draw. Both are pinned by tests in `src/theme/__tests__/tokens.test.ts`.

## Regenerating the colour fixture

`src/theme/__tests__/fixtures/derived-tokens.json` is measured out of a real
browser, so the tests can disagree with the implementation. Regenerate after
any palette change:

```sh
bun run tools/extract-desktop-tokens.ts   # or: node --experimental-strip-types
```

It needs Chrome or Edge on PATH (or `CHROMIUM_BIN` set). sRGB mixes are read
from `getComputedStyle` — screenshotting a 6% tint and un-compositing it
amplifies 8-bit rounding by ~17× and would fail a correct implementation.
OKLCH tokens are read from rendered pixels, because computed style serialises
an out-of-gamut `oklch()` straight back as `oklch()`.

## Not done here

- No transport, pairing, approval key, or companion-service client.
- The sidebar gradient is a flat `sidebarTop` fill; the desktop runs
  `sidebarTop` → `sidebarBottom`. Reproducing it needs another native
  dependency and reads the same at drawer width.
- Markdown in turn bodies renders as plain text.
- No copy anywhere implies an assurance level — no "secured", no "verified
  device". None of it is true yet, and placeholder trust copy is exactly what
  survives to release.
