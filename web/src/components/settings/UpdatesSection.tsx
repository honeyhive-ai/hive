import { useMutation } from "@tanstack/react-query";
import { checkForUpdate } from "@/lib/ipc";
import { Button, Section } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";

/// Updates & data: check for a newer signed build. The button works once the
/// updater is configured (signing keys + a published latest.json); until then
/// it reports that gracefully.
export function UpdatesSection() {
  const check = useMutation({
    mutationFn: checkForUpdate,
    onSuccess: (version) => {
      if (version) toast.success(`Update available: v${version}. Download from the latest release.`);
      else toast.success("You're on the latest version.");
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  return (
    <Section title="Updates">
      <div className="flex items-center justify-between gap-3">
        <p className="text-sm opacity-60">Check whether a newer signed build is available.</p>
        <Button onClick={() => check.mutate()} disabled={check.isPending} className="shrink-0">
          {check.isPending ? "Checking…" : "Check for updates"}
        </Button>
      </div>
    </Section>
  );
}
