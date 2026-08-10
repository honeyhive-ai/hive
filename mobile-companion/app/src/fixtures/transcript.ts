// Static fixture data for the base build. There is no transport in this
// milestone — pairing, crypto and the companion service are a separate one —
// so the transcript renders from here.
//
// The content is deliberately ordinary engineering chatter. Nothing in this
// app may carry placeholder trust copy: no "secured", no "verified device", no
// wording that implies an assurance level. None of that is true yet, and
// placeholder text is exactly what survives to release.

import type { TurnData } from "../components/Turn";

export interface Workspace {
  id: string;
  name: string;
  /// Short label for the switcher.
  initials: string;
}

export interface Channel {
  id: string;
  workspaceId: string;
  name: string;
  topic: string;
  unread: number;
}

export const WORKSPACES: Workspace[] = [
  { id: "ws-hive", name: "Hive", initials: "HI" },
  { id: "ws-mobile", name: "Mobile Companion", initials: "MC" },
  { id: "ws-infra", name: "Infra", initials: "IN" },
];

export const CHANNELS: Channel[] = [
  {
    id: "ch-general",
    workspaceId: "ws-mobile",
    name: "general",
    topic: "Design parity with the desktop shell",
    unread: 2,
  },
  {
    id: "ch-theme",
    workspaceId: "ws-mobile",
    name: "theme-port",
    topic: "Derived tokens, gamut behaviour",
    unread: 0,
  },
  {
    id: "ch-review",
    workspaceId: "ws-mobile",
    name: "review",
    topic: "Open proposals",
    unread: 5,
  },
  {
    id: "ch-releases",
    workspaceId: "ws-hive",
    name: "releases",
    topic: "v1.5.x",
    unread: 0,
  },
];

export type TranscriptEntry =
  | { type: "turn"; turn: TurnData }
  | { type: "system"; id: string; body: string; time?: string };

/// Exercises every branch the turn treatment has: human, agent, shared, an
/// agent with its own accent, a streaming turn, tool calls in both states, a
/// stale turn, reactions, and the system divider.
export const TRANSCRIPT: TranscriptEntry[] = [
  { type: "system", id: "sys-1", body: "Yesterday", time: undefined },
  {
    type: "turn",
    turn: {
      id: "t1",
      kind: "human",
      author: "claudeza",
      body: "I want a base build that mirrors the desktop app's design. Start with the theme layer — that's the part that will be wrong quietly if we guess.",
      time: "09:14",
      reactions: [{ emoji: "👍", count: 2 }],
    },
  },
  {
    type: "turn",
    turn: {
      id: "t2",
      kind: "agent",
      author: "Coder2",
      handle: "coder2",
      model: "claude-opus-5",
      via: "local runtime",
      host: "local",
      body: "Ported the palettes verbatim and rebuilt the derived layer in JS. The desktop builds ~20 tokens with color-mix() and oklch() in CSS, none of which React Native evaluates.",
      time: "09:31",
      toolCalls: [
        {
          id: "tc1",
          name: "Read",
          args: "web/src/styles.css",
          status: "ok",
          output: ":root { font-size: 14px }\n--hive-ink-soft: color-mix(in srgb, var(--hive-ink) 55%, transparent);\n--tf: color-mix(in srgb, var(--hive-panel) 92%, var(--turn-accent));",
        },
      ],
    },
  },
  {
    type: "turn",
    turn: {
      id: "t3",
      kind: "shared",
      author: "Hive",
      handle: "hive",
      badge: "workspace",
      body: "Reviewer flagged that every rem-based length on the desktop resolves against a 14px root, so the nominal Tailwind numbers are all 14% too large.",
      time: "09:33",
    },
  },
  {
    type: "turn",
    turn: {
      id: "t4",
      kind: "agent",
      author: "Reviewer",
      handle: "reviewer",
      // An agent carrying its own stored avatar colour: the accent overrides
      // the family's cool accent, but the theme still owns fill and rail
      // lightness, so the turn stays a whisper rather than a coloured card.
      accentHex: "#8a5a9e",
      model: "claude-opus-5",
      host: "remote",
      body: "Confirmed against the source. rounded-xl is 10.5px, not 12px — 118 uses. Multiply by 0.875 or route everything through a rem() helper.",
      time: "09:40",
      reactions: [
        { emoji: "🎯", count: 1 },
        { emoji: "👀", count: 3 },
      ],
    },
  },
  {
    type: "turn",
    turn: {
      id: "t5",
      kind: "agent",
      author: "Coder2",
      handle: "coder2",
      model: "claude-opus-5",
      host: "local",
      body: "Measured the OKLCH tokens out of real Chromium pixels rather than trusting the spec. Chromium clips per channel; it does not run the CSS Color 4 chroma reduction.",
      time: "10:02",
      toolCalls: [
        {
          id: "tc2",
          name: "Bash",
          args: "bun run tools/extract-desktop-tokens.ts",
          status: "ok",
          output: "wrote 250 measured tokens (130 computed, 120 rendered)",
        },
        {
          id: "tc3",
          name: "Bash",
          args: "bun add culori",
          status: "error",
          output: "cross-check disagreed with the browser by up to 21/255 — discarded as a reference",
        },
      ],
    },
  },
  {
    type: "turn",
    turn: {
      id: "t6",
      kind: "human",
      author: "You",
      body: "Does the phone need to be on the same network for the next milestone?",
      time: "10:20",
      stale: true,
    },
  },
  {
    type: "turn",
    turn: {
      id: "t7",
      kind: "agent",
      author: "Coder2",
      handle: "coder2",
      model: "claude-opus-5",
      host: "local",
      body: "Yes — the transport milestone is LAN-only by design. For this build there is no transport at all; the transcript you are reading is fixture data",
      time: "10:21",
      streaming: true,
    },
  },
];
