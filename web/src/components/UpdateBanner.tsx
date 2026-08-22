import { useEffect, useState } from "react";
import {
  checkForAppUpdate,
  installUpdate,
  onUpdateProgress,
  openExternal,
  type AppUpdateInfo,
} from "@/lib/ipc";
import { IconSparkle, IconX } from "@/lib/icons";

const DISMISS_KEY = "hive.updateDismissed";

/// A quiet, non-disruptive "update available" banner. Slides up from the
/// bottom-right; never blocks the UI. It only appears when the backend reports a
/// newer published tag (dev builds never do), and stays hidden once dismissed
/// for that specific version. Failures are silent — it never nags on its own.
export function UpdateBanner() {
  const [info, setInfo] = useState<AppUpdateInfo | null>(null);
  // Install state: null = idle, else a 0–100 percent (or -1 while starting).
  const [pct, setPct] = useState<number | null>(null);

  useEffect(() => {
    let alive = true;
    void checkForAppUpdate()
      .then((u) => {
        if (!alive || !u) return;
        if (window.localStorage.getItem(DISMISS_KEY) === u.tag) return; // already dismissed this one
        setInfo(u);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    const un = onUpdateProgress((p) => {
      if (p.done) setPct(100);
      else if (p.total && p.downloaded != null) setPct(Math.round((p.downloaded / p.total) * 100));
    });
    return () => {
      void un.then((f) => f());
    };
  }, []);

  if (!info) return null;

  // "Later" hides it for this session only — it returns on next launch.
  function later() {
    setInfo(null);
  }

  // "Skip this version" persists, so this specific tag never shows again (until
  // a newer release supersedes it).
  function skipVersion() {
    if (info) window.localStorage.setItem(DISMISS_KEY, info.tag);
    setInfo(null);
  }

  // Try the in-place updater (download → verify → install → relaunch). If it
  // isn't available yet (no signed release / unconfigured), fall back to the
  // release page so the button always does *something*.
  async function handleInstall() {
    setPct(-1);
    try {
      await installUpdate(); // relaunches on success; never returns
    } catch {
      setPct(null);
      void openExternal(info?.url || "https://github.com/honeyhive-ai/hive/releases/latest").catch(
        () => {},
      );
    }
  }

  const installing = pct !== null;
  const label =
    pct === null ? "Download" : pct < 0 ? "Starting…" : pct >= 100 ? "Restarting…" : `Installing… ${pct}%`;

  return (
    // bottom-20 (not bottom-4): the ToastHost owns the bottom-right corner —
    // stacking above it keeps a toast from covering the banner (and vice versa).
    <div
      className="fixed bottom-20 right-4 z-[60] w-[320px] rounded-2xl border p-4 shadow-xl"
      style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)", color: "var(--hive-ink)" }}
    >
      <div className="flex items-start gap-3">
        <div className="mt-0.5" style={{ color: "var(--hive-accent-cool)" }} aria-hidden>
          <IconSparkle size={18} />
        </div>
        <div className="min-w-0 flex-1">
          <div className="text-sm font-semibold">Update available</div>
          <div className="mt-0.5 truncate text-sm opacity-70">{info.name || info.tag}</div>
          {info.notes && <div className="mt-1 text-xs opacity-55">{info.notes}</div>}
          <div className="mt-3 flex items-center gap-2">
            <button
              onClick={() => void handleInstall()}
              disabled={installing}
              className="rounded-lg px-3 py-1.5 text-sm font-medium text-white hover:brightness-110 disabled:opacity-70"
              style={{ background: "var(--hive-accent-cool)" }}
            >
              {label}
            </button>
            {!installing && (
              <>
                <button onClick={later} className="rounded-lg px-3 py-1.5 text-sm opacity-60 hover:opacity-100">
                  Later
                </button>
                <button
                  onClick={skipVersion}
                  className="rounded-lg px-2 py-1.5 text-xs opacity-45 hover:opacity-80"
                  title="Don't show this version again"
                >
                  Skip
                </button>
              </>
            )}
          </div>
        </div>
        <button onClick={later} aria-label="Remind me later" className="shrink-0 opacity-40 hover:opacity-80">
          <IconX size={14} />
        </button>
      </div>
    </div>
  );
}
