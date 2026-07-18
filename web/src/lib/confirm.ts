import { confirmDialog } from "@/components/Dialog";

/// Guard a destructive action behind an in-app confirm dialog. Keeps call sites
/// terse: `onClick={() => confirmThen("Remove X?", () => mut.mutate(id))}`.
/// Uses the in-app dialog (NOT window.confirm, which is a no-op in Tauri
/// webviews — that made these buttons dead in release builds).
export function confirmThen(message: string, run: () => void) {
  void confirmDialog(message, { danger: true }).then((ok) => {
    if (ok) run();
  });
}
