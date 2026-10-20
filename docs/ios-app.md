# Hive on iPhone: thin client + home host

A single-user iOS app (not App Store) for one person's Hive workspace. The
phone doesn't run Hive. It's a thin client over Tailscale to **`hive serve`**,
a home-host process on an always-on box in the lab. The home host is a normal
Hive device in the workspace: it keeps the E2EE keys, syncs with the relay,
and runs agent turns on the lab's model gateway. The phone gets a small JSON
API plus a Server-Sent Events stream.

```
 iPhone (Hive app)                         always-on box (tailnet)                     elsewhere
 ─────────────────                         ──────────────────────────────────────      ─────────
  SwiftUI client  ── HTTP+SSE, bearer ──▶  hive serve                                  relay (E2EE
  token in Keychain    (tailnet only)      ├─ API + SSE   (crates/hive-cli/src/serve)   opaque blobs)
                                           ├─ sync loop   ── push/pull envelopes ──▶      ▲
  ntfy app  ◀── push (Click: hive://…) ──  ├─ agent worker (crates/hive-cli/src/worker)    │
                                           │   └─ tool loop ── OpenAI wire ──▶ llama.cpp   desktop app
                                           │      (built-in file tools + MCP)   gateway    (same workspace)
                                           ├─ ntfy notifier ──▶ self-hosted ntfy
                                           └─ /v1/lab/status ──▶ Prometheus (loopback)
```

## Components

- **`hive serve`** (`crates/hive-cli/src/serve/`) runs in one process:
  - the relay sync loop (the same push/fetch/verify as `hive sync`, every 3 s
    and immediately after a local write, with exponential backoff on errors);
  - the agent worker, on its own thread (`worker.rs`, shared with
    `hive worker`). It answers `@mentions` for the agents whose host is this
    box: it claims the turn, streams the reply, checkpoints it, and files the
    turn's file changes as a review proposal;
  - the HTTP API and the SSE stream (`api.rs`, axum);
  - an event hub (`hub.rs`). After every sync, write or turn step, a differ
    compares the projected chats with the last snapshot and emits
    `message.created`, `message.done`, `proposal.created` and `chat.updated`.
    A reply the desktop streamed and synced in therefore looks the same to the
    phone as one produced here. Deltas and turn status come straight from the
    worker.
- **Agent turns** run the offline harness's loop for local model runtimes:
  `dispatch::stream_with_tools` with `CompositeTools` (Hive's built-in file
  tools plus the MCP tools from `HIVE_MCP_CONFIG`), under the same policy as
  the desktop. See "Tool policy" below.
- **Runtimes.** An agent runs on its workspace runtime (config log). Failing
  that, it runs on this host's own `runtimes.toml` entry for its runtime id, or
  on the catch-all `id = "*"` entry, which is typically the lab's keyless
  llama.cpp gateway. Desktop agents are bound to the desktop's device-local
  runtime ids, so this mapping is what lets you send them here.
- **Repos.** Agents work on checkouts under `HIVE_REPOS_DIR`
  (default `<HIVE_DATA_DIR>/repos`). `hive bind-repo <name|path> [--chat <id>]`
  binds the workspace (or one chat) to a checkout in
  `<HIVE_DATA_DIR>/repo-bindings.toml`. This is the device-local equivalent of
  the desktop's workspace folder binding.
- **Notifications** go to ntfy: one when an agent reply finishes and one when
  a proposal is filed.

## API contract

JSON, camelCase, ISO-8601 UTC timestamps (`2026-10-20T17:03:11.52Z`). Every
request needs `Authorization: Bearer <token>`, except `GET /v1/health`.
A missing or bad token gets `401 {"error": …}`. Errors are
`{"error": "<message>"}` with 400, 401, 403 (roster role too low), 404 or 409
(apply conflict). The server binds to its configured address: default
`127.0.0.1:8787`, and on the home host its Tailscale IP. It refuses `0.0.0.0`
and public addresses unless `HIVE_SERVE_ALLOW_ANY_BIND=1`.

| Method | Path | Response |
|---|---|---|
| GET | `/v1/health` | `{ok: true, version, workspace?, deviceLabel?}`. `workspace` (room name) and `deviceLabel` appear only when authorized |
| GET | `/v1/chats` | `{chats: [{id, title, updatedAt, preview, unread: int}]}`, newest first |
| POST | `/v1/chats` `{title?}` | `{chat}` |
| GET | `/v1/chats/{id}/messages?after=<messageId>&limit=<n>` | `{messages: [Message]}`, oldest first. Without `after`, the latest page (default limit 100, max 1000) |
| POST | `/v1/chats/{id}/messages` `{body}` | `{message}`. `@Name` mentions route to agents exactly as on the desktop/CLI |
| GET | `/v1/agents` | `{agents: [{id, name, runtimeLabel, model?, hostId?, hostLabel?}]}` |
| GET | `/v1/hosts` | `{hosts: [{id, label, isThis: bool, online: bool}]}` |
| PUT | `/v1/agents/{id}/host` `{hostId}` | `{agent}`. `hostId` must be a registered host. `""` hands the agent back to its owner's device |
| GET | `/v1/proposals?status=open\|all` | `{proposals: [{id, chatId, title, body, kind, diff?, status, createdAt, author}]}` |
| POST | `/v1/proposals/{id}/approve` | `{proposal}` |
| POST | `/v1/proposals/{id}/reject` `{reason?}` | `{proposal}` |
| GET | `/v1/lab/status` | `{workers: [{name, up}], powerWatts?, costPerHour?, ratePerKwh?, temps: [{name, celsius}], updatedAt}` |
| GET | `/v1/events` | `text/event-stream` |

`Message = {id, chatId, author, role: "user"|"agent"|"assistant"|"system", agentId?, body, createdAt, streaming: bool}`.
`role` is `agent` for a named agent's reply, which also carries `agentId`, and
`assistant` for the chat's primary runtime. `kind` is `fileDiff`, `command` or
`decision`. `status` is `open`, `approved`, `rejected` or `applied`. `diff` is
a unified diff.

Field notes:

- `unread` counts messages after the phone's read mark that this box didn't
  write. System notes don't count. Fetching messages moves the mark to the last
  message returned. On startup every existing chat is marked read.
- `preview` is the newest non-empty, non-system message, up to 140 characters.
- `/v1/proposals?status=open` lists open proposals that haven't been dismissed
  or superseded. `all` lists every proposal.
- An agent with no host runs on its owner's device. It has no `hostId`, and its
  `hostLabel` is "Owner's device". For an agent on this box, `hostLabel` is
  "<label> (this host)".
- `/v1/lab/status` is best-effort. Prometheus (`HIVE_PROMETHEUS_URL`, default
  `http://127.0.0.1:9090`) gets each query with a 2 s timeout. A field whose
  query fails is left out. `workers` comes from `rdnagw_worker_up{worker}`,
  power and cost from `lab:power_watts:sum`, `lab:cost_usd_per_hour` and
  `lab:electric_rate_usd_per_kwh`, and temps from `HIVE_LAB_TEMPS`
  (`Name=promql;…`).

### Events (`GET /v1/events`)

Each event has `id: <n>` (increasing), `event: <name>` and `data: <JSON>`. A
`: heartbeat` comment is sent every 15 s. `Last-Event-ID` replays buffered
events after that id, best effort: the server keeps the last 1024. A client
that falls further behind should refetch.

| `event:` | `data:` |
|---|---|
| `message.created` | `{message}` (an agent reply starts with `streaming: true` and an empty body) |
| `message.delta` | `{chatId, messageId, text}`, a streamed fragment of a reply produced on this box |
| `message.done` | `{message}`, the final body with `streaming: false`. Replies synced in already finished get `created` followed by `done` |
| `chat.updated` | `{chat}` (same shape as in `/v1/chats`) |
| `proposal.created` | `{proposal}` |
| `turn.status` | `{chatId, agentId, state: "queued"\|"running"\|"done"\|"error", detail?}` |

### Pairing and links

`hive serve --pair` prints a terminal QR code and the text link when it
starts. `hive pair` does the same without starting a server, which is handy
while the systemd unit runs. The link is
`hive://pair?url=<base64url(serverURL), unpadded>&token=<token>`, where
`serverURL` is `HIVE_SERVE_PUBLIC_URL` or `http://<listen>`. The app's URL
scheme is `hive`: `hive://pair?…` and `hive://chat/<id>`. ntfy notifications
carry `Click: hive://chat/<chatId>`.

## Security model

- **Tailnet only.** The API listens on loopback, a private LAN address or the
  box's Tailscale IP. Unspecified and public binds are refused unless
  overridden. Tailscale ACLs decide which devices can reach the port. The
  transport is plain HTTP inside WireGuard: the tailnet supplies confidentiality
  and peer authentication, and the bearer token supplies authorization.
- **Bearer token.** 32 random bytes, hex-encoded, in
  `<HIVE_DATA_DIR>/serve.token` (created with mode 0600 on first run). It's
  compared in constant time. The phone receives it once, through the pairing
  link, and keeps it in the iOS Keychain
  (`kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`, not synced to iCloud).
  To revoke it, delete the file and restart, then pair again. The token grants
  what this box's workspace member can do, and nothing more.
- **Keys never leave the box.** The workspace E2EE key (from the
  `hivews1:` invite, used with `hive enroll`) and the device and account
  signing keys stay in `HIVE_DATA_DIR`. The relay sees only ciphertext. The
  phone sees only plaintext API responses over the tailnet, and nothing it
  holds can decrypt the relay's log or sign workspace events.
- **The box is its own member.** It joins the roster as **Contributor** (self
  enrollment) and signs every event it writes, including the phone's messages,
  which appear under `HIVE_SERVE_NAME`, and the phone's votes. Peers verify
  those events on ingest like any other device's. Approve and reject cast the
  same `ProposalVoteCast` delta the desktop's Review pane does, at this
  member's real roster role. Quorum, the role floor and the no-self-approval
  rule apply unchanged, so a proposal whose floor is above this member's role
  needs another reviewer or a promotion. Agent proposals are authorless, as
  on the desktop.
- **Tool policy.** The desktop's defaults apply:
  - Built-in file tools are offered only to runtimes that may front a local
    server (Custom, HiveDaemon, Ollama) and only when the endpoint is on this
    machine, a private network or the tailnet, unless
    `HIVE_ALLOW_REMOTE_FILE_TOOLS=1`. Hosted APIs never get the tool loop.
  - Reads come from the chat's base worktree. **Writes only go to a per-turn
    git worktree**, cut on the first write, and its diff becomes a
    review-gated `fileDiff` proposal. The checkout is never written in place.
    Without a bound checkout a turn gets no file tools.
  - `run_command` is off unless `HIVE_BUILTIN_RUN_COMMAND=1`, and even then
    it runs only inside the turn's worktree.
  - **Exfiltration guard.** Once a turn has read project files, MCP calls are
    refused for the rest of the turn. Only `HIVE_MCP_AFTER_READS=1` turns
    this off. MCP results are labelled as untrusted data.
  - **Role gate.** Tools of either kind run only when the triggering
    message's author has at least `HIVE_MCP_MIN_ROLE` (default Contributor).
  - Optional: `HIVE_SERVE_APPLY_ON_APPROVE=1` applies an approved diff to the
    chat's branch (`hive/chat/<id>`) in the bound checkout, the way the
    desktop's Implement does. Only a proposal whose status is `approved`
    applies: one a qualifying reviewer rejected, or that was dismissed or
    superseded, never does.
  - A file-writing subprocess agent (aider, pi, codex, hermes, or Claude Code
    in an edit mode) runs only in a per-turn worktree of a bound repository.
    Without one the turn fails instead of writing in place.
- **Notifications carry no content.** An ntfy push says who replied in which
  chat, or that a change is ready for review, plus the deep link. It never
  includes message bodies, diffs or secrets. `HIVE_NTFY_TOKEN` (optional) is
  sent as a bearer token.
- **Secrets on the box** sit in the systemd `EnvironmentFile` (0600) and
  `config.toml`. Nothing is committed, and the pairing link is printed only on
  request.

## Limits

- Single user, single workspace per `hive serve` process, and one phone per
  token (the token isn't per-device).
- The box is a separate member identity, not the phone owner's account. The
  desktop shows the phone's messages under `HIVE_SERVE_NAME`.
- Agents run one at a time on the worker thread. A long turn delays the next
  one, but the API and sync keep running.
- Deltas are pushed only for turns that run on this box. A reply streamed on
  another device arrives as `created` and then `done` once it syncs (3 s
  cadence).
- Event replay keeps 1024 events in memory and is lost on restart. The app
  refetches its lists and the open chat whenever its stream reconnects.
- On SIGTERM a turn in flight gets 15 s to finish. A reply cut off by a stop
  is finished on the next start with an "interrupted" note; ask again to
  retry.
- A fresh box whose first relay sync fails refuses to start (systemd
  retries), so it never seeds a workspace config of its own.
- Read marks are per box, not per device, and persist in
  `serve-state.json`.
- Creating agents and editing runtimes aren't in the API. Assigning existing
  agents to hosts is.
- New agents created on the desktop default to their owner's device. Send them
  here explicitly. Once an agent is sent to a host, only that host answers its
  mentions; the desktop defers to it (and answers after 25 s only if the host
  is offline).
- Approving doesn't apply a diff by default; the desktop's Implement does that.
  Set `HIVE_SERVE_APPLY_ON_APPROVE=1` to apply on the home host.

## Running and testing

```sh
hive serve --listen 100.x.y.z:8787 --label sff --pair
hive pair                                     # show the pairing QR code again
cargo test -p hive-cli                        # unit + integration (test relay, fake model, fake ntfy)
HIVE_SERVE_LIVE_ENDPOINT=http://<gateway>:8080/v1/chat/completions \
HIVE_SERVE_LIVE_MODELS=gpt-oss:20b,gemma4:12b \
  cargo test -p hive-cli serve::tests::live -- --nocapture
```

The integration tests start a real `hive serve` against the in-process test
relay (`crates/hive-runtime/src/test_relay.rs`), with a desktop identity that
owns the workspace. They cover auth (401s), host assignment, a phone message
that reaches a hosted agent, the streamed `message.delta` and `message.done`,
a `write_file` tool call that becomes a `fileDiff` proposal, approval (also
seen by the desktop after sync), rejection with a reason, `Last-Event-ID`
replay, ntfy pushes, and the pairing link format. The live test runs one turn
per model against the real gateway, in which the agent has to `read_file` a
planted note to answer.

Deployment: [`deploy/hive-serve/README.md`](../deploy/hive-serve/README.md).
