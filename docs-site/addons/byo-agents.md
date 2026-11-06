# Bring-your-own agents

Hive doesn't ship hardcoded agent personas. Instead, a CLI agent you
trust can become a workspace participant. Built-in support covers
**aider**, **pi**, **Claude Code**, **Codex**, the subscription CLIs
**Google Antigravity** (`agy`) and **GitHub Copilot** (`copilot`), and
**Hermes**, a generic provider for any stdin-driven CLI.

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

The CLI agents Hive spawns are `aider`, `pi`, `claude-code`, `codex`,
`antigravity`, `copilot`, and `hermes` (a generic stdin-in, stdout-out
provider: set the executable and flags on the runtime). An
OpenAI-compatible custom HTTP endpoint uses `provider = "custom"`, which
talks over HTTP rather than spawning a binary. To give a different CLI
first-class streaming, extend the per-provider dispatch in
`crates/hive-runtime/src/provider/dispatch.rs`.

## Subscription CLIs: Google Antigravity and GitHub Copilot

Use a **Google AI Pro / Ultra** or **GitHub Copilot** subscription the same
way Claude Code uses a Claude subscription: Hive runs the vendor's official CLI
headless and streams its reply, text and tool steps included. The login stays
in the CLI's own config / OS keychain — Hive never reads, copies or syncs it,
so these runtimes are keyless.

| | Google Antigravity CLI | GitHub Copilot CLI |
|---|---|---|
| Binary | `agy` | `copilot` |
| Install | `curl -fsSL https://antigravity.google/cli/install.sh \| bash` | `brew install --cask copilot-cli` or `npm install -g @github/copilot` |
| Sign in (in a terminal on the host) | run `agy` once → Sign in with Google | `copilot login` (headless hosts: `COPILOT_GITHUB_TOKEN` = a fine-grained token with *Copilot Requests*) |
| Models | `agy models` | `--model` ids from the CLI's `/model` picker; blank = the CLI default |

**Gemini CLI no longer serves Google AI Pro / Ultra sign-ins** (it stopped
serving individual Google accounts on 2026-06-18; API-key and Code Assist
Standard/Enterprise use is unaffected). Hive detects a `gemini` binary only to
point you at `agy`.

**Detect and add.** *Settings → Models → LLM providers → Subscription CLIs*
lists each CLI with its version and sign-in state, the exact login command when
it isn't signed in, and a one-click **Add runtime** (optionally with a model).
Copilot's sign-in is read from its config (`loggedInUsers`) or the presence of
its token env vars; Antigravity keeps its Google sign-in in the keychain, so
**Check Google sign-in** runs `agy models` (read-only, no quota) to confirm it.
Detection never signs in or out.

**Permissions follow Hive's Claude Code setting** (*Settings → Permissions*),
mapped to each CLI's own flags; headless, anything not granted is denied
rather than prompted:

| Hive tier | `agy` | `copilot` |
|---|---|---|
| default (read-only) | no flags — edits and commands need approval, which headless mode denies | `--allow-tool=read --deny-tool=write --deny-tool=shell` |
| accept edits | `--mode accept-edits` | `--allow-tool=read --allow-tool=write --deny-tool=shell` |
| run commands | `--mode auto` (edits + safe, reversible commands in the sandbox; risky ones still denied) | `--allow-tool=read --allow-tool=write --allow-tool=shell` |
| bypass | `--dangerously-skip-permissions` | `--allow-all-tools` (paths and URLs still verified; never `--yolo`) |

As with Claude Code, a raised tier makes the turn run in an isolated git
worktree and come back as a review-gated proposal.

**Errors are specific.** A missing login says which command to run; a usage
limit (Google AI quota, Copilot premium requests / AI credits) says so instead
of failing silently. Copilot bills premium models at several requests per turn
— set a cheaper model on the runtime if that matters.

**On `hive serve` / `hive worker` hosts.** Install and sign in to the CLI on
that host, then `hive setup` lists it (with sign-in state) and
`hive setup --apply` defines `antigravity` / `copilot` runtimes, or
`hive add-runtime copilot "Copilot" copilot <model>`. The worker's tier is
`HIVE_WORKER_PERMISSION_MODE` (`acceptEdits`, `runCommands`,
`bypassPermissions`; read-only when unset).

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
| `antigravity` | positional arg (`--print "<prompt>"`), stream-json | `--output-format stream-json --disable-slash-commands`; `--model` from the runtime; permission tier flags (below) |
| `copilot`     | positional arg (`-p "<prompt>"`), JSONL | `--output-format json --stream on --no-ask-user --no-color`; `--model` from the runtime; permission tier flags (below) |

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
