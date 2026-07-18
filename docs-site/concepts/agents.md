# Agents (built-in vs BYO)

Hive's agent model is intentionally thin. The app isn't an agent
itself — it hosts agents you bring.

## WorkspaceAgent — the user-facing concept

A `WorkspaceAgent` has three fields:

- `name` — what peers `@mention` (e.g. `@coder`).
- `runtimeID` — which configured runtime drives it.
- `ownerActorID` — whose device the runtime lives on (`""` = shared).

That's it. The agent isn't a "persona" with hardcoded prompts. It's
a pointer: "when someone in this chat says `@coder`, route the
message to runtime `aider-runtime` on Alice's device."

Configure agents from the right-rail **Tools** pane → **Workspace Agents**.

![Workspace Agents pane](../images/agents-pane.png){ width="700" }

## Why no built-in personas?

Two reasons:

1. **They would duplicate work that aider, pi, and Claude Code
   already do well.** Maintaining a curated set of agent prompts
   and a multi-stage planning harness inside Hive would mean
   keeping up with the state-of-the-art in agent design — which
   is the whole industry. Better to host the agents you bring.
2. **The product story is "bring your own."** If your team uses
   aider for code edits and Claude for reviews, point Hive at
   those. Hive doesn't need its own Coder.

## Bring-your-own agent

Any LLM (API or CLI) becomes a Hive agent via a runtime + a
WorkspaceAgent pointing at it.

=== "API agent (Anthropic)"

    1. Configure a runtime `anthropic` in Settings → Models.
    2. Add a WorkspaceAgent `@claude` pointing at it.
    3. `@claude` participates in chats.

=== "Subprocess agent (aider)"

    1. `pip install aider-chat`.
    2. Configure a runtime `aider` with `endpoint =
       "/usr/local/bin/aider"` in Settings → Models.
    3. Add a WorkspaceAgent `@coder` pointing at it.
    4. `@coder` writes code through aider when mentioned.

=== "Peer-owned agent"

    Bob has Claude Code on his machine; he doesn't want to give
    you his API key. He configures `@bob-claude` with
    `ownerActorID = bob`. When you `@bob-claude` in chat:

    1. Hive routes the message to Bob's device over the P2P link.
    2. Bob's instance runs Claude Code locally with his subscription.
    3. The reply flows back through the envelope log.

    No credentials leave Bob's machine.

See [BYO agents](../addons/byo-agents.md) for the full configuration
recipe.

## Tool capabilities

Different runtimes expose different capabilities:

| Runtime kind        | What it can do                                          |
|---------------------|---------------------------------------------------------|
| API providers       | Enabled MCP tools (whatever MCP servers you've turned on for the chat). |
| Claude Code         | Claude Code's own tools, governed by its own permission mode. |
| aider / pi          | The agent's own internal tools. Hive captures stdout as the reply. |
| Ollama / hive-daemon| Text only (no tool calls in either direction yet). |

MCP servers are inert until you enable them, so an API runtime starts with no
tools until you turn some on in the Tools pane.

## Agent dispatch model

When you send a message with no `@mention`, the message goes to the
chat's primary runtime. When you `@agent`:

1. Hive resolves the mention to a workspace agent.
2. If the agent runs on this device, Hive calls the runtime locally.
3. If the agent runs on a peer's device, Hive routes the message
   through the P2P link (relay-forwarded when no direct link exists).
4. The reply comes back as a normal assistant message in the log.

This is the same path whether the peer is on your LAN, across the
internet via the rendezvous relay, or via the relay-forwarded
fallback path.

## Reviewing agent changes

Hive has no inline per-tool consent prompt of its own. Trust is enforced at
three real seams instead:

- **MCP tools are inert until enabled.** An API runtime can't call a tool you
  haven't turned on for the chat.
- **Claude Code brings its own permission mode.** It gates its own file and
  command actions; Hive doesn't second-guess it.
- **Changes land as proposals you review.** Edits an agent makes surface in
  the **Diff** tab and the **Review** pane, where peers approve (and reach
  quorum, if configured) before they're implemented.

Those decisions are events too; they sync to peers and survive relaunches.
