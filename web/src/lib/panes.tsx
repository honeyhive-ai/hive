import type { ReactNode } from "react";
import {
  IconActivity,
  IconBook,
  IconFlow,
  IconHexagon,
  IconInbox,
  IconSparkle,
  IconUsers,
  IconWrench,
} from "@/lib/icons";

/// The utility-pane identities — ONE ordered source of truth shared by the
/// right-pane icon rail AND the sidebar's Workspace nav rows, so the two can
/// never disagree on icon, label, or order. (Previously they diverged: the same
/// glyph meant "Context" in the sidebar but "Activity" in the rail, and the
/// order differed, so collapsing the sidebar reshuffled the icons.)
export type UtilityPane =
  | "people"
  | "tools"
  | "review"
  | "context"
  | "skills"
  | "vaults"
  | "workflows"
  | "activity";

export interface PaneDef {
  id: UtilityPane;
  label: string;
  /// Render the pane's glyph at a given size (rail uses 17, sidebar 15).
  icon: (size: number) => ReactNode;
}

export const PANES: PaneDef[] = [
  { id: "people", label: "People", icon: (s) => <IconUsers size={s} /> },
  { id: "tools", label: "Agents & tools", icon: (s) => <IconWrench size={s} /> },
  { id: "review", label: "Review", icon: (s) => <IconInbox size={s} /> },
  { id: "context", label: "Context", icon: (s) => <IconHexagon size={s} /> },
  { id: "skills", label: "Skills", icon: (s) => <IconSparkle size={s} /> },
  { id: "vaults", label: "Vaults", icon: (s) => <IconBook size={s} /> },
  { id: "workflows", label: "Workflows", icon: (s) => <IconFlow size={s} /> },
  { id: "activity", label: "Activity", icon: (s) => <IconActivity size={s} /> },
];
