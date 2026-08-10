// The composer — port of the `.cmp` card. A turn-shaped surface, "the next
// turn in the transcript", which is why it carries the same border, radius and
// panel fill as a turn rather than reading as a toolbar.
//
// Inert in this milestone. It says so, plainly, instead of offering a send
// button that silently does nothing: an affordance that looks live and is not
// is worse than no affordance.

import { StyleSheet, Text, TextInput, View } from "react-native";
import Svg, { Path } from "react-native-svg";

import { useTheme } from "../theme/ThemeProvider";
import { composer, space } from "../theme/scale";

export function Composer() {
  const { tokens: t, monoFont } = useTheme();

  return (
    <View style={styles.wrap}>
      <View style={[styles.card, { borderColor: t.line, backgroundColor: t.panel }]}>
        <TextInput
          editable={false}
          multiline
          placeholder="Sending arrives with the transport milestone"
          placeholderTextColor={t.inkFaint}
          style={[styles.input, { color: t.ink }]}
          accessibilityLabel="Message composer, not yet enabled"
        />
        <View style={styles.footer}>
          <View style={styles.spacer} />
          <View style={[styles.send, { backgroundColor: t.mist }]}>
            {/* Disabled styling from `.send:disabled` — mist fill, faint ink. */}
            <SendGlyph color={t.inkFaint} />
          </View>
        </View>
      </View>
      <View style={styles.hint}>
        <Text style={[styles.hintText, { color: t.inkSoft }]}>Read-only build</Text>
        <View style={[styles.kbd, { backgroundColor: t.mist, borderColor: t.line }]}>
          <Text style={[styles.kbdText, { color: t.inkSoft, fontFamily: monoFont }]}>MC-001</Text>
        </View>
      </View>
    </View>
  );
}

function SendGlyph({ color }: { color: string }) {
  return (
    <Svg
      width={composer.sendGlyphSize}
      height={composer.sendGlyphSize}
      viewBox="0 0 24 24"
      fill="none"
      stroke={color}
      strokeWidth={2}
    >
      <Path d="M22 2 11 13M22 2l-7 20-4-9-9-4Z" strokeLinecap="round" strokeLinejoin="round" />
    </Svg>
  );
}

const styles = StyleSheet.create({
  wrap: { paddingHorizontal: space(2), paddingBottom: space(2) },
  card: {
    borderWidth: 1,
    // 12, not `radius.xl`'s 10.5 — `.cmp-card` writes the radius literally, so
    // the rem factor never applied to it on the desktop either. See `composer`.
    borderRadius: composer.cardRadius,
    paddingVertical: composer.cardPadV,
    paddingHorizontal: composer.cardPadH,
  },
  input: {
    minHeight: composer.inputMinHeight,
    // The desktop caps at 220px against an 832px window. A phone keyboard
    // leaves far less, so this one is chosen rather than ported.
    maxHeight: 120, // px-ok: phone-specific cap, no desktop equivalent
    fontSize: composer.inputSize,
    lineHeight: composer.inputLineHeight,
    paddingVertical: composer.inputPadV,
    paddingHorizontal: composer.inputPadH,
    textAlignVertical: "top",
  },
  footer: {
    flexDirection: "row",
    alignItems: "center",
    gap: composer.footerGap,
    marginTop: composer.footerMarginTop,
  },
  spacer: { flex: 1 },
  send: {
    width: composer.sendSize,
    height: composer.sendSize,
    borderRadius: composer.sendRadius,
    alignItems: "center",
    justifyContent: "center",
  },
  hint: {
    flexDirection: "row",
    alignItems: "center",
    gap: composer.hintGap,
    paddingTop: composer.hintPadTop,
    paddingHorizontal: composer.hintPadH,
  },
  // `.hint` is a literal 12px. It sits one notch below `text-sm`'s 12.25 — a
  // quarter-pixel apart, and still two different scales.
  hintText: { fontSize: composer.hintSize },
  kbd: {
    borderWidth: 1,
    borderBottomWidth: 2,
    borderRadius: composer.kbdRadius,
    paddingHorizontal: composer.kbdPadH,
  },
  kbdText: { fontSize: composer.kbdSize, lineHeight: composer.kbdLineHeight },
});
