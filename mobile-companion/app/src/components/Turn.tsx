// The turn treatment — port of the `.tt` rules in web/src/styles.css.
//
// ONE component, tinted per identity by a single accent. Fill, rail and the
// author label all derive from that one input (see `turnColors`). Identity is
// carried by tint and avatar shape, never by side: every turn is leading-
// aligned, there are no right-hand bubbles.

import { memo, useEffect, useRef } from "react";
import { Animated, Easing, StyleSheet, Text, View } from "react-native";

import { Avatar, type AvatarHost } from "./Avatar";
import { ToolCallCard, type ToolCall } from "./ToolCallCard";
import { useTheme } from "../theme/ThemeProvider";
import { turnColors, type TurnKind } from "../theme/tokens";
import { chat } from "../theme/scale";

export interface TurnData {
  id: string;
  kind: TurnKind;
  /// Display name for humans; the handle (without `@`) for agents.
  author: string;
  handle?: string;
  avatarUrl?: string | null;
  /// An agent may carry its own stored avatar colour, which overrides the
  /// turn accent. The theme still owns the fill and rail lightness.
  accentHex?: string | null;
  host?: AvatarHost | null;
  model?: string | null;
  /// "via <runtime>" attribution under the header.
  via?: string | null;
  body: string;
  time?: string | null;
  /// Renders the workspace badge on shared turns.
  badge?: string | null;
  streaming?: boolean;
  stale?: boolean;
  toolCalls?: ToolCall[];
  reactions?: Array<{ emoji: string; count: number }>;
}

export const Turn = memo(function Turn({ turn }: { turn: TurnData }) {
  const { tokens: t, reduceMotion, monoFont } = useTheme();
  const c = turnColors(t, turn.kind, turn.accentHex);
  const isHuman = turn.kind === "human";

  return (
    <View
      style={[
        styles.row,
        {
          backgroundColor: c.fill,
          // `border-inline-start`, not `borderLeft`: the logical property
          // follows the writing direction, so the rail stays on the leading
          // edge under RTL instead of jumping to the wrong side of the turn.
          // The width is always 2 — shared turns make the colour transparent
          // rather than dropping the border, so text stays aligned across kinds.
          borderStartWidth: chat.turnRailWidth,
          borderStartColor: c.rail,
        },
        turn.stale && styles.stale,
      ]}
      accessibilityRole="text"
      accessibilityLabel={`${turn.author}${turn.time ? `, ${turn.time}` : ""}: ${turn.body}`}
    >
      <View style={styles.avatarCol}>
        <Avatar
          name={turn.author}
          url={turn.avatarUrl}
          colorHex={turn.accentHex}
          kind={isHuman ? "human" : "agent"}
          host={isHuman ? undefined : turn.host}
          size={chat.avatarSize}
        />
      </View>

      <View style={styles.body}>
        <View style={styles.head}>
          {/* Humans get `.tt-name` (plain ink); agents get `.tt-handle`
              (tinted). `turnColors` has already resolved which. */}
          <Text style={[styles.author, { color: c.author }]} numberOfLines={1}>
            {isHuman ? turn.author : `@${turn.handle ?? turn.author}`}
          </Text>

          {turn.badge ? <Badge label={turn.badge} /> : null}

          {!isHuman && turn.model ? (
            <Text style={[styles.model, { color: c.meta, fontFamily: monoFont }]}>
              {turn.model}
            </Text>
          ) : null}

          {turn.streaming ? <LiveTag color={c.tagAccent} still={reduceMotion} /> : null}

          <View style={styles.spacer} />
          {turn.time ? <Text style={[styles.time, { color: c.meta }]}>{turn.time}</Text> : null}
        </View>

        {turn.via && !isHuman ? (
          <Text style={[styles.attr, { color: c.meta }]}>via {turn.via}</Text>
        ) : null}

        <Text style={[styles.text, { color: c.text }]}>
          {turn.body}
          {turn.streaming ? <Caret color={c.railAccent} still={reduceMotion} /> : null}
        </Text>

        {turn.toolCalls?.length ? (
          <View>
            {turn.toolCalls.map((call) => (
              <ToolCallCard key={call.id} call={call} />
            ))}
          </View>
        ) : null}

        {turn.reactions?.length ? (
          <View style={styles.actions}>
            {turn.reactions.map((r) => (
              <View
                key={r.emoji}
                style={[styles.react, { backgroundColor: t.panel, borderColor: t.line }]}
              >
                <Text style={{ fontSize: chat.reactSize }}>{r.emoji}</Text>
                <Text style={{ fontSize: chat.reactCountSize, color: c.meta, fontFamily: monoFont }}>
                  {r.count}
                </Text>
              </View>
            ))}
          </View>
        ) : null}
      </View>
    </View>
  );
});

function Badge({ label }: { label: string }) {
  const { tokens: t } = useTheme();
  return (
    <View style={[styles.badge, { backgroundColor: t.overlay, borderColor: t.line }]}>
      <Text style={[styles.badgeText, { color: t.inkSoft }]}>{label.toUpperCase()}</Text>
    </View>
  );
}

/// The three blinking dots on a generating turn. `styles.css` disables this
/// animation under `prefers-reduced-motion`; React Native has no such media
/// query, so the flag comes down from `AccessibilityInfo` via the theme.
function LiveTag({ color, still }: { color: string; still: boolean }) {
  const { tokens: t, monoFont } = useTheme();
  return (
    <View style={[styles.tag, { borderColor: color, backgroundColor: t.overlay }]}>
      {[0, 1, 2].map((i) => (
        <Blip key={i} color={color} delay={i * 160} still={still} />
      ))}
      <Text style={[styles.tagText, { color, fontFamily: monoFont }]}>GENERATING</Text>
    </View>
  );
}

function Blip({ color, delay, still }: { color: string; delay: number; still: boolean }) {
  // 0.25 → 1 → 0.25 over 1.1s, matching @keyframes blip.
  const opacity = useRef(new Animated.Value(still ? 1 : 0.25)).current;

  useEffect(() => {
    if (still) {
      opacity.setValue(1);
      return;
    }
    const loop = Animated.loop(
      Animated.sequence([
        Animated.delay(delay),
        Animated.timing(opacity, {
          toValue: 1,
          duration: 550,
          easing: Easing.inOut(Easing.ease),
          useNativeDriver: true,
        }),
        Animated.timing(opacity, {
          toValue: 0.25,
          duration: 550,
          easing: Easing.inOut(Easing.ease),
          useNativeDriver: true,
        }),
      ]),
    );
    loop.start();
    return () => loop.stop();
  }, [opacity, delay, still]);

  return <Animated.View style={[styles.blip, { backgroundColor: color, opacity }]} />;
}

/// The streaming caret — `@keyframes cblink`, a hard 1s on/off (steps(1)), not
/// a fade. Also stilled under reduced motion.
function Caret({ color, still }: { color: string; still: boolean }) {
  const opacity = useRef(new Animated.Value(1)).current;

  useEffect(() => {
    if (still) {
      opacity.setValue(1);
      return;
    }
    const loop = Animated.loop(
      Animated.sequence([
        Animated.timing(opacity, { toValue: 1, duration: 0, useNativeDriver: true }),
        Animated.delay(500),
        Animated.timing(opacity, { toValue: 0, duration: 0, useNativeDriver: true }),
        Animated.delay(500),
      ]),
    );
    loop.start();
    return () => loop.stop();
  }, [opacity, still]);

  return <Animated.View style={[styles.caret, { backgroundColor: color, opacity }]} />;
}

const styles = StyleSheet.create({
  row: {
    flexDirection: "row",
    gap: chat.turnGap,
    paddingVertical: chat.turnPaddingV,
    paddingHorizontal: chat.turnPaddingH,
    borderRadius: chat.turnRadius,
    marginTop: chat.turnSpacing,
  },
  stale: { opacity: 0.6 },
  avatarCol: { paddingTop: chat.avatarPadTop },
  // `flexShrink: 1` is React Native's equivalent of the `min-width: 0` that
  // lets a flex child actually shrink instead of overflowing its row.
  body: { flex: 1, flexShrink: 1 },
  head: { flexDirection: "row", alignItems: "baseline", gap: chat.headGap, flexWrap: "wrap" },
  author: { fontWeight: "600", fontSize: chat.nameSize, flexShrink: 1 },
  model: { fontSize: chat.modelSize },
  spacer: { flex: 1 },
  time: { fontSize: chat.timeSize, fontVariant: ["tabular-nums"] },
  attr: { fontSize: chat.attrSize, marginTop: chat.attrMarginTop },
  text: {
    fontSize: chat.textSize,
    lineHeight: chat.textLineHeight,
    marginTop: chat.textMarginTop,
  },
  actions: {
    marginTop: chat.actionsMarginTop,
    minHeight: chat.actionsMinHeight,
    flexDirection: "row",
    alignItems: "center",
    gap: chat.actionsGap,
    flexWrap: "wrap",
  },
  badge: {
    flexDirection: "row",
    alignItems: "center",
    gap: chat.badgeGap,
    borderWidth: 1,
    borderRadius: chat.badgeRadius,
    paddingVertical: chat.badgePadV,
    paddingHorizontal: chat.badgePadH,
  },
  badgeText: { fontSize: chat.badgeSize, fontWeight: "600", letterSpacing: 0.4 },
  tag: {
    flexDirection: "row",
    alignItems: "center",
    gap: chat.tagGap,
    borderWidth: 1,
    borderRadius: chat.badgeRadius,
    paddingVertical: chat.tagPadV,
    paddingHorizontal: chat.tagPadH,
  },
  tagText: { fontSize: chat.badgeSize, letterSpacing: 0.3 },
  blip: { width: chat.blipSize, height: chat.blipSize, borderRadius: chat.blipRadius },
  caret: {
    width: chat.caretWidth,
    height: chat.caretHeight,
    marginStart: chat.caretMarginStart,
  },
  react: {
    flexDirection: "row",
    alignItems: "center",
    gap: chat.reactGap,
    borderWidth: 1,
    borderRadius: chat.reactRadius,
    paddingVertical: chat.reactPadV,
    paddingHorizontal: chat.reactPadH,
  },
});
