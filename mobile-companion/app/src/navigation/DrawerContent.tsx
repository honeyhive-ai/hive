// The drawer folds the desktop's two leftmost columns together.
//
// On desktop `WorkspaceRail` (a narrow vertical strip of workspaces) sits
// beside `Sidebar` (the channel list). Neither survives as a permanent column
// at phone width — the desktop shell has an 880px minimum and four columns —
// so both live here: the rail becomes the strip along the top of the drawer,
// the sidebar becomes its body. The visual language is what mirrors; the
// column count is not.

import type { DrawerContentComponentProps } from "@react-navigation/drawer";
import { Pressable, ScrollView, StyleSheet, Text, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { useTokens } from "../theme/ThemeProvider";
import { useWorkspace } from "../state/workspace";
import { inlineDesktop, radius, rem, space, text, touch } from "../theme/scale";
import { avColor } from "../lib/avatar";

export function DrawerContent({ navigation }: DrawerContentComponentProps) {
  const t = useTokens();
  const insets = useSafeAreaInsets();
  const { workspace, workspaces, channel, channels, selectWorkspace, selectChannel } =
    useWorkspace();

  return (
    // The sidebar carries its own gradient identity on desktop
    // (`sidebarTop`→`sidebarBottom`). A flat `sidebarTop` fill reads the same
    // at this width; the gradient would need an extra native dependency to
    // reproduce faithfully, which is not worth it for the base build.
    <View style={[styles.root, { backgroundColor: t.sidebarTop, paddingTop: insets.top }]}>
      <View style={styles.rail}>
        {workspaces.map((w) => {
          const active = w.id === workspace.id;
          return (
            <Pressable
              key={w.id}
              onPress={() => selectWorkspace(w.id)}
              accessibilityRole="button"
              accessibilityState={{ selected: active }}
              accessibilityLabel={`${w.name} workspace`}
              style={[
                styles.railItem,
                { backgroundColor: avColor(w.name) },
                active && { borderColor: t.sidebarInk, borderWidth: 2 },
              ]}
            >
              <Text style={styles.railText}>{w.initials}</Text>
            </Pressable>
          );
        })}
      </View>

      <Text style={[styles.wsName, { color: t.sidebarInk }]} numberOfLines={1}>
        {workspace.name}
      </Text>

      <ScrollView contentContainerStyle={styles.list}>
        {channels.map((c) => {
          const active = c.id === channel?.id;
          return (
            <Pressable
              key={c.id}
              onPress={() => {
                selectChannel(c.id);
                navigation.closeDrawer();
              }}
              accessibilityRole="button"
              accessibilityState={{ selected: active }}
              style={({ pressed }) => [
                styles.channel,
                active && { backgroundColor: t.overlay },
                pressed && { backgroundColor: t.overlay },
              ]}
            >
              <Text style={[styles.hash, { color: t.sidebarInkMuted }]}>#</Text>
              <View style={styles.channelBody}>
                <Text style={[styles.channelName, { color: t.sidebarInk }]} numberOfLines={1}>
                  {c.name}
                </Text>
                <Text style={[styles.topic, { color: t.sidebarInkMuted }]} numberOfLines={1}>
                  {c.topic}
                </Text>
              </View>
              {c.unread > 0 ? (
                <View style={[styles.unread, { backgroundColor: t.accentFill }]}>
                  <Text style={[styles.unreadText, { color: t.onAccent }]}>{c.unread}</Text>
                </View>
              ) : null}
            </Pressable>
          );
        })}
      </ScrollView>

      <Pressable
        onPress={() => {
          navigation.closeDrawer();
          navigation.navigate("Appearance" as never);
        }}
        accessibilityRole="button"
        style={({ pressed }) => [
          styles.footer,
          { borderTopColor: t.overlay, paddingBottom: insets.bottom + space(2) },
          pressed && { backgroundColor: t.overlay },
        ]}
      >
        <Text style={[styles.footerText, { color: t.sidebarInk }]}>Appearance</Text>
      </Pressable>
    </View>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1 },
  rail: {
    flexDirection: "row",
    gap: space(2),
    paddingHorizontal: space(3),
    paddingTop: space(3),
    paddingBottom: space(2),
  },
  railItem: {
    // `h-10 w-10` on the desktop tile — 2.5rem, which is 35 here, not 40.
    width: rem(2.5),
    height: rem(2.5),
    // Workspaces are containers, not people: rounded square, like agents,
    // rather than the circle that means "human". The 12 is the desktop's own
    // inline value for the *active* tile and is not on the radius ramp.
    borderRadius: inlineDesktop.railTileRadiusActive,
    alignItems: "center",
    justifyContent: "center",
  },
  // `text-sm` on the tile — 12.25, matching Avatar.tsx's hardcoded `#fff`.
  railText: { color: "#fff", fontWeight: "600", fontSize: text.sm.fontSize },
  wsName: {
    fontSize: text.lg.fontSize,
    fontWeight: "600",
    paddingHorizontal: space(3),
    paddingBottom: space(2),
  },
  list: { paddingHorizontal: space(2), paddingBottom: space(2) },
  channel: {
    flexDirection: "row",
    alignItems: "center",
    gap: space(2),
    minHeight: touch.minTarget,
    paddingHorizontal: space(2),
    borderRadius: radius.xl,
  },
  hash: { fontSize: text.base.fontSize, fontWeight: "600" },
  channelBody: { flex: 1, flexShrink: 1 },
  channelName: { fontSize: text.sm.fontSize, fontWeight: "600" },
  topic: { fontSize: text.xs.fontSize },
  // The desktop shows an unread *dot* (`h-1.5 w-1.5 rounded-full`), never a
  // count, so there is nothing to port — a phone drawer has room for the number
  // and benefits from it. On the ramp, since it is ours.
  unread: {
    minWidth: space(5),
    borderRadius: radius.full,
    paddingHorizontal: space(2),
    paddingVertical: space(0.5),
  },
  unreadText: { fontSize: text.xs.fontSize, fontWeight: "700", textAlign: "center" },
  footer: {
    borderTopWidth: 1,
    paddingHorizontal: space(3),
    paddingTop: space(3),
    minHeight: touch.minTarget,
  },
  footerText: { fontSize: text.sm.fontSize, fontWeight: "600" },
});
