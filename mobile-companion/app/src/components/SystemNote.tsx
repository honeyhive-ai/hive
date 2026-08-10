// `.tt-system` — summaries and notices read as centred meta between rules,
// not as a reply from anyone. Deliberately not a Turn: it has no author, no
// avatar and no accent.

import { StyleSheet, Text, View } from "react-native";

import { useTokens } from "../theme/ThemeProvider";
import { chat } from "../theme/scale";

export function SystemNote({ body, time }: { body: string; time?: string | null }) {
  const t = useTokens();
  return (
    <View style={styles.row}>
      <View style={[styles.rule, { backgroundColor: t.line }]} />
      <Text style={[styles.text, { color: t.inkSoft }]}>{body}</Text>
      {time ? <Text style={[styles.text, { color: t.inkSoft, opacity: 0.7 }]}>{time}</Text> : null}
      <View style={[styles.rule, { backgroundColor: t.line }]} />
    </View>
  );
}

const styles = StyleSheet.create({
  row: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "center",
    gap: chat.systemGap,
    paddingVertical: chat.systemPadV,
  },
  // `flex: 1` with a cap, matching `.tt-system .ln { flex: 1; max-width: 120px }`.
  rule: { height: chat.systemRuleHeight, flex: 1, maxWidth: chat.systemRuleMaxWidth },
  text: { fontSize: chat.systemSize },
});
