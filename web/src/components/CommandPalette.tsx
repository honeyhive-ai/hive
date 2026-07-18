import { useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { listChats, listWorkspaces } from "@/lib/ipc";
import { Modal } from "@/components/ui";

export interface PaletteActions {
  newChat: () => void;
  openSettings: () => void;
  selectChat: (id: string) => void;
  selectWorkspace: (id: string) => void;
  /// Focus mode: collapse/restore the sidebar (⌘B) and tools rail (⌘J).
  toggleSidebar?: () => void;
  toggleTools?: () => void;
}

interface Cmd {
  id: string;
  label: string;
  hint?: string;
  run: () => void;
}

/// ⌘K / Ctrl-K command palette: fuzzy-jump to a chat, switch workspace, or run
/// a quick action. Data is only fetched while open; no extra deps.
export function CommandPalette({
  open,
  onClose,
  actions,
}: {
  open: boolean;
  onClose: () => void;
  actions: PaletteActions;
}) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);

  const chats = useQuery({ queryKey: ["chats"], queryFn: listChats, enabled: open });
  const workspaces = useQuery({ queryKey: ["workspaces-palette"], queryFn: listWorkspaces, enabled: open });

  // Reset on open; Modal autofocuses the input (its first control).
  useEffect(() => {
    if (open) {
      setQuery("");
      setActive(0);
    }
  }, [open]);

  const commands = useMemo<Cmd[]>(() => {
    const run = (fn: () => void) => () => {
      fn();
      onClose();
    };
    const items: Cmd[] = [
      { id: "new-chat", label: "New chat", hint: "Action", run: run(actions.newChat) },
      { id: "settings", label: "Open settings", hint: "Action", run: run(actions.openSettings) },
    ];
    if (actions.toggleSidebar) {
      items.push({
        id: "toggle-sidebar",
        label: "Toggle sidebar",
        hint: "View · ⌘B",
        run: run(actions.toggleSidebar),
      });
    }
    if (actions.toggleTools) {
      items.push({
        id: "toggle-tools",
        label: "Toggle tools rail",
        hint: "View · ⌘J",
        run: run(actions.toggleTools),
      });
    }
    for (const w of workspaces.data ?? []) {
      items.push({
        id: `ws-${w.id}`,
        label: `Switch to ${w.name}`,
        hint: w.kind === "room" ? "Workspace · room" : "Workspace",
        run: run(() => actions.selectWorkspace(w.id)),
      });
    }
    for (const c of (chats.data ?? []).filter((c) => !c.archived)) {
      items.push({
        id: `chat-${c.id}`,
        label: c.title || "Untitled chat",
        hint: "Chat",
        run: run(() => actions.selectChat(c.id)),
      });
    }
    return items;
  }, [chats.data, workspaces.data, actions, onClose]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return commands;
    return commands.filter(
      (c) => c.label.toLowerCase().includes(q) || (c.hint?.toLowerCase().includes(q) ?? false),
    );
  }, [commands, query]);

  // Keep the active index in range as the filtered list shrinks.
  useEffect(() => {
    setActive((a) => Math.min(a, Math.max(0, filtered.length - 1)));
  }, [filtered.length]);

  if (!open) return null;

  return (
    <Modal
      onClose={onClose}
      overlayClassName="z-[900] flex items-start justify-center pt-[12vh]"
      overlayStyle={{ background: "rgba(0,0,0,0.45)" }}
      panelClassName="w-full max-w-lg overflow-hidden rounded-2xl border shadow-2xl"
      panelStyle={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)", color: "var(--hive-ink)" }}
    >
      <input
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        placeholder="Search chats, workspaces, actions…"
        className="w-full border-b bg-transparent px-4 py-3 text-base outline-none"
        style={{ borderColor: "var(--hive-line)" }}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") {
            e.preventDefault();
            setActive((a) => Math.min(a + 1, filtered.length - 1));
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            setActive((a) => Math.max(a - 1, 0));
          } else if (e.key === "Enter") {
            e.preventDefault();
            filtered[active]?.run();
          }
        }}
      />
      <div className="max-h-[50vh] overflow-y-auto py-1">
        {filtered.length === 0 && <div className="px-4 py-6 text-center text-sm opacity-50">No matches.</div>}
        {filtered.map((c, i) => (
          <button
            key={c.id}
            onMouseEnter={() => setActive(i)}
            onClick={c.run}
            className="flex w-full items-center justify-between gap-3 px-4 py-2.5 text-left text-sm"
            style={{ background: i === active ? "var(--hive-mist)" : "transparent" }}
          >
            <span className="truncate">{c.label}</span>
            {c.hint && <span className="shrink-0 text-xs opacity-45">{c.hint}</span>}
          </button>
        ))}
      </div>
    </Modal>
  );
}
