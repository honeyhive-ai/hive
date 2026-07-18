# Tools & permissions

Agents in Hive reach the outside world through **MCP tools**, and the
authority they have over your files depends on the **runtime** they run
on. There is no separate Hive-owned tool registry or inline consent
banner — file and command access is governed by the two mechanisms
below, plus the human review gate on proposals.

## MCP tools

Model Context Protocol servers are the way you give agents tools
(filesystem, search, a ticketing system, anything with an MCP server).
Add them in the **Tools** pane of the right rail (or in
`hive.config.toml` under `[[mcp_servers]]`).

Two properties make this safe by default:

- **Inert until enabled.** A newly added MCP server is disabled; its
  tools are never offered to a model until you flip it on. Enabling is
  an explicit, per-server action.
- **Only Anthropic runtimes drive them through Hive.** Hive runs the
  MCP tool loop for runtimes on the Anthropic API. Subprocess runtimes
  (Claude Code, aider, pi) bring their own tools and their own
  permissioning — see below.

Enabled servers show their connection status in the Tools pane.

## File & command authority (Claude Code)

When a chat runs on the **Claude Code** runtime, the agent edits files
and runs commands through Claude Code itself, and you choose how much
it may do without asking under **Settings → Team → Agent file access**
(the `claude_permission_mode` setting):

- **default** — Claude Code prompts for each edit/command in its own
  flow.
- **acceptEdits** — file edits are auto-accepted; commands still prompt.
- **bypassPermissions** — no prompts (use only in a sandbox you trust).

This is Claude Code's permission model, surfaced in Hive's settings —
Hive does not intercept individual tool calls itself.

## The human review gate (proposals)

The strongest control is agreement-gated execution. An agent can raise
a **proposal** — a titled change or decision — that lands in the
**Review** pane. A proposal does nothing on its own: a human (or a
quorum of them, when `required_approvals > 1`) approves it, and only
then does clicking **Implement** dispatch it to an agent to carry out.
[Workflows](../features/workflows.md) use exactly this mechanism for
their approval gates.

## Threat model

What these controls protect against:

- ✅ An agent using a tool you never enabled — MCP servers are inert
  until you turn them on.
- ✅ Unreviewed changes landing silently — proposals require explicit
  human approval before they are implemented.

What they do **not** protect against:

- ❌ The runtime endpoint itself. If you point a runtime at a malicious
  endpoint, that endpoint sees your conversation. Use providers you
  trust.
- ❌ MCP servers you enable. An MCP server runs on your machine with
  whatever permissions its binary has. Vet the binary before enabling.
- ❌ A subprocess agent's own tools. Under `bypassPermissions`, Claude
  Code acts without prompting — that's the setting's purpose; scope it
  to a sandbox.
