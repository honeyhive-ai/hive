import { useEffect, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { checkForUpdate, installUpdate, onUpdateProgress, openExternal, getAppInfo } from "@/lib/ipc";
import { Button, Section, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";

/// Updates & data: check for a newer signed build and install it in place — the
/// same in-place updater the launch banner uses (previously this only reported
/// availability, so a user had to restart to get the banner's Download button).
export function UpdatesSection() {
  const [available, setAvailable] = useState<string | null>(null);
  // Install progress: null = idle, -1 = starting, else 0–100 percent.
  const [pct, setPct] = useState<number | null>(null);

  useEffect(() => {
    const un = onUpdateProgress((p) => {
      if (p.done) setPct(100);
      else if (p.total && p.downloaded != null) setPct(Math.round((p.downloaded / p.total) * 100));
    });
    return () => {
      void un.then((f) => f());
    };
  }, []);

  const info = useQuery({ queryKey: ["app-info"], queryFn: getAppInfo });

  const check = useMutation({
    mutationFn: checkForUpdate,
    onSuccess: (version) => {
      if (version) {
        setAvailable(version);
      } else {
        setAvailable(null);
        toast.success("You're on the latest version.");
      }
    },
    onError: (e) => toast.error(errMsg(e)),
  });

  // In-place download → verify → install → relaunch. Falls back to the release
  // page if the updater isn't available (unsigned/unconfigured build).
  async function install() {
    setPct(-1);
    try {
      await installUpdate(); // relaunches on success; never returns
    } catch (e) {
      setPct(null);
      toast.error(`Couldn't install in place (${errMsg(e)}); opening the release page.`);
      void openExternal("https://github.com/honeyhive-ai/hive/releases/latest").catch(() => {});
    }
  }

  const installing = pct !== null;
  const installLabel =
    pct === null ? "Download & install" : pct < 0 ? "Starting…" : pct >= 100 ? "Restarting…" : `Installing… ${pct}%`;

  return (
    <Section title="Updates">
      <div className="text-sm">
        You're on <b>Hive v{info.data?.coreVersion ?? "…"}</b>
        {info.data?.buildProfile === "debug" && <span className="opacity-50"> · debug build</span>}
      </div>
      <div className="flex items-center justify-between gap-3">
        <p className="text-sm opacity-60">Check whether a newer signed build is available, and install it.</p>
        <Button
          onClick={() => check.mutate()}
          disabled={check.isPending || installing}
          className="shrink-0"
        >
          {check.isPending ? "Checking…" : "Check for updates"}
        </Button>
      </div>
      {available && (
        <div
          className="mt-2 flex items-center justify-between gap-3 rounded-xl border p-3"
          style={fieldStyle}
        >
          <div className="text-sm">
            <b>v{available}</b> is available.
          </div>
          <Button variant="primary" size="sm" onClick={() => void install()} disabled={installing}>
            {installLabel}
          </Button>
        </div>
      )}
    </Section>
  );
}
