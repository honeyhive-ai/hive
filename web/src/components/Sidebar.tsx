import { useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  archiveChat,
  createChat,
  deleteChat,
  getAppSettings,
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
import { Avatar } from "@/components/Avatar";
import { confirmDialog } from "@/components/Dialog";
import {
  NavRow,
  ChatRow,
  SectionCap,
  EmptyHint,
  Popover,
  PopoverHeader,
  PopoverItem,
} from "@/components/ui";
import {
  IconPlus,
  IconCheck,
  IconMessage,
  IconUsers,
  IconWrench,
  IconInbox,
  IconActivity,
  IconSparkle,
  IconBook,
  IconFlow,
  IconGrid,
  IconGear,
} from "@/lib/icons";

/// Last path segment of a folder path (cross-platform separators).
function folderBasename(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
}

/// Compact relative time for chat rows ("now", "5m", "3h", "2d", "4w").
function relTime(iso: string): string {
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return "";
  const s = Math.floor((Date.now() - t) / 1000);
  if (s < 60) return "now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  const d = Math.floor(h / 24);
  if (d < 7) return `${d}d`;
  const w = Math.floor(d / 7);
  if (w < 5) return `${w}w`;
  const mo = Math.floor(d / 30);
  if (mo < 12) return `${mo}mo`;
  return `${Math.floor(d / 365)}y`;
}

type ChatFilter = "all" | "recent";

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
  const settings = useQuery({ queryKey: ["settings"], queryFn: getAppSettings });
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
  const [filter, setFilter] = useState<ChatFilter>("all");
  const [search, setSearch] = useState("");
  const [menuFor, setMenuFor] = useState<string | null>(null);
  const menuAnchor = useRef<HTMLElement | null>(null);
  const [showWorkspaceMenu, setShowWorkspaceMenu] = useState(false);
  const wsAnchor = useRef<HTMLButtonElement | null>(null);
  const [workspaceError, setWorkspaceError] = useState<string | null>(null);
  const [workspaceBusy, setWorkspaceBusy] = useState(false);

  const all: ChatSummaryDto[] = chats.data ?? [];
  const visible = useMemo(() => {
    const q = search.trim().toLowerCase();
    let out = all.filter((c) => c.archived === showArchived);
    if (q) out = out.filter((c) => (c.title || "Untitled").toLowerCase().includes(q));
    if (filter === "recent") {
      out = [...out].sort(
        (a, b) => new Date(b.lastActivityAt).getTime() - new Date(a.lastActivityAt).getTime(),
      );
    }
    return out;
  }, [all, showArchived, search, filter]);
  const menuChat = menuFor ? visible.find((c) => c.id === menuFor) ?? null : null;
  const canRemoveCurrentWorkspace = workspacePath.trim().length > 0;
  const memberCount = (members.data ?? []).filter((m) => !m.isSelf).length;
  const deviceName = settings.data?.deviceName ?? "";

  async function handleWorkspaceAdd() {
    if (workspaceBusy) return;
    setWorkspaceBusy(true);
    setWorkspaceError(null);
    try {
      await onAddWorkspace();
      setShowWorkspaceMenu(false);
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
      setShowWorkspaceMenu(false);
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
    setMenuFor(null);
    const ok = await confirmDialog(`Permanently delete "${c.title || "Untitled"}"? This cannot be undone.`, {
      danger: true,
      confirmLabel: "Delete",
    });
    if (!ok) return;
    try {
      await deleteChat(c.id);
      qc.invalidateQueries({ queryKey: ["chats"] });
    } catch (e) {
      toast.error(`Couldn't delete chat: ${errMsg(e)}`);
    }
  }

  // Inside the sidebar, text rides the dedicated sidebar-ink tokens (spec §2)
  // and hover is a light overlay — so the shared primitives (which reference
  // --hive-ink/--hive-overlay) render correctly on the dark gradient without
  // being parameterized.
  const asideStyle: Record<string, string | number> = {
    width,
    background: "linear-gradient(180deg, var(--hive-sidebar-top), var(--hive-sidebar-bottom))",
    color: "var(--hive-sidebar-ink)",
    borderColor: "var(--hive-line)",
    "--hive-ink": "var(--hive-sidebar-ink)",
    "--hive-ink-soft": "var(--hive-sidebar-ink-muted)",
    "--hive-overlay": "color-mix(in srgb, var(--hive-sidebar-ink) 10%, transparent)",
  };

  return (
    <aside className="flex shrink-0 flex-col border-r" style={asideStyle as CSSProperties}>
      {/* 1 — Workspace header: one identity row (spec §6.2). */}
      <div className="px-2.5 pt-3 pb-1.5">
        <button
          ref={wsAnchor}
          onClick={() => setShowWorkspaceMenu((v) => !v)}
          title="Switch workspace folder"
          className="flex w-full items-center gap-2.5 rounded-xl px-1.5 py-1.5 text-left transition-colors hover:bg-[color:var(--hive-overlay)]"
        >
          <span
            className="grid h-[26px] w-[26px] shrink-0 place-items-center rounded-lg border"
            style={{
              borderColor: "var(--hive-line)",
              background: "color-mix(in srgb, var(--hive-sidebar-ink) 8%, transparent)",
            }}
            aria-hidden
          >
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
              <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
            </svg>
          </span>
          <span className="min-w-0 flex-1">
            <span className="block truncate text-[13px] font-semibold" style={{ letterSpacing: "-0.01em" }}>
              {workspaceLabel}
            </span>
            <span
              className="block truncate font-mono text-[11px]"
              style={{ color: "var(--hive-sidebar-ink-muted)" }}
            >
              {workspacePath ? folderBasename(workspacePath) : "No project folder"}
            </span>
          </span>
          <span style={{ color: "var(--hive-sidebar-ink-muted)" }} aria-hidden>
            <IconChevronDownGlyph />
          </span>
        </button>
        <div
          className="mt-0.5 flex items-center gap-1.5 px-1.5 text-[11px]"
          style={{ color: "var(--hive-sidebar-ink-muted)" }}
        >
          <span className="h-1.5 w-1.5 rounded-full bg-current opacity-60" aria-hidden />
          <span className="truncate">Runs on this device</span>
        </div>
        {showWorkspaceMenu && (
          <Popover anchorRef={wsAnchor} minWidth={width - 24} onDismiss={() => setShowWorkspaceMenu(false)}>
            <PopoverHeader>Project folders</PopoverHeader>
            {knownWorkspaces.map((path) => (
              <PopoverItem
                key={path}
                glyph={path === workspacePath ? <IconCheck size={13} /> : <span className="h-1 w-1 rounded-full bg-current opacity-50" />}
                label={folderBasename(path)}
                active={path === workspacePath}
                onSelect={async () => {
                  await onSwitchWorkspace(path);
                  setShowWorkspaceMenu(false);
                }}
              />
            ))}
            <div className="my-1 h-px" style={{ background: "var(--hive-line)" }} />
            <PopoverItem label={workspaceBusy ? "Adding…" : "Add project folder…"} onSelect={handleWorkspaceAdd} />
            {canRemoveCurrentWorkspace && (
              <PopoverItem label="Remove current folder" onSelect={handleWorkspaceRemove} />
            )}
            <PopoverItem label="Workspace settings…" onSelect={() => { setShowWorkspaceMenu(false); onOpenSettings(); }} />
            {workspaceError && (
              <div className="px-2 pt-1 text-[11px]" style={{ color: "var(--hive-danger)" }}>{workspaceError}</div>
            )}
          </Popover>
        )}
      </div>

      {/* 2 — Search across chats. */}
      <div className="px-2.5 pb-1">
        <input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search chats"
          className="w-full rounded-lg border px-2.5 py-1.5 text-[12.5px] outline-none placeholder:opacity-60 focus:border-[color:var(--hive-accent-cool)]"
          style={{
            borderColor: "var(--hive-line)",
            background: "color-mix(in srgb, var(--hive-sidebar-ink) 8%, transparent)",
            color: "var(--hive-sidebar-ink)",
          }}
        />
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 py-1">
        {/* 3 — Views (cross-channel filters). "Needs review" awaits a per-chat
            review signal on the summary DTO; shipping the two backed filters. */}
        <NavRow icon={<IconMessage size={15} />} label="All chats" active={filter === "all"} onClick={() => setFilter("all")} count={all.filter((c) => !c.archived).length} />
        <NavRow icon={<IconActivity size={15} />} label="Recent" active={filter === "recent"} onClick={() => setFilter("recent")} />

        {/* 4 — Chats. (Channel grouping is deferred to the config-hoist schema.) */}
        <div className="mt-2">
          <SectionCap
            action={
              <div className="flex items-center gap-1.5">
                <button
                  className="text-[11px] opacity-70 hover:opacity-100"
                  onClick={() => setShowArchived((v) => !v)}
                  title={showArchived ? "Show active chats" : "Show archived chats"}
                >
                  {showArchived ? "Archived" : "Active"}
                </button>
                <button
                  onClick={handleNew}
                  className="grid h-5 w-5 place-items-center rounded-md opacity-80 hover:bg-[color:var(--hive-overlay)] hover:opacity-100"
                  title="New chat"
                  aria-label="New chat"
                >
                  <IconPlus size={14} />
                </button>
              </div>
            }
          >
            {showArchived ? "Archived" : "Chats"}
          </SectionCap>

          {chats.isLoading && <SkeletonRows rows={5} />}
          {!chats.isLoading &&
            visible.map((c) => (
              <ChatRow
                key={c.id}
                title={c.title || "Untitled"}
                when={relTime(c.lastActivityAt)}
                active={c.id === selectedId && view === "workspace"}
                onClick={() => onSelect(c.id)}
                onOptions={(el) => {
                  menuAnchor.current = el;
                  setMenuFor(c.id);
                }}
              />
            ))}
          {!chats.isLoading && visible.length === 0 && search.trim() && (
            <EmptyHint text={`No chats match “${search.trim()}”.`} />
          )}
          {!chats.isLoading && visible.length === 0 && !search.trim() && !showArchived && (
            <EmptyHint
              text="No chats yet."
              action={
                <button
                  onClick={handleNew}
                  className="rounded-lg px-3 py-1.5 text-xs font-medium text-white"
                  style={{ background: "var(--hive-accent-cool)" }}
                >
                  Start a chat
                </button>
              }
            />
          )}
          {!chats.isLoading && visible.length === 0 && !search.trim() && showArchived && (
            <div className="px-2.5 py-2 text-[12px]" style={{ color: "var(--hive-sidebar-ink-muted)" }}>
              Nothing archived.
            </div>
          )}
        </div>

        {/* 5 — Workspace panes, one NavRow each with live counts. */}
        <div className="mt-2">
          <SectionCap>Workspace</SectionCap>
          <NavRow icon={<IconUsers size={15} />} label="People" count={memberCount} active={utilityPane === "people"} onClick={() => onOpenUtilityPane("people")} />
          <NavRow icon={<IconWrench size={15} />} label="Agents & tools" count={(agents.data ?? []).length} active={utilityPane === "tools"} onClick={() => onOpenUtilityPane("tools")} />
          <NavRow icon={<IconInbox size={15} />} label="Review" active={utilityPane === "review"} onClick={() => onOpenUtilityPane("review")} />
          <NavRow icon={<IconActivity size={15} />} label="Context" active={utilityPane === "context"} onClick={() => onOpenUtilityPane("context")} />
          <NavRow icon={<IconSparkle size={15} />} label="Skills" count={(skills.data ?? []).length} active={utilityPane === "skills"} onClick={() => onOpenUtilityPane("skills")} />
          <NavRow icon={<IconBook size={15} />} label="Vaults" count={(vaults.data ?? []).length} active={utilityPane === "vaults"} onClick={() => onOpenUtilityPane("vaults")} />
          <NavRow icon={<IconFlow size={15} />} label="Workflows" active={utilityPane === "workflows"} onClick={() => onOpenUtilityPane("workflows")} />
          <NavRow icon={<IconGrid size={15} />} label="Activity" active={utilityPane === "activity"} onClick={() => onOpenUtilityPane("activity")} />
        </div>
      </div>

      {/* Chat options menu — one shared Popover for whichever row is open. */}
      {menuChat && (
        <Popover anchorRef={menuAnchor} align="right" minWidth={150} onDismiss={() => setMenuFor(null)}>
          <PopoverItem label={menuChat.archived ? "Restore" : "Archive"} onSelect={() => handleArchive(menuChat)} />
          <PopoverItem label="Delete…" danger onSelect={() => handleDelete(menuChat)} />
        </Popover>
      )}

      {/* 6 — Account row. */}
      <button
        onClick={onOpenSettings}
        className="flex items-center gap-2.5 border-t px-3 py-2.5 text-left transition-colors hover:bg-[color:var(--hive-overlay)]"
        style={{ borderColor: "var(--hive-line)" }}
        title="Open settings"
      >
        <Avatar name={displayName} url={settings.data?.avatarUrl} kind="human" size={26} />
        <span className="min-w-0 flex-1">
          <span className="block truncate text-[12.5px] font-semibold">{displayName}</span>
          <span className="block truncate text-[11px]" style={{ color: "var(--hive-sidebar-ink-muted)" }}>
            {deviceName || "This device"}
          </span>
        </span>
        <span style={{ color: "var(--hive-sidebar-ink-muted)" }} aria-hidden>
          <IconGear size={15} />
        </span>
      </button>
    </aside>
  );
}

/// Small downward chevron used in the workspace header (the icon set's
/// IconChevronDown expects the same size prop).
function IconChevronDownGlyph() {
  return (
    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="M6 9l6 6 6-6" />
    </svg>
  );
}
