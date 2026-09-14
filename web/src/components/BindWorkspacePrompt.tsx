import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { pickWorkspaceFolder, setWorkspaceRoot } from "@/lib/ipc";
import { Button } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";

/// Empty-state shown in place of the code/diff canvas when the ACTIVE workspace
/// has no folder bound on this device (`settings.workspaceRoot` is empty). Code
/// features (editor, tree, terminal, diff, git) all read the workspace root, so
/// there is nothing meaningful to render until a folder is linked. Chat/review
/// keep working unbound — this only gates the folder-dependent canvases.
///
/// "Choose folder…" runs the same native picker as Folder & Git settings and
/// then `setWorkspaceRoot(path)`, which the backend binds to the active
/// workspace. We refetch settings (so `workspaceRoot` flips non-empty and this
/// prompt is replaced by the real canvas) plus the queries that key off the
/// root, mirroring FolderGitSection.
export function BindWorkspacePrompt() {
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);

  async function chooseFolder() {
    if (busy) return;
    setBusy(true);
    try {
      const path = await pickWorkspaceFolder();
      if (!path) return;
      await setWorkspaceRoot(path);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ["settings"] }),
        qc.invalidateQueries({ queryKey: ["diffs"] }),
        qc.invalidateQueries({ queryKey: ["runtimes"] }),
        qc.invalidateQueries({ queryKey: ["mcp"] }),
        qc.invalidateQueries({ queryKey: ["git-status"] }),
      ]);
    } catch (e) {
      toast.error(`Couldn't link a folder: ${errMsg(e)}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex h-full items-center justify-center p-8">
      <div
        className="flex max-w-md flex-col items-center gap-4 rounded-2xl border px-8 py-10 text-center"
        style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
      >
        <div className="text-base font-semibold tracking-tight">
          This workspace isn&rsquo;t linked to a folder on this device
        </div>
        <p className="text-sm opacity-70">
          Link a local folder to use the code editor, file tree, terminal, diff, and git for
          this workspace. Chat and review keep working without one — the binding stays on this
          device.
        </p>
        <Button variant="primary" size="md" disabled={busy} onClick={() => void chooseFolder()}>
          {busy ? "Choosing…" : "Choose folder…"}
        </Button>
      </div>
    </div>
  );
}
