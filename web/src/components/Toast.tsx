import { useSyncExternalStore } from "react";
import { IconX } from "@/lib/icons";

export type ToastKind = "success" | "error" | "info";
export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
  /** Optional action button (e.g. Undo / Retry). */
  action?: { label: string; run: () => void };
}

// External store: dispatching a toast re-renders only <ToastHost>, never the
// component that fired it — so toasts are free to call from anywhere (including
// non-React mutation callbacks) with zero render cost on the caller side.
let toasts: Toast[] = [];
const listeners = new Set<() => void>();
let nextId = 1;

function emit() {
  // New array identity so useSyncExternalStore detects the change.
  toasts = toasts.slice();
  for (const l of listeners) l();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function dismissToast(id: number) {
  toasts = toasts.filter((t) => t.id !== id);
  for (const l of listeners) l();
}

const DEFAULT_TTL: Record<ToastKind, number> = {
  success: 3000,
  info: 4000,
  error: 7000, // errors linger longer so they're not missed
};

function push(kind: ToastKind, message: string, action?: Toast["action"], ttl?: number) {
  const id = nextId++;
  toasts = [...toasts, { id, kind, message, action }];
  emit();
  const life = ttl ?? DEFAULT_TTL[kind];
  if (life > 0) setTimeout(() => dismissToast(id), life);
  return id;
}

/** Fire-and-forget toast API; safe to call from anywhere. */
export const toast = {
  success: (message: string, action?: Toast["action"]) => push("success", message, action),
  error: (message: string, action?: Toast["action"]) => push("error", message, action),
  info: (message: string, action?: Toast["action"]) => push("info", message, action),
};

/** Coerce an unknown thrown value into a readable message. */
export function errMsg(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  try {
    return JSON.stringify(e);
  } catch {
    return String(e);
  }
}

const KIND_STYLE: Record<ToastKind, { bar: string; label: string }> = {
  success: { bar: "var(--hive-success)", label: "Done" },
  error: { bar: "var(--hive-danger)", label: "Error" },
  info: { bar: "var(--hive-accent-cool)", label: "Note" },
};

/** Mount once near the app root. */
export function ToastHost() {
  const items = useSyncExternalStore(subscribe, () => toasts);
  if (items.length === 0) return null;
  return (
    <div className="pointer-events-none fixed bottom-4 right-4 z-[1000] flex max-w-sm flex-col gap-2">
      {items.map((t) => {
        const s = KIND_STYLE[t.kind];
        return (
          <div
            key={t.id}
            role="status"
            className="pointer-events-auto flex items-start gap-3 rounded-xl border px-3 py-2.5 text-sm shadow-lg"
            style={{
              borderColor: "var(--hive-line)",
              background: "var(--hive-panel)",
              color: "var(--hive-ink)",
              borderLeft: `3px solid ${s.bar}`,
            }}
          >
            <div className="min-w-0 flex-1">
              <div className="text-[10px] font-semibold uppercase tracking-[0.14em] opacity-50">
                {s.label}
              </div>
              <div className="mt-0.5 break-words">{t.message}</div>
            </div>
            {t.action && (
              <button
                className="shrink-0 rounded-md px-2 py-1 text-xs font-medium underline opacity-80 hover:opacity-100"
                onClick={() => {
                  t.action!.run();
                  dismissToast(t.id);
                }}
              >
                {t.action.label}
              </button>
            )}
            <button
              className="shrink-0 px-1 leading-none opacity-50 hover:opacity-100"
              aria-label="Dismiss"
              onClick={() => dismissToast(t.id)}
            >
              <IconX size={13} />
            </button>
          </div>
        );
      })}
    </div>
  );
}
