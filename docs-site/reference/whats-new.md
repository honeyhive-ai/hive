# What's new

Changes since 1.15, newest first, with links to where each is documented.
Full release notes are on the
[GitHub releases page](https://github.com/honeyhive-ai/hive/releases).

## 1.18 (upcoming)

- **Agent-authored workflows.** The visual workflow editor and its presets
  are gone. Agents write workflows: a definition (or a revision of one)
  arrives in the Review pane as a proposal, and approving it adds it. The
  Workflows pane is read-only, with Run, Pause/Resume, Delete and *Ask an
  agent to change this*. Local-model agents get a `propose_workflow` tool
  that checks the definition in the same turn. Existing workflows keep
  working. See [Agentic workflows](../features/workflows.md).
- **Google Antigravity and GitHub Copilot runtimes.** Use your Google AI
  Pro/Ultra or Copilot subscription through the `agy` and `copilot` CLIs;
  Settings → Models detects whether each is installed and signed in. See
  [Configuring a runtime](../getting-started/configuring-a-runtime.md) and
  [Bring-your-own agents](../addons/byo-agents.md).

## 1.17

- **Host dashboard.** `hive serve` serves a dashboard to the phone: a
  built-in Hive section (agents, proposals, workflows, turns, host, relay and
  push status), your own panels from `dashboard.toml`, panels backed by MCP
  tools, a table panel kind, and `hive dashboard check | metrics | default`.
  See [Host dashboard](../features/dashboard.md).
- **MCP tools at the local-model tool cap.** Local agents no longer silently
  lose MCP tools at the 16-tool limit: slots are kept for MCP tools, tools
  you name are preferred, and dropped tools are logged. MCP errors show the
  server's own message. See [MCP servers](../addons/mcp.md).

## 1.16.1

- Every user message a sync brings in gets an answer, not only the chat's
  last one.
- Push previews are plain text.
- `hive remote status` shows how long the relay link has been up and
  notices a `hive serve` that stopped; `hive --version` prints the version.
  See [Troubleshooting](../ops/troubleshooting.md).

## 1.16

- **Hive on your phone, from anywhere.** `hive serve` plus the relay tunnel
  gives the phone app remote access without a VPN: `hive pair` prints the
  pairing link, `hive phones` lists and revokes phones, `hive remote`
  manages the link. A self-hosted relay can run the tunnel and send push
  with its own APNs key. See [Remote access for your phone](../networking/remote-access.md).
- **On the phone:** people, channels and chat details; proposal review
  with votes (who and role), a refinement thread and *request changes*;
  push for mentions and workflow approvals; the host's theme through
  *Match host*. See [Hive on your phone](../features/phone-app.md).
- **The serve box as your own device.** `hive serve enroll` makes the box
  one of your devices instead of a separate member. See
  [Running hive serve as your own device](../ops/serve-box.md).
- **Sync privacy fix.** A relay room now carries only its own workspace;
  events from other workspaces are dropped and purged on upgrade. Read the
  [upgrade note](../ops/serve-box.md#what-to-expect-and-do-when-upgrading-from-115-or-earlier)
  if you're coming from 1.15 or earlier.
- **Presence-based host liveness.** Whether a host is online rides the
  relay's presence channel instead of heartbeat events in the log. See
  [Host liveness](../ops/serve-box.md#host-liveness).
