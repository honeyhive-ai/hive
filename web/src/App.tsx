import { lazy, Suspense, useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  addWorkspaceToList,
  createChat,
  directoryRegister,
  ensureSelfMember,
  getAppSettings,
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
  removeWorkspaceFromList,
  renameChat,
  setActiveWorkspace,
  setWorkspaceRoot,
  syncStatus,
} from "@/lib/ipc";
import { Sidebar } from "@/components/Sidebar";
import { ChatView } from "@/components/ChatView";
// Lazy so Monaco (the bulk of the bundle) only loads when the Diff view opens.
const DiffView = lazy(() => import("@/components/DiffView").then((m) => ({ default: m.DiffView })));
// Lazy so react-flow only loads when the workflow editor opens.
const WorkflowBuilder = lazy(() =>
  import("@/components/WorkflowBuilder").then((m) => ({ default: m.WorkflowBuilder })),
);
import { SettingsView, type SettingsTab } from "@/components/SettingsView";
import { RightRail } from "@/components/RightRail";
import { Onboarding } from "@/components/Onboarding";
import { ToastHost, toast, errMsg } from "@/components/Toast";
import { UpdateBanner } from "@/components/UpdateBanner";
import { DialogHost, promptDialog } from "@/components/Dialog";
import { IconPencil, IconHexagon } from "@/lib/icons";
import { CommandPalette } from "@/components/CommandPalette";
import { WorkspaceRail } from "@/components/WorkspaceRail";
import { FriendsView } from "@/components/FriendsView";
import { AddWorkspaceModal } from "@/components/AddWorkspaceModal";
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

type View = "workspace" | "settings" | "friends";
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
  // Which Settings tab to open on, plus a nonce so each open (including repeat
  // tray clicks) remounts SettingsView on the requested tab.
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("Account");
  const [settingsNonce, setSettingsNonce] = useState(0);

  // Open Settings on a given tab. Bumping the nonce remounts SettingsView so a
  // repeat request (e.g. a second tray click) re-applies the requested tab even
  // when Settings is already on screen.
  function openSettings(tab: SettingsTab = "Account") {
    setSettingsTab(tab);
    setSettingsNonce((n) => n + 1);
    setView("settings");
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
  const qc = useQueryClient();

  const settings = useQuery({ queryKey: ["settings"], queryFn: getAppSettings });
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
      qc.invalidateQueries({ queryKey: ["chats"] });
      qc.invalidateQueries({ queryKey: ["chat"] });
      qc.invalidateQueries({ queryKey: ["context-telemetry"] });
      qc.invalidateQueries({ queryKey: ["proposals"] });
      qc.invalidateQueries({ queryKey: ["members"] });
      qc.invalidateQueries({ queryKey: ["agents"] });
      qc.invalidateQueries({ queryKey: ["vaults"] });
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

  // System-tray navigation: the native menu emits a route string and we switch
  // the in-window view to match (Settings/Friends are views, not OS windows).
  useEffect(() => {
    const unlisten = onTrayNavigate((route) => {
      if (route === "friends") {
        setView("friends");
      } else if (route === "workspace") {
        setView("workspace");
      } else if (route.startsWith("settings")) {
        const tab = route.includes(":") ? (route.split(":")[1] as SettingsTab) : "Account";
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
      qc.invalidateQueries({ queryKey: ["context-telemetry", selectedId] });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [qc, selectedId]);

  useEffect(() => {
    previousOverflowCount.current = 0;
  }, [selectedId]);

  useEffect(() => {
    const overflowCount = contextTelemetry.data?.overflowMessageCount ?? 0;
    if (overflowCount > 0 && previousOverflowCount.current === 0) {
      openUtilityPane("context");
    }
    previousOverflowCount.current = overflowCount;
  }, [contextTelemetry.data?.overflowMessageCount]);

  const workspaceRoot = settings.data?.workspaceRoot ?? "";
  const workspaceLabel = useMemo(() => {
    const trimmed = workspaceRoot.replace(/[\\/]+$/, "");
    if (!trimmed) return "Hive Workspace";
    const parts = trimmed.split(/[\\/]/);
    return parts[parts.length - 1] || "Hive Workspace";
  }, [workspaceRoot]);

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

  if (needsOnboarding) {
    return (
      <Onboarding
        onComplete={() => {
          window.localStorage.setItem("hive.onboarded", "1");
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
        }}
      />
      <WorkspaceRail
        onJoinRoom={() => setAddWsOpen(true)}
        onOpenFriends={() => setView("friends")}
        sidebarVisible={sidebarVisible}
        onToggleSidebar={() => setSidebarVisible((v) => !v)}
        onOpenSettings={() => openSettings()}
        settingsActive={view === "settings"}
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
        {view === "settings" ? (
          <SettingsView
            key={`settings-${settingsNonce}`}
            initialTab={settingsTab}
            palette={palette}
            onPaletteChange={setPalette}
            appearanceMode={appearanceMode}
            onAppearanceModeChange={setAppearanceMode}
          />
        ) : view === "friends" ? (
          <FriendsView
            onOpenSettings={(tab) => openSettings(tab)}
            onOpenDm={() => {
              setSelectedId(null);
              setMode("chat");
              setView("workspace");
            }}
          />
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
              syncLabel={
                sync.data?.relayConfigured
                  ? `Live · room ${sync.data.room}`
                  : "Local only"
              }
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
              </div>

              {showUtilityPane && (
                <>
                  <PaneResizeHandle
                    onPointerDown={startUtilityResize}
                    ariaLabel="Resize chat and utility panes"
                  />

                  <RightRail
                    width={utilityPaneWidth}
                    sessionId={selectedId}
                    pane={utilityPane}
                    activeRuntimeId={activeChat.data?.runtimeId ?? currentRuntime?.id ?? ""}
                    onChangePane={setUtilityPane}
                    onEditWorkflow={(def) =>
                      selectedId && setWorkflowDraft({ sessionId: selectedId, def })
                    }
                  />
                </>
              )}
            </div>
          </>
        )}
      </main>
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
      className="relative w-1.5 shrink-0 cursor-col-resize bg-transparent transition-colors hover:bg-white/10 active:bg-white/15"
      style={{ touchAction: "none" }}
      onPointerDown={onPointerDown}
    >
      <span className="pointer-events-none absolute inset-y-0 left-1/2 w-px -translate-x-1/2 bg-white/15" />
    </div>
  );
}

/// Single compact chat header: title + rename, sync state, the Chat/Diff mode
/// tabs, and the Tools toggle. Replaces the old two-row WorkspaceHeader +
/// ModeStrip stack (~130px of chrome, duplicated workspace path, a dead rename
/// button, and near-white tab text that vanished on light palettes).
function ChatHeaderBar({
  title,
  syncLabel,
  mode,
  onChangeMode,
  utilityPaneVisible,
  onToggleTools,
  onRename,
  contextPct,
  onOpenContext,
}: {
  title: string;
  syncLabel: string;
  mode: CanvasMode;
  onChangeMode: (m: CanvasMode) => void;
  utilityPaneVisible: boolean;
  onToggleTools: () => void;
  onRename: () => void;
  /// Percent of the model's context window the next reply will use (null
  /// until telemetry loads). Clicking the pill opens the Context pane.
  contextPct: number | null;
  onOpenContext: () => void;
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
        <h1 className="truncate text-base font-semibold tracking-tight">{title}</h1>
        <button
          className="shrink-0 rounded-md p-1 opacity-40 transition-opacity hover:opacity-100"
          title="Rename chat"
          aria-label="Rename chat"
          onClick={onRename}
        >
          <IconPencil size={13} />
        </button>
        <span className="ml-1 hidden shrink-0 text-xs opacity-40 sm:inline">{syncLabel}</span>
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
          {contextPct}%
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
              boxShadow: mode === t.id ? "0 1px 2px rgba(0,0,0,0.08)" : undefined,
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
          borderColor: utilityPaneVisible ? "rgba(87,161,168,0.4)" : "var(--hive-line)",
          background: utilityPaneVisible ? "rgba(87,161,168,0.18)" : "var(--hive-mist)",
          color: "var(--hive-ink)",
        }}
      >
        Tools
      </button>
    </div>
  );
}
