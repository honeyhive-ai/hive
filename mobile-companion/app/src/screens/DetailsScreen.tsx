// The desktop's RightRail equivalent — channel context that sits beside the
// transcript on a wide window and becomes a pushed screen on a phone.
//
// Deliberately thin. The rail's real content is approvals and workspace state,
// which arrive with the transport milestone; inventing a placeholder for them
// here would mean shipping copy that implies a capability this build does not
// have.

import { ScrollView, StyleSheet, Text, View } from "react-native";

import { Avatar } from "../components/Avatar";
import { useTokens } from "../theme/ThemeProvider";
import { useWorkspace } from "../state/workspace";
import { radius, space, text } from "../theme/scale";

const PARTICIPANTS: Array<{ name: string; kind: "human" | "agent"; host?: "local" | "remote" }> = [
  { name: "claudeza", kind: "human" },
  { name: "You", kind: "human" },
  { name: "Coder2", kind: "agent", host: "local" },
  { name: "Reviewer", kind: "agent", host: "remote" },
  { name: "Hive", kind: "agent", host: "local" },
];

export function DetailsScreen() {
  const t = useTokens();
  const { channel, workspace } = useWorkspace();

  return (
    <ScrollView style={{ backgroundColor: t.canvas }} contentContainerStyle={styles.content}>
      <Section title="CHANNEL">
        <Text style={[styles.title, { color: t.ink }]}>#{channel.name}</Text>
        <Text style={[styles.body, { color: t.inkSoft }]}>{channel.topic}</Text>
        <Text style={[styles.body, { color: t.inkFaint }]}>in {workspace.name}</Text>
      </Section>

      <Section title="PARTICIPANTS">
        {PARTICIPANTS.map((p) => (
          <View key={p.name} style={styles.person}>
            <Avatar name={p.name} kind={p.kind} host={p.host} size={26} />
            <Text style={[styles.personName, { color: t.ink }]}>{p.name}</Text>
            <Text style={[styles.role, { color: t.inkFaint }]}>{p.kind}</Text>
          </View>
        ))}
      </Section>
    </ScrollView>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  const t = useTokens();
  return (
    <View style={styles.section}>
      <Text style={[styles.heading, { color: t.inkSoft }]}>{title}</Text>
      <View style={[styles.card, { backgroundColor: t.panel, borderColor: t.line }]}>
        {children}
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  content: { padding: space(3), paddingBottom: space(8) },
  section: { marginBottom: space(5) },
  heading: {
    fontSize: text.xs.fontSize,
    fontWeight: "600",
    letterSpacing: 1,
    marginBottom: space(2),
  },
  card: { borderWidth: 1, borderRadius: radius.xl, padding: space(3), gap: space(2) },
  title: { fontSize: text.lg.fontSize, fontWeight: "600" },
  body: { fontSize: text.sm.fontSize, lineHeight: text.sm.lineHeight },
  person: { flexDirection: "row", alignItems: "center", gap: space(3) },
  personName: { flex: 1, fontSize: text.sm.fontSize, fontWeight: "600" },
  role: { fontSize: text.xs.fontSize },
});
