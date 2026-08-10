// One avatar for every render site — port of web/src/components/Avatar.tsx.
//
// `kind` picks the shape: human = circle, agent = rounded square. Role is
// carried by geometry, not hue alone, so it survives greyscale, colour-blind
// vision, and the first five minutes before the convention is learned. Keep
// the shapes even if the colours change.

import { Image, StyleSheet, Text, View } from "react-native";
import Svg, { Path, Rect } from "react-native-svg";

import { avColor, initials } from "../lib/avatar";
import { useTokens } from "../theme/ThemeProvider";

export type AvatarKind = "human" | "agent";
export type AvatarHost = "local" | "remote";

export function Avatar({
  name,
  url,
  colorHex,
  kind = "human",
  host,
  size = 30,
}: {
  name: string;
  url?: string | null;
  colorHex?: string | null;
  kind?: AvatarKind;
  host?: AvatarHost | null;
  size?: number;
}) {
  const t = useTokens();
  // CSS `border-radius: 50%` / `30%` are percentages of the box; React Native
  // takes absolute values, so resolve them against the size.
  const radius = kind === "agent" ? size * 0.3 : size / 2;
  const base = { width: size, height: size, borderRadius: radius };

  const face = url ? (
    <Image source={{ uri: url }} style={[base, styles.cover]} accessibilityIgnoresInvertColors />
  ) : (
    <View style={[base, styles.face, { backgroundColor: colorHex || avColor(name) }]}>
      <Text
        style={{
          // Hardcoded white, matching the desktop. `Avatar.tsx` sets this
          // inline, which beats the `.av { color: var(--hive-on-accent) }` rule
          // in styles.css — that rule is dead for the initials path. Following
          // the token instead would flip these to near-black in dark mode and
          // diverge from the desktop. Do not "fix" this to --hive-on-accent.
          color: "#fff",
          fontSize: Math.round(size * 0.4),
          fontWeight: "600",
        }}
        // The avatar is decorative next to the author name it accompanies.
        accessibilityElementsHidden
        importantForAccessibility="no"
      >
        {initials(name)}
      </Text>
    </View>
  );

  // No host badge → the bare face, so every existing call site stays identical.
  if (kind !== "agent" || !host) return face;

  const badge = Math.max(11, Math.round(size * 0.42));
  return (
    <View style={styles.badgeWrap}>
      {face}
      <View
        accessibilityRole="image"
        accessibilityLabel={host === "remote" ? "Runs on a remote host" : "Runs on this device"}
        style={[
          styles.badge,
          {
            width: badge,
            height: badge,
            borderRadius: badge / 2,
            backgroundColor: t.panel,
            // The desktop draws this ring with a box-shadow spread; React
            // Native has no spread, so a border of the same colour and width
            // reproduces it. Sized up to keep the glyph box unchanged.
            borderWidth: 1.5,
            borderColor: t.panel,
          },
        ]}
      >
        {host === "remote" ? (
          <CloudGlyph size={badge - 4} color={t.inkSoft} />
        ) : (
          <LaptopGlyph size={badge - 4} color={t.inkSoft} />
        )}
      </View>
    </View>
  );
}

function LaptopGlyph({ size, color }: { size: number; color: string }) {
  return (
    <Svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke={color} strokeWidth={2}>
      <Rect x="4" y="5" width="16" height="11" rx="1.5" strokeLinejoin="round" />
      <Path d="M2 20h20" strokeLinecap="round" />
    </Svg>
  );
}

function CloudGlyph({ size, color }: { size: number; color: string }) {
  return (
    <Svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke={color} strokeWidth={2}>
      <Path
        d="M6 18a4 4 0 0 1-.4-7.98A5.5 5.5 0 0 1 16.5 9 4.5 4.5 0 0 1 17 18Z"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </Svg>
  );
}

const styles = StyleSheet.create({
  cover: { resizeMode: "cover" },
  face: { alignItems: "center", justifyContent: "center" },
  badgeWrap: { position: "relative", alignSelf: "flex-start" },
  badge: {
    position: "absolute",
    right: -2,
    bottom: -2,
    alignItems: "center",
    justifyContent: "center",
  },
});
