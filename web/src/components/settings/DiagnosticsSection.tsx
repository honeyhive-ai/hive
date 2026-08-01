import { useQuery } from "@tanstack/react-query";
import { readRecentLogs, logDirPath } from "@/lib/ipc";
import { Section, Button, fieldStyle } from "@/components/ui";
import { toast } from "@/components/Toast";

/// Diagnostics — the tail of the app log (internal errors/warnings/panics that
/// otherwise only reach stderr, invisible in a GUI launch). Lets a user see and
/// share what went wrong without launching from a terminal.
export function DiagnosticsSection() {
  const logs = useQuery({
    queryKey: ["recent-logs"],
    queryFn: () => readRecentLogs(800),
    refetchInterval: 4000,
  });
  const dir = useQuery({ queryKey: ["log-dir"], queryFn: logDirPath });

  const copyPath = async () => {
    if (!dir.data) return;
    try {
      await navigator.clipboard.writeText(dir.data);
      toast.success("Log folder path copied.");
    } catch {
      toast.error("Couldn't copy path.");
    }
  };

  return (
    <Section title="Diagnostics">
      <p className="text-xs opacity-50">
        Recent internal log output — background errors from sync, agents, scheduling, and workflows,
        plus panics. Useful when something misbehaves silently. Set <code>HIVE_LOG=debug</code> for
        more detail. Newest lines are at the bottom.
      </p>
      <div className="flex items-center gap-2">
        <Button size="sm" onClick={() => logs.refetch()}>
          Refresh
        </Button>
        {dir.data && (
          <button
            className="text-xs underline underline-offset-2 hover:opacity-80"
            onClick={copyPath}
            title={dir.data}
          >
            Copy log-folder path
          </button>
        )}
      </div>
      <pre
        className="mt-1 max-h-[52vh] overflow-auto rounded-xl border p-3 text-[11.5px] leading-relaxed"
        style={{ ...fieldStyle, fontFamily: "var(--hive-mono, ui-monospace, monospace)", whiteSpace: "pre-wrap" }}
      >
        {logs.isError
          ? "No log file yet — it appears once the app has logged something this session."
          : logs.data?.trim()
            ? logs.data
            : "No recent log output."}
      </pre>
    </Section>
  );
}
