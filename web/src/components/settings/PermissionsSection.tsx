import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  getConnectionSettings,
  updateConnectionSettings,
  type ClaudePermissionMode,
} from "@/lib/ipc";
import { Section, SelectField } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";

/// Permissions — the one page to audit what agents may reach. Today only the
/// Claude Code permission mode is backed by an enforcement path, so it is the
/// only control here (D18 — no dead toggles). It writes through immediately via
/// update_connection_settings, preserving the rest of the connection config.
export function PermissionsSection() {
  const qc = useQueryClient();
  const conn = useQuery({ queryKey: ["connection-settings"], queryFn: getConnectionSettings });
  const c = conn.data;
  const permissionMode: ClaudePermissionMode = c?.permissionMode ?? "default";

  const save = useMutation({
    mutationFn: (mode: ClaudePermissionMode) =>
      updateConnectionSettings({
        relayUrl: c?.relayUrl ?? "",
        room: c?.room ?? "",
        // null = leave every secret unchanged; we only touch the mode here.
        workspaceKey: null,
        apiKey: null,
        relayAccessToken: null,
        permissionMode: mode,
      }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["connection-settings"] }),
    onError: (e) => toast.error(`Couldn't set permission mode: ${errMsg(e)}`),
  });

  return (
    <Section title="Permissions">
      <p className="text-xs opacity-50">
        The setting below controls the <code>claude</code> CLI's permission mode (it runs headless,
        so it can't show an approval prompt). Other agents gate file access their own way: aider/pi
        via their flags, and API/MCP-backed agents via built-in-tool consent + per-tool MCP trust.
      </p>
      <label className="block text-xs opacity-60">Claude Code permission mode</label>
      <SelectField
        value={permissionMode}
        onChange={(v) => save.mutate(v as ClaudePermissionMode)}
        ariaLabel="Claude Code permission mode"
        disabled={!c}
      >
        <option value="default">Read-only — blocks file edits (no approval prompt)</option>
        <option value="acceptEdits">Accept file edits automatically (needed to write files)</option>
        <option value="bypassPermissions">Bypass all permissions (edits + shell commands)</option>
      </SelectField>
      {permissionMode === "default" ? (
        <p className="text-xs opacity-60">
          In read-only mode the <code>claude</code> agent's file writes are blocked. Choose{" "}
          <strong>Accept file edits</strong> to let it create/modify files in your workspace.
        </p>
      ) : (
        <p className="text-xs" style={{ color: "var(--hive-accent-warm)" }}>
          The <code>claude</code> agent can modify files
          {permissionMode === "bypassPermissions" ? " and run shell commands" : ""} in your
          workspace without asking.
        </p>
      )}
      <p className="mt-1 text-xs opacity-45">More permission controls arrive with backend support.</p>
    </Section>
  );
}
