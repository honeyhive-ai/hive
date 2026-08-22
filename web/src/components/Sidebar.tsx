import { useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  archiveChat,
  archiveChannel,
  createChat,
  createChannel,
  createChatInChannel,
  deleteChat,
  getAppSettings,
  getGitStatus,
  listAgents,
  listChannels,
  listChats,
  listMembers,
  listMentionStates,
  listSkills,
  listVaults,
  renameChannel,
  type ChannelDto,
  type ChatSummaryDto,
} from "@/lib/ipc";
import type { UtilityPane } from "@/components/RightRail";
import { PANES } from "@/lib/panes";
import { toast, errMsg } from "@/components/Toast";
import { SkeletonRows } from "@/components/Skeleton";
import { Avatar } from "@/components/Avatar";
import { confirmDialog, promptDialog } from "@/components/Dialog";
import { confirmThen } from "@/lib/confirm";
import { relTime } from "@/lib/time";
import {
  NavRow,
  ChatRow,
  SectionCap,
  EmptyHint,
  ErrorState,
  Popover,
  PopoverHeader,
  PopoverItem,
  SELECT_TINT,
} from "@/components/ui";
import {
  IconPlus,
  IconCheck,
  IconMessage,
  IconActivity,
  IconGear,
  IconEllipsis,
  IconChevronRight,
  IconChevronDown,
} from "@/lib/icons";

/// Last path segment of a folder path (cross-platform separators).
function folderBasename(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
}

/// Compact relative time for chat rows ("now", "5m", "3h", "2d", "4w").
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
  activeWorkspaceId,
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
  activeWorkspaceId: string;
  knownWorkspaces: string[];
  displayName: string;
  utilityPane: UtilityPane;
}) {
  const qc = useQueryClient();
  // Scope chats + channels by the *active workspace id*. listChats/listChannels
  // return the active workspace's data, so keying globally let one workspace's
  // cache bleed into another. Keying on the id (not the folder path — a team/room
  // workspace has no folder, so the path wouldn't change on switch and the view
  // would stay stale) makes each workspace its own cache entry, so a switch shows
  // the right data. invalidateQueries(["chats"]) still prefix-matches these.
  const chats = useQuery({ queryKey: ["chats", activeWorkspaceId], queryFn: listChats });
  const channels = useQuery({ queryKey: ["channels", activeWorkspaceId], queryFn: listChannels });
  const mentionStates = useQuery({ queryKey: ["mention-states"], queryFn: listMentionStates });
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

  // Git branch for the workspace header line 2 (F14) — otherwise it just
  // repeats the folder name. Keyed on the path so switching workspaces refetches;
  // shelling out to git is why it's lazy, not baked into app settings.
  const git = useQuery({
    queryKey: ["git-status", workspacePath],
    queryFn: () => getGitStatus(),
    enabled: workspacePath.trim().length > 0,
    staleTime: 15_000,
  });
  const gitBranch = git.data?.branch ?? null;

  const [showArchived, setShowArchived] = useState(false);
  // `null` = no cross-channel View active → the channel tree is shown. A View
  // ("all"/"recent") forces the flat, cross-channel list (spec §11 rule 8).
  const [filter, setFilter] = useState<ChatFilter | null>(null);
  const [search, setSearch] = useState("");
  const [menuFor, setMenuFor] = useState<string | null>(null);
  const menuAnchor = useRef<HTMLElement | null>(null);
  const [channelMenuFor, setChannelMenuFor] = useState<string | null>(null);
  const channelMenuAnchor = useRef<HTMLElement | null>(null);
  // Per-channel collapse, persisted in localStorage (spec §6.2). Absent key =
  // expanded; "1" = collapsed. State mirrors the store so toggles re-render.
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});
  const isCollapsed = (id: string): boolean =>
    id in collapsed
      ? collapsed[id]
      : window.localStorage.getItem(`hive.sidebar.channel.${id}`) === "1";
  const setChannelCollapsed = (id: string, next: boolean) => {
    window.localStorage.setItem(`hive.sidebar.channel.${id}`, next ? "1" : "0");
    setCollapsed((prev) => ({ ...prev, [id]: next }));
  };
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

  // ── Unread @-mention highlights ────────────────────────────────────────────
  // Read state is per-device UI state, not workspace data — it lives in
  // localStorage (`hive.read.<sessionId>` = the message count when the chat was
  // last opened), mirrored into React state so updates re-render. A mention at
  // `lastMentionOrdinal` is unread when it sits past that cursor.
  const [readCursors, setReadCursors] = useState<Record<string, number>>({});
  const cursorOf = (id: string): number =>
    id in readCursors
      ? readCursors[id]
      : Number(window.localStorage.getItem(`hive.read.${id}`)) || 0;

  // Opening a chat (and any new message while it's open) marks it read.
  const selectedCount = useMemo(
    () => all.find((c) => c.id === selectedId)?.messageCount ?? null,
    [all, selectedId],
  );
  useEffect(() => {
    if (!selectedId || selectedCount == null) return;
    if (selectedCount > cursorOf(selectedId)) {
      window.localStorage.setItem(`hive.read.${selectedId}`, String(selectedCount));
      setReadCursors((prev) => ({ ...prev, [selectedId]: selectedCount }));
      // Nudge sibling views (WorkspaceRail's cross-workspace mention badge) that
      // read a `hive.read.*` cursor but don't share our React state.
      window.dispatchEvent(new CustomEvent("hive:read-cursor"));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId, selectedCount, readCursors]);

  // Chats (and their channels) holding an unread self-mention. The open chat is
  // always current, so it never highlights.
  const { unreadSessions, unreadChannels } = useMemo(() => {
    const cur = (id: string) =>
      id in readCursors
        ? readCursors[id]
        : Number(window.localStorage.getItem(`hive.read.${id}`)) || 0;
    const sessions = new Set<string>();
    const chans = new Set<string>();
    for (const m of mentionStates.data ?? []) {
      if (m.sessionId === selectedId) continue;
      if (m.lastMentionOrdinal > cur(m.sessionId)) {
        sessions.add(m.sessionId);
        if (m.channelId) chans.add(m.channelId);
      }
    }
    return { unreadSessions: sessions, unreadChannels: chans };
  }, [mentionStates.data, readCursors, selectedId]);

  // Active channels sorted by position (archived channels are hidden — their
  // history stays attributable but they leave the list, spec §11 rule 7).
  const openChannels = useMemo(
    () =>
      (channels.data ?? [])
        .filter((c) => !c.archived)
        .sort((a, b) => a.position - b.position),
    [channels.data],
  );
  // Chats grouped by channel (default chats excluded — the header IS that
  // conversation, §11 rule 1) and the unfiled remainder (empty channelId).
  const focusedByChannel = useMemo(() => {
    const map = new Map<string, ChatSummaryDto[]>();
    for (const c of all) {
      if (c.archived !== showArchived) continue;
      if (!c.channelId || c.isChannelDefault) continue;
      const list = map.get(c.channelId);
      if (list) list.push(c);
      else map.set(c.channelId, [c]);
    }
    return map;
  }, [all, showArchived]);
  const unfiled = useMemo(
    () => all.filter((c) => !c.channelId && c.archived === showArchived),
    [all, showArchived],
  );

  // A View (or an active search) forces the flat cross-channel list; otherwise
  // the channel tree is shown. With no channels at all the tree degrades to the
  // flat list plus a "+ New channel" affordance (current behavior preserved).
  const flatMode = filter !== null || search.trim().length > 0;
  const hasChannels = openChannels.length > 0;
  const menuChat = menuFor ? all.find((c) => c.id === menuFor) ?? null : null;
  const menuChannel = channelMenuFor
    ? openChannels.find((c) => c.id === channelMenuFor) ?? null
    : null;
  const canRemoveCurrentWorkspace = workspacePath.trim().length > 0;
  const memberCount = (members.data ?? []).filter((m) => !m.isSelf).length;
  // Live counts per pane, keyed by the shared PANES id. Panes without a count
  // (Review/Context/Workflows/Activity) simply omit the badge.
  const paneCounts: Partial<Record<UtilityPane, number>> = {
    people: memberCount,
    // +1 for the always-present default agent (@hive) so it never reads 0.
    tools: (agents.data ?? []).length + 1,
    skills: (skills.data ?? []).length,
    vaults: (vaults.data ?? []).length,
  };
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
      // File the new chat into the current channel so it isn't stranded in the
      // Unfiled bucket (F11): the open chat's channel, else the first (default,
      // #general) channel. Only truly channel-less workspaces make it unfiled.
      const currentChannelId = all.find((c) => c.id === selectedId)?.channelId?.trim();
      const targetChannelId = currentChannelId || openChannels[0]?.id || "";
      const created = targetChannelId
        ? await createChatInChannel(targetChannelId, "")
        : await createChat("");
      if (targetChannelId) setChannelCollapsed(targetChannelId, false);
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

  async function handleNewChannel() {
    const name = await promptDialog("Name this channel", { placeholder: "e.g. design" });
    if (!name || !name.trim()) return;
    try {
      await createChannel(name.trim());
      await Promise.all([
        qc.invalidateQueries({ queryKey: ["channels"] }),
        qc.invalidateQueries({ queryKey: ["chats"] }),
      ]);
    } catch (e) {
      toast.error(`Couldn't create channel: ${errMsg(e)}`);
    }
  }

  async function handleNewChatIn(ch: ChannelDto) {
    try {
      const created = await createChatInChannel(ch.id, "");
      setChannelCollapsed(ch.id, false);
      await qc.invalidateQueries({ queryKey: ["chats"] });
      onSelect(created.id);
    } catch (e) {
      toast.error(`Couldn't create chat: ${errMsg(e)}`);
    }
  }

  async function handleRenameChannel(ch: ChannelDto) {
    setChannelMenuFor(null);
    const name = await promptDialog("Rename channel", { defaultValue: ch.name });
    if (!name || !name.trim() || name.trim() === ch.name) return;
    try {
      await renameChannel(ch.id, name.trim(), ch.purpose ?? undefined);
      await qc.invalidateQueries({ queryKey: ["channels"] });
    } catch (e) {
      toast.error(`Couldn't rename channel: ${errMsg(e)}`);
    }
  }

  function handleArchiveChannel(ch: ChannelDto) {
    setChannelMenuFor(null);
    confirmThen(`Archive #${ch.name}? Its chats archive with it.`, async () => {
      try {
        await archiveChannel(ch.id, true);
        await Promise.all([
          qc.invalidateQueries({ queryKey: ["channels"] }),
          qc.invalidateQueries({ queryKey: ["chats"] }),
        ]);
      } catch (e) {
        toast.error(`Couldn't archive channel: ${errMsg(e)}`);
      }
    });
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
              {/* Line 2 is the repo state, not a second copy of the name (F14):
                  "<repo> · <branch>" when on a git repo, else the folder or a
                  no-folder hint. */}
              {workspacePath
                ? gitBranch
                  ? `${folderBasename(workspacePath)} · ${gitBranch}`
                  : folderBasename(workspacePath)
                : "No project folder"}
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
        {/* 3 — Views (cross-channel filters) sit ABOVE the channel tree (spec
            §11 rule 8). Clicking an active View toggles back to the tree.
            "Needs review" awaits a per-chat review signal on the summary DTO. */}
        <NavRow
          icon={<IconMessage size={15} />}
          label="All chats"
          active={filter === "all"}
          onClick={() => setFilter((f) => (f === "all" ? null : "all"))}
          // Count only discrete chat rows — a channel's default chat renders as
          // the channel header, not a row, so counting it made "3" show beside
          // two visible chats (F4: a count must equal what clicking it shows).
          count={all.filter((c) => !c.archived && !c.isChannelDefault).length}
        />
        <NavRow
          icon={<IconActivity size={15} />}
          label="Recent"
          active={filter === "recent"}
          onClick={() => setFilter((f) => (f === "recent" ? null : "recent"))}
        />

        {/* 4 — Chats. A View (or search) forces the flat cross-channel list;
            otherwise the channel tree (spec §11.1). With no channels the tree
            degrades to the flat list + a "+ New channel" affordance. */}
        <div className="mt-2">
          {(chats.isLoading || channels.isLoading) && <SkeletonRows rows={5} />}

          {/* A failed read must not render as "No chats yet." (which reads as
              data loss) — surface an error with a retry instead. */}
          {!chats.isLoading && !channels.isLoading && (chats.isError || channels.isError) && (
            <ErrorState
              text="Couldn't load chats."
              onRetry={() => {
                void chats.refetch();
                void channels.refetch();
              }}
            />
          )}

          {/* 4a — Flat, cross-channel list (a View is active, searching, or the
              workspace has no channels yet). Preserves the original behavior. */}
          {!chats.isLoading && !channels.isLoading && !chats.isError && !channels.isError && (flatMode || !hasChannels) && (
            <>
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

              {visible.map((c) => (
                <ChatRow
                  key={c.id}
                  title={c.title || "Untitled"}
                  when={relTime(c.lastActivityAt)}
                  unread={unreadSessions.has(c.id) ? 1 : 0}
                  active={c.id === selectedId && view === "workspace"}
                  onClick={() => onSelect(c.id)}
                  onOptions={(el) => {
                    menuAnchor.current = el;
                    setMenuFor(c.id);
                  }}
                />
              ))}
              {visible.length === 0 && search.trim() && (
                <EmptyHint text={`No chats match “${search.trim()}”.`} />
              )}
              {visible.length === 0 && !search.trim() && !showArchived && (
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
              {visible.length === 0 && !search.trim() && showArchived && (
                <div className="px-2.5 py-2 text-[12px]" style={{ color: "var(--hive-sidebar-ink-muted)" }}>
                  Nothing archived.
                </div>
              )}

              {/* Degraded tree: no channels yet, but keep the create path. */}
              {!flatMode && !hasChannels && <NewChannelRow onClick={handleNewChannel} />}
            </>
          )}

          {/* 4b — The channel tree. */}
          {!chats.isLoading && !channels.isLoading && !chats.isError && !channels.isError && !flatMode && hasChannels && (
            <>
              <SectionCap
                action={
                  <button
                    className="text-[11px] opacity-70 hover:opacity-100"
                    onClick={() => setShowArchived((v) => !v)}
                    title={showArchived ? "Show active chats" : "Show archived chats"}
                  >
                    {showArchived ? "Archived" : "Active"}
                  </button>
                }
              >
                Channels
              </SectionCap>

              {openChannels.map((ch) => {
                const collapsedNow = isCollapsed(ch.id);
                const active = ch.defaultChatId === selectedId && view === "workspace";
                const focused = focusedByChannel.get(ch.id) ?? [];
                const hasMention = unreadChannels.has(ch.id);
                return (
                  <div key={ch.id}>
                    {/* Header: chevron collapses; the name opens the default chat
                        (which IS this conversation — no child row, §11 rule 1). */}
                    <div
                      className="group/chan relative flex items-center rounded-xl pr-1 transition-colors hover:bg-[color:var(--hive-overlay)]"
                      style={active ? { background: SELECT_TINT } : undefined}
                    >
                      <button
                        onClick={() => setChannelCollapsed(ch.id, !collapsedNow)}
                        className="grid h-[30px] w-6 shrink-0 place-items-center rounded-lg"
                        style={{ color: "var(--hive-ink-soft)" }}
                        title={collapsedNow ? "Expand" : "Collapse"}
                        aria-label={collapsedNow ? `Expand #${ch.name}` : `Collapse #${ch.name}`}
                      >
                        {collapsedNow ? <IconChevronRight size={14} /> : <IconChevronDown size={14} />}
                      </button>
                      <button
                        onClick={() => onSelect(ch.defaultChatId)}
                        className="flex h-[30px] min-w-0 flex-1 items-center text-left"
                        title={hasMention ? `#${ch.name} — you were mentioned` : ch.purpose ?? `#${ch.name}`}
                      >
                        {hasMention && (
                          <span
                            className="mr-1.5 h-1.5 w-1.5 shrink-0 rounded-full"
                            style={{ background: "var(--hive-accent-cool)" }}
                            aria-label="unread mention"
                          />
                        )}
                        <span
                          className="min-w-0 flex-1 truncate text-[13px]"
                          style={{
                            fontWeight: active || hasMention ? 700 : 590,
                            letterSpacing: "-0.01em",
                            color: "var(--hive-ink)",
                            // Channels read as slugs (the "#" prefix implies it),
                            // so render lowercase regardless of stored casing —
                            // "#General" → "#general" (F15).
                            textTransform: "lowercase",
                          }}
                        >
                          <span style={{ color: "var(--hive-ink-soft)" }}>#</span>
                          {ch.name}
                        </span>
                      </button>
                      <div className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover/chan:opacity-100">
                        <button
                          onClick={() => handleNewChatIn(ch)}
                          className="grid h-[22px] w-[22px] place-items-center rounded-md hover:bg-[color:var(--hive-overlay)]"
                          style={{ color: "var(--hive-ink-soft)" }}
                          title="New chat in channel"
                          aria-label={`New chat in #${ch.name}`}
                        >
                          <IconPlus size={14} />
                        </button>
                        <button
                          onClick={(e) => {
                            channelMenuAnchor.current = e.currentTarget;
                            setChannelMenuFor(ch.id);
                          }}
                          className="grid h-[22px] w-[22px] place-items-center rounded-md hover:bg-[color:var(--hive-overlay)]"
                          style={{ color: "var(--hive-ink-soft)" }}
                          title="Channel options"
                          aria-label={`#${ch.name} options`}
                        >
                          <IconEllipsis size={15} />
                        </button>
                      </div>
                    </div>

                    {!collapsedNow && (
                      <div className="ml-3 border-l pl-1" style={{ borderColor: "var(--hive-line)" }}>
                        {focused.map((c) => (
                          <ChatRow
                            key={c.id}
                            title={c.title || "Untitled"}
                            when={relTime(c.lastActivityAt)}
                            unread={unreadSessions.has(c.id) ? 1 : 0}
                            active={c.id === selectedId && view === "workspace"}
                            onClick={() => onSelect(c.id)}
                            onOptions={(el) => {
                              menuAnchor.current = el;
                              setMenuFor(c.id);
                            }}
                          />
                        ))}
                        {focused.length === 0 && (
                          <div className="px-2.5 py-1 text-[11px]" style={{ color: "var(--hive-sidebar-ink-muted)" }}>
                            {showArchived ? "Nothing archived" : "No focused chats"}
                          </div>
                        )}
                      </div>
                    )}
                  </div>
                );
              })}

              <NewChannelRow onClick={handleNewChannel} />

              {/* Unfiled chats (empty channelId — the pre-migration norm) live
                  under an "Unfiled" cap so nothing disappears (§11 rule 2) and
                  the label reads as a holding area, not a peer of the channels
                  (F11: name it Unfiled, not "Chats"). */}
              {unfiled.length > 0 && (
                <div className="mt-2">
                  <SectionCap>Unfiled</SectionCap>
                  {unfiled.map((c) => (
                    <ChatRow
                      key={c.id}
                      title={c.title || "Untitled"}
                      when={relTime(c.lastActivityAt)}
                      unread={unreadSessions.has(c.id) ? 1 : 0}
                      active={c.id === selectedId && view === "workspace"}
                      onClick={() => onSelect(c.id)}
                      onOptions={(el) => {
                        menuAnchor.current = el;
                        setMenuFor(c.id);
                      }}
                    />
                  ))}
                </div>
              )}
            </>
          )}
        </div>

        {/* 5 — Workspace panes: rendered from the shared PANES source so icon,
            label, and order match the right-pane rail exactly (collapsing the
            sidebar no longer reshuffles the icons). */}
        <div className="mt-2">
          <SectionCap>Workspace</SectionCap>
          {PANES.map((p) => (
            <NavRow
              key={p.id}
              icon={p.icon(15)}
              label={p.label}
              count={paneCounts[p.id]}
              active={utilityPane === p.id}
              onClick={() => onOpenUtilityPane(p.id)}
            />
          ))}
        </div>
      </div>

      {/* Chat options menu — one shared Popover for whichever row is open. */}
      {menuChat && (
        <Popover anchorRef={menuAnchor} align="right" minWidth={150} onDismiss={() => setMenuFor(null)}>
          <PopoverItem label={menuChat.archived ? "Restore" : "Archive"} onSelect={() => handleArchive(menuChat)} />
          <PopoverItem label="Delete…" danger onSelect={() => handleDelete(menuChat)} />
        </Popover>
      )}

      {/* Channel options menu — Rename / Archive (spec §11 rules 4, 7). */}
      {menuChannel && (
        <Popover anchorRef={channelMenuAnchor} align="right" minWidth={160} onDismiss={() => setChannelMenuFor(null)}>
          <PopoverItem label="Rename…" onSelect={() => handleRenameChannel(menuChannel)} />
          <PopoverItem label="Archive…" danger onSelect={() => handleArchiveChannel(menuChannel)} />
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

/// The "+ New channel" row that closes the channel group (spec §6.2). Styled
/// as a muted NavRow so it reads as an affordance, not a channel.
function NewChannelRow({ onClick }: { onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className="flex h-[30px] w-full items-center gap-2.5 rounded-xl px-2.5 text-left text-[13px] transition-colors hover:bg-[color:var(--hive-overlay)]"
      style={{ color: "var(--hive-sidebar-ink-muted)" }}
    >
      <span className="grid h-4 w-4 shrink-0 place-items-center" aria-hidden>
        <IconPlus size={15} />
      </span>
      <span className="min-w-0 flex-1 truncate">New channel</span>
    </button>
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
