// Which workspace and channel the shell is looking at. Local state only —
// there is no transport in this milestone, so nothing here syncs.

import { createContext, useContext, useMemo, useState, type ReactNode } from "react";

import { CHANNELS, WORKSPACES, type Channel, type Workspace } from "../fixtures/transcript";

interface WorkspaceState {
  workspace: Workspace;
  channel: Channel;
  channels: Channel[];
  workspaces: Workspace[];
  selectWorkspace: (id: string) => void;
  selectChannel: (id: string) => void;
}

const Ctx = createContext<WorkspaceState | null>(null);

export function WorkspaceProvider({ children }: { children: ReactNode }) {
  const [workspaceId, setWorkspaceId] = useState(WORKSPACES[1].id);
  const [channelId, setChannelId] = useState(CHANNELS[0].id);

  const value = useMemo<WorkspaceState>(() => {
    const workspace = WORKSPACES.find((w) => w.id === workspaceId) ?? WORKSPACES[0];
    const channels = CHANNELS.filter((c) => c.workspaceId === workspace.id);
    const channel = channels.find((c) => c.id === channelId) ?? channels[0];
    return {
      workspace,
      channel,
      channels,
      workspaces: WORKSPACES,
      selectWorkspace: (id) => {
        setWorkspaceId(id);
        // Switching workspaces must land on a channel that exists in it,
        // otherwise the chat header renders against a stale channel.
        const first = CHANNELS.find((c) => c.workspaceId === id);
        if (first) setChannelId(first.id);
      },
      selectChannel: setChannelId,
    };
  }, [workspaceId, channelId]);

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useWorkspace(): WorkspaceState {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("useWorkspace must be used inside <WorkspaceProvider>");
  return ctx;
}
