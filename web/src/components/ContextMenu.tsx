import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";

/// One entry in a right-click menu. A separator draws a divider; otherwise it's a
/// clickable row (optionally an icon, a keyboard-shortcut hint, disabled, or a
/// destructive `danger` action).
export type MenuItemSpec =
  | { type: "separator" }
  | {
      type?: "item";
      label: string;
      onClick: () => void;
      icon?: ReactNode;
      hint?: string;
      danger?: boolean;
      disabled?: boolean;
    };

type OpenState = { x: number; y: number; items: MenuItemSpec[] };

/// Themed right-click menus for any element. Usage:
///
///   const { open, menu } = useContextMenu();
///   <div onContextMenu={(e) => open(e, [{ label: "Rename…", onClick: rename }])}>…</div>
///   {menu}
///
/// The hook owns the open state + the rendered menu node (fixed-positioned at the
/// cursor, edge-flipped, click-away/Esc to close, keyboard-navigable). Render
/// `menu` once anywhere in the component.
export function useContextMenu() {
  const [state, setState] = useState<OpenState | null>(null);
  const open = (e: React.MouseEvent, items: MenuItemSpec[]) => {
    if (items.length === 0) return;
    e.preventDefault();
    e.stopPropagation();
    setState({ x: e.clientX, y: e.clientY, items });
  };
  const close = () => setState(null);
  const menu = state ? <ContextMenuView {...state} onClose={close} /> : null;
  return { open, close, menu };
}

function isSep(i: MenuItemSpec): i is { type: "separator" } {
  return "type" in i && i.type === "separator";
}

function ContextMenuView({ x, y, items, onClose }: OpenState & { onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  // Index of the keyboard-focused item (−1 = none). Only real items are focusable.
  const [active, setActive] = useState(-1);
  const focusable = items
    .map((it, i) => (!isSep(it) && !it.disabled ? i : -1))
    .filter((i) => i >= 0);

  // Position after mount so we can measure the menu and flip it away from the
  // viewport edges (open leftward / upward when it would overflow).
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    const pad = 8;
    let left = x;
    let top = y;
    if (left + width + pad > window.innerWidth) left = Math.max(pad, x - width);
    if (top + height + pad > window.innerHeight) top = Math.max(pad, y - height);
    setPos({ left, top });
  }, [x, y, items]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
        return;
      }
      if (focusable.length === 0) return;
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        setActive((cur) => {
          const at = focusable.indexOf(cur);
          const next =
            e.key === "ArrowDown"
              ? focusable[(at + 1 + focusable.length) % focusable.length]
              : focusable[(at - 1 + focusable.length) % focusable.length];
          return next ?? focusable[0];
        });
      } else if (e.key === "Enter" && active >= 0) {
        const it = items[active];
        if (it && !isSep(it) && !it.disabled) {
          e.preventDefault();
          onClose();
          it.onClick();
        }
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [items, focusable, active, onClose]);

  return (
    <>
      {/* Click-away / right-click-away backdrop. */}
      <div
        className="fixed inset-0 z-[1200]"
        onClick={onClose}
        onContextMenu={(e) => {
          e.preventDefault();
          onClose();
        }}
      />
      <div
        ref={ref}
        role="menu"
        className="fixed z-[1201] min-w-[11rem] overflow-hidden rounded-lg border py-1 text-sm shadow-2xl"
        style={{
          left: pos?.left ?? x,
          top: pos?.top ?? y,
          visibility: pos ? "visible" : "hidden",
          borderColor: "var(--hive-line)",
          background: "var(--hive-panel)",
          color: "var(--hive-ink)",
        }}
      >
        {items.map((it, i) =>
          isSep(it) ? (
            <div key={i} className="my-1 h-px" style={{ background: "var(--hive-line)" }} />
          ) : (
            <button
              key={i}
              role="menuitem"
              disabled={it.disabled}
              onMouseEnter={() => setActive(i)}
              onClick={() => {
                if (it.disabled) return;
                onClose();
                it.onClick();
              }}
              className="flex w-full items-center gap-2 px-3 py-1.5 text-left transition-colors disabled:opacity-40"
              style={{
                background: active === i ? "var(--hive-overlay)" : "transparent",
                color: it.danger ? "var(--hive-danger)" : "var(--hive-ink)",
              }}
            >
              {it.icon && (
                <span aria-hidden className="shrink-0 opacity-70">
                  {it.icon}
                </span>
              )}
              <span className="min-w-0 flex-1 truncate">{it.label}</span>
              {it.hint && <span className="shrink-0 text-xs opacity-40">{it.hint}</span>}
            </button>
          ),
        )}
      </div>
    </>
  );
}

/// A menu section header (e.g. the element's name), rendered above items.
export function menuHeader(label: string): MenuItemSpec {
  // Encoded as a disabled item styled by the caller; simplest is a plain disabled
  // row, but headers read better small+uppercase — callers can prepend this.
  return { label, onClick: () => {}, disabled: true };
}
