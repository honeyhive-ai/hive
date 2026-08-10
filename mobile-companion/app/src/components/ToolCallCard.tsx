// Tool-call activity — port of the `.tcall` rules. One dim line at rest that
// expands to its output. Collapsed by default: on a phone an expanded tool
// output would push the turn it belongs to off the screen.

import { useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";
import Svg, { Path } from "react-native-svg";

import { useTheme } from "../theme/ThemeProvider";
import { chat } from "../theme/scale";
import { mixSrgb, formatColor } from "../theme/color";

export interface ToolCall {
  id: string;
  name: string;
  /// The single-line argument summary shown beside the name.
  args?: string | null;
  output?: string | null;
  status: "ok" | "error";
}

export function ToolCallCard({ call }: { call: ToolCall }) {
  const { tokens: t, monoFont } = useTheme();
  const [open, setOpen] = useState(false);
  const bad = call.status === "error";

  // One of the 52 inline `color-mix` calls the desktop makes outside :root —
  // ported at its call site rather than hoisted into the token set, because it
  // belongs to this component and nothing else uses it.
  const badgeBg = formatColor(mixSrgb(bad ? t.danger : t.success, 14, "transparent"));
  const outputBg = bad ? formatColor(mixSrgb(t.danger, 7, "transparent")) : t.panel;

  return (
    <View style={[styles.card, { borderColor: t.line, backgroundColor: t.mist }]}>
      <Pressable
        onPress={() => setOpen((o) => !o)}
        style={({ pressed }) => [styles.head, pressed && { backgroundColor: t.hover }]}
        accessibilityRole="button"
        accessibilityState={{ expanded: open }}
        accessibilityLabel={`${call.name} tool call, ${bad ? "failed" : "succeeded"}`}
      >
        <Chevron open={open} color={t.inkFaint} />
        <Text style={[styles.name, { color: t.ink, fontFamily: monoFont }]} numberOfLines={1}>
          {call.name}
        </Text>
        {call.args ? (
          <Text style={[styles.args, { color: t.inkFaint, fontFamily: monoFont }]} numberOfLines={1}>
            {call.args}
          </Text>
        ) : null}
        <View style={[styles.badge, { backgroundColor: badgeBg }]}>
          <Text
            style={[
              styles.badgeText,
              { color: bad ? t.dangerInk : t.successInk, fontFamily: monoFont },
            ]}
          >
            {bad ? "ERROR" : "OK"}
          </Text>
        </View>
      </Pressable>

      {open && call.output ? (
        <Text
          style={[
            styles.output,
            {
              borderTopColor: t.line,
              backgroundColor: outputBg,
              color: bad ? t.dangerInk : t.inkSoft,
              fontFamily: monoFont,
            },
          ]}
        >
          {call.output}
        </Text>
      ) : null}
    </View>
  );
}

function Chevron({ open, color }: { open: boolean; color: string }) {
  return (
    <Svg
      width={chat.toolChevronSize}
      height={chat.toolChevronSize}
      viewBox="0 0 24 24"
      fill="none"
      stroke={color}
      strokeWidth={2.5}
      style={{ transform: [{ rotate: open ? "90deg" : "0deg" }] }}
    >
      <Path d="M9 18l6-6-6-6" strokeLinecap="round" strokeLinejoin="round" />
    </Svg>
  );
}

const styles = StyleSheet.create({
  card: {
    marginTop: chat.toolMarginTop,
    borderWidth: 1,
    borderRadius: chat.toolRadius,
    overflow: "hidden",
  },
  head: {
    flexDirection: "row",
    alignItems: "center",
    gap: chat.toolHeadGap,
    paddingVertical: chat.toolHeadPadV,
    paddingHorizontal: chat.toolHeadPadH,
  },
  name: { fontSize: chat.toolNameSize, flexShrink: 0 },
  args: { fontSize: chat.toolArgsSize, flex: 1, flexShrink: 1 },
  badge: {
    marginStart: "auto",
    borderRadius: chat.toolBadgeRadius,
    paddingVertical: chat.toolBadgePadV,
    paddingHorizontal: chat.toolBadgePadH,
  },
  badgeText: { fontSize: chat.toolBadgeSize, letterSpacing: 0.6 },
  output: {
    paddingVertical: chat.toolOutputPadV,
    paddingHorizontal: chat.toolOutputPadH,
    borderTopWidth: 1,
    fontSize: chat.toolOutputSize,
    lineHeight: chat.toolOutputLineHeight,
  },
});
