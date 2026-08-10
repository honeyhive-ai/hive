// Navigation.
//
// The desktop shell is four columns — WorkspaceRail, Sidebar, main, RightRail
// — with a 1280x832 default window and an 880px minimum. That layout does not
// fold onto a phone, so this adapts rather than mirrors:
//
//   WorkspaceRail  →  the strip at the top of the drawer
//   Sidebar        →  the drawer body
//   main           →  the stack root (ChatScreen)
//   RightRail      →  a pushed screen (Details)
//
// The visual language mirrors; the column count does not.

import { createDrawerNavigator } from "@react-navigation/drawer";
import { createNativeStackNavigator } from "@react-navigation/native-stack";
import {
  DefaultTheme,
  DarkTheme,
  NavigationContainer,
  useNavigation,
  type Theme,
} from "@react-navigation/native";
import type { DrawerScreenProps } from "@react-navigation/drawer";
import type { NativeStackNavigationProp } from "@react-navigation/native-stack";
import { useMemo } from "react";
import { Pressable, StyleSheet, Text } from "react-native";

import { DrawerContent } from "./DrawerContent";
import { AppearanceScreen } from "../screens/AppearanceScreen";
import { ChatScreen } from "../screens/ChatScreen";
import { DetailsScreen } from "../screens/DetailsScreen";
import { useTheme } from "../theme/ThemeProvider";
import { useWorkspace } from "../state/workspace";
import { space, text, touch } from "../theme/scale";

export type StackParams = {
  Chat: undefined;
  Details: undefined;
  Appearance: undefined;
};

const Stack = createNativeStackNavigator<StackParams>();
const Drawer = createDrawerNavigator();

type DrawerParams = { Main: undefined };

function MainStack({ navigation }: DrawerScreenProps<DrawerParams, "Main">) {
  const { tokens: t } = useTheme();
  const { channel } = useWorkspace();

  return (
    <Stack.Navigator
      screenOptions={{
        headerStyle: { backgroundColor: t.panel },
        headerTintColor: t.ink,
        headerTitleStyle: { fontSize: text.base.fontSize, fontWeight: "600" },
        // The desktop draws a hairline under the header; `headerShadowVisible`
        // would give a platform shadow instead, which reads as a different
        // elevation model.
        headerShadowVisible: false,
        contentStyle: { backgroundColor: t.canvas },
      }}
    >
      <Stack.Screen
        name="Chat"
        component={ChatScreen}
        options={{
          title: channel ? `#${channel.name}` : "Hive",
          headerLeft: () => (
            <HeaderButton label="☰" onPress={navigation.openDrawer} hint="Open workspaces" />
          ),
          headerRight: () => <DetailsButton />,
        }}
      />
      <Stack.Screen name="Details" component={DetailsScreen} options={{ title: "Details" }} />
      <Stack.Screen
        name="Appearance"
        component={AppearanceScreen}
        options={{ title: "Appearance" }}
      />
    </Stack.Navigator>
  );
}

// Rendered inside `headerRight`, so it reaches navigation through the hook
// rather than the screen props.
function DetailsButton() {
  const nav = useNavigation<NativeStackNavigationProp<StackParams>>();
  return (
    <HeaderButton label="ⓘ" onPress={() => nav.navigate("Details")} hint="Channel details" />
  );
}

function HeaderButton({
  label,
  onPress,
  hint,
}: {
  label: string;
  onPress: () => void;
  hint: string;
}) {
  const { tokens: t } = useTheme();
  return (
    <Pressable
      onPress={onPress}
      accessibilityRole="button"
      accessibilityLabel={hint}
      hitSlop={touch.headerButtonSlop}
      style={styles.headerButton}
    >
      <Text style={{ color: t.ink, fontSize: touch.headerGlyph }}>{label}</Text>
    </Pressable>
  );
}

export function RootNavigator({ onReady }: { onReady?: () => void }) {
  const { tokens: t, scheme } = useTheme();

  // React Navigation keeps its own theme for the few surfaces it paints itself
  // (card backgrounds during transitions, the drawer scrim). Feed it the same
  // resolved tokens so a transition never flashes a colour from another palette.
  const navTheme = useMemo<Theme>(() => {
    const base = scheme === "dark" ? DarkTheme : DefaultTheme;
    return {
      ...base,
      dark: scheme === "dark",
      colors: {
        ...base.colors,
        primary: t.accentFill,
        background: t.canvas,
        card: t.panel,
        text: t.ink,
        border: t.line,
        notification: t.danger,
      },
    };
  }, [t, scheme]);

  return (
    <NavigationContainer theme={navTheme} onReady={onReady}>
      <Drawer.Navigator
        drawerContent={(props) => <DrawerContent {...props} />}
        screenOptions={{
          headerShown: false,
          drawerType: "front",
          drawerStyle: { backgroundColor: t.sidebarTop, width: touch.drawerWidth },
          overlayColor: "rgba(0,0,0,0.4)",
        }}
      >
        <Drawer.Screen name="Main" component={MainStack} />
      </Drawer.Navigator>
    </NavigationContainer>
  );
}

const styles = StyleSheet.create({
  headerButton: { paddingHorizontal: space(1), minWidth: touch.headerButtonMin },
});
