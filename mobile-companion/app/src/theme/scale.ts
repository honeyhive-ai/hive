// Dimension scale.
//
// THE ONE THING TO KNOW: the desktop shell sets `:root { font-size: 14px }`
// (web/src/styles.css) and uses Tailwind v4 with no `@theme` override. Every
// rem-based Tailwind utility there therefore resolves against a 14px root, not
// the 16px the utility names imply. `rounded-xl` — 118 uses in
// web/src/components, the single most common radius in the product — renders
// at 10.5px, not 12px. `p-4` is 14px. `text-sm` is 12.25px.
//
// React Native has no rem, so writing the nominal numbers here would make the
// whole app uniformly ~14% larger and rounder than the desktop. That reads as
// "similar but not the same product" and is nearly impossible to spot without
// a side-by-side. So: never write a raw px value for anything that is a
// Tailwind length on desktop — go through `rem()` / `space()` / `radius`.
//
// Literal px in the desktop CSS is a different case and is ported literally —
// see `chat` at the bottom.

export const ROOT_FONT_PX = 14;

/// Convert a CSS rem value to device-independent pixels at the desktop's root.
export function rem(n: number): number {
  return n * ROOT_FONT_PX;
}

/// Tailwind v4's spacing unit is `--spacing: 0.25rem`, so `p-4` is
/// `calc(0.25rem * 4)` = 1rem = 14px. `space(4)` gives that.
export function space(n: number): number {
  return rem(0.25 * n);
}

/// Tailwind v4 default radius ramp, resolved at the 14px root. Nominal values
/// are in the comments so a reviewer can check the arithmetic against the
/// utility names.
export const radius = {
  sm: rem(0.25), //  4px nominal →  3.5
  md: rem(0.375), //  6px nominal →  5.25
  lg: rem(0.5), //  8px nominal →  7
  xl: rem(0.75), // 12px nominal → 10.5
  "2xl": rem(1), // 16px nominal → 14
  "3xl": rem(1.5), // 24px nominal → 21
  full: 9999,
} as const;

/// Tailwind v4 default type ramp at the 14px root. Tailwind pairs each size
/// with its own rem line-height rather than a ratio, so both columns go through
/// `rem()` and both move together if the root ever changes.
export const text = {
  xs: { fontSize: rem(0.75), lineHeight: rem(1) },
  sm: { fontSize: rem(0.875), lineHeight: rem(1.25) },
  base: { fontSize: rem(1), lineHeight: rem(1.5) },
  lg: { fontSize: rem(1.125), lineHeight: rem(1.75) },
  xl: { fontSize: rem(1.25), lineHeight: rem(1.75) },
  "2xl": { fontSize: rem(1.5), lineHeight: rem(2) },
} as const;

/// The chat layer writes its own scale in literal px in styles.css while the
/// surrounding components use rem-scaled Tailwind. Those two ramps do not line
/// up on the desktop either — `.tt-text` is 14px where `text-sm` beside it is
/// 12.25px. That is the shipped design. Port the literals as literals; do not
/// reconcile them, or the transcript stops matching the desktop transcript.
/// Every value below is a literal px written in `web/src/styles.css`, keyed by
/// the rule it comes from so a reviewer can diff it against the stylesheet.
/// Nothing here is rem-scaled — do not "fix" one of these by routing it through
/// `rem()`. Where a number coincides with a point on the Tailwind ramp
/// (`.react` at 14px vs `radius["2xl"]` at 14) it is a coincidence, not a
/// relationship: they move independently.
export const chat = {
  // .tt — the turn row itself.
  turnGap: 10,
  turnPaddingV: 6,
  turnPaddingH: 12,
  turnRadius: 8,
  turnRailWidth: 2,
  turnSpacing: 2, // .tt + .tt { margin-top: 2px }

  // .tt-av / .tt-head and friends.
  avatarPadTop: 2, // .tt-av { padding-top: 2px }
  headGap: 8,
  nameSize: 14, // .tt-name / .tt-handle
  modelSize: 11, // .tt-model
  timeSize: 12, // .tt-time
  attrSize: 12, // .tt-attr
  attrMarginTop: 1,
  textSize: 14, // .tt-text
  textLineHeight: 14 * 1.5,
  textMarginTop: 2,

  // .tt-system — centred meta between rules.
  systemSize: 12,
  systemGap: 8,
  systemPadV: 8, // padding: 8px 0
  systemRuleHeight: 1, // .tt-system .ln
  systemRuleMaxWidth: 120,

  // .tt-actions — the reactions row.
  actionsMarginTop: 2,
  actionsGap: 4,
  actionsMinHeight: 20,

  // .tt-badge and .tt-tag share a size and a radius but not their padding.
  badgeSize: 10, // .tt-badge / .tt-tag
  badgeRadius: 5,
  badgeGap: 4,
  badgePadV: 1, // .tt-badge { padding: 1px 6px }
  badgePadH: 6,
  tagGap: 5,
  tagPadV: 2, // .tt-tag { padding: 2px 6px }
  tagPadH: 6,

  // .tt-tag.live i — the three blinking dots. Round via half the box, since
  // React Native has no percentage border radius.
  blipSize: 3,
  blipRadius: 1.5, // border-radius: 50% of 3px

  // .tt-caret — the streaming caret.
  caretWidth: 2,
  caretHeight: 14,
  caretMarginStart: 2,

  // .react — reaction pills.
  reactSize: 12,
  reactCountSize: 10.5, // .react span
  reactGap: 5,
  reactRadius: 14,
  reactPadV: 3, // padding: 3px 9px
  reactPadH: 9,

  // .tcall — tool-call activity cards.
  toolMarginTop: 8,
  toolRadius: 8,
  toolHeadSize: 12,
  toolHeadGap: 8,
  toolHeadPadV: 7, // .tcall-h { padding: 7px 10px }
  toolHeadPadH: 10,
  toolChevronSize: 12,
  toolNameSize: 11.5,
  toolArgsSize: 11,
  toolBadgeSize: 9.5,
  toolBadgeRadius: 4,
  toolBadgePadV: 2, // .tcall-badge { padding: 2px 5px }
  toolBadgePadH: 5,
  toolOutputSize: 11,
  toolOutputLineHeight: 11 * 1.65,
  toolOutputPadV: 10, // .tcall-o { padding: 10px 12px }
  toolOutputPadH: 12,

  // Avatar in the transcript.
  avatarSize: 30,
} as const;

/// The composer's `.cmp*` rules, plus `.send`, `.hint` and `.kbd`. Same story
/// as `chat`: literal px in styles.css, ported literally.
///
/// `.cmp-card` is the one to watch. It writes `border-radius: 12px` directly,
/// so it is 12 on the desktop — NOT `radius.xl` (10.5). It is the one card in
/// the product whose radius is off the ramp, and rounding it to the ramp is the
/// same class of error as MC-003 in the opposite direction.
export const composer = {
  cardRadius: 12, // .cmp-card { border-radius: 12px } — literal, off-ramp
  cardPadV: 9, // padding: 9px 10px
  cardPadH: 10,
  stackGap: 6, // .cmp { gap: 6px }

  inputMinHeight: 44, // .cmp-in textarea
  inputSize: 14,
  inputLineHeight: 14 * 1.5,
  inputPadV: 4, // padding: 4px 2px
  inputPadH: 2,

  footerGap: 5, // .cmp-f
  footerMarginTop: 6,

  sendSize: 30, // .send { width: 30px; height: 30px }
  sendRadius: 9,
  sendGlyphSize: 16,

  hintSize: 12, // .hint { font-size: 12px } — literal, not text-sm's 12.25
  hintGap: 5,
  hintPadTop: 7, // padding: 7px 6px 0
  hintPadH: 6,

  kbdSize: 11, // .kbd
  kbdLineHeight: 11 * 1.5,
  kbdRadius: 5,
  kbdPadH: 5,
} as const;

/// Desktop lengths written as inline JS `style={{…}}` in a .tsx rather than as
/// a Tailwind class. They are plain px like the CSS literals — the rem factor
/// never touches them — but they are easy to mistake for utilities because
/// they sit inside a className-heavy component.
export const inlineDesktop = {
  /// WorkspaceRail.tsx — `borderRadius: w.active ? 12 : 18` on the tile. The
  /// active tile squares up; the resting one is nearly a circle.
  railTileRadiusActive: 12,
  railTileRadiusResting: 18,
} as const;

/// Native shell metrics that have no desktop equivalent — the desktop's
/// 1280x832 default and 880px minimum describe a four-column window and do not
/// fold onto a phone, so these are chosen for touch rather than ported.
export const touch = {
  /// Platform minimum tap target (44pt iOS / 48dp Android — take the larger).
  minTarget: 48,
  tabBarHeight: 56,

  /// Header glyph buttons. The visible box stays small so the glyph does not
  /// crowd the title; `hitSlop` is what carries it up to `minTarget`, which is
  /// why this is below 48 and not a bug.
  headerButtonMin: 32,
  headerButtonSlop: 12,
  /// The glyph itself is an icon that happens to be a character, so it is sized
  /// like an icon rather than taken off the type ramp.
  headerGlyph: 18,

  /// Drawer width. The desktop sidebar is a fixed column in a 1280px window; a
  /// drawer sliding over a phone is a different object, so this is chosen.
  drawerWidth: 300,
} as const;
