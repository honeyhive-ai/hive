// Shared UI primitives — one button/section language for the whole app.
// The audit found three competing button implementations and four section
// header treatments; every surface should compose these instead.
//
// Radius scale (app-wide): controls = rounded-xl, cards = rounded-2xl,
// hero surfaces = rounded-3xl.

import { useEffect, useRef } from "react";
import type { ButtonHTMLAttributes, CSSProperties, ReactNode } from "react";

type Variant = "primary" | "ghost" | "danger";
type Size = "sm" | "md";

const SIZE: Record<Size, string> = {
  sm: "h-8 px-3 text-sm",
  md: "h-9 px-4 text-sm",
};

/// The app button. `primary` = accent call-to-action, `ghost` = neutral
/// control on mist, `danger` = destructive.
export function Button({
  variant = "ghost",
  size = "sm",
  className = "",
  style,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant; size?: Size }) {
  const base = `inline-flex shrink-0 items-center justify-center gap-1.5 rounded-xl font-medium transition-all disabled:cursor-not-allowed disabled:opacity-40 ${SIZE[size]}`;
  const variants: Record<Variant, { className: string; style: React.CSSProperties }> = {
    primary: {
      className: "text-white shadow-sm hover:brightness-105 disabled:shadow-none",
      style: { background: "var(--hive-accent-cool)" },
    },
    ghost: {
      className: "border hover:border-[color:var(--hive-accent-cool)]",
      style: {
        borderColor: "var(--hive-line)",
        background: "var(--hive-mist)",
        color: "var(--hive-ink)",
      },
    },
    danger: {
      className: "border hover:brightness-110",
      style: {
        borderColor: "rgba(200,70,70,0.35)",
        background: "rgba(200,70,70,0.12)",
        color: "var(--hive-danger)",
      },
    },
  };
  const v = variants[variant];
  return (
    <button
      {...rest}
      className={`${base} ${v.className} ${className}`}
      style={{ ...v.style, ...style }}
    />
  );
}

/// Square icon-only button. The aria-label is required — icon buttons were
/// the app's biggest accessibility gap.
export function IconButton({
  label,
  size = 32,
  className = "",
  children,
  ...rest
}: Omit<ButtonHTMLAttributes<HTMLButtonElement>, "aria-label"> & {
  label: string;
  size?: number;
  children: ReactNode;
}) {
  return (
    <button
      {...rest}
      aria-label={label}
      title={rest.title ?? label}
      className={`inline-flex shrink-0 items-center justify-center rounded-lg opacity-70 transition-all hover:opacity-100 hover:bg-[color:var(--hive-overlay)] disabled:cursor-not-allowed disabled:opacity-30 ${className}`}
      style={{ width: size, height: size, color: "var(--hive-ink)", ...rest.style }}
    >
      {children}
    </button>
  );
}

/// One section-header treatment for panes/settings: small-caps title, an
/// optional trailing action, consistent spacing.
export function Section({
  title,
  action,
  children,
  className = "",
}: {
  title: string;
  action?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={`mb-5 ${className}`}>
      <div className="mb-2 flex items-center justify-between gap-2">
        <h2 className="text-xs font-semibold uppercase tracking-[0.16em] opacity-60">{title}</h2>
        {action}
      </div>
      <div className="space-y-2.5">{children}</div>
    </section>
  );
}

// Selector for everything a modal's Tab cycle may land on.
const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

function focusables(panel: HTMLElement): HTMLElement[] {
  return Array.from(panel.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (el) => el.getClientRects().length > 0 || el === document.activeElement,
  );
}

// Stack of open modals so only the top-most one reacts to Escape/Tab when
// modals nest (e.g. a confirm dialog launched from inside another modal).
const modalStack: symbol[] = [];

/// Modal shell: overlay + panel with the focus management every dialog needs.
/// - Tab / Shift-Tab cycle inside the panel (focus can't escape behind the overlay)
/// - Escape closes, document-wide — no element inside needs focus
/// - On open, focuses the child that used `autoFocus`, else the first control
/// - On close, focus returns to whatever had it before the modal opened
/// - Clicking the overlay closes; clicks inside the panel don't propagate out
/// Render it conditionally (`open && <Modal …>`); it assumes it is mounted open.
/// Consumers style layering/placement via overlayClassName and the panel via
/// panelClassName/panelStyle, so existing looks carry over unchanged.
export function Modal({
  onClose,
  overlayClassName = "z-[900] flex items-center justify-center p-4",
  overlayStyle,
  panelClassName = "",
  panelStyle,
  children,
}: {
  onClose: () => void;
  overlayClassName?: string;
  overlayStyle?: CSSProperties;
  panelClassName?: string;
  panelStyle?: CSSProperties;
  children: ReactNode;
}) {
  const panelRef = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    const id = Symbol("modal");
    modalStack.push(id);
    const prev = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const panel = panelRef.current;
    if (!panel) return;

    // React runs children's autoFocus during mount, before this effect — if a
    // child already claimed focus, respect it; otherwise take the first control.
    if (!panel.contains(document.activeElement)) {
      (focusables(panel)[0] ?? panel).focus();
    }

    const onKeyDown = (e: KeyboardEvent) => {
      if (modalStack[modalStack.length - 1] !== id) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onCloseRef.current();
      } else if (e.key === "Tab") {
        const nodes = focusables(panel);
        if (nodes.length === 0) {
          e.preventDefault();
          panel.focus();
          return;
        }
        const first = nodes[0];
        const last = nodes[nodes.length - 1];
        const active = document.activeElement;
        if (e.shiftKey) {
          if (active === first || !panel.contains(active)) {
            e.preventDefault();
            last.focus();
          }
        } else if (active === last || !panel.contains(active)) {
          e.preventDefault();
          first.focus();
        }
      }
    };
    // Capture phase so the trap holds even if a child handler stops propagation.
    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("keydown", onKeyDown, true);
      modalStack.splice(modalStack.indexOf(id), 1);
      prev?.focus();
    };
  }, []);

  return (
    <div
      className={`fixed inset-0 ${overlayClassName}`}
      style={overlayStyle}
      onClick={() => onCloseRef.current()}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        tabIndex={-1}
        className={panelClassName}
        style={{ outline: "none", ...panelStyle }}
        onClick={(e) => e.stopPropagation()}
      >
        {children}
      </div>
    </div>
  );
}

/// Card surface for grouped content inside a pane (radius scale: 2xl).
export function Card({
  children,
  className = "",
  style,
}: {
  children: ReactNode;
  className?: string;
  style?: React.CSSProperties;
}) {
  return (
    <div
      className={`rounded-2xl border ${className}`}
      style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)", ...style }}
    >
      {children}
    </div>
  );
}
