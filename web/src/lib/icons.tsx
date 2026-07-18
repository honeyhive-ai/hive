// Shared inline icon set — Lucide-style 24×24 line icons (strokeWidth 1.7),
// promoted from ChatView so the whole app speaks one visual language instead
// of a mix of emoji (🛠 👥 📚) and text glyphs (＋ ⋯ ▸ ✕). Icons inherit
// `currentColor`, so they tint correctly in every palette.
//
// Convention: icons are decorative (`aria-hidden`); the enclosing control owns
// the accessible name (`aria-label`/`title`).

import type { ReactNode } from "react";

export function Icon({ size = 16, children }: { size?: number; children: ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.7}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      {children}
    </svg>
  );
}

type P = { size?: number };

// --- chat actions ----------------------------------------------------------
export const IconCopy = ({ size }: P) => (
  <Icon size={size}>
    <rect x="9" y="9" width="13" height="13" rx="2" />
    <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
  </Icon>
);
export const IconRegenerate = ({ size }: P) => (
  <Icon size={size}>
    <path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8" />
    <path d="M21 3v5h-5" />
    <path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16" />
    <path d="M8 16H3v5" />
  </Icon>
);
export const IconSmile = ({ size }: P) => (
  <Icon size={size}>
    <circle cx="12" cy="12" r="10" />
    <path d="M8 14s1.5 2 4 2 4-2 4-2" />
    <line x1="9" x2="9.01" y1="9" y2="9" />
    <line x1="15" x2="15.01" y1="9" y2="9" />
  </Icon>
);
export const IconSend = ({ size = 18 }: P) => (
  <Icon size={size}>
    <path d="M12 19V5" />
    <path d="m5 12 7-7 7 7" />
  </Icon>
);
export const IconPaperclip = ({ size }: P) => (
  <Icon size={size}>
    <path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l8.57-8.57A4 4 0 1 1 18 8.84l-8.59 8.57a2 2 0 0 1-2.83-2.83l8.49-8.48" />
  </Icon>
);

// --- navigation chrome -----------------------------------------------------
export const IconPlus = ({ size }: P) => (
  <Icon size={size}>
    <path d="M5 12h14" />
    <path d="M12 5v14" />
  </Icon>
);
export const IconUsers = ({ size }: P) => (
  <Icon size={size}>
    <path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" />
    <circle cx="9" cy="7" r="4" />
    <path d="M22 21v-2a4 4 0 0 0-3-3.87" />
    <path d="M16 3.13a4 4 0 0 1 0 7.75" />
  </Icon>
);
export const IconWrench = ({ size }: P) => (
  <Icon size={size}>
    <path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" />
  </Icon>
);
export const IconInbox = ({ size }: P) => (
  <Icon size={size}>
    <path d="M22 12h-6l-2 3h-4l-2-3H2" />
    <path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z" />
  </Icon>
);
export const IconBook = ({ size }: P) => (
  <Icon size={size}>
    <path d="M2 3h6a4 4 0 0 1 4 4v14a3 3 0 0 0-3-3H2z" />
    <path d="M22 3h-6a4 4 0 0 0-4 4v14a3 3 0 0 1 3-3h7z" />
  </Icon>
);
export const IconSparkle = ({ size }: P) => (
  <Icon size={size}>
    <path d="M12 3l1.9 5.8 5.8 1.9-5.8 1.9L12 18.4l-1.9-5.8-5.8-1.9 5.8-1.9z" />
  </Icon>
);
export const IconGrid = ({ size }: P) => (
  <Icon size={size}>
    <rect x="3" y="3" width="7" height="7" rx="1" />
    <rect x="14" y="3" width="7" height="7" rx="1" />
    <rect x="14" y="14" width="7" height="7" rx="1" />
    <rect x="3" y="14" width="7" height="7" rx="1" />
  </Icon>
);
export const IconHexagon = ({ size }: P) => (
  <Icon size={size}>
    <path d="M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z" />
  </Icon>
);
export const IconActivity = ({ size }: P) => (
  <Icon size={size}>
    <path d="M22 12h-4l-3 9L9 3l-3 9H2" />
  </Icon>
);
export const IconGear = ({ size }: P) => (
  <Icon size={size}>
    <circle cx="12" cy="12" r="3" />
    <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
  </Icon>
);

// --- generic controls ------------------------------------------------------
export const IconX = ({ size }: P) => (
  <Icon size={size}>
    <path d="M18 6 6 18" />
    <path d="m6 6 12 12" />
  </Icon>
);
export const IconCheck = ({ size }: P) => (
  <Icon size={size}>
    <path d="M20 6 9 17l-5-5" />
  </Icon>
);
export const IconChevronRight = ({ size }: P) => (
  <Icon size={size}>
    <path d="m9 18 6-6-6-6" />
  </Icon>
);
export const IconChevronDown = ({ size }: P) => (
  <Icon size={size}>
    <path d="m6 9 6 6 6-6" />
  </Icon>
);
export const IconEllipsis = ({ size }: P) => (
  <Icon size={size}>
    <circle cx="12" cy="12" r="1" />
    <circle cx="19" cy="12" r="1" />
    <circle cx="5" cy="12" r="1" />
  </Icon>
);
export const IconPencil = ({ size }: P) => (
  <Icon size={size}>
    <path d="M17 3a2.85 2.83 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z" />
  </Icon>
);
export const IconPanelLeft = ({ size }: P) => (
  <Icon size={size}>
    <rect x="3" y="3" width="18" height="18" rx="2" />
    <path d="M9 3v18" />
  </Icon>
);
export const IconArrowDown = ({ size }: P) => (
  <Icon size={size}>
    <path d="M12 5v14" />
    <path d="m19 12-7 7-7-7" />
  </Icon>
);

// --- status ----------------------------------------------------------------
export const IconLock = ({ size }: P) => (
  <Icon size={size}>
    <rect x="3" y="11" width="18" height="11" rx="2" />
    <path d="M7 11V7a5 5 0 0 1 10 0v4" />
  </Icon>
);
export const IconAlertTriangle = ({ size }: P) => (
  <Icon size={size}>
    <path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z" />
    <path d="M12 9v4" />
    <path d="M12 17h.01" />
  </Icon>
);
export const IconInfo = ({ size }: P) => (
  <Icon size={size}>
    <circle cx="12" cy="12" r="10" />
    <path d="M12 16v-4" />
    <path d="M12 8h.01" />
  </Icon>
);
export const IconMessage = ({ size }: P) => (
  <Icon size={size}>
    <path d="M7.9 20A9 9 0 1 0 4 16.1L2 22Z" />
  </Icon>
);

// --- files -----------------------------------------------------------------
export const IconFile = ({ size }: P) => (
  <Icon size={size}>
    <path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" />
    <path d="M14 2v4a2 2 0 0 0 2 2h4" />
  </Icon>
);
export const IconImage = ({ size }: P) => (
  <Icon size={size}>
    <rect x="3" y="3" width="18" height="18" rx="2" />
    <circle cx="9" cy="9" r="2" />
    <path d="m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21" />
  </Icon>
);
export const IconFlow = ({ size }: P) => (
  <Icon size={size}>
    <rect x="3" y="3" width="6" height="6" rx="1.5" />
    <rect x="15" y="5" width="6" height="6" rx="1.5" />
    <rect x="9" y="15" width="6" height="6" rx="1.5" />
    <path d="M9 6h3a2 2 0 0 1 2 2" />
    <path d="M18 11v2a2 2 0 0 1-2 2h-1" />
    <path d="M6 9v7a2 2 0 0 0 2 2h1" />
  </Icon>
);
