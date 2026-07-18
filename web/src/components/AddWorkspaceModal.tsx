import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  listWorkspaces,
  createWorkspace,
  joinWorkspace,
  workspaceInvite,
  removeWorkspace,
  workspaceShareCode,
  redeemShortCode,
  type WorkspaceInfoDto,
} from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";
import { Modal } from "@/components/ui";

type Tab = "create" | "join";

/// Modal for the rail's ＋ button: create a team workspace (generates a room +
/// E2EE key) or join one from an invite code, plus manage/copy-invite/leave for
/// the workspaces already joined. Replaces the old "dump into Settings" flow.
export function AddWorkspaceModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const qc = useQueryClient();
  const [tab, setTab] = useState<Tab>("create");
  const [name, setName] = useState("");
  const [invite, setInvite] = useState("");
  const [joinCode, setJoinCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [lastInvite, setLastInvite] = useState<string | null>(null);

  const workspaces = useQuery({
    queryKey: ["workspaces"],
    queryFn: listWorkspaces,
    enabled: open,
  });
  const rooms = (workspaces.data ?? []).filter((w) => w.kind === "room");

  if (!open) return null;

  async function refresh() {
    await qc.invalidateQueries({ queryKey: ["workspaces"] });
    await qc.invalidateQueries({ queryKey: ["chats"] });
    await qc.invalidateQueries({ queryKey: ["sync-status"] });
  }

  async function onCreate() {
    const n = name.trim();
    if (!n || busy) return;
    setBusy(true);
    try {
      const ws = await createWorkspace(n);
      const code = await workspaceInvite(ws.id);
      setLastInvite(code);
      setName("");
      await refresh();
      toast.success(`Created “${ws.name}”. Share the invite below to add people.`);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  async function onJoin() {
    const code = invite.trim();
    if (!code || busy) return;
    setBusy(true);
    try {
      const ws = await joinWorkspace(code);
      setInvite("");
      await refresh();
      toast.success(`Joined “${ws.name}”.`);
      onClose();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  async function copyInvite(ws: WorkspaceInfoDto) {
    try {
      const code = await workspaceInvite(ws.id);
      await navigator.clipboard.writeText(code);
      toast.success(`Invite for “${ws.name}” copied.`);
    } catch (e) {
      toast.error(errMsg(e));
    }
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

  async function joinByShortCode() {
    const c = joinCode.trim();
    if (!c || busy) return;
    setBusy(true);
    try {
      const r = await redeemShortCode(c);
      setJoinCode("");
      await refresh();
      toast.success(r.kind === "workspace" ? `Joined “${r.label}”.` : "Added.");
      onClose();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
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
    <Modal
      onClose={onClose}
      overlayClassName="z-[900] flex items-center justify-center p-4"
      overlayStyle={{ background: "rgba(0,0,0,0.5)" }}
      panelClassName="w-full max-w-md rounded-2xl border p-5 shadow-2xl"
      panelStyle={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)", color: "var(--hive-ink)" }}
    >
      <div className="mb-3 flex items-center justify-between">
        <h2 className="text-base font-semibold">Add workspace</h2>
        <button className="opacity-50 hover:opacity-100" aria-label="Close" onClick={onClose}>
          ✕
        </button>
      </div>

      <div className="mb-4 flex gap-1 rounded-lg p-1" style={{ background: "rgba(127,127,127,0.12)" }}>
        {(["create", "join"] as Tab[]).map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            className="flex-1 rounded-md px-3 py-1.5 text-sm font-medium capitalize transition-colors"
            style={{
              background: tab === t ? "var(--hive-accent-cool)" : "transparent",
              color: tab === t ? "#fff" : "var(--hive-ink)",
            }}
          >
            {t === "create" ? "Create" : "Join"}
          </button>
        ))}
      </div>

      {tab === "create" ? (
        <div className="space-y-3">
          <p className="text-xs opacity-60">
            Creates a private, end-to-end-encrypted team room. You become the owner and get an
            invite code to share — anyone who pastes it joins and syncs with you.
          </p>
          <input
            autoFocus
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && onCreate()}
            placeholder="Workspace name (e.g. Acme App)"
            className="w-full rounded-lg border px-3 py-2 text-sm outline-none"
            style={{ borderColor: "var(--hive-line)", background: "var(--hive-canvas)", color: "var(--hive-ink)" }}
          />
          <button
            onClick={onCreate}
            disabled={busy || !name.trim()}
            className="w-full rounded-lg px-3 py-2 text-sm font-semibold text-white disabled:opacity-40"
            style={{ background: "var(--hive-accent-cool)" }}
          >
            {busy ? "Creating…" : "Create workspace"}
          </button>
          {lastInvite && (
            <div className="rounded-lg border p-2" style={{ borderColor: "var(--hive-line)" }}>
              <div className="mb-1 text-[10px] font-semibold uppercase tracking-wider opacity-50">
                Invite code
              </div>
              <code className="block break-all text-xs opacity-80">{lastInvite}</code>
              <button
                className="mt-2 rounded-md px-2 py-1 text-xs font-medium underline opacity-80 hover:opacity-100"
                onClick={() => {
                  navigator.clipboard.writeText(lastInvite);
                  toast.success("Invite copied.");
                }}
              >
                Copy invite
              </button>
            </div>
          )}
        </div>
      ) : (
        <div className="space-y-3">
          <p className="text-xs opacity-60">
            Paste an invite code (<code>hivews1:…</code>) someone shared with you. It carries the
            relay, room, and key needed to sync.
          </p>
          <textarea
            autoFocus
            value={invite}
            onChange={(e) => setInvite(e.target.value)}
            placeholder="hivews1:…"
            rows={3}
            className="w-full resize-none rounded-lg border px-3 py-2 font-mono text-xs outline-none"
            style={{ borderColor: "var(--hive-line)", background: "var(--hive-canvas)", color: "var(--hive-ink)" }}
          />
          <button
            onClick={onJoin}
            disabled={busy || !invite.trim()}
            className="w-full rounded-lg px-3 py-2 text-sm font-semibold text-white disabled:opacity-40"
            style={{ background: "var(--hive-accent-cool)" }}
          >
            {busy ? "Joining…" : "Join workspace"}
          </button>
          <div className="flex items-center gap-2 pt-1 text-xs opacity-50">
            <div className="h-px flex-1" style={{ background: "var(--hive-line)" }} />
            or a short code
            <div className="h-px flex-1" style={{ background: "var(--hive-line)" }} />
          </div>
          <div className="flex gap-2">
            <input
              value={joinCode}
              onChange={(e) => setJoinCode(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && joinByShortCode()}
              placeholder="e.g. K7P2QX"
              className="flex-1 rounded-lg border px-3 py-2 font-mono text-sm uppercase outline-none"
              style={{ borderColor: "var(--hive-line)", background: "var(--hive-canvas)", color: "var(--hive-ink)" }}
            />
            <button
              onClick={joinByShortCode}
              disabled={busy || !joinCode.trim()}
              className="shrink-0 rounded-lg px-3 py-2 text-sm font-semibold text-white disabled:opacity-40"
              style={{ background: "var(--hive-accent-cool)" }}
            >
              Use code
            </button>
          </div>
        </div>
      )}

      {rooms.length > 0 && (
        <div className="mt-5 border-t pt-3" style={{ borderColor: "var(--hive-line)" }}>
          <div className="mb-2 text-[10px] font-semibold uppercase tracking-wider opacity-50">
            Your team workspaces
          </div>
          <ul className="space-y-1">
            {rooms.map((w) => (
              <li key={w.id} className="flex items-center justify-between gap-2 text-sm">
                <span className="truncate">{w.name}</span>
                <span className="flex shrink-0 gap-2 text-xs">
                  <button className="underline opacity-70 hover:opacity-100" onClick={() => shareShortCode(w)}>
                    Short code
                  </button>
                  <button className="underline opacity-70 hover:opacity-100" onClick={() => copyInvite(w)}>
                    Copy invite
                  </button>
                  <button
                    className="underline opacity-70 hover:opacity-100"
                    style={{ color: "#ff5a5f" }}
                    onClick={() => leave(w)}
                  >
                    Leave
                  </button>
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </Modal>
  );
}
