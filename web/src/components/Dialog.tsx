import { useEffect, useState, useSyncExternalStore } from "react";
import { Modal } from "@/components/ui";

// In-app confirm/prompt dialogs. Tauri v2 webviews treat window.confirm/prompt/
// alert as no-ops (they return false/null without showing anything), which made
// every confirm-gated button dead in release builds. These replace them with
// real modals, dispatched from anywhere via confirmDialog()/promptDialog() and
// rendered once by <DialogHost/>.

interface ConfirmReq {
  kind: "confirm";
  title?: string;
  message: string;
  confirmLabel?: string;
  danger?: boolean;
  resolve: (ok: boolean) => void;
}
interface PromptReq {
  kind: "prompt";
  title?: string;
  message: string;
  placeholder?: string;
  password?: boolean;
  defaultValue?: string;
  resolve: (value: string | null) => void;
}
type DialogReq = ConfirmReq | PromptReq;

let current: DialogReq | null = null;
const listeners = new Set<() => void>();

function emit() {
  for (const l of listeners) l();
}
function subscribe(l: () => void) {
  listeners.add(l);
  return () => listeners.delete(l);
}
function getSnapshot() {
  return current;
}

function cancel(req: DialogReq) {
  if (req.kind === "prompt") req.resolve(null);
  else req.resolve(false);
}

function show(req: DialogReq) {
  // If one's already open, cancel it first so its promise always settles.
  if (current) cancel(current);
  current = req;
  emit();
}
function close() {
  current = null;
  emit();
}

/** Ask the user to confirm a (usually destructive) action. */
export function confirmDialog(
  message: string,
  opts: { title?: string; confirmLabel?: string; danger?: boolean } = {},
): Promise<boolean> {
  return new Promise((resolve) => show({ kind: "confirm", message, resolve, ...opts }));
}

/** Prompt the user for a string. Resolves to null if cancelled. */
export function promptDialog(
  message: string,
  opts: { title?: string; placeholder?: string; password?: boolean; defaultValue?: string } = {},
): Promise<string | null> {
  return new Promise((resolve) => show({ kind: "prompt", message, resolve, ...opts }));
}

export function DialogHost() {
  const req = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  const [value, setValue] = useState("");

  // Seed the input when a prompt opens.
  useEffect(() => {
    if (req?.kind === "prompt") setValue(req.defaultValue ?? "");
  }, [req]);

  if (!req) return null;

  const settle = (result: boolean | string | null) => {
    const r = req;
    close();
    if (r.kind === "confirm") (r.resolve as (v: boolean) => void)(result as boolean);
    else (r.resolve as (v: string | null) => void)(result as string | null);
  };

  const onCancel = () => settle(req.kind === "prompt" ? null : false);
  const onAccept = () => settle(req.kind === "prompt" ? value : true);

  return (
    // z-[990]: confirm/prompt dialogs are launched from inside other modals
    // (e.g. "Leave workspace" in AddWorkspaceModal at z-[900]), so they must
    // stack above every modal — only toasts (z-[1000]) sit higher.
    <Modal
      onClose={onCancel}
      overlayClassName="z-[990] flex items-center justify-center p-6"
      overlayStyle={{ background: "rgba(0,0,0,0.4)" }}
      panelClassName="w-full max-w-sm rounded-2xl border p-5 shadow-2xl"
      panelStyle={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)", color: "var(--hive-ink)" }}
    >
      {req.title && <div className="text-base font-semibold">{req.title}</div>}
      <div className={`text-sm ${req.title ? "mt-1 opacity-70" : "font-medium"}`}>{req.message}</div>

      {req.kind === "prompt" && (
        <input
          autoFocus
          type={req.password ? "password" : "text"}
          value={value}
          placeholder={req.placeholder}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onAccept();
          }}
          className="mt-3 w-full rounded-lg border px-3 py-2 text-sm outline-none"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-canvas)" }}
        />
      )}

      <div className="mt-5 flex justify-end gap-2">
        <button
          onClick={onCancel}
          className="rounded-lg px-3 py-1.5 text-sm opacity-60 hover:opacity-100"
        >
          Cancel
        </button>
        <button
          autoFocus={req.kind === "confirm"}
          onClick={onAccept}
          className="rounded-lg px-4 py-1.5 text-sm font-medium text-white hover:brightness-110"
          style={{
            background:
              req.kind === "confirm" && req.danger ? "rgb(200,70,70)" : "var(--hive-accent-cool)",
          }}
        >
          {req.kind === "prompt" ? "OK" : req.confirmLabel ?? "Confirm"}
        </button>
      </div>
    </Modal>
  );
}
