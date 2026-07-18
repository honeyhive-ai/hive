import type { ReactNode } from "react";
import { useEffect, useState } from "react";
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
  listRuntimes,
  listSkills,
  listVaults,
  previewVault,
  removeAgent,
  removeMcpServer,
  removeSkill,
  removeVault,
  setChatRuntime,
  setMcpEnabled,
  syncStatus,
  onChatStream,
  voteProposal,
  type RuntimeSummaryDto,
} from "@/lib/ipc";
import { LogsView } from "@/components/LogsView";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";
import { Button, IconButton } from "@/components/ui";
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
  const [agentName, setAgentName] = useState("");
  const [agentRole, setAgentRole] = useState("");
  const [agentRuntimeId, setAgentRuntimeId] = useState(activeRuntimeId);
  const [mcpSource, setMcpSource] = useState("");
  const [mcpError, setMcpError] = useState<string | null>(null);

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

  return (
    <RailFrame title="Tools" subtitle="Workspace agents, runtimes, and MCP servers for this chat.">
      <Section title="Configured Runtimes">
        <div className="space-y-2">
          {(runtimes.data ?? []).map((rt) => (
            <RuntimeCard
              key={rt.id}
              runtime={rt}
              active={rt.id === activeRuntimeId}
              onSelect={async () => {
                await setChatRuntime(sessionId, rt.id);
                qc.invalidateQueries({ queryKey: ["chat", sessionId] });
                qc.invalidateQueries({ queryKey: ["chats"] });
              }}
            />
          ))}
          {(runtimes.data ?? []).length === 0 && <EmptyHint text="No runtimes configured yet." />}
        </div>
      </Section>

      <Section title="Workspace Agents">
        <Stack>
          {(agents.data ?? []).map((agent) => (
            <div key={agent.id} className="rounded-2xl border px-3 py-3" style={panelStyle}>
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="font-medium">@{agent.name}</div>
                  <div className="mt-1 text-xs opacity-60">
                    {agent.runtimeId} · {agent.role || "agent"}
                  </div>
                </div>
                <button
                  onClick={() => confirmThen("Remove this agent?", () => removeAgentMutation.mutate(agent.id))}
                  className="text-xs hover:opacity-80"
                  style={{ color: "var(--hive-danger)" }}
                >
                  Remove
                </button>
              </div>
            </div>
          ))}
          {(agents.data ?? []).length === 0 && (
            <EmptyHint text="No workspace agents are attached to this chat yet." />
          )}
        </Stack>
        <FormCard
          title="Add agent"
          actions={
            <PrimaryButton
              disabled={!agentName.trim() || !agentRuntimeId || addAgentMutation.isPending}
              onClick={() => addAgentMutation.mutate()}
            >
              Add
            </PrimaryButton>
          }
        >
          <input
            value={agentName}
            onChange={(event) => setAgentName(event.target.value)}
            placeholder="Agent name"
            className="w-full rounded-xl border px-3 py-2"
            style={fieldStyle}
          />
          <input
            value={agentRole}
            onChange={(event) => setAgentRole(event.target.value)}
            placeholder="Role (optional)"
            className="w-full rounded-xl border px-3 py-2"
            style={fieldStyle}
          />
          <SubtleSelectField
            label="Agent runtime"
            value={agentRuntimeId}
            onChange={setAgentRuntimeId}
          >
            {(runtimes.data ?? []).map((runtime) => (
              <option key={runtime.id} value={runtime.id}>
                {runtimePickerLabel(runtime)}
              </option>
            ))}
          </SubtleSelectField>
        </FormCard>
      </Section>

      <Section title="MCP Servers">
        <Stack>
          {(mcp.data ?? []).map((server) => (
            <div key={server.id} className="rounded-2xl border px-3 py-3" style={panelStyle}>
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="font-medium">{server.id}</div>
                  <div className="mt-1 text-xs opacity-60">
                    [{server.transport}] {server.detail}
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    checked={server.enabled}
                    onChange={async (event) => {
                      await setMcpEnabled(server.id, event.target.checked);
                      qc.invalidateQueries({ queryKey: ["mcp"] });
                    }}
                  />
                  {server.isManaged && (
                    <button
                      onClick={() => confirmThen("Remove this MCP server?", () => removeMcpMutation.mutate(server.id))}
                      className="text-xs hover:opacity-80"
                      style={{ color: "var(--hive-danger)" }}
                    >
                      Remove
                    </button>
                  )}
                </div>
              </div>
            </div>
          ))}
          {(mcp.data ?? []).length === 0 && <EmptyHint text="No MCP servers configured." />}
        </Stack>
        <FormCard
          title="Install from the internet"
          hint="Paste a manifest URL, GitHub blob URL, or owner/repo/path reference."
          actions={
            <PrimaryButton
              disabled={!mcpSource.trim() || installMcpMutation.isPending}
              onClick={() => installMcpMutation.mutate()}
            >
              Install
            </PrimaryButton>
          }
        >
          <input
            value={mcpSource}
            onChange={(event) => setMcpSource(event.target.value)}
            placeholder="owner/repo/mcp.json or https://…"
            className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
            style={fieldStyle}
          />
          {mcpError && <div className="text-xs" style={{ color: "var(--hive-danger)" }}>{mcpError}</div>}
        </FormCard>
      </Section>
    </RailFrame>
  );
}

function RuntimeCard({
  runtime,
  active,
  onSelect,
}: {
  runtime: RuntimeSummaryDto;
  active: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      onClick={onSelect}
      className="w-full rounded-2xl border px-3.5 py-3.5 text-left"
      style={{
        ...panelStyle,
        borderColor: active ? "var(--hive-accent-cool)" : panelStyle.borderColor,
        background: active ? "rgba(87,161,168,0.12)" : panelStyle.background,
      }}
    >
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
            <span className="rounded-full px-2 py-1 text-[10px] font-semibold uppercase opacity-70" style={{ background: "var(--hive-mist)" }}>
              Added
            </span>
          )}
          {active && (
            <span className="rounded-full px-2 py-1 text-xs font-semibold" style={{ background: "rgba(34,160,90,0.18)", color: "var(--hive-success)" }}>
              Active
            </span>
          )}
        </div>
      </div>
    </button>
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
      <Section title="Pending Proposals">
        <Stack>
          {(proposals.data ?? []).map((proposal) => (
            <div key={proposal.id} className="rounded-2xl border px-4 py-4" style={panelStyle}>
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
                  style={{ background: "rgba(34,160,90,0.2)", color: "var(--hive-success)" }}
                  onClick={async () => {
                    await voteProposal(sessionId, proposal.id, true);
                    qc.invalidateQueries({ queryKey: ["proposals", sessionId] });
                  }}
                >
                  Approve
                </button>
                <button
                  className="rounded-xl px-3 py-2 text-sm font-medium"
                  style={{ background: "rgba(200,70,70,0.18)", color: "var(--hive-danger)" }}
                  onClick={async () => {
                    await voteProposal(sessionId, proposal.id, false);
                    qc.invalidateQueries({ queryKey: ["proposals", sessionId] });
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
            </div>
          ))}
          {(proposals.data ?? []).length === 0 && <EmptyHint text="No proposals are waiting for review." />}
        </Stack>
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
      refresh();
    },
    onError: (e) => setError(String(e)),
  });
  const [org, setOrg] = useState("");
  const importTeams = useMutation({
    mutationFn: () => importGithubTeams(sessionId, org.trim()),
    onSuccess: (n) => {
      const handle = org.trim();
      setOrg("");
      refresh();
      toast.success(`Imported ${n} member${n === 1 ? "" : "s"} from @${handle}.`);
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
        <Stack>
          {error && <div className="text-xs" style={{ color: "var(--hive-danger)" }}>{error}</div>}
          {(members.data ?? []).map((member) => {
            const online = onlineActors.has(member.actorId);
            const dup = (nameCounts.get(member.displayName) ?? 0) > 1 && member.index > 0;
            return (
              <div key={member.id} className="rounded-2xl border px-3 py-3" style={panelStyle}>
                <div className="flex items-center gap-2 font-medium">
                  <span
                    className="inline-block h-2 w-2 shrink-0 rounded-full"
                    style={{ background: online ? "var(--hive-success)" : "var(--hive-line)" }}
                    title={sync.data?.relayConfigured ? (online ? "Online" : "Offline") : "Presence needs a relay"}
                  />
                  {member.displayName}
                  {dup && (
                    <span className="text-xs opacity-50" title={`Mention precisely with @${member.displayName}#${member.index}`}>
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
                    onClick={() => confirmThen("Remove this member from the roster?", () => removeMutation.mutate(member.id))}
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
              </div>
            );
          })}
          {(members.data ?? []).length === 0 && <EmptyHint text="No extra members have been added yet." />}
        </Stack>
        <FormCard
          title="Invite by GitHub handle"
          hint="Adds the person and seals the workspace key to all their devices. They must have signed in to Hive with GitHub once. Needs a relay + your GitHub sign-in."
          actions={
            <PrimaryButton disabled={!handle.trim() || inviteMutation.isPending} onClick={() => inviteMutation.mutate()}>
              {inviteMutation.isPending ? "…" : "Invite"}
            </PrimaryButton>
          }
        >
          <input
            value={handle}
            onChange={(e) => setHandle(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handle.trim() && inviteMutation.mutate()}
            placeholder="@github-handle"
            className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
            style={fieldStyle}
          />
        </FormCard>
        <FormCard
          title="Add member"
          hint="Roles gate actions: owner > admin > contributor > viewer. The last owner is protected."
          actions={
            <PrimaryButton disabled={!name.trim() || addMutation.isPending} onClick={() => addMutation.mutate()}>
              Add
            </PrimaryButton>
          }
        >
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
        </FormCard>
        <FormCard
          title="Import GitHub org"
          hint="Pull an org's Teams into the roster, mapping team membership to roles (highest wins). Needs GitHub sign-in with read:org."
          actions={
            <PrimaryButton disabled={!org.trim() || importTeams.isPending} onClick={() => importTeams.mutate()}>
              {importTeams.isPending ? "Importing…" : "Import"}
            </PrimaryButton>
          }
        >
          <input
            value={org}
            onChange={(e) => setOrg(e.target.value)}
            placeholder="github-org-slug"
            className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
            style={fieldStyle}
          />
        </FormCard>
      </Section>

      <Section title="Team members (server-enforced)">
        {!relayOn ? (
          <EmptyHint text="Connect a relay (Settings → Team sync) to use server-enforced membership." />
        ) : (
          <Stack>
            <p className="text-xs opacity-60">
              Who the relay accepts writes from. Requires a membership-enforcing
              (paid/enterprise) relay; on an open relay this stays empty and
              everyone can write.
            </p>
            {(srvMembers.data ?? []).map((m) => (
              <div key={m.account} className="flex items-center justify-between rounded-2xl border px-3 py-2" style={panelStyle}>
                <div className="min-w-0">
                  <div className="truncate font-medium">@{m.login}</div>
                  <div className="text-xs opacity-50">{m.role} · {m.account}</div>
                </div>
                <button
                  className="shrink-0 text-xs font-medium hover:opacity-80"
                  style={{ color: "var(--hive-danger)" }}
                  onClick={() => confirmThen(`Remove @${m.login} from this workspace on the relay?`, () => srvRemoveMutation.mutate(m.account))}
                  title="Stop the relay from accepting their writes (pair with Remove & revoke to cut reads)"
                >
                  Remove
                </button>
              </div>
            ))}
            {(srvMembers.data ?? []).length === 0 && (
              <FormCard
                title="Enable member controls"
                hint="Claim this workspace on the relay so it enforces who can write — you become the owner. No-op on an open relay."
                actions={
                  <PrimaryButton disabled={claimMutation.isPending} onClick={() => claimMutation.mutate()}>
                    {claimMutation.isPending ? "…" : "Claim ownership"}
                  </PrimaryButton>
                }
              >
                <span className="text-xs opacity-50">No server-enforced members yet.</span>
              </FormCard>
            )}
            <FormCard
              title="Add / set member by handle"
              hint="Admin+ only. Looks the handle up in the directory and sets their role on the relay."
              actions={
                <PrimaryButton disabled={!srvHandle.trim() || srvAddMutation.isPending} onClick={() => srvAddMutation.mutate()}>
                  {srvAddMutation.isPending ? "…" : "Set"}
                </PrimaryButton>
              }
            >
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
            </FormCard>
          </Stack>
        )}
      </Section>
    </RailFrame>
  );
}

function VaultsPane({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const vaults = useQuery({ queryKey: ["vaults", sessionId], queryFn: () => listVaults(sessionId) });
  const [kind, setKind] = useState("github");
  const [reference, setReference] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<{ url: string; text: string } | null>(null);
  const addVaultMutation = useMutation({
    mutationFn: () => addVault(sessionId, kind, reference.trim()),
    onSuccess: () => {
      setReference("");
      setError(null);
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
        <Stack>
          {(vaults.data ?? []).map((vault) => (
            <div key={vault.url} className="rounded-2xl border px-3 py-3" style={panelStyle}>
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="font-medium">{vault.label}</div>
                  <div className="mt-1 truncate text-xs opacity-60">
                    {vault.kind} · {vault.url}
                  </div>
                </div>
                <div className="flex gap-2 text-xs">
                  <button className="opacity-70 hover:opacity-100" onClick={() => previewVaultMutation.mutate(vault.url)}>
                    Preview
                  </button>
                  <button className="hover:opacity-80" style={{ color: "var(--hive-danger)" }} onClick={() => confirmThen("Remove this vault source?", () => removeVaultMutation.mutate(vault.url))}>
                    Remove
                  </button>
                </div>
              </div>
              {preview?.url === vault.url && (
                <pre className="mt-3 max-h-56 overflow-auto rounded-2xl p-3 text-xs leading-5" style={{ background: "var(--hive-mist)" }}>
                  {preview.text}
                </pre>
              )}
            </div>
          ))}
          {(vaults.data ?? []).length === 0 && <EmptyHint text="No vault sources are mounted in this chat." />}
        </Stack>
        <FormCard
          title="Add vault"
          actions={
            <PrimaryButton
              disabled={!reference.trim() || addVaultMutation.isPending}
              onClick={() => addVaultMutation.mutate()}
            >
              Add
            </PrimaryButton>
          }
        >
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
          {error && <div className="text-xs" style={{ color: "var(--hive-danger)" }}>{error}</div>}
        </FormCard>
      </Section>
    </RailFrame>
  );
}

function SkillsPane({ sessionId }: { sessionId: string }) {
  const qc = useQueryClient();
  const skills = useQuery({ queryKey: ["skills", sessionId], queryFn: () => listSkills(sessionId) });
  const [name, setName] = useState("");
  const [source, setSource] = useState("");
  const [error, setError] = useState<string | null>(null);
  const installSkillMutation = useMutation({
    mutationFn: () => installSkill(sessionId, name.trim(), source.trim()),
    onSuccess: () => {
      setName("");
      setSource("");
      setError(null);
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
        <Stack>
          {(skills.data ?? []).map((skill) => (
            <div key={skill.id} className="rounded-2xl border px-3 py-3" style={panelStyle}>
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="font-medium">{skill.name}</div>
                  {skill.sourceUrl && <div className="mt-1 truncate text-xs opacity-55">{skill.sourceUrl}</div>}
                  <div className="mt-2 line-clamp-3 text-xs leading-5 opacity-60">{skill.instructions}</div>
                </div>
                <button
                  onClick={() => confirmThen("Remove this skill?", () => removeSkillMutation.mutate(skill.id))}
                  className="text-xs hover:opacity-80"
                  style={{ color: "var(--hive-danger)" }}
                >
                  Remove
                </button>
              </div>
            </div>
          ))}
          {(skills.data ?? []).length === 0 && <EmptyHint text="No skills loaded for this chat." />}
        </Stack>
        <FormCard
          title="Install from the internet"
          hint="Supports direct URLs, GitHub blob URLs, and owner/repo/path references."
          actions={
            <PrimaryButton
              disabled={!source.trim() || installSkillMutation.isPending}
              onClick={() => installSkillMutation.mutate()}
            >
              Install
            </PrimaryButton>
          }
        >
          <input
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="Display name (optional)"
            className="w-full rounded-xl border px-3 py-2"
            style={fieldStyle}
          />
          <input
            value={source}
            onChange={(event) => setSource(event.target.value)}
            placeholder="skills.sh/... or owner/repo/SKILL.md"
            className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
            style={fieldStyle}
          />
          {error && <div className="text-xs" style={{ color: "var(--hive-danger)" }}>{error}</div>}
        </FormCard>
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
    ? Math.min(100, Math.round((totalPlannedTokens / data.contextWindowTokens) * 100))
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
    <RailFrame
      title="Context"
      subtitle="Real backend context budget and compaction state."
    >
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
              style={{ background: "rgba(214,158,87,0.10)", color: "var(--hive-accent-warm)" }}
            >
              {data.overflowMessageCount} earlier messages are currently condensed to fit the context window.
            </div>
          )}
          {data && (
            <div className="mt-2 text-[11px] opacity-50">{summaryLabel}</div>
          )}
          {budgetPct >= 70 && (
            <div
              className="mt-2.5 rounded-xl px-3 py-2 text-xs leading-5"
              style={{ background: "rgba(200,70,70,0.10)", color: "var(--hive-danger)" }}
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
          <Stack>
            {(skills.data ?? []).map((skill) => {
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
            })}
          </Stack>
        )}
      </Section>

      <Section title="Mounted Vaults">
        {(vaults.data ?? []).length === 0 ? (
          <EmptyHint text="No vaults mounted. Vaults inject reference material into context." />
        ) : (
          <Stack>
            {(vaults.data ?? []).map((vault) => (
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
            ))}
          </Stack>
        )}
      </Section>
    </RailFrame>
  );
}

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
      <span className="shrink-0 opacity-40"><IconChevronDown size={13} /></span>
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

function Section({ title, action, children }: { title: string; action?: ReactNode; children: ReactNode }) {
  return (
    <section className="mb-5">
      <div className="mb-2 flex items-center justify-between gap-2">
        <h2 className="text-xs font-semibold uppercase tracking-[0.16em] opacity-60">{title}</h2>
        {action}
      </div>
      {children}
    </section>
  );
}

export function Stack({ children }: { children: ReactNode }) {
  return <div className="space-y-2">{children}</div>;
}

function FormCard({
  title,
  hint,
  actions,
  children,
}: {
  title: string;
  hint?: string;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="mt-3 rounded-2xl border p-3" style={panelStyle}>
      <div className="mb-3 flex items-start justify-between gap-3">
        <div>
          <div className="text-sm font-semibold">{title}</div>
          {hint && <div className="mt-1 text-xs opacity-55">{hint}</div>}
        </div>
        {actions}
      </div>
      <div className="space-y-2">{children}</div>
    </div>
  );
}

function PrimaryButton({
  children,
  disabled,
  onClick,
}: {
  children: ReactNode;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <Button variant="primary" disabled={disabled} onClick={onClick}>
      {children}
    </Button>
  );
}

export function EmptyHint({ text }: { text: string }) {
  return <div className="rounded-2xl border px-3 py-4 text-sm opacity-55" style={panelStyle}>{text}</div>;
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
