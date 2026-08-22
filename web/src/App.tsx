import { lazy, Suspense, useEffect, useMemo, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  addWorkspaceToList,
  createChat,
  directoryRegister,
  ensureSelfMember,
  getAppSettings,
  listWorkspaces,
  getChat,
  getContextTelemetry,
  listAgents,
  listRuntimes,
  maybeRespond,
  saveWorkflow,
  type WorkflowDefinitionDto,
  notifyMentions,
  onChatStream,
  pickWorkspaceFolder,
  onTrayNavigate,
  onWorkspaceSynced,
  onSyncError,
  onWorkspaceRemoved,
  removeWorkspaceFromList,
  renameChat,
  setActiveWorkspace,
  setWorkspaceRoot,
  syncStatus,
  type SyncStatusDto,
} from "@/lib/ipc";
import { Sidebar } from "@/components/Sidebar";
import { ChatView } from "@/components/ChatView";
// Lazy so Monaco (the bulk of the bundle) only loads when the Diff view opens.
const DiffView = lazy(() => import("@/components/DiffView").then((m) => ({ default: m.DiffView })));
// Lazy so react-flow only loads when the workflow editor opens.
const WorkflowBuilder = lazy(() =>
  import("@/components/WorkflowBuilder").then((m) => ({ default: m.WorkflowBuilder })),
);
import { SettingsView, parseSettingsTab, type SettingsTab } from "@/components/SettingsView";
import { RightRail } from "@/components/RightRail";
import { Onboarding } from "@/components/Onboarding";
import { ToastHost, toast, errMsg } from "@/components/Toast";
import { UpdateBanner } from "@/components/UpdateBanner";
import { PendingInvitesBanner } from "@/components/PendingInvitesBanner";
import { DialogHost, promptDialog } from "@/components/Dialog";
import { IconPencil, IconHexagon } from "@/lib/icons";
import { CommandPalette } from "@/components/CommandPalette";
import { WorkspaceRail } from "@/components/WorkspaceRail";
import { FriendsView } from "@/components/FriendsView";
import { AddWorkspaceModal } from "@/components/AddWorkspaceModal";
import { PaneErrorBoundary } from "@/components/ErrorBoundary";
import {
  applyTheme,
  loadMode,
  loadTheme,
  resolvePalette,
  saveMode,
  savePalette,
  watchSystemScheme,
  type AppearanceMode,
  type ThemeName,
} from "@/lib/theme";

type View = "workspace" | "friends";
type CanvasMode = "chat" | "diff";
type UtilityPane =
  | "tools"
  | "review"
  | "people"
  | "vaults"
  | "skills"
  | "workflows"
  | "activity"
  | "context";

const UI_SCALE_STORAGE_KEY = "hive.uiScale";
const DEFAULT_UI_SCALE = 1;
const MIN_UI_SCALE = 0.85;
const MAX_UI_SCALE = 1.2;
const MENU_PANE_WIDTH_STORAGE_KEY = "hive.menuPaneWidth";
const UTILITY_PANE_WIDTH_STORAGE_KEY = "hive.utilityPaneWidth";
const DEFAULT_MENU_PANE_WIDTH = 260;
const DEFAULT_UTILITY_PANE_WIDTH = 300;
const MIN_MENU_PANE_WIDTH = 200;
const MAX_MENU_PANE_WIDTH = 420;
const MIN_UTILITY_PANE_WIDTH = 220;
const MAX_UTILITY_PANE_WIDTH = 520;
const MIN_CHAT_WIDTH = 420;

function clampUiScale(value: number) {
  return Math.min(MAX_UI_SCALE, Math.max(MIN_UI_SCALE, value));
}

function clampPaneWidth(value: number, min: number, max: number) {
  if (max <= min) return min;
  return Math.min(max, Math.max(min, value));
}

function loadUiScale() {
  if (typeof window === "undefined") return DEFAULT_UI_SCALE;
  const parsed = Number(window.localStorage.getItem(UI_SCALE_STORAGE_KEY));
  return Number.isFinite(parsed) ? clampUiScale(parsed) : DEFAULT_UI_SCALE;
}

function loadPaneWidth(storageKey: string, fallback: number, min: number, max: number) {
  if (typeof window === "undefined") return fallback;
  const parsed = Number(window.localStorage.getItem(storageKey));
  return Number.isFinite(parsed) ? clampPaneWidth(parsed, min, max) : fallback;
}

export function App() {
  const [palette, setPaletteState] = useState<ThemeName>(loadTheme());
  const [appearanceMode, setAppearanceModeState] = useState<AppearanceMode>(loadMode());
  const [view, setView] = useState<View>("workspace");
  // Settings is a presented sheet over the workspace, not a view — opening it
  // never unmounts the chat behind it. A nonce remounts the sheet so a repeat
  // open (including repeat tray clicks) re-applies the requested tab.
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("account");
  const [settingsNonce, setSettingsNonce] = useState(0);

  // Open the Settings sheet on a given tab. Accepts new ids, legacy names, or a
  // deep-link fragment (parseSettingsTab normalizes them).
  function openSettings(tab: string = "account") {
    setSettingsTab(parseSettingsTab(tab));
    setSettingsNonce((n) => n + 1);
    setSettingsOpen(true);
  }
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [addWsOpen, setAddWsOpen] = useState(false);
  const [mode, setMode] = useState<CanvasMode>("chat");
  // Workflow-editor takeover of the main canvas. Keyed to its chat so
  // switching chats hides (not destroys) an in-progress draft.
  const [workflowDraft, setWorkflowDraft] = useState<
    { sessionId: string; def: WorkflowDefinitionDto } | null
  >(null);
  const [utilityPane, setUtilityPane] = useState<UtilityPane>("tools");
  // Pane visibility persists — a focused layout should survive restarts.
  const [showUtilityPane, setShowUtilityPane] = useState(
    () => window.localStorage.getItem("hive.utilityPaneVisible") !== "0",
  );
  const [sidebarVisible, setSidebarVisible] = useState(
    () => window.localStorage.getItem("hive.sidebarVisible") !== "0",
  );
  useEffect(() => {
    window.localStorage.setItem("hive.utilityPaneVisible", showUtilityPane ? "1" : "0");
  }, [showUtilityPane]);
  useEffect(() => {
    window.localStorage.setItem("hive.sidebarVisible", sidebarVisible ? "1" : "0");
  }, [sidebarVisible]);
  const [uiScale, setUiScale] = useState(loadUiScale);
  const [menuPaneWidth, setMenuPaneWidth] = useState(() =>
    loadPaneWidth(
      MENU_PANE_WIDTH_STORAGE_KEY,
      DEFAULT_MENU_PANE_WIDTH,
      MIN_MENU_PANE_WIDTH,
      MAX_MENU_PANE_WIDTH,
    ),
  );
  const [utilityPaneWidth, setUtilityPaneWidth] = useState(() =>
    loadPaneWidth(
      UTILITY_PANE_WIDTH_STORAGE_KEY,
      DEFAULT_UTILITY_PANE_WIDTH,
      MIN_UTILITY_PANE_WIDTH,
      MAX_UTILITY_PANE_WIDTH,
    ),
  );
  const previousOverflowCount = useRef(0);
  // Whether the Context pane was already auto-surfaced for the current chat, so we
  // do it at most once and don't repeatedly hijack the user's layout.
  const autoOpenedContext = useRef(false);
  const qc = useQueryClient();

  const settings = useQuery({ queryKey: ["settings"], queryFn: getAppSettings });
  // The active workspace's name (the team/room identity) — the sidebar header
  // labels the space by name, not by its project-folder basename.
  const workspaceList = useQuery({ queryKey: ["workspaces"], queryFn: listWorkspaces });
  const runtimes = useQuery({ queryKey: ["runtimes"], queryFn: listRuntimes });
  const [onboarded, setOnboarded] = useState(
    () => typeof window !== "undefined" && window.localStorage.getItem("hive.onboarded") === "1",
  );
  // First-run gate: until the user has set a real display name (or explicitly
  // completed onboarding), block the shell with the onboarding flow.
  const needsOnboarding =
    !onboarded &&
    settings.data != null &&
    (settings.data.displayName.trim() === "" || settings.data.displayName.trim() === "You");
  const sync = useQuery({ queryKey: ["sync-status"], queryFn: syncStatus });
  const activeChat = useQuery({
    queryKey: ["chat", selectedId],
    queryFn: () => getChat(selectedId ?? ""),
    enabled: Boolean(selectedId),
  });
  const contextTelemetry = useQuery({
    queryKey: ["context-telemetry", selectedId, activeChat.data?.runtimeId],
    queryFn: () => getContextTelemetry(selectedId ?? ""),
    enabled: Boolean(selectedId),
  });
  // Roster for the workflow editor's per-stage agent select.
  const workflowAgents = useQuery({
    queryKey: ["agents", workflowDraft?.sessionId],
    queryFn: () => listAgents(workflowDraft?.sessionId ?? ""),
    enabled: Boolean(workflowDraft),
  });
  const [savingWorkflow, setSavingWorkflow] = useState(false);

  async function handleSaveWorkflow(draft: WorkflowDefinitionDto) {
    if (!workflowDraft) return;
    setSavingWorkflow(true);
    try {
      await saveWorkflow(workflowDraft.sessionId, draft);
      qc.invalidateQueries({ queryKey: ["workflows", workflowDraft.sessionId] });
      setWorkflowDraft(null);
    } catch (e) {
      toast.error(`Couldn't save workflow: ${errMsg(e)}`);
    } finally {
      setSavingWorkflow(false);
    }
  }

  // Apply the resolved palette whenever the accent or the light/dark mode
  // changes, and re-resolve live when the OS appearance flips (mode = auto).
  useEffect(() => {
    applyTheme(resolvePalette(palette, appearanceMode));
    return watchSystemScheme(() => applyTheme(resolvePalette(palette, appearanceMode)));
  }, [appearanceMode, palette]);

  // Best-effort: announce this device in the directory (no-op unless signed in
  // to GitHub + a relay is set), so teammates can invite this account by handle.
  useEffect(() => {
    void directoryRegister().catch(() => {});
  }, []);

  const setPalette = (next: ThemeName) => {
    savePalette(next);
    setPaletteState(next);
  };
  const setAppearanceMode = (next: AppearanceMode) => {
    saveMode(next);
    setAppearanceModeState(next);
  };

  useEffect(() => {
    document.documentElement.style.fontSize = `${uiScale * 100}%`;
    window.localStorage.setItem(UI_SCALE_STORAGE_KEY, String(uiScale));
  }, [uiScale]);

  useEffect(() => {
    window.localStorage.setItem(MENU_PANE_WIDTH_STORAGE_KEY, String(menuPaneWidth));
  }, [menuPaneWidth]);

  useEffect(() => {
    window.localStorage.setItem(UTILITY_PANE_WIDTH_STORAGE_KEY, String(utilityPaneWidth));
  }, [utilityPaneWidth]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.altKey) return;

      if (event.key.toLowerCase() === "k") {
        event.preventDefault();
        setPaletteOpen((v) => !v);
        return;
      }

      // Focus mode: ⌘B hides the sidebar, ⌘J the tools rail (VS Code's
      // muscle memory). Both persist.
      if (event.key.toLowerCase() === "b") {
        event.preventDefault();
        setSidebarVisible((v) => !v);
        return;
      }
      if (event.key.toLowerCase() === "j") {
        event.preventDefault();
        setShowUtilityPane((v) => !v);
        return;
      }

      // ⌘, opens Settings (the platform-standard preferences shortcut).
      if (event.key === ",") {
        event.preventDefault();
        openSettings();
        return;
      }

      if (event.key === "0") {
        event.preventDefault();
        setUiScale(DEFAULT_UI_SCALE);
        return;
      }

      if (event.key === "+" || event.key === "=" || event.key === "NumpadAdd") {
        event.preventDefault();
        setUiScale((value) => clampUiScale(Number((value + 0.05).toFixed(2))));
        return;
      }

      if (event.key === "-" || event.key === "NumpadSubtract") {
        event.preventDefault();
        setUiScale((value) => clampUiScale(Number((value - 0.05).toFixed(2))));
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, []);

  useEffect(() => {
    const unlisten = onWorkspaceSynced(() => {
      // A sync succeeded: refresh the health pill (flips back to Live on recovery).
      qc.invalidateQueries({ queryKey: ["sync-status"] });
      qc.invalidateQueries({ queryKey: ["chats"] });
      qc.invalidateQueries({ queryKey: ["chat"] });
      // Channels + skills also ride the synced log (config hoist / §11): refresh
      // them too, or a channel/skill a headless agent created while you were
      // offline lands in the store but not in the sidebar until a reload.
      qc.invalidateQueries({ queryKey: ["channels"] });
      qc.invalidateQueries({ queryKey: ["skills"] });
      qc.invalidateQueries({ queryKey: ["context-telemetry"] });
      qc.invalidateQueries({ queryKey: ["proposals"] });
      qc.invalidateQueries({ queryKey: ["members"] });
      qc.invalidateQueries({ queryKey: ["agents"] });
      qc.invalidateQueries({ queryKey: ["vaults"] });
      // Queued work (unanswered mentions + host status) rides the log too, so a
      // mention/host-heartbeat that synced while offline updates the queue live.
      qc.invalidateQueries({ queryKey: ["queued-work"] });
      // Mention highlights: a synced message may @-mention the local user, so
      // refresh which channels/chats light up in the sidebar — and which
      // background workspaces flag an unread mention in the switcher.
      qc.invalidateQueries({ queryKey: ["mention-states"] });
      qc.invalidateQueries({ queryKey: ["all-mention-states"] });
      // Cross-device dispatch: if a teammate's message just synced into the open
      // chat and we own the responder, answer it (no-op otherwise).
      if (selectedId) {
        void maybeRespond(selectedId).catch(() => {});
        // Ping the local user if a synced message @-mentions them.
        void notifyMentions(selectedId).catch(() => {});
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [qc, selectedId]);

  // A sync failed: refresh the health pill so it flips to "Sync error" the
  // moment the relay/key/token goes bad (mirrors onWorkspaceSynced above).
  useEffect(() => {
    const unlisten = onSyncError(() => {
      qc.invalidateQueries({ queryKey: ["sync-status"] });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [qc]);

  // Removed from the active workspace by an owner/admin: tell the user (removals
  // were previously silent), and refresh the roster/chats so their view reflects
  // the loss of access.
  useEffect(() => {
    const unlisten = onWorkspaceRemoved(() => {
      toast.error("You've been removed from this workspace.");
      qc.invalidateQueries({ queryKey: ["members"] });
      qc.invalidateQueries({ queryKey: ["chats"] });
      qc.invalidateQueries({ queryKey: ["sync-status"] });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [qc]);

  // System-tray navigation: the native menu emits a route string and we switch
  // the in-window view to match (Settings/Friends are views, not OS windows).
  useEffect(() => {
    const unlisten = onTrayNavigate((route) => {
      if (route === "friends") {
        setView("friends");
      } else if (route === "workspace") {
        setView("workspace");
      } else if (route.startsWith("settings")) {
        const tab = route.includes(":") ? route.split(":")[1] : "account";
        openSettings(tab);
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
    // openSettings is stable (only touches setState setters).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Register the local user in the chat's roster on open, so People populates.
  useEffect(() => {
    if (!selectedId) return;
    ensureSelfMember(selectedId)
      .then(() => qc.invalidateQueries({ queryKey: ["members", selectedId] }))
      .catch(() => {});
  }, [qc, selectedId]);

  useEffect(() => {
    const unlisten = onChatStream((event) => {
      if (event.sessionId !== selectedId) return;
      // Only refresh context telemetry on a terminal phase. Invalidating on every
      // "delta" fired a fresh get_context_telemetry IPC round-trip per streamed
      // token (the query has active observers), hammering the Tauri bridge during
      // long replies. The counts only meaningfully change once the turn lands.
      if (event.phase === "delta") return;
      qc.invalidateQueries({ queryKey: ["context-telemetry", selectedId] });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [qc, selectedId]);

  useEffect(() => {
    previousOverflowCount.current = 0;
    autoOpenedContext.current = false;
  }, [selectedId]);

  useEffect(() => {
    const overflowCount = contextTelemetry.data?.overflowMessageCount ?? 0;
    // Auto-open the Context pane at most ONCE per chat when overflow first appears,
    // and never when the user has deliberately closed the tools rail (focus mode) —
    // yanking their pane open on every overflow tick feels like the app fighting
    // them. After the first surface, respect their layout choice.
    if (
      overflowCount > 0 &&
      previousOverflowCount.current === 0 &&
      !autoOpenedContext.current &&
      showUtilityPane
    ) {
      openUtilityPane("context");
      autoOpenedContext.current = true;
    }
    previousOverflowCount.current = overflowCount;
  }, [contextTelemetry.data?.overflowMessageCount, showUtilityPane]);

  const workspaceRoot = settings.data?.workspaceRoot ?? "";
  const activeWorkspace = workspaceList.data?.find((w) => w.active);
  const activeWorkspaceId = activeWorkspace?.id ?? "";
  const activeWorkspaceName = activeWorkspace?.name?.trim();
  const workspaceLabel = useMemo(() => {
    // Prefer the workspace's own name (what you named the team/room); fall back
    // to the project-folder basename only when it has none.
    if (activeWorkspaceName) return activeWorkspaceName;
    const trimmed = workspaceRoot.replace(/[\\/]+$/, "");
    if (!trimmed) return "Hive Workspace";
    const parts = trimmed.split(/[\\/]/);
    return parts[parts.length - 1] || "Hive Workspace";
  }, [activeWorkspaceName, workspaceRoot]);

  const runtimeItems = runtimes.data ?? [];
  const currentRuntime =
    runtimeItems.find((rt) => rt.id === activeChat.data?.runtimeId) ?? runtimeItems[0] ?? null;

  async function refreshWorkspaceShell() {
    await Promise.all([
      qc.invalidateQueries({ queryKey: ["settings"] }),
      qc.invalidateQueries({ queryKey: ["chats"] }),
      qc.invalidateQueries({ queryKey: ["chat"] }),
      qc.invalidateQueries({ queryKey: ["runtimes"] }),
      qc.invalidateQueries({ queryKey: ["mcp"] }),
      qc.invalidateQueries({ queryKey: ["vaults"] }),
      qc.invalidateQueries({ queryKey: ["skills"] }),
      qc.invalidateQueries({ queryKey: ["members"] }),
      qc.invalidateQueries({ queryKey: ["agents"] }),
      qc.invalidateQueries({ queryKey: ["proposals"] }),
      qc.invalidateQueries({ queryKey: ["diffs"] }),
      qc.invalidateQueries({ queryKey: ["mention-states"] }),
      qc.invalidateQueries({ queryKey: ["all-mention-states"] }),
      // Workspace-scoped but globally-keyed (no activeWorkspaceId in the key) — must
      // be invalidated on switch or the Activity/People panes show the previous
      // workspace's queued mentions / hosts / server roster until the next sync
      // (and workspace-hosts, which has no refetchInterval without a relay, never).
      qc.invalidateQueries({ queryKey: ["queued-work"] }),
      qc.invalidateQueries({ queryKey: ["workspace-hosts"] }),
      qc.invalidateQueries({ queryKey: ["server-members"] }),
    ]);
  }

  function openUtilityPane(pane: UtilityPane) {
    setUtilityPane(pane);
    setShowUtilityPane(true);
  }

  function startMenuResize(event: ReactPointerEvent<HTMLDivElement>) {
    event.preventDefault();
    const startX = event.clientX;
    const initialWidth = menuPaneWidth;

    const handleMove = (moveEvent: PointerEvent) => {
      const delta = moveEvent.clientX - startX;
      const nextMax = Math.min(
        MAX_MENU_PANE_WIDTH,
        window.innerWidth - utilityPaneWidth - MIN_CHAT_WIDTH,
      );
      setMenuPaneWidth(clampPaneWidth(initialWidth + delta, MIN_MENU_PANE_WIDTH, nextMax));
    };

    const stopResize = () => {
      document.body.style.removeProperty("cursor");
      document.body.style.removeProperty("user-select");
      window.removeEventListener("pointermove", handleMove);
      window.removeEventListener("pointerup", stopResize);
    };

    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("pointermove", handleMove);
    window.addEventListener("pointerup", stopResize);
  }

  function startUtilityResize(event: ReactPointerEvent<HTMLDivElement>) {
    event.preventDefault();
    const startX = event.clientX;
    const initialWidth = utilityPaneWidth;

    const handleMove = (moveEvent: PointerEvent) => {
      const delta = moveEvent.clientX - startX;
      const nextMax = Math.min(
        MAX_UTILITY_PANE_WIDTH,
        window.innerWidth - menuPaneWidth - MIN_CHAT_WIDTH,
      );
      setUtilityPaneWidth(clampPaneWidth(initialWidth - delta, MIN_UTILITY_PANE_WIDTH, nextMax));
    };

    const stopResize = () => {
      document.body.style.removeProperty("cursor");
      document.body.style.removeProperty("user-select");
      window.removeEventListener("pointermove", handleMove);
      window.removeEventListener("pointerup", stopResize);
    };

    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("pointermove", handleMove);
    window.addEventListener("pointerup", stopResize);
  }

  // A failed settings read must not fall through to the shell with empty
  // defaults (which reads as "onboarded, everything lost"). Show a recoverable
  // error shell that retries the load instead.
  if (settings.isError) {
    return (
      <div
        className="flex h-full flex-col items-center justify-center p-6"
        style={{ background: "var(--hive-canvas)", color: "var(--hive-ink)" }}
      >
        <ToastHost />
        <div
          className="w-full max-w-md rounded-2xl border p-6 text-center"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
        >
          <h1 className="text-lg font-semibold tracking-tight" style={{ color: "var(--hive-danger)" }}>
            Couldn't load your settings
          </h1>
          <p className="mt-2 text-sm opacity-70">
            Hive couldn't read this workspace's configuration. Nothing has been lost — retry the
            load, or reload the app.
          </p>
          <div className="mt-4 flex items-center justify-center gap-2">
            <button
              onClick={() => void settings.refetch()}
              className="rounded-xl px-4 py-2 text-sm font-semibold text-white transition-all hover:brightness-105"
              style={{ background: "var(--hive-accent-cool)" }}
            >
              Retry
            </button>
            <button
              onClick={() => location.reload()}
              className="rounded-xl border px-4 py-2 text-sm font-medium transition-colors"
              style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)", color: "var(--hive-ink)" }}
            >
              Reload
            </button>
          </div>
          <details className="mt-4 text-left">
            <summary className="cursor-pointer text-xs opacity-55">Error details</summary>
            <pre
              className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap rounded-lg p-3 text-[11px] leading-5"
              style={{ background: "var(--hive-mist)", color: "var(--hive-ink)" }}
            >
              {errMsg(settings.error)}
            </pre>
          </details>
        </div>
      </div>
    );
  }

  if (needsOnboarding) {
    return (
      <Onboarding
        onComplete={() => {
          window.localStorage.setItem("hive.onboarded", "1");
          // Onboarding's appearance step persisted the theme + mode to
          // localStorage; sync App's state so its own applyTheme effect doesn't
          // revert to the mount-time default.
          setPalette(loadTheme());
          setAppearanceMode(loadMode());
          setOnboarded(true);
          refreshWorkspaceShell();
        }}
      />
    );
  }

  return (
    <div
      className="flex h-full min-w-0 overflow-hidden"
      style={{ background: "var(--hive-canvas)", color: "var(--hive-ink)" }}
    >
      <ToastHost />
      <DialogHost />
      <UpdateBanner />
      <PendingInvitesBanner />
      <CommandPalette
        open={paletteOpen}
        onClose={() => setPaletteOpen(false)}
        actions={{
          newChat: async () => {
            try {
              const created = await createChat("");
              await qc.invalidateQueries({ queryKey: ["chats"] });
              setSelectedId(created.id);
              setView("workspace");
            } catch (e) {
              toast.error(`Couldn't create chat: ${errMsg(e)}`);
            }
          },
          openSettings: () => openSettings(),
          selectChat: (id) => {
            setSelectedId(id);
            setView("workspace");
          },
          selectWorkspace: async (id) => {
            await setActiveWorkspace(id);
            await qc.invalidateQueries({ queryKey: ["workspaces"] });
            await qc.invalidateQueries({ queryKey: ["chats"] });
            setView("workspace");
          },
          toggleSidebar: () => setSidebarVisible((v) => !v),
          toggleTools: () => setShowUtilityPane((v) => !v),
          setCanvasMode: (m) => {
            setView("workspace");
            setMode(m);
          },
          openFriends: () => setView("friends"),
          openPane: (pane) => {
            setView("workspace");
            openUtilityPane(pane);
          },
          openSettingsTab: (tab) => openSettings(tab),
          cycleAppearance: () =>
            setAppearanceMode(appearanceMode === "dark" ? "light" : "dark"),
        }}
      />
      <WorkspaceRail
        onJoinRoom={() => setAddWsOpen(true)}
        onOpenFriends={() => setView("friends")}
        sidebarVisible={sidebarVisible}
        onToggleSidebar={() => setSidebarVisible((v) => !v)}
        onOpenSettings={() => openSettings()}
        settingsActive={settingsOpen}
      />
      <AddWorkspaceModal open={addWsOpen} onClose={() => setAddWsOpen(false)} />
      {sidebarVisible && (
      <Sidebar
        width={menuPaneWidth}
        selectedId={selectedId}
        sessionId={selectedId}
        view={view}
        workspaceLabel={workspaceLabel}
        workspacePath={workspaceRoot}
        activeWorkspaceId={activeWorkspaceId}
        knownWorkspaces={settings.data?.knownWorkspaces ?? []}
        displayName={settings.data?.displayName ?? "You"}
        utilityPane={utilityPane}
        onSelect={(id) => {
          setSelectedId(id);
          setView("workspace");
          setMode("chat");
        }}
        onOpenSettings={() => openSettings()}
        onAddWorkspace={async () => {
          const path = await pickWorkspaceFolder();
          if (!path) return;
          await addWorkspaceToList(path);
          await qc.invalidateQueries({ queryKey: ["settings"] });
        }}
        onRemoveWorkspace={async (path) => {
          await removeWorkspaceFromList(path);
          await qc.invalidateQueries({ queryKey: ["settings"] });
        }}
        onSwitchWorkspace={async (path) => {
          await setWorkspaceRoot(path);
          setSelectedId(null);
          setMode("chat");
          setUtilityPane("tools");
          await refreshWorkspaceShell();
          setView("workspace");
        }}
        onOpenUtilityPane={(pane) => {
          setView("workspace");
          openUtilityPane(pane);
        }}
      />
      )}

      {sidebarVisible && (
        <PaneResizeHandle onPointerDown={startMenuResize} ariaLabel="Resize menu and chat panes" />
      )}

      <main className="flex min-w-0 flex-1 flex-col overflow-hidden">
        {view === "friends" ? (
          <PaneErrorBoundary label="Friends">
            <FriendsView
              onOpenSettings={(tab) => openSettings(tab)}
              onOpenDm={() => {
                setSelectedId(null);
                setMode("chat");
                setView("workspace");
              }}
            />
          </PaneErrorBoundary>
        ) : !selectedId ? (
          <div className="flex flex-1 items-center justify-center opacity-60">
            Select or create a chat to begin.
          </div>
        ) : (
          <>
            <ChatHeaderBar
              title={activeChat.data?.title ?? "New Chat"}
              contextPct={
                contextTelemetry.data
                  ? Math.min(
                      100,
                      Math.round(
                        ((contextTelemetry.data.systemPromptTokens +
                          contextTelemetry.data.keptTokens) /
                          Math.max(1, contextTelemetry.data.contextWindowTokens)) *
                          100,
                      ),
                    )
                  : null
              }
              onOpenContext={() => openUtilityPane("context")}
              syncPill={deriveSyncPill(sync.data)}
              onFixSync={() => openSettings("team")}
              workspaceCrumb={sidebarVisible ? undefined : workspaceLabel}
              mode={mode}
              onChangeMode={setMode}
              utilityPaneVisible={showUtilityPane}
              onToggleTools={() => {
                if (showUtilityPane) {
                  setShowUtilityPane(false);
                  return;
                }
                openUtilityPane("tools");
              }}
              onRename={async () => {
                if (!selectedId) return;
                const next = await promptDialog("Rename chat", {
                  placeholder: "Chat title",
                  defaultValue: activeChat.data?.title ?? "",
                });
                if (next === null || !next.trim()) return;
                try {
                  await renameChat(selectedId, next.trim());
                  await qc.invalidateQueries({ queryKey: ["chat", selectedId] });
                  await qc.invalidateQueries({ queryKey: ["chats"] });
                } catch (e) {
                  toast.error(`Couldn't rename chat: ${errMsg(e)}`);
                }
              }}
            />

            <div className="flex min-h-0 min-w-0 flex-1 overflow-hidden">
              <div className="min-w-0 flex-1">
                <PaneErrorBoundary label="This view">
                {workflowDraft && workflowDraft.sessionId === selectedId ? (
                  <Suspense
                    fallback={
                      <div className="flex h-full items-center justify-center opacity-50">
                        Loading workflow editor…
                      </div>
                    }
                  >
                    <WorkflowBuilder
                      key={workflowDraft.def.id || "new"}
                      initial={workflowDraft.def}
                      agents={workflowAgents.data ?? []}
                      saving={savingWorkflow}
                      onSave={handleSaveWorkflow}
                      onClose={() => setWorkflowDraft(null)}
                    />
                  </Suspense>
                ) : (
                  <>
                {mode === "chat" && (
                  <ChatView
                    sessionId={selectedId}
                    runtimes={runtimeItems}
                    currentRuntimeId={activeChat.data?.runtimeId ?? currentRuntime?.id ?? ""}
                    onOpenTools={() => openUtilityPane("tools")}
                  />
                )}
                {mode === "diff" && (
                  <Suspense
                    fallback={
                      <div className="flex h-full items-center justify-center opacity-50">
                        Loading diff editor…
                      </div>
                    }
                  >
                    <DiffView />
                  </Suspense>
                )}
                  </>
                )}
                </PaneErrorBoundary>
              </div>

              {showUtilityPane && (
                <>
                  <PaneResizeHandle
                    onPointerDown={startUtilityResize}
                    ariaLabel="Resize chat and utility panes"
                  />

                  <PaneErrorBoundary label="This panel">
                    <RightRail
                      width={utilityPaneWidth}
                      sessionId={selectedId}
                      pane={utilityPane}
                      activeRuntimeId={activeChat.data?.runtimeId ?? currentRuntime?.id ?? ""}
                      onChangePane={setUtilityPane}
                      onClose={() => setShowUtilityPane(false)}
                      // The pane's own icon rail duplicates the sidebar's
                      // Workspace list (F10) — only show it when the sidebar is
                      // hidden, so panes stay reachable without doubling nav.
                      showPaneRail={!sidebarVisible}
                      onEditWorkflow={(def) =>
                        selectedId && setWorkflowDraft({ sessionId: selectedId, def })
                      }
                    />
                  </PaneErrorBoundary>
                </>
              )}
            </div>
          </>
        )}
      </main>

      {/* Settings presents OVER the workspace — the chat behind it stays mounted.
          The nonce remounts the sheet so a repeat open re-applies the tab. */}
      {settingsOpen && (
        <SettingsView
          key={`settings-${settingsNonce}`}
          initialTab={settingsTab}
          palette={palette}
          onPaletteChange={setPalette}
          appearanceMode={appearanceMode}
          onAppearanceModeChange={setAppearanceMode}
          onClose={() => setSettingsOpen(false)}
        />
      )}
    </div>
  );
}

function PaneResizeHandle({
  onPointerDown,
  ariaLabel,
}: {
  onPointerDown: (event: ReactPointerEvent<HTMLDivElement>) => void;
  ariaLabel: string;
}) {
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={ariaLabel}
      className="relative w-1.5 shrink-0 cursor-col-resize bg-transparent transition-colors hover:bg-[color:var(--hive-overlay)] active:bg-[color:var(--hive-overlay)]"
      style={{ touchAction: "none" }}
      onPointerDown={onPointerDown}
    >
      <span className="pointer-events-none absolute inset-y-0 left-1/2 w-px -translate-x-1/2 bg-[color:var(--hive-line)]" />
    </div>
  );
}

/// Single compact chat header: title + rename, sync state, the Chat/Diff mode
/// tabs, and the Tools toggle. Replaces the old two-row WorkspaceHeader +
/// ModeStrip stack (~130px of chrome, duplicated workspace path, a dead rename
/// button, and near-white tab text that vanished on light palettes).
/// The header status pill, driven by real connection health (not just "a relay
/// URL is set"): live → success tint, error → danger tint (with the failure in
/// the tooltip), offline → a muted neutral state. All color via --hive-* tokens.
type SyncPill = { label: string; color: string | null; title?: string; actionable?: boolean };

function deriveSyncPill(s: SyncStatusDto | undefined): SyncPill {
  const state = s?.connectionState ?? "offline";
  if (state === "live") {
    return {
      label: `Live · room ${s?.room ?? ""}`,
      color: "var(--hive-success)",
      title: "Live — syncing with your team",
    };
  }
  if (state === "error") {
    return {
      label: "Sync error",
      color: "var(--hive-danger)",
      title: s?.lastError ?? "Sync failed — reconnect or check your key / token",
      actionable: true,
    };
  }
  // Relay configured but no E2EE key yet — setup-incomplete, not a failure.
  // Calm amber prompt to set a passphrase (Settings → Team), never a red error.
  if (state === "needs_key") {
    return {
      label: "Set a passphrase to sync",
      color: "var(--hive-warn)",
      title:
        s?.lastError ??
        "A relay is configured but this workspace has no encryption key — set a workspace passphrase in Settings → Team to sync encrypted, or clear the relay URL to work local-only.",
      actionable: true,
    };
  }
  return {
    label: s?.relayConfigured ? "Offline · not syncing" : "Local only",
    color: null,
    title: s?.relayConfigured
      ? "A relay is configured but nothing is syncing right now"
      : undefined,
  };
}

/// The header sync status. A tinted state (live/error/needs-key) reads as a
/// chip, not amber prose beside the title (F16); actionable states (needs-key,
/// error) are a button that opens Settings → Team. Neutral offline/local stays
/// a quiet unboxed label.
function SyncPillChip({ pill, onFix }: { pill: SyncPill; onFix?: () => void }) {
  if (!pill.color) {
    return (
      <span className="ml-1 hidden shrink-0 text-xs opacity-40 sm:inline" title={pill.title}>
        {pill.label}
      </span>
    );
  }
  const chipStyle: CSSProperties = {
    color: pill.color,
    borderColor: `color-mix(in srgb, ${pill.color} 40%, transparent)`,
    background: `color-mix(in srgb, ${pill.color} 12%, transparent)`,
  };
  const className =
    "ml-1 hidden shrink-0 items-center gap-1 rounded-full border px-2 py-0.5 text-xs font-medium sm:inline-flex";
  if (pill.actionable && onFix) {
    return (
      <button type="button" className={`${className} hover:brightness-110`} style={chipStyle} title={pill.title} onClick={onFix}>
        {pill.label}
      </button>
    );
  }
  return (
    <span className={className} style={chipStyle} title={pill.title}>
      {pill.label}
    </span>
  );
}

function ChatHeaderBar({
  title,
  syncPill,
  mode,
  onChangeMode,
  utilityPaneVisible,
  onToggleTools,
  onRename,
  contextPct,
  onOpenContext,
  onFixSync,
  workspaceCrumb,
}: {
  title: string;
  syncPill: SyncPill;
  mode: CanvasMode;
  onChangeMode: (m: CanvasMode) => void;
  utilityPaneVisible: boolean;
  onToggleTools: () => void;
  onRename: () => void;
  /// The active workspace name, shown as a crumb before the chat title when the
  /// sidebar is collapsed — otherwise "which workspace am I in?" is invisible.
  workspaceCrumb?: string;
  /// Percent of the model's context window the next reply will use (null
  /// until telemetry loads). Clicking the pill opens the Context pane.
  contextPct: number | null;
  onOpenContext: () => void;
  /// Opens Settings → Team, so an actionable sync chip (needs-key / error) is a
  /// button that takes the user to the fix.
  onFixSync?: () => void;
}) {
  const tabs: { id: CanvasMode; label: string }[] = [
    { id: "chat", label: "Chat" },
    { id: "diff", label: "Diff" },
  ];
  return (
    <div
      className="flex items-center gap-3 border-b px-4 py-2"
      style={{ borderColor: "var(--hive-line)" }}
    >
      <div className="flex min-w-0 flex-1 items-center gap-1.5">
        {workspaceCrumb && (
          <>
            <span className="shrink-0 truncate text-base font-semibold tracking-tight opacity-55">
              {workspaceCrumb}
            </span>
            <span className="shrink-0 opacity-30" aria-hidden>/</span>
          </>
        )}
        <h1 className="truncate text-base font-semibold tracking-tight">{title}</h1>
        <button
          className="shrink-0 rounded-md p-1 opacity-40 transition-opacity hover:opacity-100"
          title="Rename chat"
          aria-label="Rename chat"
          onClick={onRename}
        >
          <IconPencil size={13} />
        </button>
        <SyncPillChip pill={syncPill} onFix={onFixSync} />
      </div>

      {contextPct !== null && (
        <button
          onClick={onOpenContext}
          title={`${contextPct}% of the model's context window in use — open the Context pane`}
          aria-label="Context usage"
          className="flex shrink-0 items-center gap-1 rounded-full border px-2 py-0.5 text-xs transition-colors hover:border-[color:var(--hive-accent-cool)]"
          style={{
            borderColor: "var(--hive-line)",
            background: "var(--hive-mist)",
            color: contextPct >= 80 ? "var(--hive-warn)" : "var(--hive-ink)",
          }}
        >
          <IconHexagon size={11} />
          {contextPct}% context
        </button>
      )}

      <div
        className="flex shrink-0 items-center gap-0.5 rounded-xl border p-0.5"
        style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
        role="tablist"
        aria-label="Canvas mode"
      >
        {tabs.map((t) => (
          <button
            key={t.id}
            role="tab"
            aria-selected={mode === t.id}
            onClick={() => onChangeMode(t.id)}
            className="rounded-[10px] px-3 py-1 text-sm font-medium transition-colors"
            style={{
              background: mode === t.id ? "var(--hive-panel)" : "transparent",
              color: "var(--hive-ink)",
              opacity: mode === t.id ? 1 : 0.55,
              boxShadow: mode === t.id ? "0 1px 2px color-mix(in srgb, var(--hive-ink) 12%, transparent)" : undefined,
            }}
          >
            {t.label}
          </button>
        ))}
      </div>

      <button
        onClick={onToggleTools}
        aria-pressed={utilityPaneVisible}
        className="shrink-0 rounded-xl border px-3 py-1 text-sm font-medium transition-colors"
        style={{
          borderColor: utilityPaneVisible
            ? "color-mix(in srgb, var(--hive-accent-cool) 40%, transparent)"
            : "var(--hive-line)",
          background: utilityPaneVisible
            ? "color-mix(in srgb, var(--hive-accent-cool) 18%, transparent)"
            : "var(--hive-mist)",
          color: "var(--hive-ink)",
        }}
      >
        Tools
      </button>
    </div>
  );
}
