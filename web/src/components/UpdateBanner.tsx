import { useEffect, useState } from "react";
import { checkForAppUpdate, openExternal, type AppUpdateInfo } from "@/lib/ipc";
import { IconSparkle, IconX } from "@/lib/icons";

const DISMISS_KEY = "hive.updateDismissed";

/// A quiet, non-disruptive "update available" banner. Slides up from the
/// bottom-right; never blocks the UI. It only appears when the backend reports a
/// newer published tag (dev builds never do), and stays hidden once dismissed
/// for that specific version. Failures are silent — it never nags on its own.
export function UpdateBanner() {
  const [info, setInfo] = useState<AppUpdateInfo | null>(null);

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

  if (!info) return null;

  function dismiss() {
    if (info) window.localStorage.setItem(DISMISS_KEY, info.tag);
    setInfo(null);
  }

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
              onClick={() => {
                void openExternal(info.url || "https://github.com/honeyhive-ai/hive/releases/latest").catch(() => {});
              }}
              className="rounded-lg px-3 py-1.5 text-sm font-medium text-white hover:brightness-110"
              style={{ background: "var(--hive-accent-cool)" }}
            >
              Download
            </button>
            <button onClick={dismiss} className="rounded-lg px-3 py-1.5 text-sm opacity-60 hover:opacity-100">
              Later
            </button>
          </div>
        </div>
        <button onClick={dismiss} aria-label="Dismiss" className="shrink-0 opacity-40 hover:opacity-80">
          <IconX size={14} />
        </button>
      </div>
    </div>
  );
}
