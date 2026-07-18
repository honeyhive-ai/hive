import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  friendsOverview,
  friendSendRequest,
  friendAccept,
  friendReject,
  friendRemove,
  friendSetVisibility,
  friendOpenDm,
  type Presence,
} from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";
import { IconUsers } from "@/lib/icons";

/// Dot color for a friend's presence. Exported for unit testing.
export function presenceColor(p: Presence): string {
  switch (p) {
    case "online":
      return "var(--hive-success)";
    case "away":
      return "var(--hive-warn)";
    default:
      return "var(--hive-line)"; // muted/offline
  }
}

/// Human label for the result of sending a friend request.
export function requestOutcomeMessage(outcome: string, login: string): { ok: boolean; msg: string } {
  switch (outcome) {
    case "sent":
      return { ok: true, msg: `Request sent to @${login}.` };
    case "alreadyFriends":
      return { ok: false, msg: `You're already connected with @${login}.` };
    case "capReached":
      return { ok: false, msg: "Free plan limit reached (5 collaborators). Upgrade for more." };
    case "userNotFound":
      return { ok: false, msg: `@${login} hasn't joined Hive yet.` };
    default:
      return { ok: false, msg: "Couldn't send that request." };
  }
}

export function FriendsView({
  onOpenDm,
  onOpenSettings,
}: {
  onOpenDm?: () => void;
  /// Jump to Settings on a given tab (used by the not-yet-enabled empty state).
  onOpenSettings?: (tab: "Account" | "Team") => void;
} = {}) {
  const qc = useQueryClient();
  const [handle, setHandle] = useState("");
  const [busy, setBusy] = useState(false);
  const [appearOffline, setAppearOffline] = useState(false);

  const overview = useQuery({
    queryKey: ["friends-overview"],
    queryFn: friendsOverview,
    refetchInterval: 20_000, // keep presence fresh + pick up incoming requests
  });

  const refresh = () => qc.invalidateQueries({ queryKey: ["friends-overview"] });

  async function send() {
    const login = handle.trim().replace(/^@/, "");
    if (!login) return;
    setBusy(true);
    try {
      const res = await friendSendRequest(login);
      const { ok, msg } = requestOutcomeMessage(res.outcome, login);
      ok ? toast.success(msg) : toast.error(msg);
      if (ok) setHandle("");
      await refresh();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  async function accept(id: string, login: string) {
    try {
      await friendAccept(id);
      toast.success(`You're now connected with @${login}.`);
      await refresh();
    } catch (e) {
      toast.error(errMsg(e));
    }
  }
  async function reject(id: string) {
    try {
      await friendReject(id);
      await refresh();
    } catch (e) {
      toast.error(errMsg(e));
    }
  }
  async function message(accountId: string, login: string) {
    try {
      await friendOpenDm(accountId, login);
      await qc.invalidateQueries({ queryKey: ["workspaces"] });
      await qc.invalidateQueries({ queryKey: ["chats"] });
      onOpenDm?.();
    } catch (e) {
      toast.error(errMsg(e));
    }
  }
  function remove(accountId: string, login: string) {
    confirmThen(`Remove @${login} from your collaborators?`, async () => {
      try {
        await friendRemove(accountId);
        await refresh();
      } catch (e) {
        toast.error(errMsg(e));
      }
    });
  }
  async function toggleVisibility() {
    const next = !appearOffline;
    setAppearOffline(next);
    try {
      await friendSetVisibility(next);
      await refresh();
    } catch (e) {
      setAppearOffline(!next);
      toast.error(errMsg(e));
    }
  }

  const data = overview.data;

  if (data && !data.enabled) {
    return (
      <div className="mx-auto max-w-xl p-8 text-sm" style={{ color: "var(--hive-ink)" }}>
        <div
          className="rounded-2xl border p-6"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
        >
          <div
            className="flex h-11 w-11 items-center justify-center rounded-2xl"
            style={{ background: "rgba(87,161,168,0.18)", color: "var(--hive-accent-cool)" }}
            aria-hidden
          >
            <IconUsers size={20} />
          </div>
          <h1 className="mt-4 text-xl font-semibold tracking-tight">Collaborators</h1>
          <p className="mt-2 opacity-70">
            Add teammates by GitHub username, see who's online, and DM them — end-to-end
            encrypted. Two things to set up first:
          </p>
          <div className="mt-4 flex flex-wrap gap-2">
            <button
              onClick={() => onOpenSettings?.("Account")}
              className="rounded-xl px-3.5 py-2 text-sm font-semibold text-white transition-all hover:brightness-105"
              style={{ background: "var(--hive-accent-cool)" }}
            >
              Sign in with GitHub
            </button>
            <button
              onClick={() => onOpenSettings?.("Team")}
              className="rounded-xl border px-3.5 py-2 text-sm font-medium transition-colors hover:border-[color:var(--hive-accent-cool)]"
              style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
            >
              Configure a relay
            </button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="mx-auto flex max-w-xl flex-col gap-6 p-8" style={{ color: "var(--hive-ink)" }}>
      <header className="flex items-center justify-between">
        <h1 className="text-lg font-semibold">Collaborators</h1>
        <label className="flex items-center gap-2 text-xs opacity-70">
          <input type="checkbox" checked={appearOffline} onChange={toggleVisibility} />
          Appear offline
        </label>
      </header>

      {/* Add by GitHub username */}
      <div className="flex gap-2">
        <input
          value={handle}
          onChange={(e) => setHandle(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && void send()}
          placeholder="Add by GitHub username…"
          className="flex-1 rounded-lg border px-3 py-2 text-sm outline-none"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)", color: "var(--hive-ink)" }}
        />
        <button
          onClick={() => void send()}
          disabled={busy || !handle.trim()}
          className="rounded-lg px-4 py-2 text-sm font-medium transition-all hover:brightness-110 disabled:opacity-50"
          style={{ background: "var(--hive-accent-cool)", color: "#fff" }}
        >
          Add
        </button>
      </div>

      {/* Incoming requests */}
      {data && data.incoming.length > 0 && (
        <section className="flex flex-col gap-2">
          <h2 className="text-[11px] font-semibold uppercase tracking-wider opacity-60">Requests</h2>
          {data.incoming.map((r) => (
            <div
              key={r.requestId}
              className="flex items-center justify-between rounded-lg border px-3 py-2"
              style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
            >
              <span className="text-sm font-medium">@{r.fromLogin}</span>
              <div className="flex gap-2">
                <button
                  onClick={() => void accept(r.requestId, r.fromLogin)}
                  className="rounded-md px-3 py-1 text-xs font-medium text-white transition-all hover:brightness-110"
                  style={{ background: "var(--hive-accent-cool)" }}
                >
                  Accept
                </button>
                <button
                  onClick={() => void reject(r.requestId)}
                  className="rounded-md border px-3 py-1 text-xs opacity-80 hover:opacity-100"
                  style={{ borderColor: "var(--hive-line)" }}
                >
                  Decline
                </button>
              </div>
            </div>
          ))}
        </section>
      )}

      {/* Friends list */}
      <section className="flex flex-col gap-2">
        <h2 className="text-[11px] font-semibold uppercase tracking-wider opacity-60">Connected</h2>
        {data && data.friends.length === 0 && (
          <p className="text-sm opacity-60">
            No collaborators yet. Add someone by their GitHub username above.
          </p>
        )}
        {data?.friends.map((f) => (
          <div
            key={f.accountId}
            className="group flex items-center justify-between rounded-lg border px-3 py-2"
            style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
          >
            <span className="flex items-center gap-2 text-sm">
              <span
                className="inline-block h-2.5 w-2.5 rounded-full"
                style={{ background: presenceColor(f.presence) }}
                title={f.presence}
              />
              @{f.login}
            </span>
            <div className="flex items-center gap-3">
              <button
                onClick={() => void message(f.accountId, f.login)}
                className="rounded-md border px-3 py-1 text-xs font-medium transition-all hover:brightness-110"
                style={{ borderColor: "var(--hive-line)", background: "var(--hive-canvas)", color: "var(--hive-ink)" }}
              >
                Message
              </button>
              <button
                onClick={() => remove(f.accountId, f.login)}
                className="text-xs opacity-0 transition-opacity group-hover:opacity-100"
                style={{ color: "#ff5a5f" }}
              >
                Remove
              </button>
            </div>
          </div>
        ))}
      </section>
    </div>
  );
}
