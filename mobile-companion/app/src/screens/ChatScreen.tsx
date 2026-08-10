// The transcript — the desktop's `main` column, which is the one part of the
// four-column shell that maps onto a phone unchanged.

import { useCallback } from "react";
import { FlatList, StyleSheet, Text, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

import { SystemNote } from "../components/SystemNote";
import { Turn } from "../components/Turn";
import { Composer } from "../components/Composer";
import { TRANSCRIPT, type TranscriptEntry } from "../fixtures/transcript";
import { useTokens } from "../theme/ThemeProvider";
import { space, text } from "../theme/scale";

export function ChatScreen() {
  const t = useTokens();
  const insets = useSafeAreaInsets();

  const renderItem = useCallback(({ item }: { item: TranscriptEntry }) => {
    if (item.type === "system") return <SystemNote body={item.body} time={item.time} />;
    return <Turn turn={item.turn} />;
  }, []);

  const keyExtractor = useCallback(
    (item: TranscriptEntry) => (item.type === "system" ? item.id : item.turn.id),
    [],
  );

  return (
    <View style={[styles.root, { backgroundColor: t.canvas }]}>
      <FlatList
        data={TRANSCRIPT}
        renderItem={renderItem}
        keyExtractor={keyExtractor}
        contentContainerStyle={styles.list}
        ListFooterComponent={<TranscriptFooter />}
        // The desktop pins to the bottom of a long log; the same reading order
        // applies here, but the fixture is short enough to start at the top.
        showsVerticalScrollIndicator={false}
      />
      <View style={{ paddingBottom: insets.bottom }}>
        <Composer />
      </View>
    </View>
  );
}

function TranscriptFooter() {
  const t = useTokens();
  return (
    <Text style={[styles.footer, { color: t.inkFaint }]}>
      Fixture transcript — this build has no transport.
    </Text>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1 },
  list: { paddingHorizontal: space(2), paddingTop: space(2), paddingBottom: space(3) },
  footer: {
    fontSize: text.xs.fontSize,
    textAlign: "center",
    paddingTop: space(4),
    paddingBottom: space(2),
  },
});
