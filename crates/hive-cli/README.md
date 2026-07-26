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
hive agent [name]                  # act as the primary responder (see below)
```

Chat ids come from `hive chats` (or `hive sync` then `hive chats` to pull a
workspace the app created).

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

## Limits & security (spec §12)

- **Credentials are provisioned out-of-band.** The relay token and workspace
  key must be handed to the box (env/secret); there's no headless self-service
  for either.
- **The provider key is personal.** `hive agent` runs on the API key you give
  it. Spec §12.5's *workspace-owned* credential for a truly detached worker
  (owner offline, workspace-funded) does not exist yet — build that before
  running an agent on shared, workspace-funded infrastructure.
- **Agents never touch another machine's filesystem** (spec §12.6): what crosses
  the relay is chat and, eventually, diffs — not remote file writes.
