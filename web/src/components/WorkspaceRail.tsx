import { useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  listWorkspaces,
  setActiveWorkspace,
  setWorkspaceIcon,
  syncStatus,
  workspaceInvite,
  workspaceShareCode,
  removeWorkspace,
  type WorkspaceInfoDto,
} from "@/lib/ipc";
import { HiveBrandMark } from "@/components/HiveBrand";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";
import { IconPlus, IconUsers, IconPanelLeft, IconGear } from "@/lib/icons";

/// Discord-style rail of workspace "servers": Personal (local) pinned at top,
/// then each joined team room, then a ＋ to create/join one. Click selects
/// (scopes the chat list); right-click a team opens a menu to share or leave it.
export function WorkspaceRail({
  onJoinRoom,
  onOpenFriends,
  sidebarVisible,
  onToggleSidebar,
  onOpenSettings,
  settingsActive,
}: {
  onJoinRoom: () => void;
  onOpenFriends?: () => void;
  /// Focus mode: collapse/restore the sidebar (also ⌘B).
  sidebarVisible?: boolean;
  onToggleSidebar?: () => void;
  /// Settings lives at the rail's bottom (VS Code's activity-bar pattern) so
  /// it stays reachable when the sidebar is dismissed.
  onOpenSettings?: () => void;
  settingsActive?: boolean;
}) {
  const qc = useQueryClient();
  const sync = useQuery({ queryKey: ["sync-status"], queryFn: syncStatus });
  const workspaces = useQuery({
    queryKey: ["workspaces", sync.data?.relayConfigured, sync.data?.room],
    queryFn: listWorkspaces,
  });
  const [menu, setMenu] = useState<{ ws: WorkspaceInfoDto; x: number; y: number } | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);
  const iconTarget = useRef<string | null>(null);

  function chooseIcon(ws: WorkspaceInfoDto) {
    iconTarget.current = ws.id;
    fileInput.current?.click();
  }

  async function onIconFile(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    e.target.value = ""; // allow re-picking the same file
    const wsId = iconTarget.current;
    iconTarget.current = null;
    if (!file || !wsId) return;
    try {
      const dataUrl = await fileToIconDataUrl(file);
      await setWorkspaceIcon(wsId, dataUrl);
      await refresh();
      toast.success("Workspace icon updated.");
    } catch (err) {
      toast.error(errMsg(err));
    }
  }

  function removeIcon(ws: WorkspaceInfoDto) {
    void (async () => {
      try {
        await setWorkspaceIcon(ws.id, null);
        await refresh();
        toast.success("Workspace icon removed.");
      } catch (err) {
        toast.error(errMsg(err));
      }
    })();
  }

  async function select(id: string) {
    await setActiveWorkspace(id);
    await qc.invalidateQueries({ queryKey: ["workspaces"] });
    await qc.invalidateQueries({ queryKey: ["chats"] });
  }

  async function refresh() {
    await qc.invalidateQueries({ queryKey: ["workspaces"] });
    await qc.invalidateQueries({ queryKey: ["chats"] });
    await qc.invalidateQueries({ queryKey: ["sync-status"] });
  }

  async function shareShortCode(ws: WorkspaceInfoDto) {
    try {
      const { code, expiresIn } = await workspaceShareCode(ws.id);
      await navigator.clipboard.writeText(code);
      toast.success(`Short code ${code} copied — expires in ${Math.round(expiresIn / 60)} min.`);
    } catch (e) {
      toast.error(errMsg(e));
    }
  }
  async function copyInvite(ws: WorkspaceInfoDto) {
    try {
      await navigator.clipboard.writeText(await workspaceInvite(ws.id));
      toast.success(`Invite for “${ws.name}” copied.`);
    } catch (e) {
      toast.error(errMsg(e));
    }
  }
  function leave(ws: WorkspaceInfoDto) {
    confirmThen(`Leave “${ws.name}”? Its chats stop syncing on this device.`, async () => {
      try {
        await removeWorkspace(ws.id);
        await refresh();
        toast.success(`Left “${ws.name}”.`);
      } catch (e) {
        toast.error(errMsg(e));
      }
    });
  }

  return (
    <div
      className="flex w-14 shrink-0 flex-col items-center gap-2 border-r py-3"
      style={{ background: "var(--hive-sidebar-bottom)", borderColor: "var(--hive-line)" }}
    >
      {(workspaces.data ?? []).map((w) => {
        const local = w.kind === "local";
        return (
          <button
            key={w.id}
            onClick={() => select(w.id)}
            onContextMenu={(e) => {
              e.preventDefault();
              setMenu({ ws: w, x: e.clientX, y: e.clientY });
            }}
            title={local ? "Personal — right-click to set an icon" : `${w.name} — right-click for options`}
            className="flex h-10 w-10 items-center justify-center overflow-hidden rounded-2xl text-sm font-semibold transition-all hover:brightness-110"
            style={{
              background: w.active ? "var(--hive-accent-cool)" : "var(--hive-panel)",
              color: w.active ? "#fff" : "var(--hive-ink)",
              border: w.active ? "none" : "1px solid var(--hive-line)",
              boxShadow: w.active ? "0 0 0 2px var(--hive-accent-cool)" : "none",
            }}
          >
            {w.iconUrl ? (
              <img src={w.iconUrl} alt="" className="h-full w-full object-cover" />
            ) : local ? (
              <HiveBrandMark size={22} />
            ) : (
              <span>{roomInitials(w.name)}</span>
            )}
          </button>
        );
      })}
      <button
        onClick={onJoinRoom}
        title="Add a workspace — create or join a team"
        aria-label="Add a workspace"
        className="mt-1 flex h-10 w-10 items-center justify-center rounded-2xl transition-all hover:brightness-110"
        style={{ background: "var(--hive-panel)", color: "var(--hive-ink)", border: "1px solid var(--hive-line)", opacity: 0.85 }}
      >
        <IconPlus size={18} />
      </button>

      {onOpenFriends && (
        <button
          onClick={onOpenFriends}
          title="Friends — add collaborators by GitHub username, see who's online"
          aria-label="Friends"
          className="mt-auto flex h-10 w-10 items-center justify-center rounded-2xl transition-all hover:brightness-110"
          style={{ background: "var(--hive-panel)", color: "var(--hive-ink)", border: "1px solid var(--hive-line)", opacity: 0.85 }}
        >
          <IconUsers size={17} />
        </button>
      )}

      {onToggleSidebar && (
        <button
          onClick={onToggleSidebar}
          title={`${sidebarVisible ? "Hide" : "Show"} sidebar (⌘B)`}
          aria-label={`${sidebarVisible ? "Hide" : "Show"} sidebar`}
          aria-pressed={!sidebarVisible}
          className={`flex h-10 w-10 items-center justify-center rounded-2xl transition-all hover:brightness-110 ${onOpenFriends ? "" : "mt-auto"}`}
          style={{
            background: "var(--hive-panel)",
            color: "var(--hive-ink)",
            border: "1px solid var(--hive-line)",
            opacity: sidebarVisible ? 0.85 : 1,
          }}
        >
          <IconPanelLeft size={16} />
        </button>
      )}

      {onOpenSettings && (
        <button
          onClick={onOpenSettings}
          title="Settings"
          aria-label="Settings"
          aria-pressed={settingsActive}
          className={`flex h-10 w-10 items-center justify-center rounded-2xl transition-all hover:brightness-110 ${onOpenFriends || onToggleSidebar ? "" : "mt-auto"}`}
          style={{
            background: settingsActive ? "var(--hive-accent-cool)" : "var(--hive-panel)",
            color: settingsActive ? "#fff" : "var(--hive-ink)",
            border: settingsActive ? "none" : "1px solid var(--hive-line)",
            opacity: settingsActive ? 1 : 0.85,
          }}
        >
          <IconGear size={17} />
        </button>
      )}

      {menu && (
        <>
          {/* click-away backdrop */}
          <div className="fixed inset-0 z-[950]" onClick={() => setMenu(null)} onContextMenu={(e) => { e.preventDefault(); setMenu(null); }} />
          <div
            className="fixed z-[951] min-w-44 rounded-lg border py-1 text-sm shadow-2xl"
            style={{ left: menu.x + 4, top: menu.y, borderColor: "var(--hive-line)", background: "var(--hive-panel)", color: "var(--hive-ink)" }}
          >
            <div className="truncate px-3 py-1 text-[10px] font-semibold uppercase tracking-wider opacity-50">
              {menu.ws.name}
            </div>
            <MenuItem label="Set icon…" onClick={() => { const ws = menu.ws; setMenu(null); chooseIcon(ws); }} />
            {menu.ws.iconUrl && (
              <MenuItem label="Remove icon" onClick={() => { removeIcon(menu.ws); setMenu(null); }} />
            )}
            {menu.ws.kind === "room" && (
              <>
                <MenuItem label="Share short code" onClick={() => { void shareShortCode(menu.ws); setMenu(null); }} />
                <MenuItem label="Copy invite" onClick={() => { void copyInvite(menu.ws); setMenu(null); }} />
                <MenuItem label="Leave workspace" danger onClick={() => { leave(menu.ws); setMenu(null); }} />
              </>
            )}
          </div>
        </>
      )}

      <input
        ref={fileInput}
        type="file"
        accept="image/png,image/jpeg,image/webp,image/gif,image/svg+xml"
        className="hidden"
        onChange={(e) => void onIconFile(e)}
      />
    </div>
  );
}

function MenuItem({ label, onClick, danger }: { label: string; onClick: () => void; danger?: boolean }) {
  return (
    <button
      className="block w-full px-3 py-1.5 text-left hover:bg-[rgba(127,127,127,0.15)]"
      style={danger ? { color: "#ff5a5f" } : undefined}
      onClick={onClick}
    >
      {label}
    </button>
  );
}

/// Read an image file and downscale it to a small square PNG data URL so the
/// rail glyph stays tiny in settings (we cap the backend at ~512 KB anyway).
/// SVGs are passed through as-is (they're already small and scale crisply).
export async function fileToIconDataUrl(file: File, size = 128): Promise<string> {
  const readAsDataUrl = () =>
    new Promise<string>((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result));
      reader.onerror = () => reject(reader.error ?? new Error("could not read file"));
      reader.readAsDataURL(file);
    });

  const raw = await readAsDataUrl();
  if (file.type === "image/svg+xml") return raw;

  const img = new Image();
  await new Promise<void>((resolve, reject) => {
    img.onload = () => resolve();
    img.onerror = () => reject(new Error("could not decode image"));
    img.src = raw;
  });

  const canvas = document.createElement("canvas");
  canvas.width = size;
  canvas.height = size;
  const ctx = canvas.getContext("2d");
  if (!ctx) return raw;
  // Cover-crop to a centered square so non-square uploads aren't distorted.
  const side = Math.min(img.width, img.height);
  const sx = (img.width - side) / 2;
  const sy = (img.height - side) / 2;
  ctx.drawImage(img, sx, sy, side, side, 0, 0, size, size);
  return canvas.toDataURL("image/png");
}

function roomInitials(name: string): string {
  const trimmed = name.trim();
  if (!trimmed) return "#";
  const parts = trimmed.split(/[\s\-_]+/).filter(Boolean);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return trimmed.slice(0, 2).toUpperCase();
}
