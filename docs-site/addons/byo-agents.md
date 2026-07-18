# Bring-your-own agents

Hive doesn't ship hardcoded agent personas. Instead, a CLI agent you
trust can become a workspace participant. Built-in support covers
**aider**, **pi**, and **Claude Code** — the three CLI agents Hive knows
how to spawn.

## Why BYO?

Aider, pi, Claude Code — these projects each maintain
state-of-the-art code-editing agents. Recreating that work inside
Hive would mean staying current with a fast-moving field of agent
design. Instead, Hive treats them as runtimes: they generate the
replies, Hive hosts the conversation, presence, and sync.

## Installing an agent

Pick whichever you like:

=== "aider"

    ```bash
    pip install aider-chat
    which aider
    # → /usr/local/bin/aider
    ```

=== "pi"

    Install the **pi** CLI (an OpenAI-compatible coding-agent backend)
    per its own instructions, then confirm the binary is on your PATH:

    ```bash
    which pi
    # → /usr/local/bin/pi
    ```

=== "Claude Code"

    Already supported as `provider = "claude-code"`. Install:

    ```bash
    npm install -g @anthropic-ai/claude-code
    which claude
    # → /usr/local/bin/claude
    ```

There is **no generic `subprocess` provider** — only `aider`, `pi`, and
`claude-code` are spawned as CLI agents. (An OpenAI-compatible custom HTTP
endpoint uses `provider = "custom"`, which talks over HTTP rather than
spawning a binary.) To wire a different CLI, extend the per-provider dispatch
in `crates/hive-runtime/src/provider/dispatch.rs`.

## Configuring as a runtime

In **Settings → Models → Add runtime**, or directly in TOML:

```toml
[[runtimes]]
id = "aider"
name = "Aider"
provider = "aider"
kind = "local"
endpoint = "/usr/local/bin/aider"
preferred_model = "gpt-4o"
supports_tools = true
```

```toml
[[runtimes]]
id = "claude"
name = "Claude Code"
provider = "claude-code"
kind = "local"
endpoint = "/usr/local/bin/claude"
preferred_model = ""
```

The provider determines how Hive talks to the binary:

| Provider     | Prompt delivery     | Default flags                                  |
|--------------|---------------------|------------------------------------------------|
| `aider`       | stdin                              | interactive CLI; prompt piped in              |
| `pi`          | positional arg (`-p … "<prompt>"`) | `-p --no-session --offline --mode text`; adds `--provider`/`--model` and a temp Ollama provider config when an Ollama base URL is set |
| `claude-code` | stdin, stream-json                 | `-p --output-format stream-json --include-partial-messages` |

For agents that need a different invocation, the per-provider wiring lives in
`crates/hive-runtime/src/provider/dispatch.rs` (with the generic spawn/stream
helper in `subprocess.rs`). It supports stdin delivery or final-positional-arg
delivery, and bounds each run with a wall-clock timeout
(`HIVE_AGENT_TIMEOUT_SECS`, default 300s).

## Attaching to a workspace agent

Configure a `WorkspaceAgent` that points at the runtime — the easiest way is
the right-rail **Tools** pane → **Workspace Agents → New Agent**:

- Name: `coder` (so peers `@coder` to invoke)
- Runtime: pick `aider`
- Owner: this device (for runtimes that need your local binary)

(Workspace agents aren't in `hive.config.toml` — they're records in the
workspace event log, so add them through the UI rather than a config file.)

Peers can now `@coder` in chat; Hive routes the message through
aider, captures stdout as the reply, and broadcasts the envelope.

## Subprocess agents edit files directly

When the active runtime is a CLI agent (aider, pi, Claude Code), it runs as a
local subprocess and edits files on this machine itself; Hive captures its
stdout as the agent's reply. These agents bring their **own** permission UX —
aider prompts before each write unless `--yes-always` is set, and Claude Code
has its own permission mode. Hive surfaces the resulting changes as reviewable
diffs (the **Diff** tab and the **Review** pane) rather than gating each edit.

## Output handling

The bridge captures **stdout** as the agent's reply. Stderr
surfaces as an error if the process exits non-zero. Multi-step
flows (aider's "I'll edit these files, do you want me to commit?")
just become multi-turn conversations — the user types a reply,
Hive sends it again, aider continues from where it left off.

For streaming output (incremental token display), use
`provider = "claude-code"` which has a dedicated streaming bridge.
Default subprocess mode is whole-stdout-on-completion.

## Peer-owned agents

A workspace agent's `ownerActorID` controls where the runtime
runs:

- Empty → runs on whichever device handles the chat.
- Set to a peer's account ID → runs on **that peer's** device.

The second mode is the killer feature: Bob has Claude Code on his
machine with his Pro subscription; you don't have a Claude
subscription, but you can still `@bob-claude` and have Bob's
device respond with Claude's output. No credentials cross
machines.

Bob has to be online for the invocation to complete. When he's
offline, Hive surfaces the message as pending and routes it when
he reconnects.

See [Agents (built-in vs BYO)](../concepts/agents.md) for the
agent dispatch model in full.
