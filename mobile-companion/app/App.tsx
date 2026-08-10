// Root. Loads the bundled mono face, then hands the resolved theme to the
// navigator.

import { useCallback, useEffect } from "react";
import { StatusBar } from "expo-status-bar";
import * as SplashScreen from "expo-splash-screen";
import { GestureHandlerRootView } from "react-native-gesture-handler";
import { SafeAreaProvider } from "react-native-safe-area-context";
import { useFonts, JetBrainsMono_400Regular } from "@expo-google-fonts/jetbrains-mono";

import { RootNavigator } from "./src/navigation";
import { ThemeProvider, useTheme } from "./src/theme/ThemeProvider";
import { WorkspaceProvider } from "./src/state/workspace";

// Hold the splash until both the font and the persisted theme are ready.
// Without this the first frame paints in the default palette and then snaps to
// the stored one, which is a visible flash on every cold start.
void SplashScreen.preventAutoHideAsync().catch(() => {});

export default function App() {
  // Body text uses the platform system face — the desktop's `-apple-system` /
  // `Segoe UI` stack maps cleanly onto San Francisco and Roboto, so there is
  // nothing to bundle there. JetBrains Mono has no system equivalent and does
  // need bundling: it carries timestamps, tool-call names, keycaps and badges.
  const [monoLoaded, fontError] = useFonts({ JetBrainsMono_400Regular });

  useEffect(() => {
    // A font that fails to load must not hold the app off the screen — the
    // theme falls back to the platform monospace face.
    if (fontError) console.warn("JetBrains Mono failed to load:", fontError);
  }, [fontError]);

  const ready = monoLoaded || !!fontError;

  return (
    <GestureHandlerRootView style={{ flex: 1 }}>
      <SafeAreaProvider>
        <ThemeProvider monoLoaded={monoLoaded}>
          <WorkspaceProvider>
            <Shell fontsSettled={ready} />
          </WorkspaceProvider>
        </ThemeProvider>
      </SafeAreaProvider>
    </GestureHandlerRootView>
  );
}

function Shell({ fontsSettled }: { fontsSettled: boolean }) {
  const { ready: themeReady, scheme } = useTheme();
  const settled = fontsSettled && themeReady;

  // Drop the splash once the navigator has actually mounted, rather than the
  // moment the flags flip — hiding it a frame early shows a blank canvas.
  const onNavigatorReady = useCallback(() => {
    void SplashScreen.hideAsync().catch(() => {});
  }, []);

  if (!settled) return null;

  return (
    <>
      {/* The status bar has to invert with the scheme, not the accent family —
          a light-on-light clock is invisible. */}
      <StatusBar style={scheme === "dark" ? "light" : "dark"} />
      <RootNavigator onReady={onNavigatorReady} />
    </>
  );
}
