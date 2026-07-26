# `hive` — headless Hive client

The binary a cloud-hosted agent (or a script) runs to participate in a Hive
workspace **without the desktop app**. It runs the same runtime the app does
(`hive-core` + `hive-runtime`, no Tauri): create-or-load a signing identity,
connect to a relay, sync the workspace's end-to-end-encrypted event log, and
read/write **signed** events. This is the "worker" client from the design
spec §12 (D21–D23).

## Build / install

```sh
cargo build --release -p hive-cli      # → target/release/hive
# or run in place:
cargo run -p hive-cli -- <command>
```

## Configuration (environment)

Everything is env-driven so it drops into a container or a secret mount. A
**persistent data dir** keeps a stable identity across restarts.

| Variable | Purpose |
| --- | --- |
| `HIVE_DATA_DIR` | Where identity + `hive.db` live. Default: `$HOME/.hive`. **Must persist** for a stable identity. |
| `HIVE_RELAY_URL` | Relay base URL. Required for `sync`, `send --push`, `agent`. |
| `HIVE_RELAY_ACCESS_TOKEN` | Relay entitlement token (`hrt1…`/hex). **Provision out-of-band** — there is no headless self-service. |
| `HIVE_RELAY_GITHUB_TOKEN` | Only on membership-enforcing relays. |
| `HIVE_WORKSPACE` | Relay room / workspace name. Default: `default`. |
| `HIVE_WORKSPACE_KEY` | E2EE passphrase. Omit only if the relay is plaintext. **Provision out-of-band.** |
| `HIVE_PROVIDER` | For `agent`: `anthropic` (default) \| `openai` \| `openrouter` \| `ollama`. |
| `HIVE_MODEL` | For `agent`: model id (e.g. `claude-sonnet-4-5`, `gpt-5`). |
| `HIVE_PROVIDER_API_KEY` | For `agent`: the model key (or provider-specific `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / `OPENROUTER_API_KEY`). |

## Commands

```
hive whoami                        # this client's identity (account/device ids); bootstraps keys on first run
hive chats                         # list chats in the local store
hive new [title]                   # create a chat in the workspace → prints its id
hive tail <chat-id> [--watch]      # print a chat's transcript; --watch syncs + follows
hive send <chat-id> <message…>     # post a signed message
       [--push]                    #   …and sync it out to the relay
hive sync [--watch]                # one sync round (push everything unpushed, then fetch+ingest)
hive runtimes                      # list workspace-owned runtimes (spec §12.5)
hive add-runtime <id> <name> <provider> <model>
       [--endpoint <url>] [--secret-ref <name> | --secret <value>]
hive rm-runtime <id>
hive agents                        # list workspace agents
hive add-agent <name> <runtime-id> [--host <id>] [--role <role>]
hive hosts                         # list workspace hosts (devices + workers)
hive register-worker [--label <name>]      # register this box as a worker host
hive set-agent-host <agent> <host-id>      # bind an agent to a host
hive worker [--label <name>]       # run the worker daemon (spec §12.4)
hive agent [name] [--runtime <id>] # act as the primary responder (see below)
```

Chat ids come from `hive chats` (or `hive sync` then `hive chats` to pull a
workspace the app created).

> **Where should the agent run?** See
> [Headless agents & setups](../../docs-site/concepts/headless-agents.md) for the
> four topologies (shared data dir · local relay · remote worker · team) and how
> to pick one.

## Using this as an agent

`hive agent [name]` is the built-in loop: after each sync it replies — as the
workspace's **primary responder** — to any new user turn that is un-mentioned
or `@primary`, generating the reply with your configured provider and posting
it back as signed events. It seeds on the existing backlog so it never answers
history, and idles between polls.

```sh
export HIVE_DATA_DIR=/var/lib/hive HIVE_RELAY_URL=https://relay.example \
       HIVE_RELAY_ACCESS_TOKEN=hrt1… HIVE_WORKSPACE=acme HIVE_WORKSPACE_KEY=… \
       HIVE_PROVIDER=anthropic HIVE_MODEL=claude-sonnet-4-5 ANTHROPIC_API_KEY=sk-…
hive agent acme-bot
```

### Rolling your own loop

If you want custom behaviour (only answer certain chats, run tools, decide when
to speak), skip `hive agent` and script the primitives:

1. `hive sync` — pull new events.
2. `hive chats` — see chats; `hive tail <id>` — read a transcript.
3. Decide whether/what to reply.
4. `hive send <id> "<reply>" --push` — post it and sync out.
5. Sleep briefly; repeat from 1.

Each `send` is a signed event; the next push syncs it to every member.

### Agent operating instructions (copy into a system prompt)

> You participate in a Hive workspace through the `hive` CLI (already configured
> via environment variables). To read the conversation, run `hive sync` then
> `hive tail <chat-id>`; list chats with `hive chats`. To reply, run
> `hive send <chat-id> "<your message>" --push`. Only reply when a message is
> addressed to you (`@primary` or an un-directed question). Keep replies concise.
> Re-run `hive sync` before reading so you see the latest. Never invent chat ids —
> get them from `hive chats`.

## Workspace-owned runtimes (spec §12.5)

A **detached** agent — one that keeps working when its owner's laptop is closed —
must run on a **workspace-owned credential**, never a member's personal API key.
A *workspace runtime* is that: a provider/model the workspace owns, synced to
every member and worker via the config log, with a credential the workspace
provides. Its raw key does **not** have to ride the synced log.

Define one (owner/admin), then run an agent on it:

```sh
# key stays OFF the synced log — the worker holds it out-of-band:
hive add-runtime ws-claude "Team Claude" anthropic claude-sonnet-4-5 --secret-ref acme
hive sync                                   # share it with the workspace
# on the worker, provision the workspace secret and run the agent on it:
HIVE_WS_SECRET_acme=sk-… hive agent teambot --runtime ws-claude
```

- `--secret-ref <name>` — the worker resolves the key from env
  `HIVE_WS_SECRET_<name>`. Raw keys never touch the synced stream. `hive runtimes`
  shows `✓` when the secret is present on this box, `(missing)` otherwise.
- `--secret <value>` — carries the key **E2EE on the config log**. Zero worker
  provisioning (the workspace key already unlocks it); the key does land on every
  member's disk, so use it only for trusted workspaces.
- neither — a keyless runtime (e.g. local Ollama).

`hive agent --runtime <id>` resolves provider/model/credential from the workspace
runtime; without `--runtime` it falls back to a **personal** env key
(`HIVE_PROVIDER`/`HIVE_MODEL` + key).

## The worker daemon (spec §12.4)

A **host** is a machine that executes an agent's turns — a member's `device` or an
always-on `worker` the workspace owns. An agent binds to a host; an agent bound to
a **worker** is *detached* — it keeps running when its owner's laptop is closed.

`hive worker` is that always-on process. It registers this box as a worker host
(its host id **is** the box's device id — no extra provisioning), then continuously
hosts the agents bound to it: when a hosted agent is `@mentioned`, it replies via
that agent's **workspace runtime** (§12.5) and posts the reply. A failing turn is
isolated and logged; the loop keeps running.

Full setup on a worker box:

```sh
# once, to define the shared roster (syncs to everyone):
hive add-runtime ws-claude "Team Claude" anthropic claude-sonnet-4-5 --secret-ref acme
hive register-worker --label prod-box            # → prints this box's host id
hive add-agent reviewer ws-claude --host <host-id> --role "Reviews diffs"
hive sync

# then run the daemon (with the workspace secret provisioned):
HIVE_WS_SECRET_acme=sk-…  hive worker --label prod-box
```

Now `@reviewer` in any chat is answered by this worker, on the workspace credential
— even while every human is offline. `hive hosts` / `hive agents` show the roster
and bindings; `hive set-agent-host <agent> <host>` re-binds.

**Enforced:** a detached agent must run on a *workspace* runtime. If a hosted
agent isn't bound to one (or its secret isn't on the box), the worker logs a skip
rather than falling back to a personal key.

**Behaviour on (re)start:** the worker seeds on the existing backlog so a restart
doesn't re-answer old messages; it responds to mentions that arrive while it's up.
(Picking up work addressed while it was *down* — the "queued work" handoff — is a
tracked follow-up.)

## Limits & security (spec §12)

- **Credentials are provisioned out-of-band.** The relay token and workspace
  key must be handed to the box (env/secret); there's no headless self-service
  for either.
- **Personal vs workspace credential.** Without `--runtime`, `hive agent` runs on
  a *personal* env key. With `--runtime <id>` — and always inside `hive worker` —
  it runs on a **workspace-owned** runtime (§12.5); the worker daemon *enforces*
  that a detached agent uses one. What's still missing from §12.4–12.5 is the
  **queue/offline handoff** (picking up work addressed while a host was down, and
  the "run on worker instead" reassignment) and live host presence.
- **Agents never touch another machine's filesystem** (spec §12.6): what crosses
  the relay is chat and, eventually, diffs — not remote file writes.
