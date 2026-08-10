// Theme context: chosen accent family + appearance mode in, resolved tokens
// out. Mirrors what `applyTheme()` does on the desktop, minus the CSS custom
// properties — components read `useTheme()` instead of `var(--hive-*)`.

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { AccessibilityInfo, Appearance, Platform } from "react-native";
import AsyncStorage from "@react-native-async-storage/async-storage";

import {
  DEFAULT_THEME,
  MODE_KEY,
  STORAGE_KEY,
  THEMES,
  type AppearanceMode,
  type Scheme,
  type ThemeName,
} from "./palettes";
import { resolveTokens, type Tokens } from "./tokens";

interface ThemeContextValue {
  /// The chosen accent family, independent of light/dark.
  name: ThemeName;
  /// The chosen appearance mode, including `auto`.
  mode: AppearanceMode;
  /// The scheme actually in force once `auto` is resolved.
  scheme: Scheme;
  tokens: Tokens;
  /// True when the OS asks for reduced motion. The desktop honours this in CSS
  /// (`@media (prefers-reduced-motion: reduce)` disables the caret blink and
  /// the live dots); React Native has no such media query, so the flag has to
  /// be threaded explicitly or the accessibility behaviour is silently lost.
  reduceMotion: boolean;
  /// The monospace family to render with — the bundled face once it has
  /// loaded, the platform default before that so text is never invisible.
  monoFont: string;
  setName: (name: ThemeName) => void;
  setMode: (mode: AppearanceMode) => void;
  /// False until the persisted choice has been read back.
  ready: boolean;
}

const ThemeContext = createContext<ThemeContextValue | null>(null);

function systemScheme(): Scheme {
  return Appearance.getColorScheme() === "dark" ? "dark" : "light";
}

export function ThemeProvider({
  children,
  monoLoaded = false,
}: {
  children: ReactNode;
  /// Passed down from the root, which owns `useFonts`.
  monoLoaded?: boolean;
}) {
  const [name, setNameState] = useState<ThemeName>(DEFAULT_THEME);
  const [mode, setModeState] = useState<AppearanceMode>("auto");
  const [osScheme, setOsScheme] = useState<Scheme>(systemScheme);
  const [reduceMotion, setReduceMotion] = useState(false);
  const [ready, setReady] = useState(false);

  // Restore the persisted choice. Keys match the desktop's localStorage keys,
  // so a future settings sync has nothing to translate.
  useEffect(() => {
    let live = true;
    (async () => {
      try {
        const [storedName, storedMode] = await AsyncStorage.multiGet([STORAGE_KEY, MODE_KEY]);
        if (!live) return;
        const n = storedName[1];
        const m = storedMode[1];
        if (n && n in THEMES) setNameState(n as ThemeName);
        if (m === "light" || m === "dark" || m === "auto") setModeState(m);
      } catch {
        // A failed read must never keep the app off the screen — fall through
        // to the launch default.
      } finally {
        if (live) setReady(true);
      }
    })();
    return () => {
      live = false;
    };
  }, []);

  // Track the OS light/dark preference so `auto` follows it live.
  useEffect(() => {
    const sub = Appearance.addChangeListener(({ colorScheme }) =>
      setOsScheme(colorScheme === "dark" ? "dark" : "light"),
    );
    return () => sub.remove();
  }, []);

  useEffect(() => {
    let live = true;
    AccessibilityInfo.isReduceMotionEnabled()
      .then((v) => live && setReduceMotion(v))
      .catch(() => {});
    const sub = AccessibilityInfo.addEventListener("reduceMotionChanged", setReduceMotion);
    return () => {
      live = false;
      sub.remove();
    };
  }, []);

  const setName = useCallback((next: ThemeName) => {
    setNameState(next);
    void AsyncStorage.setItem(STORAGE_KEY, next).catch(() => {});
  }, []);

  const setMode = useCallback((next: AppearanceMode) => {
    setModeState(next);
    void AsyncStorage.setItem(MODE_KEY, next).catch(() => {});
  }, []);

  const scheme: Scheme = mode === "auto" ? osScheme : mode;
  // Resolving walks every derived token, including a binary search per OKLCH
  // colour, so memoise on the only two inputs that can change it.
  const tokens = useMemo(() => resolveTokens(name, scheme), [name, scheme]);

  const monoFont = monoLoaded ? MONO_FAMILY : MONO_FALLBACK;

  const value = useMemo(
    () => ({ name, mode, scheme, tokens, reduceMotion, monoFont, setName, setMode, ready }),
    [name, mode, scheme, tokens, reduceMotion, monoFont, setName, setMode, ready],
  );

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}

export function useTheme(): ThemeContextValue {
  const ctx = useContext(ThemeContext);
  if (!ctx) throw new Error("useTheme must be used inside <ThemeProvider>");
  return ctx;
}

/// Just the tokens, for the common case.
export function useTokens(): Tokens {
  return useTheme().tokens;
}

/// The monospace family. `JetBrains Mono` carries timestamps, tool-call names,
/// keycaps and badges on the desktop and has to be bundled — the platform
/// system faces cover the body text (`-apple-system` / `Segoe UI` map cleanly
/// to San Francisco and Roboto) but there is no bundled mono to fall back on.
/// Until the font finishes loading, use the platform default so text is never
/// invisible.
export const MONO_FAMILY = "JetBrainsMono_400Regular";
export const MONO_FALLBACK = Platform.select({
  ios: "Menlo",
  android: "monospace",
  default: "monospace",
}) as string;
