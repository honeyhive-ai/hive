import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  getConnectionSettings,
  updateConnectionSettings,
  getAgentToolSettings,
  setAgentToolSettings,
  launchHealth,
  openFileAccessSettings,
  type AgentToolSettings,
  type ClaudePermissionMode,
} from "@/lib/ipc";
import { Button, Section, SelectField, Switch } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";

/// Permissions — the one page to audit what agents may reach: the Claude Code
/// permission mode, and the built-in file tools offered to HTTP model runtimes
/// (local models over Ollama / an OpenAI-compatible server). Every control is
/// backed by an enforcement path (D18 — no dead toggles) and writes through
/// immediately.
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

  // Granting unattended shell execution deserves at least the friction a data
  // reset gets — confirm before switching into bypass mode (a plain dropdown
  // pick was writing it through instantly).
  const setMode = (v: ClaudePermissionMode) => {
    // Granting unattended shell (runCommands or bypass) deserves the same friction
    // a data reset gets — the agent will run commands with no approval prompt.
    if (v === "bypassPermissions") {
      confirmThen(
        "Bypass all permissions? The claude agent will be able to edit files, run shell " +
          "commands, and use every tool in this workspace with no approval prompt. Only enable " +
          "this if you fully trust the agents and tasks running here.",
        () => save.mutate(v),
      );
      return;
    }
    if (v === "runCommands") {
      confirmThen(
        "Let the agent run commands? The claude agent will be able to edit files and run shell " +
          "commands (builds, tests, git) in this workspace with no approval prompt. Other tools " +
          "like web access stay gated.",
        () => save.mutate(v),
      );
      return;
    }
    save.mutate(v);
  };

  const agentTools = useQuery({ queryKey: ["agent-tool-settings"], queryFn: getAgentToolSettings });
  const tools: AgentToolSettings = agentTools.data ?? { enabled: true, writeInPlace: false, runCommand: false };
  const saveTools = useMutation({
    mutationFn: (next: AgentToolSettings) => setAgentToolSettings(next),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["agent-tool-settings"] }),
    onError: (e) => toast.error(`Couldn't save tool settings: ${errMsg(e)}`),
  });
  const setTools = (patch: Partial<AgentToolSettings>) => {
    const next = { ...tools, ...patch };
    // Unattended shell access and unreviewed edits get the same friction as
    // the Claude Code modes above.
    if (patch.runCommand) {
      confirmThen(
        "Let local models run shell commands? A model with tools on will be able to run any " +
          "command (builds, tests, git, network) in the turn's folder with no approval prompt. In an " +
          "isolated chat worktree its file changes still come back for review, but a command can " +
          "reach anything your user account can.",
        () => saveTools.mutate(next),
      );
      return;
    }
    if (patch.writeInPlace) {
      confirmThen(
        "Let local models edit files in place? Where a turn can't run in an isolated worktree " +
          "(the folder isn't a git repository), the model will change your files directly, with no " +
          "review step.",
        () => saveTools.mutate(next),
      );
      return;
    }
    saveTools.mutate(next);
  };

  const health = useQuery({ queryKey: ["launch-health"], queryFn: launchHealth });
  const openFileAccess = useMutation({
    mutationFn: () => openFileAccessSettings(),
    onError: (e) => toast.error(`Couldn't open settings: ${errMsg(e)}`),
  });

  return (
    <Section title="Permissions">
      {health.data?.macos && (
        <div
          className="space-y-2 rounded-2xl border p-3"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
        >
          <div className="text-sm font-medium">macOS file access</div>
          <p className="text-xs opacity-60">
            Agents run local CLIs (Claude Code, Codex) that read your workspace files. macOS
            attributes that access to Hive and may put up a folder-permission modal a turn silently
            waits on. Grant Hive <strong>Full Disk Access</strong> so agent turns don't stall on a
            hidden prompt.
          </p>
          {health.data?.translocated && (
            <p className="text-xs" style={{ color: "var(--hive-danger)" }}>
              Hive is running from a temporary location (App Translocation), so permission grants
              won't stick. Move Hive to <code>/Applications</code> and reopen it — or install via{" "}
              <code>brew install --cask honeyhive-ai/hive/hive</code>.
            </p>
          )}
          {health.data?.quarantined && !health.data?.translocated && (
            <p className="text-xs" style={{ color: "var(--hive-accent-warm)" }}>
              This copy still carries the “downloaded from the internet” quarantine flag, which can
              keep permissions from persisting. Installing via Homebrew avoids it.
            </p>
          )}
          <Button size="sm" onClick={() => openFileAccess.mutate()}>
            Open Full Disk Access settings
          </Button>
        </div>
      )}
      <p className="text-xs opacity-50">
        The setting below controls the <code>claude</code> CLI's permission mode (it runs headless,
        so it can't show an approval prompt). Other agents gate file access their own way: aider/pi
        via their flags, and API/MCP-backed agents via built-in-tool consent + per-tool MCP trust.
      </p>
      <label className="block text-xs opacity-60">Claude Code permission mode</label>
      <SelectField
        value={permissionMode}
        onChange={(v) => setMode(v as ClaudePermissionMode)}
        ariaLabel="Claude Code permission mode"
        disabled={!c}
      >
        <option value="default">Read-only — blocks file edits (no approval prompt)</option>
        <option value="acceptEdits">Edit files — write files, but not run commands</option>
        <option value="runCommands">Edit files &amp; run commands — build, test, git (recommended for coding)</option>
        <option value="bypassPermissions">Bypass all permissions — edits, commands, and every tool</option>
      </SelectField>
      {permissionMode === "default" ? (
        <p className="text-xs opacity-60">
          In read-only mode the <code>claude</code> agent's file writes are blocked. Choose{" "}
          <strong>Accept file edits</strong> to let it create/modify files in your workspace.
        </p>
      ) : (
        <p className="text-xs" style={{ color: "var(--hive-accent-warm)" }}>
          The <code>claude</code> agent can modify files
          {permissionMode === "runCommands" ? " and run shell commands (build, test, git)" : ""}
          {permissionMode === "bypassPermissions" ? " and run any command or tool" : ""} in your
          workspace without asking.
        </p>
      )}
      <div className="mt-4 space-y-2">
        <div className="text-sm font-medium">Local model file tools</div>
        <p className="text-xs opacity-60">
          Model runtimes reached over HTTP (Ollama, llama.cpp, OpenAI-compatible servers) with{" "}
          <strong>Supports tools</strong> on get Hive's built-in tools: <code>read_file</code>,{" "}
          <code>list_dir</code> and <code>search</code>, plus <code>edit_file</code> and{" "}
          <code>write_file</code> when the turn runs in the chat's isolated git worktree — their
          changes come back as a proposal in the Review pane, never straight into your folder.
        </p>
        <div className="flex items-center gap-2 text-sm opacity-75">
          <Switch
            on={tools.enabled}
            onChange={(v) => setTools({ enabled: v })}
            label="Built-in file tools"
            disabled={!agentTools.data}
          />
          <span>Offer built-in file tools to local models</span>
        </div>
        <div className="flex items-center gap-2 text-sm opacity-75">
          <Switch
            on={tools.writeInPlace}
            onChange={(v) => setTools({ writeInPlace: v })}
            label="Edit files in place"
            disabled={!agentTools.data || !tools.enabled}
          />
          <span>Allow edits outside an isolated worktree (folders that aren't git repositories)</span>
        </div>
        <div className="flex items-center gap-2 text-sm opacity-75">
          <Switch
            on={tools.runCommand}
            onChange={(v) => setTools({ runCommand: v })}
            label="Run commands"
            disabled={!agentTools.data || !tools.enabled}
          />
          <span>
            Allow <code>run_command</code> — shell commands for builds and tests (only where edits are
            allowed)
          </span>
        </div>
        {tools.enabled && tools.runCommand && (
          <p className="text-xs" style={{ color: "var(--hive-accent-warm)" }}>
            Local models can run any shell command in the turn's folder without asking. A command is
            not sandboxed: it can reach the network and anything your account can.
          </p>
        )}
        {tools.enabled && tools.writeInPlace && (
          <p className="text-xs" style={{ color: "var(--hive-accent-warm)" }}>
            Outside a git repository, local models edit your files directly with no review step.
          </p>
        )}
      </div>
    </Section>
  );
}
