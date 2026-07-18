# MCP servers

Hive speaks **Model Context Protocol** — the spec for LLM tool
servers Anthropic standardized. Each MCP server is a separate
process (or HTTP endpoint) that exposes named tools the agent can call.

Use MCP when you want to:

- Surface a *new tool* to your agents (file-system, HTTP, database,
  proprietary internal API).
- Share a tool set across many agents and workspaces.
- Run a long-lived tool process separately from Hive.

Don't use MCP when:

- You want a *whole agent* (wire a CLI-agent runtime instead —
  aider, pi, or Claude Code; see [BYO agents](byo-agents.md)).

## Configuration

```toml
[[mcp_servers]]
id = "filesystem"
transport = "stdio"
command = "/usr/local/bin/mcp-filesystem"
args = ["--workspace", "."]
enabled = false
```

Or HTTP:

```toml
[[mcp_servers]]
id = "github"
transport = "http"
url = "https://mcp.example.com"
enabled = false
```

| Field | What |
|-------|------|
| `id` | Workspace-unique identifier |
| `transport` | `stdio` or `http` |
| `command` / `args` | For stdio — Hive spawns it |
| `url` | For HTTP — the endpoint Hive talks to |
| `enabled` | Inert until true (default `false`); enabling launches/connects it |

## Install from the internet

You don't have to hand-edit TOML. In the right-rail **Tools** pane,
**Install from the internet** fetches a server's JSON manifest and adds it to a
per-workspace catalog (`.hive/mcp-servers.json`). Accepted references:

```text
https://raw.githubusercontent.com/owner/repo/main/server.json
https://github.com/owner/repo/blob/main/server.json   # blob URLs become raw
owner/repo/path/server.json                            # shorthand, ref defaults to main
```

The manifest can be a flat object or the Claude-desktop shape:

```json
{ "id": "git", "transport": "stdio", "command": "uvx", "args": ["mcp-server-git"] }
```

```json
{ "mcpServers": { "git": { "command": "uvx", "args": ["mcp-server-git"] } } }
```

An `http` server uses `url` (or `endpoint`) instead of `command`:

```json
{ "name": "remote", "url": "https://mcp.example/sse" }
```

### Install is inert — enabling launches it

!!! warning "Installing never runs anything"
    A freshly installed server is **disabled**. Hive does not touch its
    command or endpoint until you flip **Enable**. *Enabling* is the step
    that launches a stdio command or connects to an endpoint — only enable
    servers you trust.

Enabling registers the server with the live runtime so its tools connect;
disabling reverses that; Remove deletes it from the workspace catalog.
Enablement is **per server** — an enabled server exposes all of its tools to
the model. To narrow what an agent can reach, enable only the servers whose
tools you want available (or split a broad tool set across separate servers).

## Remote MCP servers (OAuth)

Beyond `stdio` and static-token `http` servers, Hive speaks the modern
**Streamable-HTTP** transport with **OAuth 2.1 + PKCE** — for hosted MCP
services that authenticate you in the browser and hand back short-lived,
auto-refreshing tokens. There's a one-click **Add Linear (issues)** preset
and a per-server **Connect** button that runs the browser flow (loopback
redirect `http://127.0.0.1:51736/callback`).

See **[Linear & remote MCP servers (OAuth)](linear.md)** for the full
walkthrough, including creating a Linear OAuth app and the `/linear`
composer command.

## Health & status

The **Tools** pane shows MCP connectivity:

- Green: all enabled servers connected.
- Warm tint: some connected, some failed.
- Gray: none enabled.

Open the Tools pane (the Tools toggle in the chat header) to inspect
per-server status.

## Recommended ecosystem

- [Anthropic's reference servers](https://github.com/modelcontextprotocol/servers)
  — filesystem, github, gitlab, postgres, etc.
- Community servers — search GitHub for `mcp-server-*`.
- Wrappers around your own tools — the
  [MCP TypeScript SDK](https://github.com/modelcontextprotocol/typescript-sdk)
  makes new servers cheap to spin up.
