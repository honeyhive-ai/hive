import { THEMES, type AppearanceMode, type ThemeName } from "@/lib/theme";
import { Section } from "@/components/ui";

/// Appearance: light/dark mode + accent family. Every control applies live.
export function AppearanceSection({
  palette,
  onPaletteChange,
  appearanceMode,
  onAppearanceModeChange,
}: {
  palette: ThemeName;
  onPaletteChange: (t: ThemeName) => void;
  appearanceMode: AppearanceMode;
  onAppearanceModeChange: (m: AppearanceMode) => void;
}) {
  return (
    <Section title="Appearance">
      <label className="block text-sm opacity-70">Mode</label>
      <div className="flex gap-2">
        {(["auto", "light", "dark"] as AppearanceMode[]).map((m) => (
          <button
            key={m}
            onClick={() => onAppearanceModeChange(m)}
            className="rounded-xl border px-3 py-2 capitalize"
            style={{
              borderColor: m === appearanceMode ? "var(--hive-accent-cool)" : "var(--hive-line)",
              background: m === appearanceMode ? "var(--hive-mist)" : "transparent",
            }}
          >
            {m === "auto" ? "Auto (system)" : m}
          </button>
        ))}
      </div>
      <p className="text-xs opacity-50">
        Auto follows your operating system's light/dark setting. Every theme has a light and a dark
        variant.
      </p>

      <label className="mt-3 block text-sm opacity-70">Theme</label>
      <div className="flex flex-wrap gap-2">
        {(Object.keys(THEMES) as ThemeName[]).map((t) => (
          <button
            key={t}
            onClick={() => onPaletteChange(t)}
            className="flex items-center gap-2 rounded-xl border px-3 py-2 capitalize"
            style={{
              borderColor: t === palette ? "var(--hive-accent-cool)" : "var(--hive-line)",
              background: t === palette ? "var(--hive-mist)" : "transparent",
            }}
          >
            <span className="h-3 w-3 rounded-full" style={{ background: THEMES[t].light.accentCool }} />
            {t}
          </button>
        ))}
      </div>
    </Section>
  );
}
