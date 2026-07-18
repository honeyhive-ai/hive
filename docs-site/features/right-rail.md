# Right Rail

The right side of the window is a single utility pane with a
vertical icon strip you click to switch contents. Eight sections:

| Pane      | What's in it                                                    |
|-----------|-----------------------------------------------------------------|
| Tools     | Runtimes, workspace agent CRUD, MCP servers                     |
| Context   | Context-budget telemetry + summarize/compact controls           |
| Review    | Pending proposals: quorum votes + agreement-gated implement      |
| People    | Members, presence, roles, invites                               |
| Vaults    | Reference-material sources mounted into the session             |
| Skills    | Instruction bundles injected into participants' prompts         |
| Workflows | Agent pipelines: definitions, live runs, gates — see [Agentic workflows](workflows.md) |
| Activity  | Streaming runtime activity for the workspace                    |

![Right rail with Files pane active](../images/right-rail-files.png){ width="800" }

Switching panes preserves the visible width. Click the `×` in the
pane header to close the rail entirely.

## When to use each

- **Files**: when you want to read what the agent is reading. The
  file viewer cycles source ↔ rendered ↔ diff with `⌘ /`.
- **Context**: when a chat's reasoning depends on specific facts.
  Pinning a context item bumps it to the top of the conversation
  prompt.
- **Agents**: when a run is in flight. Shows planning → working →
  awaiting-review states across each agent.
- **Tools**: when an MCP server is misbehaving or you want to
  disable a specific tool for a chat. The wrench glyph in the
  workspace bar also opens this pane.
- **Git**: when you want a per-file status view. The pill in the
  workspace bar shows the rollup count; the pane shows what's
  changed.
- **Review**: when an agent has proposed a write. Lists pending
  proposals, lets you claim / approve / disposition.
- **People**: members, online status, invites, cross-network
  rendezvous status, nearby-on-LAN peers.

## Why a single rail

The icon strip + tinted card is the only chrome that needs to
exist for these signals. A window-level toolbar duplicating the
same affordances would just steal screen space; the rail is the
canonical place to switch panes and close them.
