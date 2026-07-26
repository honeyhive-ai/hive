import { resetLocalData } from "@/lib/ipc";
import { Button, Section } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";

// A true "factory reset" — wipes the local DB, identity, keys, settings, and
// workspaces, then relaunches fresh. This is the supported way to start over
// (uninstalling leaves the data dir behind on every OS), so testers never have
// to hand-delete a hidden app-data folder.
export function DangerZoneSection() {
  async function reset() {
    try {
      // Clear the webview's own UI state too (theme prefs, etc.) so the relaunch
      // is genuinely a clean slate. The backend wipes its files on next launch.
      try {
        localStorage.clear();
      } catch {
        /* ignore */
      }
      await resetLocalData(); // backend writes the sentinel and restarts the app
    } catch (e) {
      toast.error(`Couldn't reset: ${errMsg(e)}`);
    }
  }
  return (
    <Section title="Danger zone">
      <div
        className="rounded-2xl border p-4"
        style={{
          borderColor: "color-mix(in srgb, var(--hive-danger) 35%, transparent)",
          background: "color-mix(in srgb, var(--hive-danger) 8%, transparent)",
        }}
      >
        <div className="font-medium">Reset local data</div>
        <p className="mt-1 text-sm opacity-60">
          Permanently deletes this device's chats, identity, keys, settings, and workspaces, then
          restarts Hive fresh. Anything synced to teammates or a relay is unaffected. This can't be
          undone.
        </p>
        <Button
          variant="danger"
          size="md"
          className="mt-3"
          onClick={() =>
            confirmThen(
              "Reset all local data? This deletes your chats, identity, and settings on this device and restarts Hive. This cannot be undone.",
              () => void reset(),
            )
          }
        >
          Reset local data…
        </Button>
      </div>
    </Section>
  );
}
