// Appearance settings — accent family and light/dark mode.
//
// This screen is also how the definition of done gets checked by eye: every
// accent family is previewed with a live human/agent/shared turn trio, so
// switching mode walks all five families through both schemes in one place.

import { Pressable, ScrollView, StyleSheet, Text, View } from "react-native";

import { Turn } from "../components/Turn";
import { useTheme } from "../theme/ThemeProvider";
import { THEMES, THEME_NAMES, type AppearanceMode, type ThemeName } from "../theme/palettes";
import { resolveTokens } from "../theme/tokens";
import { radius, space, text, touch } from "../theme/scale";

const MODES: Array<{ value: AppearanceMode; label: string }> = [
  { value: "auto", label: "Auto" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

export function AppearanceScreen() {
  const { tokens: t, name, mode, scheme, setName, setMode } = useTheme();

  return (
    <ScrollView
      style={{ backgroundColor: t.canvas }}
      contentContainerStyle={styles.content}
      showsVerticalScrollIndicator={false}
    >
      <Text style={[styles.heading, { color: t.inkSoft }]}>APPEARANCE</Text>
      <View style={[styles.segment, { backgroundColor: t.mist, borderColor: t.line }]}>
        {MODES.map((m) => {
          const active = m.value === mode;
          return (
            <Pressable
              key={m.value}
              onPress={() => setMode(m.value)}
              accessibilityRole="radio"
              accessibilityState={{ selected: active }}
              style={[styles.segmentItem, active && { backgroundColor: t.panel }]}
            >
              <Text
                style={[
                  styles.segmentText,
                  { color: active ? t.ink : t.inkSoft, fontWeight: active ? "600" : "400" },
                ]}
              >
                {m.label}
              </Text>
            </Pressable>
          );
        })}
      </View>
      <Text style={[styles.note, { color: t.inkFaint }]}>
        {mode === "auto" ? `Following the system — currently ${scheme}.` : `Forced ${mode}.`}
      </Text>

      <Text style={[styles.heading, { color: t.inkSoft, marginTop: space(6) }]}>ACCENT</Text>
      {THEME_NAMES.map((n) => (
        <ThemeRow key={n} name={n} active={n === name} onPress={() => setName(n)} />
      ))}

      <Text style={[styles.heading, { color: t.inkSoft, marginTop: space(6) }]}>PREVIEW</Text>
      <Turn
        turn={{
          id: "preview-human",
          kind: "human",
          author: "You",
          body: "A human turn — warm accent, circular avatar, plain author name.",
          time: "12:00",
        }}
      />
      <Turn
        turn={{
          id: "preview-agent",
          kind: "agent",
          author: "Coder2",
          handle: "coder2",
          model: "claude-opus-5",
          body: "An agent turn — cool accent, rounded-square avatar, tinted handle.",
          time: "12:01",
        }}
      />
      <Turn
        turn={{
          id: "preview-shared",
          kind: "shared",
          author: "Hive",
          handle: "hive",
          badge: "workspace",
          body: "A shared turn — mist fill, no rail, softened text. Not a third accent.",
          time: "12:02",
        }}
      />
    </ScrollView>
  );
}

/// A family row previews its own palette rather than the active one, so the
/// choice is visible before it is made.
function ThemeRow({
  name,
  active,
  onPress,
}: {
  name: ThemeName;
  active: boolean;
  onPress: () => void;
}) {
  const { tokens: t, scheme } = useTheme();
  const preview = resolveTokens(name, scheme);
  const palette = THEMES[name][scheme];

  return (
    <Pressable
      onPress={onPress}
      accessibilityRole="radio"
      accessibilityState={{ selected: active }}
      accessibilityLabel={`${name} accent`}
      style={({ pressed }) => [
        styles.themeRow,
        { borderColor: active ? preview.accentFill : t.line, backgroundColor: t.panel },
        pressed && { backgroundColor: t.hover },
      ]}
    >
      <View style={styles.chips}>
        {[palette.canvas, palette.panel, palette.accentWarm, palette.accentCool, palette.ink].map(
          (color, i) => (
            <View key={i} style={[styles.chip, { backgroundColor: color, borderColor: t.line }]} />
          ),
        )}
      </View>
      <Text style={[styles.themeName, { color: t.ink }]}>{name}</Text>
      {active ? <Text style={[styles.check, { color: preview.accentFill }]}>✓</Text> : null}
    </Pressable>
  );
}

const styles = StyleSheet.create({
  content: { padding: space(3), paddingBottom: space(10) },
  heading: {
    fontSize: text.xs.fontSize,
    fontWeight: "600",
    letterSpacing: 1,
    marginBottom: space(2),
  },
  segment: {
    flexDirection: "row",
    borderWidth: 1,
    borderRadius: radius.xl,
    // A hairline inset so the selected thumb's radius nests inside the track's.
    // The desktop has no segmented control — these are chosen, not ported.
    padding: 3, // px-ok: thumb inset, mobile-only control
    gap: 3, // px-ok: thumb inset, mobile-only control
  },
  segmentItem: {
    flex: 1,
    minHeight: touch.minTarget - space(3),
    alignItems: "center",
    justifyContent: "center",
    borderRadius: radius.lg,
  },
  segmentText: { fontSize: text.sm.fontSize },
  note: { fontSize: text.xs.fontSize, marginTop: space(2) },
  themeRow: {
    flexDirection: "row",
    alignItems: "center",
    gap: space(3),
    minHeight: touch.minTarget,
    paddingHorizontal: space(3),
    borderWidth: 1,
    borderRadius: radius.xl,
    marginBottom: space(2),
  },
  // Palette swatches: five thin bars reading as one strip, so the gap and the
  // corner are deliberately below anything on the ramp. Mobile-only.
  chips: { flexDirection: "row", gap: 2 }, // px-ok: swatch strip, no desktop equivalent
  chip: {
    width: 14, // px-ok: swatch strip, no desktop equivalent
    height: 22, // px-ok: swatch strip, no desktop equivalent
    borderRadius: 3, // px-ok: swatch strip, no desktop equivalent
    borderWidth: 1,
  },
  themeName: { flex: 1, fontSize: text.sm.fontSize, fontWeight: "600", textTransform: "capitalize" },
  check: { fontSize: text.base.fontSize, fontWeight: "700" },
});
