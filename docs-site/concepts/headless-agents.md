# Headless agents & setups

Hive can run an agent **without the desktop app**, using the `hive` CLI — on a
cloud VM, a spare box, or as a background process on your own machine. This page
covers the ways to wire that up and which one fits your situation.

> The command reference for the CLI lives in
> [`crates/hive-cli/README.md`](https://github.com/honeyhive-ai/hive/blob/main/crates/hive-cli/README.md).
> This page is about **topology** — where the agent runs and how it reaches your
> workspace.

## The one rule that decides your setup

Hive state is a **per-device, end-to-end-encrypted event log** — a local
`hive.db`. Anything participating in a workspace must either

- **share that file** (same machine), or
- **sync it over a relay** (different machines or processes).

Everything below follows from that.

## The four setups

### 1 — Personal agent, one machine, no relay

Point the CLI at the app's own data dir so both read/write the same `hive.db`.
Simplest possible; zero infrastructure.

```sh
# macOS app data dir shown; adjust per-OS
HIVE_DATA_DIR="$HOME/Library/Application Support/com.hive.desktop" \
  hive agent bot
```

Trade-offs: the agent **signs as you** (shared identity, not a distinct agent
account); the app refreshes on window focus rather than instantly; and it's a
single-writer-ish SQLite file (fine for one agent, don't pile on writers).

### 2 — Personal agent, local relay

Run the `hive-relay` binary on `localhost` and point **both** the app and the CLI
at it. Now the agent has its **own identity** (a distinct member), you get live
updates, and concurrency is clean. Cost: one extra process to run. See
[Self-hosting a relay](../networking/self-host.md).

### 3 — Remote agent / always-on worker

The agent lives on a **remote machine** (cloud VM, container, an office box).
Here a relay is **required** — there is no shared filesystem across machines —
but it can be **self-hosted**. This is the setup that lets an agent keep working
while your laptop is closed: the relay's **store-and-forward** holds the agent's
events, and your app catches up when it next comes online.

Provision the relay URL, token, room, and workspace key out-of-band, then run the
**worker daemon** — it registers the box as a host and runs every agent bound to
it on a **workspace-owned credential** (never your personal key):

```sh
export HIVE_RELAY_URL=…  HIVE_RELAY_ACCESS_TOKEN=…  HIVE_WORKSPACE=acme  HIVE_WORKSPACE_KEY=…
export HIVE_WS_SECRET_acme=sk-…
hive register-worker --label prod-box        # → this box's host id
hive add-agent reviewer ws-claude --host <host-id>
hive sync
hive worker --label prod-box                 # always-on; @reviewer is now answered here
```

The worker *enforces* the §12.5 rule — a detached agent must use a workspace
runtime, so it never falls back to a personal key. It also **drains queued work**:
an unanswered `@mention` waits for its agent's host, and the worker answers every
one bound to it — backlog included — so it catches up on mentions addressed while
it was down. `hive queue` shows what's waiting and whether each agent's host is
online; if a member's device host is offline, `hive set-agent-host <agent>
<worker>` reassigns the agent to a worker to drain it ("run on worker instead").
See the
[CLI README](https://github.com/honeyhive-ai/hive/blob/main/crates/hive-cli/README.md#the-worker-daemon-spec-124)
for the full daemon behaviour.

The **desktop app** surfaces the same queue without the CLI: the **Review**
pane lists every unanswered agent mention with its host's live status, and an
offline or device-bound agent gets a **Run on worker** button that reassigns it
to an online worker — the GUI equivalent of `hive set-agent-host`. See
[Right Rail — Review](../features/right-rail.md).

**Give the worker tools.** A worker (or `hive agent`) can reach external boards
like **Linear** or **GitHub** through MCP tools: point `HIVE_MCP_CONFIG` at a TOML
listing MCP servers and provision each server's token via the environment (never
in the file). With no config it runs tool-free, exactly as before. See the
[CLI README — MCP tools for headless agents](https://github.com/honeyhive-ai/hive/blob/main/crates/hive-cli/README.md#mcp-tools-for-headless-agents).

**Tools are gated on the requester's role.** A synced message only drives the
worker's MCP tools if its author holds at least `HIVE_MCP_MIN_ROLE`
(`viewer`|`contributor`|`admin`|`owner`, default `contributor`). Below that bar
the agent still replies — it just runs **tool-free**, so a Viewer can't provoke
a write to Linear or GitHub through a headless agent. Raise the bar with
`HIVE_MCP_MIN_ROLE=admin`. See the
[CLI README — requester-role gate](https://github.com/honeyhive-ai/hive/blob/main/crates/hive-cli/README.md#requester-role-gate-hive_mcp_min_role).

Do **not** try to substitute a network filesystem (SSHFS/NFS) for the relay —
SQLite over a network mount is corruption-prone. Use the relay.

### 4 — A team

Everyone joins the same room on a **shared relay** (self-hosted or hosted) with
the workspace key. The relay is content-blind (E2EE), channels organize the work,
and configuration is **hoisted to the workspace** so every chat inherits the same
agents, skills, vaults, and runtimes. Members' apps and any workers all sync
through the one room.

## Which setup?

| You want… | Setup |
|---|---|
| A personal agent, no infra | **1** — shared data dir |
| A personal agent, live + clean identity | **2** — local relay |
| An agent that runs while you're away / on a server | **3** — remote worker + relay |
| A team | **4** — shared relay |

## Constraints that apply to every setup

- **E2EE key.** The workspace key is shared out-of-band. The relay never sees
  plaintext and cannot help you recover a lost key.
- **The relay is content-blind and self-hostable** — it stores and forwards
  sealed envelopes only. It is not a required cloud service.
- **Retention.** A device fetches events after its cursor; if you're offline
  longer than the relay retains events, you miss anything pushed beyond retention
  (you keep everything you'd already synced).
- **Headless credentials** — relay token, workspace key, and any provider or
  workspace-runtime secret — are provisioned out-of-band (env / secret mount).

## Why a remote agent doesn't need *your* API key

A detached agent runs on a **workspace-owned runtime** (spec §12.5): a
provider/model the workspace owns, synced to every member and worker, with a
credential the *workspace* provides. Your personal key never leaves your device.

Define one, share it, provision its secret on the worker, then point the agent at
it:

```sh
hive add-runtime ws-claude "Team Claude" anthropic claude-sonnet-4-5 --secret-ref acme
hive sync                                          # share it with the workspace
HIVE_WS_SECRET_acme=sk-…  hive agent bot --runtime ws-claude
```

- `--secret-ref <name>` keeps the key **off** the synced log — the worker holds
  it in env `HIVE_WS_SECRET_<name>`. `hive runtimes` shows `✓` when it's present.
- `--secret <value>` carries the key **E2EE on the log** (zero worker
  provisioning; use only for trusted workspaces).

## Coming updates you see on reconnect

Whatever a headless agent does while you're offline — reply in a chat, create a
channel, change workspace config — rides the same synced event log, so your app
pulls it on the next sync and the UI (chats **and** the channel tree) refreshes.
The only requirement is that both sides share the **same relay, room, and
workspace key**. On a local-only workspace (no relay), there's nothing to sync
through — use setup 1 (shared data dir) instead.
