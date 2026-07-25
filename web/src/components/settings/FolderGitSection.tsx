import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { getAppSettings, getGitStatus, openInEditor, setWorkspaceRoot } from "@/lib/ipc";
import { Section, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";

/// Folder & Git: the workspace root the Diff canvas + git operate on. Writes
/// through on blur (D18) — no Save button.
export function FolderGitSection() {
  const qc = useQueryClient();
  const settings = useQuery({ queryKey: ["settings"], queryFn: getAppSettings });
  // Git status is its own (lazy) query — it shells out to git, so it's only
  // fetched here where the pill is shown, not on every getAppSettings.
  const git = useQuery({ queryKey: ["git-status"], queryFn: getGitStatus });
  const [root, setRoot] = useState("");

  useEffect(() => {
    if (settings.data) setRoot(settings.data.workspaceRoot);
  }, [settings.data]);

  async function commitRoot() {
    const next = root.trim();
    if (next === (settings.data?.workspaceRoot ?? "")) return;
    try {
      await setWorkspaceRoot(next);
      qc.invalidateQueries({ queryKey: ["settings"] });
      qc.invalidateQueries({ queryKey: ["diffs"] });
      qc.invalidateQueries({ queryKey: ["runtimes"] });
      qc.invalidateQueries({ queryKey: ["mcp"] });
      qc.invalidateQueries({ queryKey: ["git-status"] });
    } catch (e) {
      toast.error(`Couldn't set workspace root: ${errMsg(e)}`);
    }
  }

  return (
    <Section title="Folder & Git">
      <label className="block text-sm opacity-70">Root path (for the Diff canvas + git)</label>
      <input
        value={root}
        onChange={(e) => setRoot(e.target.value)}
        onBlur={() => void commitRoot()}
        className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
        style={fieldStyle}
      />
      <div className="flex items-center gap-3 text-xs opacity-60">
        <span>
          git: {git.data?.branch ?? "—"}
          {git.data ? ` · ${git.data.dirtyCount} changed` : ""}
        </span>
        <button className="underline" onClick={() => openInEditor()}>
          Open in editor
        </button>
      </div>
    </Section>
  );
}
