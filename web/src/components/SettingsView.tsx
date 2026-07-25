import { useMemo, useState } from "react";
import type { AppearanceMode, ThemeName } from "@/lib/theme";
import { IconButton, Modal, NavRow, SectionCap } from "@/components/ui";
import {
  IconUsers,
  IconSparkle,
  IconFile,
  IconActivity,
  IconHexagon,
  IconWrench,
  IconLock,
  IconAlertTriangle,
  IconArrowDown,
  IconX,
} from "@/lib/icons";
import { AccountSection } from "@/components/settings/AccountSection";
import { AppearanceSection } from "@/components/settings/AppearanceSection";
import { FolderGitSection } from "@/components/settings/FolderGitSection";
import { TeamSyncSection } from "@/components/settings/TeamSyncSection";
import { SchedulesSection } from "@/components/settings/SchedulesSection";
import { ModelsSection } from "@/components/settings/ModelsSection";
import { ToolsSection } from "@/components/settings/ToolsSection";
import { PermissionsSection } from "@/components/settings/PermissionsSection";
import { UpdatesSection } from "@/components/settings/UpdatesSection";
import { DangerZoneSection } from "@/components/settings/DangerZoneSection";

// ── Grouped nav (§6.5) ──────────────────────────────────────────────────────
// Settings is grouped by who a setting affects, not by a flat tab strip.
export type SettingsTab =
  | "account"
  | "appearance"
  | "folder-git"
  | "team-sync"
  | "schedules"
  | "models"
  | "tools"
  | "permissions"
  | "updates"
  | "danger";

type NavItem = { id: SettingsTab; label: string; icon: React.ReactNode };
type NavGroup = { cap: string; items: NavItem[] };

const NAV_GROUPS: NavGroup[] = [
  {
    cap: "You",
    items: [
      { id: "account", label: "Account", icon: <IconUsers size={16} /> },
      { id: "appearance", label: "Appearance", icon: <IconSparkle size={16} /> },
    ],
  },
  {
    cap: "This workspace",
    items: [
      { id: "folder-git", label: "Folder & Git", icon: <IconFile size={16} /> },
      { id: "team-sync", label: "Team sync", icon: <IconUsers size={16} /> },
      { id: "schedules", label: "Schedules", icon: <IconActivity size={16} /> },
    ],
  },
  {
    cap: "Agents",
    items: [
      { id: "models", label: "Models & runtimes", icon: <IconHexagon size={16} /> },
      { id: "tools", label: "Tools & MCP", icon: <IconWrench size={16} /> },
      { id: "permissions", label: "Permissions", icon: <IconLock size={16} /> },
    ],
  },
  {
    cap: "Advanced",
    items: [
      { id: "updates", label: "Updates & data", icon: <IconArrowDown size={16} /> },
      { id: "danger", label: "Danger zone", icon: <IconAlertTriangle size={16} /> },
    ],
  },
];

const TAB_LABEL: Record<SettingsTab, string> = Object.fromEntries(
  NAV_GROUPS.flatMap((g) => g.items.map((i) => [i.id, i.label])),
) as Record<SettingsTab, string>;

// Accepts a new id, a legacy tab name ("Account", "Team", …), or a deep-link
// fragment and resolves it to a current tab. Keeps the `settings:<tab>` tray
// route and older callers (e.g. FriendsView) working across the rename.
export function parseSettingsTab(raw: string | undefined | null): SettingsTab {
  switch ((raw ?? "").toLowerCase()) {
    case "account":
      return "account";
    case "appearance":
      return "appearance";
    case "folder-git":
    case "workspace":
      return "folder-git";
    case "team-sync":
    case "team":
      return "team-sync";
    case "schedules":
      return "schedules";
    case "models":
      return "models";
    case "tools":
      return "tools";
    case "permissions":
      return "permissions";
    case "updates":
      return "updates";
    case "danger":
      return "danger";
    default:
      return "account";
  }
}

/// Settings, presented as a focus-trapped sheet over the workspace (§6.5/§5.7).
/// It composes `Modal` from ui.tsx for the scrim + Escape/click-away + trap.
/// Nav is grouped and searchable; every section applies its changes immediately.
export function SettingsView({
  palette,
  onPaletteChange,
  appearanceMode,
  onAppearanceModeChange,
  initialTab,
  onClose,
}: {
  palette: ThemeName;
  onPaletteChange: (t: ThemeName) => void;
  appearanceMode: AppearanceMode;
  onAppearanceModeChange: (m: AppearanceMode) => void;
  /// Which tab to open on (defaults to Account). The tray uses this to jump
  /// straight to e.g. "Team sync".
  initialTab?: SettingsTab;
  onClose: () => void;
}) {
  const [tab, setTab] = useState<SettingsTab>(initialTab ?? "account");
  const [query, setQuery] = useState("");

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return NAV_GROUPS;
    return NAV_GROUPS.map((g) => ({
      cap: g.cap,
      items: g.items.filter((i) => i.label.toLowerCase().includes(q)),
    })).filter((g) => g.items.length > 0);
  }, [query]);

  return (
    <Modal
      onClose={onClose}
      overlayClassName="z-[900] flex items-center justify-center p-4"
      overlayStyle={{ background: "color-mix(in srgb, var(--hive-canvas) 60%, transparent)" }}
      panelClassName="flex overflow-hidden shadow-2xl"
      panelStyle={{
        width: 840,
        height: 600,
        maxWidth: "95vw",
        maxHeight: "90vh",
        borderRadius: 14,
        border: "1px solid var(--hive-line)",
        background: "var(--hive-canvas)",
        color: "var(--hive-ink)",
      }}
    >
      {/* Nav column (§4: 208) */}
      <nav
        className="flex w-[208px] shrink-0 flex-col gap-3 overflow-y-auto border-r p-3"
        style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
      >
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search settings"
          aria-label="Search settings"
          className="w-full rounded-xl border px-3 py-1.5 text-[13px] outline-none focus:border-[color:var(--hive-accent-cool)]"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)", color: "var(--hive-ink)" }}
        />
        {filtered.map((g) => (
          <div key={g.cap} className="flex flex-col">
            <SectionCap>{g.cap}</SectionCap>
            {g.items.map((i) => (
              <NavRow
                key={i.id}
                icon={i.icon}
                label={i.label}
                active={tab === i.id}
                onClick={() => setTab(i.id)}
              />
            ))}
          </div>
        ))}
        {filtered.length === 0 && (
          <p className="px-2.5 text-xs opacity-50">No settings match “{query}”.</p>
        )}
      </nav>

      {/* Content column */}
      <div className="flex min-w-0 flex-1 flex-col">
        <header
          className="flex shrink-0 items-center justify-between gap-3 border-b px-6 py-3"
          style={{ borderColor: "var(--hive-line)" }}
        >
          <h1 className="text-[19px] font-semibold">{TAB_LABEL[tab]}</h1>
          <IconButton label="Close settings" onClick={onClose}>
            <IconX size={18} />
          </IconButton>
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto px-6 py-5">
          {tab === "account" && <AccountSection />}
          {tab === "appearance" && (
            <AppearanceSection
              palette={palette}
              onPaletteChange={onPaletteChange}
              appearanceMode={appearanceMode}
              onAppearanceModeChange={onAppearanceModeChange}
            />
          )}
          {tab === "folder-git" && <FolderGitSection />}
          {tab === "team-sync" && <TeamSyncSection />}
          {tab === "schedules" && <SchedulesSection />}
          {tab === "models" && <ModelsSection />}
          {tab === "tools" && <ToolsSection />}
          {tab === "permissions" && <PermissionsSection />}
          {tab === "updates" && <UpdatesSection />}
          {tab === "danger" && <DangerZoneSection />}
        </div>
      </div>
    </Modal>
  );
}
