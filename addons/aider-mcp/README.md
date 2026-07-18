# aider-mcp

An MCP stdio server that exposes [aider](https://github.com/paul-gauthier/aider)
as a set of named tools your Hive agents can call.

Hive supports two distinct ways to plug aider in. Use whichever
fits your team — or both at once:

| Pattern | When |
|---------|------|
| **As a runtime** (`provider = "aider"` in `[[runtimes]]`) | You want aider to be the conversational agent itself. |
| **As an MCP server** (this addon) | Your chat agent is Claude / GPT-4 / etc., and you want it to delegate code edits to aider as a tool call. |

Both can coexist — one chat with aider driving, another with
Claude orchestrating aider through MCP.

## Install

```bash
cd addons/aider-mcp
pip install -e .
which aider-mcp
# → /usr/local/bin/aider-mcp
```

This pulls in `aider-chat` as a dependency, so a single
`pip install -e .` gets you both.

If you want to use a non-PATH aider binary:

```bash
export AIDER_BINARY=/path/to/aider
```

## Configure Hive

In your workspace's `hive.config.toml`:

```toml
[[mcp_servers]]
id = "aider"
transport = "stdio"
command = "/usr/local/bin/aider-mcp"
autoload = true
allowed_in_runtimes = ["anthropic", "openai"]
```

Or via the GUI: **Settings → MCP Servers → Add**. Restart Hive (or
re-open the workspace); the wrench glyph in the workspace bar
should turn green.

## Tools exposed

| Tool          | What it does                                                    | Approval (Hive-side) |
|---------------|-----------------------------------------------------------------|----------------------|
| `aider_edit`  | `aider --message <prompt> <files>` — edits files in place        | yes |
| `aider_review`| Asks aider to review a file; doesn't write                       | no  |
| `aider_commit`| Has aider craft a commit message + commit staged changes         | yes |
| `aider_undo`  | Reverts aider's last commit                                      | yes |

Each tool returns aider's stdout (or stderr on non-zero exit) as a
text content block.

## Example usage

In a chat with `provider = "anthropic"` and the `aider` MCP server
loaded:

> **You:** add a regression test for the off-by-one in
> `Sources/HiveCore/PeerLink.swift`'s `canonicalChallenge`.

> **Claude:** calls `aider_edit` with
> `files = ["Sources/HiveCore/PeerLink.swift",
> "Tests/HiveCoreTests/HiveCoreTests.swift"]` and your prompt.

> **Hive:** prompts you to approve the call.

> **You:** Allow once.

> **Claude:** reads the tool result (aider's stdout — diff +
> commit) and summarizes what it did.

## Splitting to its own repo

When this addon stabilizes, it moves to a sibling repo
(`hive-aider-mcp`). The interface is just MCP — no Hive-specific
imports — so the move is mechanical.

The same template fits other CLI agents: copy this directory,
rename, replace the `_run_aider` body with your binary's CLI, you
have a `pi-mcp` / `claude-code-mcp` / `your-tool-mcp` server.

## Troubleshooting

**Hive's wrench glyph is gray.** The MCP server didn't start.
Check:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize"}' | aider-mcp
```

Should print a JSON-RPC response. If it hangs or errors, your
Python env is misconfigured.

**aider can't find your model.** Set the env var aider expects
(`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, etc.) before launching
Hive. The MCP server inherits the env.
