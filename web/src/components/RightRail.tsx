import type { ReactNode } from "react";
import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  addAgent,
  addMember,
  importGithubTeams,
  inviteByHandle,
  removeMember,
  removeAndRevoke,
  setMemberRole,
  workspaceMembers,
  workspaceAddMember,
  workspaceRemoveMember,
  workspaceClaimMembership,
  addVault,
  getContextTelemetry,
  implementProposal,
  installMcpServer,
  installSkill,
  listAgents,
  listMcpServers,
  listMembers,
  presenceList,
  listProposals,
  listQueuedWork,
  listRuntimes,
  listSkills,
  listVaults,
  listWorkspaceHosts,
  previewVault,
  removeAgent,
  setAgentAvatar,
  setAgentHost,
  removeMcpServer,
  removeSkill,
  removeVault,
  setChatRuntime,
  setMcpEnabled,
  syncStatus,
  onChatStream,
  voteProposal,
  type RuntimeSummaryDto,
  type McpServerDto,
  type QueuedWorkDto,
  type WorkspaceHostDto,
  type WorkspaceAgentDto,
} from "@/lib/ipc";
import { LogsView } from "@/components/LogsView";
import { Avatar } from "@/components/Avatar";
import { fileToAvatarDataUrl } from "@/lib/avatar";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";
import {
  Button,
  IconButton,
  Card,
  Section,
  Switch,
  Field,
  SelectField,
  FormDisclosure,
  EmptyHint,
  Popover,
  PopoverHeader,
  PopoverItem,
  SELECT_TINT,
} from "@/components/ui";
import {
  IconWrench,
  IconHexagon,
  IconInbox,
  IconUsers,
  IconBook,
  IconSparkle,
  IconFlow,
  IconActivity,
  IconX,
  IconChevronDown,
} from "@/lib/icons";
import { WorkflowsPane } from "@/components/WorkflowsPane";

export type UtilityPane =
  | "tools"
  | "review"
  | "people"
  | "vaults"
  | "skills"
  | "workflows"
  | "activity"
  | "context";

export function RightRail({
  width,
  sessionId,
  pane,
  activeRuntimeId,
  onChangePane,
  onEditWorkflow,
}: {
  width: number;
  sessionId: string;
  pane: UtilityPane;
  activeRuntimeId: string;
  onChangePane: (pane: UtilityPane) => void;
  /// Opens the DAG editor in the main canvas (owned by App).
  onEditWorkflow: (def: import("@/lib/ipc").WorkflowDefinitionDto) => void;
}) {
  const panes: { id: UtilityPane; label: string; icon: ReactNode }[] = [
    { id: "tools", label: "Tools", icon: <IconWrench size={17} /> },
    { id: "context", label: "Context", icon: <IconHexagon size={17} /> },
    { id: "review", label: "Review", icon: <IconInbox size={17} /> },
    { id: "people", label: "People", icon: <IconUsers size={17} /> },
    { id: "vaults", label: "Vaults", icon: <IconBook size={17} /> },
    { id: "skills", label: "Skills", icon: <IconSparkle size={17} /> },
    { id: "workflows", label: "Workflows", icon: <IconFlow size={17} /> },
    { id: "activity", label: "Activity", icon: <IconActivity size={17} /> },
  ];

  return (
    <aside
      className="flex shrink-0 overflow-hidden border-l"
      style={{ width, borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
    >
      <div
        className="flex w-10 shrink-0 flex-col items-center gap-2 border-r py-3"
        style={{ borderColor: "var(--hive-line)", background: "var(--hive-overlay)" }}
      >
        {panes.map((item) => (
          <button
            key={item.id}
            onClick={() => onChangePane(item.id)}
            className="flex h-10 w-10 items-center justify-center rounded-2xl transition-colors"
            style={{
              background: pane === item.id ? "var(--hive-mist)" : "transparent",
              color: pane === item.id ? "var(--hive-accent-cool)" : "var(--hive-ink)",
              opacity: pane === item.id ? 1 : 0.72,
            }}
            title={item.label}
            aria-label={`${item.label} pane`}
            aria-pressed={pane === item.id}
          >
            {item.icon}
          </button>
        ))}
      </div>
      <div className="min-h-0 min-w-0 flex-1 overflow-hidden">
        {pane === "tools" && <ToolsPane sessionId={sessionId} activeRuntimeId={activeRuntimeId} />}
        {pane === "review" && <ReviewPane sessionId={sessionId} />}
        {pane === "people" && <PeoplePane sessionId={sessionId} />}
        {pane === "vaults" && <VaultsPane sessionId={sessionId} />}
        {pane === "skills" && <SkillsPane sessionId={sessionId} />}
        {pane === "workflows" && (
          <WorkflowsPane sessionId={sessionId} onEditWorkflow={onEditWorkflow} />
        )}
        {pane === "context" && <ContextPane sessionId={sessionId} activeRuntimeId={activeRuntimeId} />}
        {pane === "activity" && <ActivityPane />}
      </div>
    </aside>
  );
}

export function RailFrame({
  title,
  subtitle,
  children,
}: {
  title: string;
  subtitle: string;
  children: ReactNode;
}) {
  return (
    <div className="flex h-full flex-col">
      <div className="border-b px-4 py-3" style={{ borderColor: "var(--hive-line)" }}>
        <div className="text-lg font-semibold tracking-tight">{title}</div>
        <div className="mt-0.5 text-xs leading-5 opacity-60">{subtitle}</div>
      </div>
      <div className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto px-3 py-3">{children}</div>
    </div>
  );
}

// ─── Tools pane (design spec §6.3 / §6.4 / §7.5) ─────────────────────────────
// Scope is the top-level split: "In this chat" is READ-ONLY (what the agents
// here can reach, inherited from the workspace); "Workspace" is where those
// same records are actually edited. §11.1 made the workspace own configuration,
// so the chat side never renders an editable control — except the answering
// runtime, which is genuinely per-chat.

type ToolsScope = "chat" | "workspace";

function ToolsPane({
  sessionId,
  activeRuntimeId,
}: {
  sessionId: string;
  activeRuntimeId: string;
}) {
  const qc = useQueryClient();
  const runtimes = useQuery({ queryKey: ["runtimes"], queryFn: listRuntimes });
  const agents = useQuery({ queryKey: ["agents", sessionId], queryFn: () => listAgents(sessionId) });
  const mcp = useQuery({ queryKey: ["mcp"], queryFn: listMcpServers });
  const [scope, setScope] = useState<ToolsScope>("chat");
  const [agentName, setAgentName] = useState("");
  const [agentRole, setAgentRole] = useState("");
  const [agentRuntimeId, setAgentRuntimeId] = useState(activeRuntimeId);
  const [showAddAgent, setShowAddAgent] = useState(false);
  const [mcpSource, setMcpSource] = useState("");
  const [mcpError, setMcpError] = useState<string | null>(null);
  const [showInstallMcp, setShowInstallMcp] = useState(false);

  useEffect(() => {
    if (!agentRuntimeId) {
      setAgentRuntimeId(activeRuntimeId || runtimes.data?.[0]?.id || "");
    }
  }, [activeRuntimeId, agentRuntimeId, runtimes.data]);

  const addAgentMutation = useMutation({
    mutationFn: () => addAgent(sessionId, agentName.trim(), agentRuntimeId, agentRole.trim()),
    onSuccess: () => {
      setAgentName("");
      setAgentRole("");
      setShowAddAgent(false);
      qc.invalidateQueries({ queryKey: ["agents", sessionId] });
      toast.success("Agent added.");
    },
    onError: (e) => toast.error(`Couldn't add agent: ${errMsg(e)}`),
  });
  const removeAgentMutation = useMutation({
    mutationFn: (agentId: string) => removeAgent(sessionId, agentId),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["agents", sessionId] }),
    onError: (e) => toast.error(`Couldn't remove agent: ${errMsg(e)}`),
  });
  const installMcpMutation = useMutation({
    mutationFn: () => installMcpServer(mcpSource.trim()),
    onSuccess: () => {
      setMcpSource("");
      setMcpError(null);
      setShowInstallMcp(false);
      qc.invalidateQueries({ queryKey: ["mcp"] });
      toast.success("MCP server installed (disabled until you enable it).");
    },
    onError: (error) => {
      setMcpError(String(error));
      toast.error(`Couldn't install MCP server: ${errMsg(error)}`);
    },
  });
  const removeMcpMutation = useMutation({
    mutationFn: (serverId: string) => removeMcpServer(serverId),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["mcp"] }),
    onError: (e) => toast.error(`Couldn't remove MCP server: ${errMsg(e)}`),
  });

  const selectRuntime = async (id: string) => {
    try {
      await setChatRuntime(sessionId, id);
      qc.invalidateQueries({ queryKey: ["chat", sessionId] });
      qc.invalidateQueries({ queryKey: ["chats"] });
    } catch (e) {
      toast.error(`Couldn't switch runtime: ${errMsg(e)}`);
    }
  };
  const toggleMcp = async (server: McpServerDto, enabled: boolean) => {
    try {
      await setMcpEnabled(server.id, enabled);
      qc.invalidateQueries({ queryKey: ["mcp"] });
    } catch (e) {
      toast.error(`Couldn't ${enabled ? "enable" : "disable"} ${server.id}: ${errMsg(e)}`);
    }
  };

  const runtimeList = runtimes.data ?? [];
  const agentList = agents.data ?? [];
  const mcpList = mcp.data ?? [];
  const enabledMcp = mcpList.filter((s) => s.enabled);

  return (
    <RailFrame title="Tools" subtitle="What the agents here can reach, and where it's configured.">
      <ScopeToggle scope={scope} onScope={setScope} />

      {scope === "chat" ? (
        <>
          <div
            className="mb-4 flex flex-wrap items-center gap-x-2 gap-y-1 rounded-xl border px-3 py-2.5 text-[12px] leading-5"
            style={{
              borderColor: "color-mix(in srgb, var(--hive-accent-cool) 38%, transparent)",
              background: SELECT_TINT,
              color: "var(--hive-ink)",
            }}
          >
            <span>These are inherited from the workspace.</span>
            <button
              onClick={() => setScope("workspace")}
              className="font-medium underline underline-offset-2"
              style={{ color: "var(--hive-accent-cool)" }}
            >
              Edit in Workspace
            </button>
          </div>

          <Section title="Answering runtime">
            {runtimeList.length === 0 ? (
              <EmptyHint text="No runtimes configured yet." />
            ) : (
              runtimeList.map((rt) => (
                <RuntimeCard
                  key={rt.id}
                  runtime={rt}
                  active={rt.id === activeRuntimeId}
                  onSelect={() => selectRuntime(rt.id)}
                />
              ))
            )}
          </Section>

          <Section title="Agents available to @mention">
            {agentList.length === 0 ? (
              <EmptyHint text="No agents in this workspace yet." />
            ) : (
              agentList.map((agent) => (
                <Card key={agent.id} className="flex items-center gap-2.5 px-3 py-2.5">
                  <Avatar
                    name={agent.name}
                    url={agent.avatarUrl}
                    colorHex={agent.avatarColorHex}
                    kind="agent"
                    size={28}
                  />
                  <div className="min-w-0">
                    <div className="truncate text-[13px] font-medium">@{agent.name}</div>
                    <div className="truncate text-[11.5px]" style={{ color: "var(--hive-ink-soft)" }}>
                      {agent.runtimeId} · {agent.role || "agent"}
                    </div>
                  </div>
                </Card>
              ))
            )}
          </Section>

          <Section title="Tools reachable">
            {enabledMcp.length === 0 ? (
              <EmptyHint text="No MCP servers are enabled, so no external tools are reachable here." />
            ) : (
              enabledMcp.map((server) => (
                <McpServerCard key={server.id} server={server} editable={false} />
              ))
            )}
          </Section>
        </>
      ) : (
        <>
          <Section title="Agent roster">
            {agentList.length === 0 ? (
              <EmptyHint text="No workspace agents yet. Add one below." />
            ) : (
              agentList.map((agent) => (
                <AgentRosterRow
                  key={agent.id}
                  sessionId={sessionId}
                  agent={agent}
                  onRemove={() =>
                    confirmThen("Remove this agent?", () => removeAgentMutation.mutate(agent.id))
                  }
                  onChanged={() => qc.invalidateQueries({ queryKey: ["agents", sessionId] })}
                />
              ))
            )}
            <FormDisclosure
              label="Add an agent"
              open={showAddAgent}
              onToggle={() => setShowAddAgent((v) => !v)}
            >
              <Card className="space-y-2 p-3">
                <input
                  value={agentName}
                  onChange={(event) => setAgentName(event.target.value)}
                  placeholder="Agent name"
                  className="w-full rounded-xl border px-3 py-2 text-sm"
                  style={fieldStyle}
                />
                <input
                  value={agentRole}
                  onChange={(event) => setAgentRole(event.target.value)}
                  placeholder="Role (optional)"
                  className="w-full rounded-xl border px-3 py-2 text-sm"
                  style={fieldStyle}
                />
                <Field label="Agent runtime">
                  <SelectField value={agentRuntimeId} onChange={setAgentRuntimeId} ariaLabel="Agent runtime">
                    {runtimeList.map((runtime) => (
                      <option key={runtime.id} value={runtime.id}>
                        {runtimePickerLabel(runtime)}
                      </option>
                    ))}
                  </SelectField>
                </Field>
                <Button
                  variant="primary"
                  disabled={!agentName.trim() || !agentRuntimeId || addAgentMutation.isPending}
                  onClick={() => addAgentMutation.mutate()}
                >
                  Add agent
                </Button>
              </Card>
            </FormDisclosure>
          </Section>

          <Section title="Runtimes">
            {runtimeList.length === 0 ? (
              <EmptyHint text="No runtimes configured yet." />
            ) : (
              runtimeList.map((rt) => (
                <RuntimeCard key={rt.id} runtime={rt} active={rt.id === activeRuntimeId} />
              ))
            )}
            <p className="px-1 pt-1 text-[11.5px]" style={{ color: "var(--hive-ink-soft)" }}>
              Manage in Settings → Models.
            </p>
          </Section>

          <Section title="MCP servers">
            {mcpList.length === 0 ? (
              <EmptyHint text="No MCP servers configured." />
            ) : (
              mcpList.map((server) => (
                <McpServerCard
                  key={server.id}
                  server={server}
                  editable
                  onToggle={(v) => toggleMcp(server, v)}
                  onRemove={
                    server.isManaged
                      ? () =>
                          confirmThen("Remove this MCP server?", () =>
                            removeMcpMutation.mutate(server.id),
                          )
                      : undefined
                  }
                />
              ))
            )}
            <FormDisclosure
              label="Install a server"
              open={showInstallMcp}
              onToggle={() => setShowInstallMcp((v) => !v)}
            >
              <Card className="space-y-2 p-3">
                <p className="text-[11.5px]" style={{ color: "var(--hive-ink-soft)" }}>
                  Paste a manifest URL, GitHub blob URL, or owner/repo/path reference.
                </p>
                <input
                  value={mcpSource}
                  onChange={(event) => setMcpSource(event.target.value)}
                  placeholder="owner/repo/mcp.json or https://…"
                  className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
                  style={fieldStyle}
                />
                {mcpError && (
                  <div className="text-xs" style={{ color: "var(--hive-danger)" }}>
                    {mcpError}
                  </div>
                )}
                <Button
                  variant="primary"
                  disabled={!mcpSource.trim() || installMcpMutation.isPending}
                  onClick={() => installMcpMutation.mutate()}
                >
                  Install
                </Button>
              </Card>
            </FormDisclosure>
          </Section>
        </>
      )}
    </RailFrame>
  );
}

/// The top-level scope split for the Tools pane (§6.3).
function ScopeToggle({ scope, onScope }: { scope: ToolsScope; onScope: (s: ToolsScope) => void }) {
  const opts: { id: ToolsScope; label: string }[] = [
    { id: "chat", label: "In this chat" },
    { id: "workspace", label: "Workspace" },
  ];
  return (
    <div
      className="mb-4 flex rounded-xl border p-0.5"
      style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
    >
      {opts.map((o) => {
        const active = scope === o.id;
        return (
          <button
            key={o.id}
            onClick={() => onScope(o.id)}
            aria-pressed={active}
            className="flex-1 rounded-lg px-3 py-1.5 text-[13px] font-medium transition-colors"
            style={
              active
                ? {
                    background: "var(--hive-panel)",
                    color: "var(--hive-ink)",
                    boxShadow: "0 1px 2px color-mix(in srgb, var(--hive-ink) 14%, transparent)",
                  }
                : { color: "var(--hive-ink-soft)" }
            }
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

/// One MCP server card (§6.4). Header carries the disclosure chevron, id,
/// transport detail, an enabled/inert label and a Switch (editable) or a plain
/// state pill (read-only). The backend exposes no per-tool policy or tool list
/// (McpServerDto = {id,transport,detail,enabled,isManaged}), so there are no
/// per-tool rows — only an honest note in the expanded body.
function McpServerCard({
  server,
  editable,
  onToggle,
  onRemove,
}: {
  server: McpServerDto;
  editable: boolean;
  onToggle?: (enabled: boolean) => void;
  onRemove?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const disabled = !server.enabled;
  return (
    <Card className="px-3 py-2.5">
      <div className="flex items-center gap-2">
        <button
          onClick={() => setOpen((o) => !o)}
          aria-label={open ? `Collapse ${server.id}` : `Expand ${server.id}`}
          title={open ? "Collapse" : "Expand"}
          className="grid h-6 w-6 shrink-0 place-items-center rounded-md hover:bg-[color:var(--hive-overlay)]"
          style={{ color: "var(--hive-ink-soft)" }}
        >
          <span
            className="transition-transform"
            style={{ transform: open ? "rotate(0deg)" : "rotate(-90deg)" }}
          >
            <IconChevronDown size={14} />
          </span>
        </button>
        <div className="min-w-0 flex-1" style={{ opacity: disabled ? 0.6 : 1 }}>
          <div className="flex items-center gap-2">
            <span className="truncate text-[13px] font-medium">{server.id}</span>
            <span
              className="shrink-0 text-[11px] uppercase tracking-[0.08em]"
              style={{ color: disabled ? "var(--hive-ink-soft)" : "var(--hive-success)" }}
            >
              {disabled ? "inert" : "enabled"}
            </span>
          </div>
          <div className="truncate text-[11.5px]" style={{ color: "var(--hive-ink-soft)" }}>
            [{server.transport}] {server.detail}
          </div>
        </div>
        {editable ? (
          <Switch
            on={server.enabled}
            onChange={(v) => onToggle?.(v)}
            label={`Enable ${server.id}`}
          />
        ) : (
          <span
            className="shrink-0 rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase"
            style={{ background: "var(--hive-mist)", color: "var(--hive-ink-soft)" }}
          >
            {disabled ? "off" : "on"}
          </span>
        )}
      </div>
      {open && (
        <div
          className="mt-2.5 border-t pt-2.5 text-[11.5px] leading-5"
          style={{ borderColor: "var(--hive-line)", color: "var(--hive-ink-soft)" }}
        >
          {disabled
            ? "Disabled — inert gate: this server is never launched or connected."
            : "Per-tool permissions arrive with backend support."}
          {editable && onRemove && (
            <div className="mt-2.5">
              <Button variant="danger" onClick={onRemove}>
                Remove server
              </Button>
            </div>
          )}
        </div>
      )}
    </Card>
  );
}

/// Editable agent row (Workspace scope): avatar upload + remove.
function AgentRosterRow({
  sessionId,
  agent,
  onRemove,
  onChanged,
}: {
  sessionId: string;
  agent: import("@/lib/ipc").WorkspaceAgentDto;
  onRemove: () => void;
  onChanged: () => void;
}) {
  return (
    <Card className="px-3 py-3">
      <div className="flex items-start justify-between gap-3">
        <div className="flex min-w-0 items-start gap-2.5">
          <label className="group/av relative cursor-pointer" title="Upload an avatar for this agent">
            <Avatar
              name={agent.name}
              url={agent.avatarUrl}
              colorHex={agent.avatarColorHex}
              kind="agent"
              size={32}
            />
            <span
              className="absolute inset-0 flex items-center justify-center rounded-[30%] text-[9px] font-semibold opacity-0 transition-opacity group-hover/av:opacity-100"
              style={{
                background: "color-mix(in srgb, var(--hive-ink) 55%, transparent)",
                color: "var(--hive-canvas)",
              }}
            >
              Edit
            </span>
            <input
              type="file"
              accept="image/*"
              className="hidden"
              onChange={async (e) => {
                const file = e.target.files?.[0];
                e.target.value = "";
                if (!file) return;
                try {
                  const url = await fileToAvatarDataUrl(file);
                  await setAgentAvatar(sessionId, agent.id, url, null);
                  onChanged();
                  toast.success("Agent avatar updated.");
                } catch (err) {
                  toast.error(`Couldn't set avatar: ${errMsg(err)}`);
                }
              }}
            />
          </label>
          <div className="min-w-0">
            <div className="font-medium">@{agent.name}</div>
            <div className="mt-1 text-xs opacity-60">
              {agent.runtimeId} · {agent.role || "agent"}
            </div>
            {agent.avatarUrl && (
              <button
                onClick={async () => {
                  try {
                    await setAgentAvatar(sessionId, agent.id, null, null);
                    onChanged();
                  } catch (err) {
                    toast.error(`Couldn't clear avatar: ${errMsg(err)}`);
                  }
                }}
                className="mt-1 text-xs opacity-60 hover:opacity-100"
              >
                Remove avatar
              </button>
            )}
          </div>
        </div>
        <button
          onClick={onRemove}
          className="text-xs hover:opacity-80"
          style={{ color: "var(--hive-danger)" }}
        >
          Remove
        </button>
      </div>
    </Card>
  );
}

function RuntimeCard({
  runtime,
  active,
  onSelect,
}: {
  runtime: RuntimeSummaryDto;
  active: boolean;
  onSelect?: () => void;
}) {
  const style = {
    borderColor: active ? "var(--hive-accent-cool)" : "var(--hive-line)",
    background: active ? SELECT_TINT : "var(--hive-mist)",
  } as const;
  const inner = (
    <div className="flex items-center justify-between gap-3">
      <div className="min-w-0">
        <div className="truncate text-base font-semibold">{runtime.label}</div>
        <div className="mt-1 truncate text-xs opacity-55">
          {runtime.location} · {runtime.provider}
          {runtime.endpoint ? ` · ${runtime.endpoint}` : ""}
        </div>
      </div>
      <div className="flex items-center gap-2">
        {runtime.isManaged && (
          <span
            className="rounded-full px-2 py-1 text-[10px] font-semibold uppercase opacity-70"
            style={{ background: "var(--hive-mist)" }}
          >
            Added
          </span>
        )}
        {active && (
          <span
            className="rounded-full px-2 py-1 text-xs font-semibold"
            style={{
              background: "color-mix(in srgb, var(--hive-success) 18%, transparent)",
              color: "var(--hive-success)",
            }}
          >
            Active
          </span>
        )}
      </div>
    </div>
  );
  if (!onSelect) {
    return (
      <div className="w-full rounded-2xl border px-3.5 py-3.5 text-left" style={style}>
        {inner}
      </div>
    );
  }
  return (
    <button
      onClick={onSelect}
      className="w-full rounded-2xl border px-3.5 py-3.5 text-left transition-colors hover:bg-[color:var(--hive-overlay)]"
      style={style}
    >
      {inner}
    </button>
  );
}

// ─── Queued work (spec §12.5) ────────────────────────────────────────────────
// Unanswered agent mentions in the workspace, tagged with each agent's host
// status. The desktop counterpart of the CLI's `hive queue`: an agent bound to
// an offline host (or a member's device) has its mentions *queued* until the
// host returns — so we surface a one-click "run on worker" handoff
// (`hive set-agent-host`) to move it to an always-on worker instead.

/// Status pill for a queued item's host: online = success, offline = warn
/// ("OFFLINE → queued"), device/unknown = neutral. All tints route through
/// --hive-* tokens (R1).
function HostStatusBadge({ item }: { item: QueuedWorkDto }) {
  const style: { bg: string; fg: string; label: string } = (() => {
    switch (item.hostStatus) {
      case "online":
        return {
          bg: "color-mix(in srgb, var(--hive-success) 18%, transparent)",
          fg: "var(--hive-success)",
          label: item.hostLabel ? `online · ${item.hostLabel}` : "online",
        };
      case "offline":
        return {
          bg: "color-mix(in srgb, var(--hive-accent-warm) 18%, transparent)",
          fg: "var(--hive-accent-warm)",
          label: "OFFLINE → queued",
        };
      case "device":
        return {
          bg: "var(--hive-mist)",
          fg: "var(--hive-ink-soft)",
          label: "device",
        };
      default:
        return {
          bg: "var(--hive-mist)",
          fg: "var(--hive-ink-soft)",
          label: "unknown host",
        };
    }
  })();
  return (
    <span
      className="shrink-0 rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase tracking-[0.06em]"
      style={{ background: style.bg, color: style.fg }}
    >
      {style.label}
    </span>
  );
}

/// "Run on worker" affordance: a small menu of the online workers. Selecting
/// one reassigns the agent's host and refreshes the queue + roster.
function RunOnWorkerMenu({
  workers,
  onReassign,
}: {
  workers: WorkspaceHostDto[];
  onReassign: (hostId: string) => void;
}) {
  const anchorRef = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        ref={anchorRef}
        onClick={() => setOpen((v) => !v)}
        className="shrink-0 rounded-lg border px-2 py-1 text-[11.5px] font-medium transition-colors hover:bg-[color:var(--hive-overlay)]"
        style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)", color: "var(--hive-ink)" }}
      >
        Run on worker
      </button>
      {open && (
        <Popover anchorRef={anchorRef} onDismiss={() => setOpen(false)} align="right" minWidth={200}>
          <PopoverHeader>Online workers</PopoverHeader>
          {workers.length === 0 ? (
            <div className="px-2 py-1.5 text-[12px]" style={{ color: "var(--hive-ink-soft)" }}>
              No online workers.
            </div>
          ) : (
            workers.map((w) => (
              <PopoverItem
                key={w.id}
                label={w.label || w.id}
                onSelect={() => {
                  setOpen(false);
                  onReassign(w.id);
                }}
              />
            ))
          )}
        </Popover>
      )}
    </>
  );
}

function QueuedWorkRow({
  item,
  workers,
  onReassign,
}: {
  item: QueuedWorkDto;
  workers: WorkspaceHostDto[];
  onReassign: (agentId: string, hostId: string) => void;
}) {
  const needsHandoff = item.hostStatus === "offline" || item.hostStatus === "device";
  return (
    <Card className="px-3 py-2.5">
      <div className="flex items-center gap-2">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-1.5">
            <span className="truncate text-[13px] font-medium">@{item.agentName}</span>
            <HostStatusBadge item={item} />
          </div>
          <div className="truncate text-[11.5px]" style={{ color: "var(--hive-ink-soft)" }}>
            {item.chatTitle || "Untitled chat"}
          </div>
        </div>
        {needsHandoff && (
          <RunOnWorkerMenu
            workers={workers}
            onReassign={(hostId) => onReassign(item.agentId, hostId)}
          />
        )}
      </div>
    </Card>
  );
}

/// The "Queued work" section: unanswered agent mentions across the workspace,
/// each with its host status + a "run on worker" handoff for offline/device
/// agents.
function QueuedWorkSection() {
  const qc = useQueryClient();
  const queued = useQuery({ queryKey: ["queued-work"], queryFn: listQueuedWork });
  const hosts = useQuery({ queryKey: ["workspace-hosts"], queryFn: listWorkspaceHosts });
  const workers = (hosts.data ?? []).filter((h) => h.kind === "worker" && h.online);

  const reassign = useMutation({
    mutationFn: (v: { agentId: string; hostId: string }) => setAgentHost(v.agentId, v.hostId),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["queued-work"] });
      qc.invalidateQueries({ queryKey: ["agents"] });
      toast.success("Moved to worker — it'll run there.");
    },
    onError: (e) => toast.error(`Couldn't reassign: ${errMsg(e)}`),
  });

  const items = queued.data ?? [];
  return (
    <Section title="Queued work">
      {items.length === 0 ? (
        <EmptyHint text="Nothing queued." />
      ) : (
        items.map((item) => (
          <QueuedWorkRow
            key={`${item.sessionId}:${item.agentId}`}
            item={item}
            workers={workers}
            onReassign={(agentId, hostId) => reassign.mutate({ agentId, hostId })}
          />
        ))
      )}
    </Section>
  );
}

function ReviewPane({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const proposals = useQuery({
    queryKey: ["proposals", sessionId],
    queryFn: () => listProposals(sessionId),
  });

  return (
    <RailFrame title="Review" subtitle="Pending decisions and proposal quorum for this chat.">
      <QueuedWorkSection />
      <Section title="Pending Proposals">
        {(proposals.data ?? []).map((proposal) => (
          <Card key={proposal.id} className="px-4 py-4">
            <div className="flex items-center justify-between gap-3">
              <div className="font-semibold">{proposal.title}</div>
              <div className="text-xs uppercase tracking-[0.16em] opacity-55">{proposal.status}</div>
            </div>
            {proposal.body && <p className="mt-2 text-sm leading-6 opacity-75">{proposal.body}</p>}
            <div className="mt-3 text-xs opacity-60">
              {proposal.qualifyingApprovals}/{proposal.requiredApprovals} approvals
            </div>
            <div className="mt-3 flex gap-2">
              <button
                className="rounded-xl px-3 py-2 text-sm font-medium"
                style={{
                  background: "color-mix(in srgb, var(--hive-success) 20%, transparent)",
                  color: "var(--hive-success)",
                }}
                onClick={async () => {
                  try {
                    await voteProposal(sessionId, proposal.id, true);
                    qc.invalidateQueries({ queryKey: ["proposals", sessionId] });
                  } catch (e) {
                    toast.error(`Couldn't approve: ${errMsg(e)}`);
                  }
                }}
              >
                Approve
              </button>
              <button
                className="rounded-xl px-3 py-2 text-sm font-medium"
                style={{
                  background: "color-mix(in srgb, var(--hive-danger) 18%, transparent)",
                  color: "var(--hive-danger)",
                }}
                onClick={async () => {
                  try {
                    await voteProposal(sessionId, proposal.id, false);
                    qc.invalidateQueries({ queryKey: ["proposals", sessionId] });
                  } catch (e) {
                    toast.error(`Couldn't reject: ${errMsg(e)}`);
                  }
                }}
              >
                Reject
              </button>
              {/* Agreement gate: an approved proposal only runs when a human
                  explicitly implements it; the agent then carries it out.
                  (Ported from the retired standalone ReviewView.) */}
              {proposal.quorumMet && proposal.status !== "applied" && (
                <Button
                  variant="primary"
                  className="ml-auto"
                  onClick={async () => {
                    try {
                      await implementProposal(sessionId, proposal.id);
                      qc.invalidateQueries({ queryKey: ["proposals", sessionId] });
                      qc.invalidateQueries({ queryKey: ["chat", sessionId] });
                      toast.success("Sent to the agent to implement.");
                    } catch (e) {
                      toast.error(`Couldn't implement: ${errMsg(e)}`);
                    }
                  }}
                >
                  Implement
                </Button>
              )}
              {proposal.status === "applied" && (
                <span className="ml-auto self-center text-xs opacity-60">Implemented</span>
              )}
            </div>
          </Card>
        ))}
        {(proposals.data ?? []).length === 0 && (
          <EmptyHint text="No proposals are waiting for review." />
        )}
      </Section>
    </RailFrame>
  );
}

const MEMBER_ROLES = ["owner", "admin", "contributor", "viewer"];

function PeoplePane({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const members = useQuery({ queryKey: ["members", sessionId], queryFn: () => listMembers(sessionId) });
  // Presence only exists with a relay; skip the poll entirely when solo so we
  // don't burn IPC round-trips (helps the Windows shell stay responsive).
  const sync = useQuery({ queryKey: ["sync-status"], queryFn: syncStatus });
  const presence = useQuery({
    queryKey: ["presence", sessionId],
    queryFn: presenceList,
    enabled: Boolean(sync.data?.relayConfigured),
    refetchInterval: sync.data?.relayConfigured ? 5000 : false,
  });
  const onlineActors = new Set((presence.data ?? []).map((p) => p.actorId));
  // Registered devices/workers for this workspace. Shares the ["workspace-hosts"]
  // cache with the queue's "run on worker" menu. Poll on the same cadence as
  // member presence so online/offline + "last seen" stay live (only with a
  // relay — solo has no remote hosts to refresh).
  const hosts = useQuery({
    queryKey: ["workspace-hosts"],
    queryFn: listWorkspaceHosts,
    refetchInterval: sync.data?.relayConfigured ? 5000 : false,
  });

  // Show `#index` only on names that actually collide; matchable as `@Name#N`.
  const nameCounts = new Map<string, number>();
  for (const m of members.data ?? []) {
    nameCounts.set(m.displayName, (nameCounts.get(m.displayName) ?? 0) + 1);
  }

  const [name, setName] = useState("");
  const [role, setRole] = useState("contributor");
  const [title, setTitle] = useState("");
  const [handle, setHandle] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [showInvite, setShowInvite] = useState(false);
  const [showAdd, setShowAdd] = useState(false);
  const [showImport, setShowImport] = useState(false);
  const [showSrvAdd, setShowSrvAdd] = useState(false);
  const refresh = () => qc.invalidateQueries({ queryKey: ["members", sessionId] });

  const setRoleMutation = useMutation({
    mutationFn: (v: { id: string; role: string }) => setMemberRole(sessionId, v.id, v.role),
    onSuccess: () => {
      setError(null);
      refresh();
    },
    onError: (e) => setError(String(e)),
  });
  const removeMutation = useMutation({
    mutationFn: (id: string) => removeMember(sessionId, id),
    onSuccess: () => {
      setError(null);
      refresh();
    },
    onError: (e) => setError(String(e)),
  });
  // Owner/admin "kick a bad actor": remove + rotate the workspace key so they
  // lose access to new messages.
  const revokeMutation = useMutation({
    mutationFn: (id: string) => removeAndRevoke(sessionId, id),
    onSuccess: (r) => {
      setError(null);
      refresh();
      toast.success(
        r.rotated
          ? `Removed and revoked — key rotated to ${r.recipients} member${r.recipients === 1 ? "" : "s"}.`
          : "Removed. (No relay configured, so the key wasn't rotated.)",
      );
    },
    onError: (e) => toast.error(`Couldn't revoke: ${errMsg(e)}`),
  });
  const inviteMutation = useMutation({
    mutationFn: () => inviteByHandle(sessionId, handle.trim()),
    onSuccess: (r) => {
      setHandle("");
      setError(null);
      setShowInvite(false);
      refresh();
      toast.success(
        r.sealed
          ? `Invited @${r.login} — workspace key sealed to ${r.devices} device${r.devices === 1 ? "" : "s"}.`
          : `Invited @${r.login}.`,
      );
    },
    onError: (e) => toast.error(`Couldn't invite: ${errMsg(e)}`),
  });
  const addMutation = useMutation({
    mutationFn: () => addMember(sessionId, name.trim(), role, title.trim()),
    onSuccess: () => {
      setName("");
      setTitle("");
      setError(null);
      setShowAdd(false);
      refresh();
    },
    onError: (e) => setError(String(e)),
  });
  const [org, setOrg] = useState("");
  const importTeams = useMutation({
    mutationFn: () => importGithubTeams(sessionId, org.trim()),
    onSuccess: (n) => {
      const imported = org.trim();
      setOrg("");
      setShowImport(false);
      refresh();
      toast.success(`Imported ${n} member${n === 1 ? "" : "s"} from @${imported}.`);
    },
    onError: (e) => toast.error(`Import failed: ${errMsg(e)}`),
  });

  // Server-side membership (enforced by paid/enterprise relays). Distinct from
  // the roster above: this is who the *relay* will accept writes from.
  const relayOn = Boolean(sync.data?.relayConfigured);
  const srvMembers = useQuery({
    queryKey: ["server-members"],
    queryFn: workspaceMembers,
    enabled: relayOn,
  });
  const [srvHandle, setSrvHandle] = useState("");
  const [srvRole, setSrvRole] = useState("contributor");
  const refreshSrv = () => qc.invalidateQueries({ queryKey: ["server-members"] });
  const claimMutation = useMutation({
    mutationFn: workspaceClaimMembership,
    onSuccess: (claimed) => {
      refreshSrv();
      toast.success(
        claimed
          ? "Membership enabled — you're the owner. The relay now enforces who can write."
          : "This relay doesn't enforce membership (open/self-host), or it's already enabled.",
      );
    },
    onError: (e) => toast.error(`Couldn't claim: ${errMsg(e)}`),
  });
  const srvAddMutation = useMutation({
    mutationFn: () => workspaceAddMember(srvHandle.trim(), srvRole),
    onSuccess: () => {
      setSrvHandle("");
      setShowSrvAdd(false);
      refreshSrv();
      toast.success("Member updated on the relay.");
    },
    onError: (e) => toast.error(`Couldn't add member: ${errMsg(e)}`),
  });
  const srvRemoveMutation = useMutation({
    mutationFn: (account: string) => workspaceRemoveMember(account),
    onSuccess: () => {
      refreshSrv();
      toast.success("Removed on the relay. Rotate the key too (Remove & revoke above) to cut read access.");
    },
    onError: (e) => toast.error(`Couldn't remove: ${errMsg(e)}`),
  });

  return (
    <RailFrame title="People" subtitle="Workspace members and governance roles for this chat.">
      <Section title="Members">
        {error && (
          <div className="text-xs" style={{ color: "var(--hive-danger)" }}>
            {error}
          </div>
        )}
        {(members.data ?? []).map((member) => {
          const online = onlineActors.has(member.actorId);
          const dup = (nameCounts.get(member.displayName) ?? 0) > 1 && member.index > 0;
          return (
            <Card key={member.id} className="px-3 py-3">
              <div className="flex items-center gap-2 font-medium">
                <span className="relative shrink-0">
                  <Avatar name={member.displayName} url={member.avatarUrl} kind="human" size={26} />
                  <span
                    className="absolute -bottom-0.5 -right-0.5 h-2.5 w-2.5 rounded-full"
                    style={{
                      background: online ? "var(--hive-success)" : "var(--hive-line)",
                      boxShadow: "0 0 0 2px var(--hive-panel)",
                    }}
                    title={sync.data?.relayConfigured ? (online ? "Online" : "Offline") : "Presence needs a relay"}
                  />
                </span>
                {member.displayName}
                {dup && (
                  <span
                    className="text-xs opacity-50"
                    title={`Mention precisely with @${member.displayName}#${member.index}`}
                  >
                    #{member.index}
                  </span>
                )}
                {member.title && <span className="text-xs opacity-50">· {member.title}</span>}
              </div>
              <div className="mt-2 flex items-center gap-2">
                <SubtleSelectField
                  label="Role"
                  value={member.role}
                  onChange={(r) => setRoleMutation.mutate({ id: member.id, role: r })}
                >
                  {MEMBER_ROLES.map((r) => (
                    <option key={r} value={r}>
                      {r}
                    </option>
                  ))}
                </SubtleSelectField>
                <button
                  className="shrink-0 text-xs opacity-70 hover:opacity-100"
                  onClick={() =>
                    confirmThen("Remove this member from the roster?", () =>
                      removeMutation.mutate(member.id),
                    )
                  }
                  title="Remove from the member list only"
                >
                  Remove
                </button>
                <button
                  className="shrink-0 text-xs font-medium hover:opacity-80"
                  style={{ color: "var(--hive-danger)" }}
                  onClick={() =>
                    confirmThen(
                      `Remove ${member.displayName} and revoke their access? The workspace key will be rotated so they can't read new messages.`,
                      () => revokeMutation.mutate(member.id),
                    )
                  }
                  title="Remove and rotate the key so they lose access (for a bad actor)"
                >
                  Remove &amp; revoke
                </button>
              </div>
            </Card>
          );
        })}
        {(members.data ?? []).length === 0 && (
          <EmptyHint text="No extra members have been added yet." />
        )}

        <FormDisclosure label="Invite by GitHub handle" open={showInvite} onToggle={() => setShowInvite((v) => !v)}>
          <Card className="space-y-2 p-3">
            <p className="text-[11.5px]" style={{ color: "var(--hive-ink-soft)" }}>
              Adds the person and seals the workspace key to all their devices. They must have signed
              in to Hive with GitHub once. Needs a relay + your GitHub sign-in.
            </p>
            <input
              value={handle}
              onChange={(e) => setHandle(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handle.trim() && inviteMutation.mutate()}
              placeholder="@github-handle"
              className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
              style={fieldStyle}
            />
            <Button variant="primary" disabled={!handle.trim() || inviteMutation.isPending} onClick={() => inviteMutation.mutate()}>
              {inviteMutation.isPending ? "…" : "Invite"}
            </Button>
          </Card>
        </FormDisclosure>

        <FormDisclosure label="Add member" open={showAdd} onToggle={() => setShowAdd((v) => !v)}>
          <Card className="space-y-2 p-3">
            <p className="text-[11.5px]" style={{ color: "var(--hive-ink-soft)" }}>
              Roles gate actions: owner &gt; admin &gt; contributor &gt; viewer. The last owner is protected.
            </p>
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Display name"
              className="w-full rounded-xl border px-3 py-2 text-sm"
              style={fieldStyle}
            />
            <input
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="Title (optional)"
              className="w-full rounded-xl border px-3 py-2 text-sm"
              style={fieldStyle}
            />
            <SubtleSelectField label="Role" value={role} onChange={setRole}>
              {MEMBER_ROLES.map((r) => (
                <option key={r} value={r}>
                  {r}
                </option>
              ))}
            </SubtleSelectField>
            <Button variant="primary" disabled={!name.trim() || addMutation.isPending} onClick={() => addMutation.mutate()}>
              Add
            </Button>
          </Card>
        </FormDisclosure>

        <FormDisclosure label="Import GitHub org" open={showImport} onToggle={() => setShowImport((v) => !v)}>
          <Card className="space-y-2 p-3">
            <p className="text-[11.5px]" style={{ color: "var(--hive-ink-soft)" }}>
              Pull an org's Teams into the roster, mapping team membership to roles (highest wins).
              Needs GitHub sign-in with read:org.
            </p>
            <input
              value={org}
              onChange={(e) => setOrg(e.target.value)}
              placeholder="github-org-slug"
              className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
              style={fieldStyle}
            />
            <Button variant="primary" disabled={!org.trim() || importTeams.isPending} onClick={() => importTeams.mutate()}>
              {importTeams.isPending ? "Importing…" : "Import"}
            </Button>
          </Card>
        </FormDisclosure>
      </Section>

      <Section title="Team members (server-enforced)">
        {!relayOn ? (
          <EmptyHint text="Connect a relay (Settings → Team sync) to use server-enforced membership." />
        ) : (
          <>
            <p className="text-xs opacity-60">
              Who the relay accepts writes from. Requires a membership-enforcing (paid/enterprise)
              relay; on an open relay this stays empty and everyone can write.
            </p>
            {(srvMembers.data ?? []).map((m) => (
              <Card key={m.account} className="flex items-center justify-between px-3 py-2">
                <div className="min-w-0">
                  <div className="truncate font-medium">@{m.login}</div>
                  <div className="text-xs opacity-50">
                    {m.role} · {m.account}
                  </div>
                </div>
                <button
                  className="shrink-0 text-xs font-medium hover:opacity-80"
                  style={{ color: "var(--hive-danger)" }}
                  onClick={() =>
                    confirmThen(`Remove @${m.login} from this workspace on the relay?`, () =>
                      srvRemoveMutation.mutate(m.account),
                    )
                  }
                  title="Stop the relay from accepting their writes (pair with Remove & revoke to cut reads)"
                >
                  Remove
                </button>
              </Card>
            ))}
            {(srvMembers.data ?? []).length === 0 && (
              <Card className="space-y-2 p-3">
                <div className="text-sm font-semibold">Enable member controls</div>
                <p className="text-[11.5px]" style={{ color: "var(--hive-ink-soft)" }}>
                  Claim this workspace on the relay so it enforces who can write — you become the
                  owner. No-op on an open relay.
                </p>
                <Button variant="primary" disabled={claimMutation.isPending} onClick={() => claimMutation.mutate()}>
                  {claimMutation.isPending ? "…" : "Claim ownership"}
                </Button>
              </Card>
            )}
            <FormDisclosure label="Add or set member by handle" open={showSrvAdd} onToggle={() => setShowSrvAdd((v) => !v)}>
              <Card className="space-y-2 p-3">
                <p className="text-[11.5px]" style={{ color: "var(--hive-ink-soft)" }}>
                  Admin+ only. Looks the handle up in the directory and sets their role on the relay.
                </p>
                <input
                  value={srvHandle}
                  onChange={(e) => setSrvHandle(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && srvHandle.trim() && srvAddMutation.mutate()}
                  placeholder="@github-handle"
                  className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
                  style={fieldStyle}
                />
                <SubtleSelectField label="Role" value={srvRole} onChange={setSrvRole}>
                  {MEMBER_ROLES.map((r) => (
                    <option key={r} value={r}>
                      {r}
                    </option>
                  ))}
                </SubtleSelectField>
                <Button variant="primary" disabled={!srvHandle.trim() || srvAddMutation.isPending} onClick={() => srvAddMutation.mutate()}>
                  {srvAddMutation.isPending ? "…" : "Set"}
                </Button>
              </Card>
            </FormDisclosure>
          </>
        )}
      </Section>

      <Section title="Workers & hosts">
        {(hosts.data ?? []).length === 0 ? (
          <EmptyHint text="No hosts registered." />
        ) : (
          (hosts.data ?? []).map((host) => <HostRow key={host.id} host={host} />)
        )}
      </Section>
    </RailFrame>
  );
}

/// A single registered device/worker: label, a kind chip, an online/offline
/// status dot, and a relative "last seen".
function HostRow({ host }: { host: WorkspaceHostDto }) {
  return (
    <Card className="flex items-center justify-between gap-2 px-3 py-2">
      <div className="flex min-w-0 items-center gap-2">
        <span
          className="h-2.5 w-2.5 shrink-0 rounded-full"
          style={{ background: host.online ? "var(--hive-success)" : "var(--hive-line)" }}
          title={host.online ? "Online" : "Offline"}
        />
        <span className="truncate font-medium">{host.label || "Unnamed host"}</span>
        <span
          className="shrink-0 rounded-full px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wider"
          style={{
            background: "color-mix(in srgb, var(--hive-accent-cool) 15%, transparent)",
            color: "var(--hive-accent-cool)",
          }}
        >
          {host.kind}
        </span>
      </div>
      <span className="shrink-0 text-[11px]" style={{ color: "var(--hive-ink-soft)" }}>
        {host.online ? "online" : relativeAgo(host.lastSeen)}
      </span>
    </Card>
  );
}

/// Coarse "3m ago" / "2h ago" from an RFC 3339 timestamp. Empty/unparseable
/// input renders nothing meaningful ("—").
function relativeAgo(iso: string): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "—";
  const secs = Math.max(0, Math.round((Date.now() - then) / 1000));
  if (secs < 60) return "just now";
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.round(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.round(hrs / 24);
  return `${days}d ago`;
}

/** Human label for a skill/vault's agent targeting. Empty ⇒ every participant. */
function targetLabel(agentIds: string[], agents: WorkspaceAgentDto[]): string {
  if (agentIds.length === 0) return "All agents";
  const names = agentIds.map((id) => agents.find((a) => a.id === id)?.name ?? "unknown");
  return names.join(", ");
}

/**
 * Compact multi-select of workspace agents for scoping a skill/vault. No
 * selection ⇒ global (primary + every agent). Colors route through --hive-*.
 */
function AgentTargetPicker({
  agents,
  selected,
  onChange,
}: {
  agents: WorkspaceAgentDto[];
  selected: string[];
  onChange: (ids: string[]) => void;
}) {
  const toggle = (id: string) =>
    onChange(selected.includes(id) ? selected.filter((x) => x !== id) : [...selected, id]);
  return (
    <div>
      <div className="mb-1 text-[11.5px]" style={{ color: "var(--hive-ink-soft)" }}>
        Applies to {selected.length === 0 ? "all agents (global)" : `${selected.length} agent(s)`}
      </div>
      {agents.length === 0 ? (
        <div className="text-[11.5px]" style={{ color: "var(--hive-ink-soft)" }}>
          No agents in this workspace — this stays global.
        </div>
      ) : (
        <div
          className="max-h-32 space-y-1 overflow-auto rounded-xl p-2"
          style={{ background: "var(--hive-mist)" }}
        >
          {agents.map((agent) => (
            <label key={agent.id} className="flex cursor-pointer items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={selected.includes(agent.id)}
                onChange={() => toggle(agent.id)}
                style={{ accentColor: "var(--hive-accent-cool)" }}
              />
              <span className="truncate">{agent.name}</span>
            </label>
          ))}
        </div>
      )}
    </div>
  );
}

/** Small pill showing a skill/vault's target scope on its row. */
function TargetBadge({ agentIds, agents }: { agentIds: string[]; agents: WorkspaceAgentDto[] }) {
  return (
    <span
      className="inline-block rounded-full px-2 py-0.5 text-[10.5px] font-medium"
      style={{ background: "var(--hive-mist)", color: "var(--hive-ink-soft)" }}
    >
      {targetLabel(agentIds, agents)}
    </span>
  );
}

function VaultsPane({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const vaults = useQuery({ queryKey: ["vaults", sessionId], queryFn: () => listVaults(sessionId) });
  const agents = useQuery({ queryKey: ["agents", sessionId], queryFn: () => listAgents(sessionId) });
  const [kind, setKind] = useState("github");
  const [reference, setReference] = useState("");
  const [targetIds, setTargetIds] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<{ url: string; text: string } | null>(null);
  const [showAdd, setShowAdd] = useState(false);
  const addVaultMutation = useMutation({
    mutationFn: () => addVault(sessionId, kind, reference.trim(), targetIds),
    onSuccess: () => {
      setReference("");
      setTargetIds([]);
      setError(null);
      setShowAdd(false);
      qc.invalidateQueries({ queryKey: ["vaults", sessionId] });
    },
    onError: (mutationError) => setError(String(mutationError)),
  });
  const removeVaultMutation = useMutation({
    mutationFn: (url: string) => removeVault(sessionId, url),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["vaults", sessionId] }),
  });
  const previewVaultMutation = useMutation({
    mutationFn: (url: string) => previewVault(url).then((text) => ({ url, text })),
    onSuccess: setPreview,
    onError: (mutationError) => setError(String(mutationError)),
  });
  const placeholder = kind === "https" ? "https://…/file.md" : "owner/repo/path.md@main";

  return (
    <RailFrame title="Vaults" subtitle="Reference material mounted into this workspace.">
      <Section title="Sources">
        {(vaults.data ?? []).map((vault) => (
          <Card key={vault.url} className="px-3 py-3">
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0">
                <div className="font-medium">{vault.label}</div>
                <div className="mt-1 truncate text-xs opacity-60">
                  {vault.kind} · {vault.url}
                </div>
                <div className="mt-1.5">
                  <TargetBadge agentIds={vault.agentIds} agents={agents.data ?? []} />
                </div>
              </div>
              <div className="flex gap-2 text-xs">
                <button className="opacity-70 hover:opacity-100" onClick={() => previewVaultMutation.mutate(vault.url)}>
                  Preview
                </button>
                <button
                  className="hover:opacity-80"
                  style={{ color: "var(--hive-danger)" }}
                  onClick={() => confirmThen("Remove this vault source?", () => removeVaultMutation.mutate(vault.url))}
                >
                  Remove
                </button>
              </div>
            </div>
            {preview?.url === vault.url && (
              <pre
                className="mt-3 max-h-56 overflow-auto rounded-2xl p-3 text-xs leading-5"
                style={{ background: "var(--hive-mist)" }}
              >
                {preview.text}
              </pre>
            )}
          </Card>
        ))}
        {(vaults.data ?? []).length === 0 && (
          <EmptyHint text="No vault sources are mounted in this chat." />
        )}
        <FormDisclosure label="Add vault" open={showAdd} onToggle={() => setShowAdd((v) => !v)}>
          <Card className="space-y-2 p-3">
            <SubtleSelectField label="Vault source" value={kind} onChange={setKind}>
              <option value="github">GitHub</option>
              <option value="gitlab">GitLab</option>
              <option value="https">HTTPS</option>
            </SubtleSelectField>
            <input
              value={reference}
              onChange={(event) => setReference(event.target.value)}
              placeholder={placeholder}
              className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
              style={fieldStyle}
            />
            <AgentTargetPicker
              agents={agents.data ?? []}
              selected={targetIds}
              onChange={setTargetIds}
            />
            {error && (
              <div className="text-xs" style={{ color: "var(--hive-danger)" }}>
                {error}
              </div>
            )}
            <Button variant="primary" disabled={!reference.trim() || addVaultMutation.isPending} onClick={() => addVaultMutation.mutate()}>
              Add
            </Button>
          </Card>
        </FormDisclosure>
      </Section>
    </RailFrame>
  );
}

function SkillsPane({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const skills = useQuery({ queryKey: ["skills", sessionId], queryFn: () => listSkills(sessionId) });
  const agents = useQuery({ queryKey: ["agents", sessionId], queryFn: () => listAgents(sessionId) });
  const [name, setName] = useState("");
  const [source, setSource] = useState("");
  const [targetIds, setTargetIds] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [showAdd, setShowAdd] = useState(false);
  const installSkillMutation = useMutation({
    mutationFn: () => installSkill(sessionId, name.trim(), source.trim(), targetIds),
    onSuccess: () => {
      setName("");
      setSource("");
      setTargetIds([]);
      setError(null);
      setShowAdd(false);
      qc.invalidateQueries({ queryKey: ["skills", sessionId] });
    },
    onError: (mutationError) => setError(String(mutationError)),
  });
  const removeSkillMutation = useMutation({
    mutationFn: (skillId: string) => removeSkill(sessionId, skillId),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["skills", sessionId] }),
  });

  return (
    <RailFrame title="Skills" subtitle="Instruction bundles injected into the active participants.">
      <Section title="Loaded Skills">
        {(skills.data ?? []).map((skill) => (
          <Card key={skill.id} className="px-3 py-3">
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0">
                <div className="font-medium">{skill.name}</div>
                {skill.sourceUrl && <div className="mt-1 truncate text-xs opacity-55">{skill.sourceUrl}</div>}
                <div className="mt-2 line-clamp-3 text-xs leading-5 opacity-60">{skill.instructions}</div>
                <div className="mt-1.5">
                  <TargetBadge agentIds={skill.agentIds} agents={agents.data ?? []} />
                </div>
              </div>
              <button
                onClick={() => confirmThen("Remove this skill?", () => removeSkillMutation.mutate(skill.id))}
                className="text-xs hover:opacity-80"
                style={{ color: "var(--hive-danger)" }}
              >
                Remove
              </button>
            </div>
          </Card>
        ))}
        {(skills.data ?? []).length === 0 && <EmptyHint text="No skills loaded for this chat." />}
        <FormDisclosure label="Install skill" open={showAdd} onToggle={() => setShowAdd((v) => !v)}>
          <Card className="space-y-2 p-3">
            <p className="text-[11.5px]" style={{ color: "var(--hive-ink-soft)" }}>
              Supports direct URLs, GitHub blob URLs, and owner/repo/path references.
            </p>
            <input
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder="Display name (optional)"
              className="w-full rounded-xl border px-3 py-2 text-sm"
              style={fieldStyle}
            />
            <input
              value={source}
              onChange={(event) => setSource(event.target.value)}
              placeholder="skills.sh/... or owner/repo/SKILL.md"
              className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
              style={fieldStyle}
            />
            <AgentTargetPicker
              agents={agents.data ?? []}
              selected={targetIds}
              onChange={setTargetIds}
            />
            {error && (
              <div className="text-xs" style={{ color: "var(--hive-danger)" }}>
                {error}
              </div>
            )}
            <Button variant="primary" disabled={!source.trim() || installSkillMutation.isPending} onClick={() => installSkillMutation.mutate()}>
              Install
            </Button>
          </Card>
        </FormDisclosure>
      </Section>
    </RailFrame>
  );
}

function ActivityPane() {
  return (
    <RailFrame title="Activity" subtitle="Streaming runtime activity for the current workspace.">
      <div className="h-[calc(100vh-12rem)] min-h-[18rem] rounded-2xl border p-1.5" style={panelStyle}>
        <div className="h-full overflow-hidden rounded-xl">
          <LogsView />
        </div>
      </div>
    </RailFrame>
  );
}

// ─── Context pane ────────────────────────────────────────────────────────────

function fmtTokens(n: number): string {
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

function runtimePickerLabel(runtime: RuntimeSummaryDto) {
  const provider = runtime.provider.trim();
  const model = runtime.model.trim();
  if (provider && model) return `${provider} / ${model}`;
  if (provider) return provider;
  if (model) return model;
  return runtime.label.trim() || runtime.name.trim() || "runtime";
}

function ContextPane({
  sessionId,
  activeRuntimeId,
}: {
  sessionId: string;
  activeRuntimeId: string;
}) {
  const qc = useQueryClient();
  const telemetry = useQuery({
    queryKey: ["context-telemetry", sessionId, activeRuntimeId],
    queryFn: () => getContextTelemetry(sessionId),
  });
  const skills = useQuery({ queryKey: ["skills", sessionId], queryFn: () => listSkills(sessionId) });
  const vaults = useQuery({ queryKey: ["vaults", sessionId], queryFn: () => listVaults(sessionId) });

  useEffect(() => {
    const unlisten = onChatStream((event) => {
      if (event.sessionId !== sessionId) return;
      qc.invalidateQueries({ queryKey: ["context-telemetry", sessionId] });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [qc, sessionId]);

  const removeSkillMutation = useMutation({
    mutationFn: (id: string) => removeSkill(sessionId, id),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ["skills", sessionId] });
      await qc.invalidateQueries({ queryKey: ["context-telemetry", sessionId] });
    },
  });
  const removeVaultMutation = useMutation({
    mutationFn: (url: string) => removeVault(sessionId, url),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ["vaults", sessionId] });
      await qc.invalidateQueries({ queryKey: ["context-telemetry", sessionId] });
    },
  });

  const data = telemetry.data;
  const totalPlannedTokens = (data?.systemPromptTokens ?? 0) + (data?.keptTokens ?? 0);
  const budgetPct = data
    ? Math.min(100, Math.round((totalPlannedTokens / Math.max(1, data.contextWindowTokens)) * 100))
    : 0;
  const budgetColor =
    budgetPct >= 90
      ? "var(--hive-danger)"
      : budgetPct >= 70
        ? "var(--hive-accent-warm)"
        : "var(--hive-accent-cool)";
  const hasCompaction = (data?.overflowMessageCount ?? 0) > 0;
  const summaryLabel =
    data?.summaryStrategy === "cached"
      ? "Using cached summary"
      : data?.summaryStrategy === "incremental"
        ? "Refreshing summary incrementally"
        : data?.summaryStrategy === "fresh"
          ? "Creating fresh summary"
          : "No compaction active";

  if (telemetry.isError) {
    return (
      <RailFrame title="Context" subtitle="Real backend context budget and compaction state.">
        <EmptyHint text="Context telemetry is temporarily unavailable. Keep chatting and retry this tab." />
      </RailFrame>
    );
  }

  return (
    <RailFrame title="Context" subtitle="Real backend context budget and compaction state.">
      <Section title="Budget">
        <div className="rounded-2xl border p-3" style={panelStyle}>
          <div className="mb-2 flex items-center justify-between text-xs">
            <span className="opacity-65">
              {data ? `${fmtTokens(totalPlannedTokens)} in prompt` : "Loading..."}
            </span>
            <span className="opacity-40">
              {data ? `${fmtTokens(data.contextWindowTokens)} window` : ""}
            </span>
          </div>
          <div className="h-2 overflow-hidden rounded-full" style={{ background: "var(--hive-mist)" }}>
            <div
              className="h-full rounded-full transition-all duration-300"
              style={{ width: `${budgetPct}%`, background: budgetColor }}
            />
          </div>
          <div className="mt-1.5 text-right text-[11px] font-medium" style={{ color: budgetColor }}>
            {budgetPct}%
          </div>
          {data && (
            <div className="mt-2.5 grid grid-cols-2 gap-1.5 text-[11px] opacity-55">
              <span>System {fmtTokens(data.systemPromptTokens)}</span>
              <span className="text-right">History budget {fmtTokens(data.historyBudgetTokens)}</span>
              <span>Reserved output {fmtTokens(data.reservedOutputTokens)}</span>
              <span className="text-right">Summary reserve {fmtTokens(data.summaryReserveTokens)}</span>
            </div>
          )}
          {hasCompaction && data && (
            <div
              className="mt-2.5 rounded-xl px-3 py-2 text-xs leading-5"
              style={{
                background: "color-mix(in srgb, var(--hive-accent-warm) 10%, transparent)",
                color: "var(--hive-accent-warm)",
              }}
            >
              {data.overflowMessageCount} earlier messages are currently condensed to fit the context window.
            </div>
          )}
          {data && <div className="mt-2 text-[11px] opacity-50">{summaryLabel}</div>}
          {budgetPct >= 70 && (
            <div
              className="mt-2.5 rounded-xl px-3 py-2 text-xs leading-5"
              style={{
                background: "color-mix(in srgb, var(--hive-danger) 10%, transparent)",
                color: "var(--hive-danger)",
              }}
            >
              Context is getting full — consider removing unused skills or vaults below.
            </div>
          )}
        </div>
        <div className="mt-2 grid grid-cols-3 gap-1.5 text-center">
          <ContextStat label="Messages" value={String(data?.messageCount ?? 0)} />
          <ContextStat label="Skills" value={String(data?.skillCount ?? 0)} />
          <ContextStat label="Vaults" value={String(data?.vaultCount ?? 0)} />
        </div>
      </Section>

      <Section title="Injected Skills">
        {(skills.data ?? []).length === 0 ? (
          <EmptyHint text="No skills loaded. Skills inject instructions into every turn." />
        ) : (
          (skills.data ?? []).map((skill) => {
            const est = Math.ceil(skill.instructions.length / 4);
            return (
              <div
                key={skill.id}
                className="flex items-start gap-2 rounded-xl border px-2.5 py-2"
                style={panelStyle}
              >
                <div className="min-w-0 flex-1">
                  <div className="truncate text-sm font-medium">{skill.name}</div>
                  <div className="mt-0.5 text-xs opacity-50">~{fmtTokens(est)} tokens in prompt</div>
                </div>
                <IconButton
                  label="Remove skill"
                  size={24}
                  onClick={() => confirmThen("Remove this skill?", () => removeSkillMutation.mutate(skill.id))}
                >
                  <IconX size={14} />
                </IconButton>
              </div>
            );
          })
        )}
      </Section>

      <Section title="Mounted Vaults">
        {(vaults.data ?? []).length === 0 ? (
          <EmptyHint text="No vaults mounted. Vaults inject reference material into context." />
        ) : (
          (vaults.data ?? []).map((vault) => (
            <div
              key={vault.url}
              className="flex items-start gap-2 rounded-xl border px-2.5 py-2"
              style={panelStyle}
            >
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-medium">{vault.label}</div>
                <div className="mt-0.5 text-xs opacity-50">{vault.kind} reference source</div>
              </div>
              <IconButton
                label="Unmount vault"
                size={24}
                onClick={() => confirmThen("Remove this vault source?", () => removeVaultMutation.mutate(vault.url))}
              >
                <IconX size={14} />
              </IconButton>
            </div>
          ))
        )}
      </Section>
    </RailFrame>
  );
}

// ─── Back-compat shims (retained deliberately) ───────────────────────────────
// `SubtleSelectField`, `Stack`, `panelStyle` and `fieldStyle` are still imported
// by WorkflowsPane.tsx and WorkflowBuilder.tsx, which are out of scope for this
// task. ui.tsx has no `Stack` and its `SubtleSelectField` shape differs, and its
// same-named `panelStyle`/`fieldStyle` tokens carry *swapped* panel/mist
// semantics — re-exporting them would invert contrast in those files and in this
// pane. So these keep RightRail's original values. RightRail's own new code
// composes the ui.tsx primitives (Button, Card, Section, SelectField, Switch,
// FormDisclosure, EmptyHint) instead. `EmptyHint` is re-exported from ui.tsx.

export { EmptyHint };

export function SubtleSelectField({
  label,
  value,
  onChange,
  children,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  children: ReactNode;
}) {
  return (
    <label
      className="inline-flex w-full min-w-0 items-center gap-2.5 rounded-xl border px-2.5 py-1.5"
      style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
    >
      <span className="shrink-0 text-[10px] font-medium uppercase tracking-[0.12em] opacity-45">
        {label}
      </span>
      <select
        value={value}
        onChange={(event) => onChange(event.target.value)}
        className="min-w-0 flex-1 appearance-none bg-transparent pr-1 text-sm font-medium outline-none"
        style={{ color: "var(--hive-ink)", fontFamily: "inherit" }}
      >
        {children}
      </select>
      <span className="shrink-0 opacity-40">
        <IconChevronDown size={13} />
      </span>
    </label>
  );
}

function ContextStat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-xl border py-2 text-center" style={panelStyle}>
      <div className="text-base font-semibold">{value}</div>
      <div className="text-[10px] uppercase tracking-[0.12em] opacity-50">{label}</div>
    </div>
  );
}

export function Stack({ children }: { children: ReactNode }) {
  return <div className="space-y-2">{children}</div>;
}

export const panelStyle = {
  borderColor: "var(--hive-line)",
  background: "var(--hive-mist)",
} as const;

export const fieldStyle = {
  borderColor: "var(--hive-line)",
  background: "var(--hive-panel)",
  color: "var(--hive-ink)",
} as const;
