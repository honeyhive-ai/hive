import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { listPendingInvites, acceptInvite, dismissInvite } from "@/lib/ipc";
import { toast, errMsg } from "@/components/Toast";
import { IconUsers, IconX } from "@/lib/icons";

/// A quiet top-right banner for workspace invites delivered to this account's
/// inbox. Polls periodically; Accept joins the workspace (the invite carries the
/// sealed key), Dismiss hides it. Silent when there are none.
export function PendingInvitesBanner() {
  const qc = useQueryClient();
  const invites = useQuery({
    queryKey: ["pending-invites"],
    queryFn: listPendingInvites,
    refetchInterval: 20000,
  });
  const accept = useMutation({
    mutationFn: (code: string) => acceptInvite(code),
    onSuccess: async (ws) => {
      toast.success(`Joined ${ws.name}.`);
      await qc.invalidateQueries({ queryKey: ["workspaces"] });
      await qc.invalidateQueries({ queryKey: ["pending-invites"] });
    },
    onError: (e) => toast.error(`Couldn't join: ${errMsg(e)}`),
  });
  const dismiss = useMutation({
    mutationFn: (seq: number) => dismissInvite(seq),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["pending-invites"] }),
    onError: (e) => toast.error(`Couldn't dismiss: ${errMsg(e)}`),
  });

  const list = invites.data ?? [];
  if (list.length === 0) return null;

  return (
    // Cap the stack to the viewport with internal scroll — a user invited to many
    // workspaces at once shouldn't get a column of cards taller than the window.
    <div className="fixed right-4 top-4 z-[60] flex max-h-[calc(100vh-2rem)] w-[320px] flex-col gap-2 overflow-y-auto">
      {list.map((inv) => {
        // Scope the busy state to THIS invite so joining one doesn't grey out the
        // rest (the mutations are single-flight, `variables` is the in-flight arg).
        const joining = accept.isPending && accept.variables === inv.code;
        const dismissing = dismiss.isPending && dismiss.variables === inv.seq;
        const busy = joining || dismissing;
        return (
        <div
          key={inv.seq}
          className="rounded-2xl border p-4 shadow-xl"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)", color: "var(--hive-ink)" }}
        >
          <div className="flex items-start gap-3">
            <div className="mt-0.5" style={{ color: "var(--hive-accent-cool)" }} aria-hidden>
              <IconUsers size={18} />
            </div>
            <div className="min-w-0 flex-1">
              <div className="text-sm font-semibold">Workspace invite</div>
              <div className="mt-0.5 text-sm opacity-70">
                <b>@{inv.fromLogin || "someone"}</b> invited you to <b>{inv.workspaceName}</b>
              </div>
              <div className="mt-3 flex items-center gap-2">
                <button
                  onClick={() => accept.mutate(inv.code)}
                  disabled={busy}
                  className="rounded-lg px-3 py-1.5 text-sm font-medium text-white hover:brightness-110 disabled:opacity-70"
                  style={{ background: "var(--hive-accent-cool)" }}
                >
                  {joining ? "Joining…" : "Accept"}
                </button>
                <button
                  onClick={() => dismiss.mutate(inv.seq)}
                  disabled={busy}
                  className="rounded-lg px-3 py-1.5 text-sm opacity-60 hover:opacity-100 disabled:opacity-40"
                >
                  Dismiss
                </button>
              </div>
            </div>
            <button
              onClick={() => dismiss.mutate(inv.seq)}
              disabled={busy}
              aria-label="Dismiss"
              className="shrink-0 opacity-40 hover:opacity-80 disabled:opacity-20"
            >
              <IconX size={14} />
            </button>
          </div>
        </div>
        );
      })}
    </div>
  );
}
