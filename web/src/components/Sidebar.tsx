import type { ReactNode } from "react";
import { useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  archiveChat,
  createChat,
  deleteChat,
  listAgents,
  listChats,
  listMembers,
  listSkills,
  listVaults,
  type ChatSummaryDto,
} from "@/lib/ipc";
import type { UtilityPane } from "@/components/RightRail";
import { toast, errMsg } from "@/components/Toast";
import { SkeletonRows } from "@/components/Skeleton";
import { confirmDialog } from "@/components/Dialog";
import {
  IconPlus,
  IconCheck,
  IconEllipsis,
  IconChevronRight,
  IconChevronDown,
} from "@/lib/icons";

/// Last path segment of a folder path (cross-platform separators).
function folderBasename(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
}

export function Sidebar({
  width,
  selectedId,
  sessionId,
  onSelect,
  view,
  onOpenSettings,
  onAddWorkspace,
  onRemoveWorkspace,
  onSwitchWorkspace,
  onOpenUtilityPane,
  workspaceLabel,
  workspacePath,
  knownWorkspaces,
  displayName,
  utilityPane,
}: {
  width: number;
  selectedId: string | null;
  sessionId: string | null;
  onSelect: (id: string) => void;
  view: "workspace" | "settings" | "friends";
  onOpenSettings: () => void;
  onAddWorkspace: () => void | Promise<void>;
  onRemoveWorkspace: (path: string) => void | Promise<void>;
  onSwitchWorkspace: (path: string) => void | Promise<void>;
  onOpenUtilityPane: (pane: UtilityPane) => void;
  workspaceLabel: string;
  workspacePath: string;
  knownWorkspaces: string[];
  displayName: string;
  utilityPane: UtilityPane;
}) {
  const qc = useQueryClient();
  const chats = useQuery({ queryKey: ["chats"], queryFn: listChats });
  // Workspace scope (Personal / rooms) now lives in the WorkspaceRail.
  const members = useQuery({
    queryKey: ["members", sessionId],
    queryFn: () => listMembers(sessionId ?? ""),
    enabled: Boolean(sessionId),
  });
  const agents = useQuery({
    queryKey: ["agents", sessionId],
    queryFn: () => listAgents(sessionId ?? ""),
    enabled: Boolean(sessionId),
  });
  const vaults = useQuery({
    queryKey: ["vaults", sessionId],
    queryFn: () => listVaults(sessionId ?? ""),
    enabled: Boolean(sessionId),
  });
  const skills = useQuery({
    queryKey: ["skills", sessionId],
    queryFn: () => listSkills(sessionId ?? ""),
    enabled: Boolean(sessionId),
  });
  const [showArchived, setShowArchived] = useState(false);
  const [menuFor, setMenuFor] = useState<string | null>(null);
  const [showWorkspaceMenu, setShowWorkspaceMenu] = useState(false);
  const [workspaceError, setWorkspaceError] = useState<string | null>(null);
  const [workspaceBusy, setWorkspaceBusy] = useState(false);

  const all: ChatSummaryDto[] = chats.data ?? [];
  const visible = useMemo(
    () => all.filter((c) => c.archived === showArchived),
    [all, showArchived],
  );
  const canRemoveCurrentWorkspace = workspacePath.trim().length > 0;

  async function handleWorkspaceAdd() {
    if (workspaceBusy) return;
    setWorkspaceBusy(true);
    setWorkspaceError(null);
    try {
      await onAddWorkspace();
    } catch (error) {
      setWorkspaceError(error instanceof Error ? error.message : String(error));
    } finally {
      setWorkspaceBusy(false);
    }
  }

  async function handleWorkspaceRemove() {
    if (workspaceBusy || !canRemoveCurrentWorkspace) return;
    setWorkspaceBusy(true);
    setWorkspaceError(null);
    try {
      await onRemoveWorkspace(workspacePath);
    } catch (error) {
      setWorkspaceError(error instanceof Error ? error.message : String(error));
    } finally {
      setWorkspaceBusy(false);
    }
  }

  async function handleNew() {
    try {
      const created = await createChat("");
      await qc.invalidateQueries({ queryKey: ["chats"] });
      onSelect(created.id);
    } catch (e) {
      toast.error(`Couldn't create chat: ${errMsg(e)}`);
    }
  }

  async function handleArchive(c: ChatSummaryDto) {
    try {
      await archiveChat(c.id, !c.archived);
      setMenuFor(null);
      qc.invalidateQueries({ queryKey: ["chats"] });
    } catch (e) {
      toast.error(`Couldn't ${c.archived ? "unarchive" : "archive"} chat: ${errMsg(e)}`);
    }
  }

  async function handleDelete(c: ChatSummaryDto) {
    const ok = await confirmDialog(`Permanently delete "${c.title}"? This cannot be undone.`, {
      danger: true,
      confirmLabel: "Delete",
    });
    if (!ok) return;
    try {
      await deleteChat(c.id);
      setMenuFor(null);
      qc.invalidateQueries({ queryKey: ["chats"] });
    } catch (e) {
      toast.error(`Couldn't delete chat: ${errMsg(e)}`);
    }
  }

  return (
    <aside
      className="flex shrink-0 flex-col border-r"
      style={{
        width,
        background: "linear-gradient(180deg, var(--hive-sidebar-top), var(--hive-sidebar-bottom))",
        color: "var(--hive-sidebar-ink)",
        borderColor: "rgba(255,255,255,0.06)",
      }}
    >
      <div className="relative border-b px-3.5 py-4" style={{ borderColor: "rgba(255,255,255,0.08)" }}>
        {/* The rail (left) now owns workspace switching; the working folder is a
            demoted header property here — a static identity row with a small
            "Change" link to the folder menu, plus an execution-location chip. */}
        <div className="flex w-full items-center gap-3">
          {/* A folder glyph (not the Hive logo) — the rail already shows the
              workspace's brand icon, so repeating it here read as the logo next
              to itself. This header is about the working folder. */}
          <div
            className="flex h-10 w-10 shrink-0 items-center justify-center rounded-2xl border opacity-80"
            style={{
              background: "linear-gradient(180deg, rgba(255,255,255,0.08), rgba(255,255,255,0.03))",
              borderColor: "rgba(255,255,255,0.08)",
            }}
          >
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
              <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
            </svg>
          </div>
          <div className="min-w-0">
            <div className="truncate text-xl font-semibold">{workspaceLabel}</div>
            <div className="truncate text-sm opacity-65">{workspacePath || "No project folder"}</div>
          </div>
          <button
            className="ml-auto shrink-0 rounded-md px-2 py-1 text-xs opacity-65 hover:opacity-95"
            onClick={() => setShowWorkspaceMenu((value) => !value)}
            title="Change the working folder"
          >
            Change
          </button>
        </div>

        {/* Execution-location chip: makes it clear where agent work runs. Local
            workspaces execute on this device; the dot is the host status. */}
        <div className="mt-2.5 flex items-center gap-1.5 text-xs opacity-70">
          {/* Neutral bullet — a green dot implied a live health check we don't do. */}
          <span className="h-1.5 w-1.5 rounded-full bg-current opacity-60" />
          <span className="truncate">
            Runs on this device{workspacePath ? ` · ${folderBasename(workspacePath)}` : ""}
          </span>
        </div>
        {showWorkspaceMenu && (
          <div
            className="absolute left-3 right-3 top-[calc(100%-0.25rem)] z-20 rounded-2xl border p-2 shadow-2xl"
            style={{ background: "var(--hive-panel)", color: "var(--hive-ink)", borderColor: "var(--hive-line)" }}
          >
            <div className="px-2 pb-2 text-[11px] font-semibold uppercase tracking-[0.18em] opacity-55">
              Project folders
            </div>
            <div className="space-y-1">
              {knownWorkspaces.map((path) => {
                const label = path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
                const isCurrent = path === workspacePath;
                return (
                  <button
                    key={path}
                    onClick={async () => {
                      await onSwitchWorkspace(path);
                      setShowWorkspaceMenu(false);
                    }}
                    className="flex w-full items-start gap-2 rounded-xl px-2.5 py-2 text-left hover:opacity-85"
                    style={{ background: isCurrent ? "rgba(87,161,168,0.14)" : "transparent" }}
                  >
                    <span className="flex w-4 shrink-0 justify-center pt-0.5 opacity-65">
                      {isCurrent ? (
                        <IconCheck size={12} />
                      ) : (
                        <span className="mt-1.5 h-1 w-1 rounded-full bg-current opacity-50" />
                      )}
                    </span>
                    <span className="min-w-0">
                      <span className="block truncate text-sm font-medium">{label}</span>
                      <span className="block truncate text-xs opacity-55">{path}</span>
                    </span>
                  </button>
                );
              })}
            </div>
            <div className="my-2 border-t" style={{ borderColor: "var(--hive-line)" }} />
            <div className="space-y-1">
              <button
                onClick={handleWorkspaceAdd}
                disabled={workspaceBusy}
                className="w-full rounded-xl px-2.5 py-2 text-left text-sm font-medium hover:opacity-85 disabled:cursor-not-allowed disabled:opacity-40"
              >
                {workspaceBusy ? "Adding…" : "Add project folder…"}
              </button>
              <button
                onClick={handleWorkspaceRemove}
                disabled={!canRemoveCurrentWorkspace || workspaceBusy}
                className="w-full rounded-xl px-2.5 py-2 text-left text-sm font-medium hover:opacity-85 disabled:cursor-not-allowed disabled:opacity-40"
              >
                Remove current folder from list
              </button>
              <button
                onClick={() => {
                  setShowWorkspaceMenu(false);
                  onOpenSettings();
                }}
                className="w-full rounded-xl px-2.5 py-2 text-left text-sm opacity-65 hover:opacity-85"
              >
                Workspace Settings…
              </button>
              {workspaceError && (
                <div className="px-2.5 pt-1 text-xs text-red-400">
                  {workspaceError}
                </div>
              )}
            </div>
          </div>
        )}
      </div>

      <div className="border-b px-3.5 py-3.5" style={{ borderColor: "rgba(255,255,255,0.08)" }}>
        <div className="flex items-center gap-3">
          <div
            className="flex h-10 w-10 items-center justify-center rounded-full text-base font-semibold"
            style={{ background: "rgba(87,182,122,0.92)", color: "#13201a" }}
          >
            {displayName.slice(0, 1).toUpperCase()}
          </div>
          <div className="min-w-0 flex-1">
            <div className="truncate text-base font-semibold">{displayName}</div>
            <div className="truncate text-sm opacity-65">{displayName.toLowerCase()}</div>
          </div>
          {/* No presence dot here: there's no self-presence signal to reflect,
              and a permanently-green dot reads as a fake status. */}
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-2 py-2.5">
        <SidebarSection
          title="Chats"
          action={
            <button
              onClick={handleNew}
              className="flex h-6 w-6 items-center justify-center rounded-md opacity-80 transition-opacity hover:opacity-100"
              title="New chat"
              aria-label="New chat"
            >
              <IconPlus size={15} />
            </button>
          }
          trailing={
            <button className="text-xs underline opacity-60" onClick={() => setShowArchived((v) => !v)}>
              {showArchived ? "active" : "archived"}
            </button>
          }
        >
          <div className="space-y-1">
            {visible.map((c) => (
              <div key={c.id} className="group relative">
                <button
                  onClick={() => onSelect(c.id)}
                    className="w-full rounded-2xl px-3 py-2.5 text-left"
                  style={{
                    background: c.id === selectedId && view === "workspace" ? "rgba(87,161,168,0.18)" : "transparent",
                  }}
                >
                  <div className="truncate text-base font-semibold">{c.title}</div>
                  <div className="mt-1 text-sm opacity-60">{c.messageCount} messages</div>
                </button>
                <button
                  className="absolute right-2 top-2 rounded-md px-2 py-1 opacity-0 transition-opacity group-hover:opacity-70"
                  onClick={(e) => {
                    e.stopPropagation();
                    setMenuFor((m) => (m === c.id ? null : c.id));
                  }}
                  title="Chat options"
                  aria-label="Chat options"
                >
                  <IconEllipsis size={15} />
                </button>
                {menuFor === c.id && (
                  <div
                    className="absolute right-2 top-10 z-10 w-32 rounded-xl border py-1 text-xs shadow-lg"
                    style={{ background: "var(--hive-panel)", color: "var(--hive-ink)", borderColor: "var(--hive-line)" }}
                  >
                    <button className="block w-full px-3 py-1.5 text-left hover:opacity-70" onClick={() => handleArchive(c)}>
                      {c.archived ? "Restore" : "Archive"}
                    </button>
                    <button className="block w-full px-3 py-1.5 text-left text-red-500 hover:opacity-70" onClick={() => handleDelete(c)}>
                      Delete…
                    </button>
                  </div>
                )}
              </div>
            ))}
            {chats.isLoading && <SkeletonRows rows={5} />}
            {!chats.isLoading && visible.length === 0 && !showArchived && (
              <div className="rounded-2xl border px-3 py-4 text-sm" style={{ borderColor: "var(--hive-line)" }}>
                <div className="opacity-60">No chats yet.</div>
                <button
                  onClick={handleNew}
                  className="mt-2 rounded-lg px-3 py-1.5 text-xs font-medium text-white"
                  style={{ background: "var(--hive-accent-cool)" }}
                >
                  Start a chat
                </button>
              </div>
            )}
            {!chats.isLoading && visible.length === 0 && showArchived && (
              <div className="px-3 py-2 text-sm opacity-55">Nothing archived.</div>
            )}
          </div>
        </SidebarSection>

        <SidebarSection title="People" collapsible>
          <div className="space-y-2 px-1">
            {/* Hide "you" — you're shown in the identity card above; People lists
                collaborators. */}
            {(members.data ?? [])
              .filter((m) => !m.isSelf)
              .slice(0, 4)
              .map((member) => (
                <button
                  key={member.id}
                  onClick={() => onOpenUtilityPane("people")}
                  className="flex w-full items-center gap-3 rounded-xl px-2 py-2 text-left hover:bg-white/5"
                >
                  <div className="flex h-9 w-9 items-center justify-center rounded-full bg-white/18 text-sm font-semibold">
                    {member.displayName.slice(0, 1).toUpperCase()}
                  </div>
                  <div className="min-w-0">
                    <div className="truncate text-base font-medium">{member.displayName}</div>
                    <div className="truncate text-sm opacity-60">{member.title || member.role}</div>
                  </div>
                </button>
              ))}
            {(members.data ?? []).filter((m) => !m.isSelf).length === 0 && (
              <button
                onClick={() => onOpenUtilityPane("people")}
                className="flex w-full items-center justify-between rounded-xl px-2 py-1.5 text-left text-sm opacity-55 transition-opacity hover:bg-white/5 hover:opacity-90"
              >
                Invite collaborators
                <IconChevronRight size={13} />
              </button>
            )}
          </div>
        </SidebarSection>

        <SidebarSection title="Agents" collapsible>
          <div className="space-y-2 px-1">
            {(agents.data ?? []).slice(0, 4).map((agent) => (
              <button
                key={agent.id}
                onClick={() => onOpenUtilityPane("tools")}
                className="flex w-full items-center gap-3 rounded-xl px-2 py-2 text-left hover:bg-white/5"
              >
                <div
                  className="flex h-9 w-9 items-center justify-center rounded-2xl text-sm font-semibold"
                  style={{ background: "rgba(214,158,87,0.18)" }}
                >
                  {agent.name.slice(0, 1).toUpperCase()}
                </div>
                <div className="min-w-0">
                  <div className="truncate text-base font-medium">@{agent.name}</div>
                  <div className="truncate text-sm opacity-60">{agent.runtimeId}</div>
                </div>
              </button>
            ))}
            {(agents.data ?? []).length === 0 && (
              <button
                onClick={() => onOpenUtilityPane("tools")}
                className="flex w-full items-center justify-between rounded-xl px-2 py-1.5 text-left text-sm opacity-55 transition-opacity hover:bg-white/5 hover:opacity-90"
              >
                Add an agent
                <IconChevronRight size={13} />
              </button>
            )}
          </div>
        </SidebarSection>

        <SidebarNavRow
          label="Vaults"
          count={(vaults.data ?? []).length}
          active={utilityPane === "vaults"}
          onClick={() => onOpenUtilityPane("vaults")}
        />
        <SidebarNavRow
          label="Skills"
          count={(skills.data ?? []).length}
          active={utilityPane === "skills"}
          onClick={() => onOpenUtilityPane("skills")}
        />
        <SidebarNavRow
          label="Activity"
          active={utilityPane === "activity"}
          onClick={() => onOpenUtilityPane("activity")}
        />
      </div>

      {/* Settings moved to the workspace rail's bottom (gear) so it survives
          sidebar dismissal; onOpenSettings is still used by the workspace
          menu's "Workspace Settings…" entry above. */}
    </aside>
  );
}

function SidebarSection({
  title,
  action,
  trailing,
  children,
  collapsible = false,
}: {
  title: string;
  action?: ReactNode;
  trailing?: ReactNode;
  children: ReactNode;
  collapsible?: boolean;
}) {
  // Collapse state persists per section so it survives remounts/restarts.
  const storageKey = `hive.sidebar.collapsed.${title}`;
  const [collapsed, setCollapsed] = useState(
    () => typeof window !== "undefined" && window.localStorage.getItem(storageKey) === "1",
  );
  const toggle = () =>
    setCollapsed((c) => {
      const next = !c;
      window.localStorage.setItem(storageKey, next ? "1" : "0");
      return next;
    });
  return (
    <section className="mb-4">
      <div className="mb-2 flex items-center px-2">
        {collapsible ? (
          <button
            onClick={toggle}
            aria-expanded={!collapsed}
            className="flex items-center gap-1 text-xs font-semibold uppercase tracking-[0.18em] opacity-65 hover:opacity-100"
          >
            <span className="opacity-70">
              {collapsed ? <IconChevronRight size={11} /> : <IconChevronDown size={11} />}
            </span>
            {title}
          </button>
        ) : (
          <div className="text-xs font-semibold uppercase tracking-[0.18em] opacity-65">{title}</div>
        )}
        <div className="ml-auto flex items-center gap-2">{trailing}{action}</div>
      </div>
      {!collapsed && children}
    </section>
  );
}

function SidebarNavRow({
  label,
  count,
  active,
  onClick,
}: {
  label: string;
  count?: number;
  active?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className="mb-2 flex w-full items-center justify-between rounded-2xl px-4 py-2.5 text-left hover:bg-white/6"
      style={{ background: active ? "rgba(255,255,255,0.08)" : "transparent" }}
    >
      <span className="text-base font-medium">{label}</span>
      <span className="flex items-center text-sm opacity-60">
        {typeof count === "number" ? count : <IconChevronRight size={13} />}
      </span>
    </button>
  );
}
