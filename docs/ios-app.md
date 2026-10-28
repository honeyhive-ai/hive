# Hive on iPhone: thin client + home host

> The iPhone app's source now lives in a separate repository. This document
> stays here as the design and **API contract of `hive serve`**, which any
> client can implement.

A single-user iOS app for one person's Hive workspace. The
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
                                           └─ /v1/dashboard ──▶ Prometheus, JSON URLs (host side)
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
    `message.created`, `message.done`, `proposal.created`, `proposal.updated`
    and `chat.updated`.
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
| GET | `/v1/chats` | `{chats: [{id, title, updatedAt, preview, unread: int, channelId, participants: [Participant]}]}`, newest first |
| POST | `/v1/chats` `{title?}` | `{chat}` |
| GET | `/v1/chats/{id}/messages?after=<messageId>&limit=<n>` | `{messages: [Message]}`, oldest first. Without `after`, the latest page (default limit 100, max 1000) |
| POST | `/v1/chats/{id}/messages` `{body}` | `{message}`. `@Name` mentions route to agents exactly as on the desktop/CLI |
| GET | `/v1/chats/{id}/details` | The chat's details (the desktop's right-hand panel, read-only); see below |
| GET | `/v1/agents` | `{agents: [{id, name, runtimeLabel, model?, hostId?, hostLabel?, hostKind, hostOnline?, role?, avatarUrl?, avatarColorHex?}]}` |
| GET | `/v1/people` | `{people: [Person]}`, the workspace's human members |
| GET | `/v1/channels` | `{channels: [{id, name, purpose?, position, archived, defaultChatId}]}`, in sidebar order |
| GET | `/v1/hosts` | `{hosts: [{id, label, isThis: bool, online: bool}]}` |
| PUT | `/v1/agents/{id}/host` `{hostId}` | `{agent}`. `hostId` must be a registered host. `""` hands the agent back to its owner's device |
| GET | `/v1/proposals?status=open\|all` | `{proposals: [{id, chatId, title, body, kind, diff?, status, createdAt, author}]}` |
| POST | `/v1/proposals/{id}/approve` | `{proposal}` |
| POST | `/v1/proposals/{id}/reject` `{reason?}` | `{proposal}` |
| GET | `/v1/proposals/{id}` | `{proposal}` |
| GET | `/v1/proposals/{id}/comments` | `{comments: [Comment]}`, the refinement thread, oldest first |
| POST | `/v1/proposals/{id}/comments` `{text, requestChanges?}` | `{comment, proposal}`. `requestChanges: true` marks it a change request |
| POST | `/v1/proposals/{id}/request-changes` `{note?}` | `{proposal, message}`. The desktop's "Send to agent to revise"; `note` is added first as a change request |
| GET | `/v1/dashboard` | `{title, source, refreshSeconds, sections: [{title, panels: [Panel]}], updatedAt, error?}`. Defined by the host's `dashboard.toml`; see [dashboard.md](dashboard.md) |
| GET | `/v1/appearance` | `{palette, mode, source}`, the host's theme for a phone set to "Match host". `palette` is `pollen`, `studio`, `harbor`, `slate`, `meadow`, `obsidian` or `null`; `mode` is `auto`, `light`, `dark` or `null`; `source` is `config`, `desktop` or `none` |
| GET | `/v1/lab/status` | **Deprecated** (`Deprecation: true`; use `/v1/dashboard`). `{workers: [{name, up}], powerWatts?, costPerHour?, ratePerKwh?, temps: [{name, celsius}], solar?, updatedAt}` |
| GET | `/v1/events` | `text/event-stream` |

`Message = {id, chatId, author, role: "user"|"agent"|"assistant"|"system", agentId?, actorId?, body, createdAt, streaming: bool}`.
`actorId` (on a person's message) matches `Person.actorId`.
`role` is `agent` for a named agent's reply, which also carries `agentId`, and
`assistant` for the chat's primary runtime. `kind` is `fileDiff`, `command` or
`decision`. `status` is `open`, `approved`, `rejected` or `applied`. `diff` is
a unified diff.

Every proposal also carries its collaboration state, as the desktop's Review
card shows it: `requiredApprovals`, `qualifyingApprovals`, `quorumMet`,
`approvalRoleFloor`, `votes: [{actorId, voter, role, approved, createdAt}]`
(one per member, latest wins), `comments: [Comment]`, `commentCount`,
`changeRequestCount`, `changedFiles`, `dismissed`, `parentId?` (the version it
revises) and `supersededBy?` (its newer version). Proposals in API responses
and events also say what this box's identity, the one the phone acts as, can
do: `myRole`, `myVote: "approve"|"reject"|null`, `canVote` (its role meets the
content floor the core applies to votes and notes) and `isMine`.
`Comment = {id, actorId, author, role, text, requestChanges, createdAt}`.
`role` everywhere is `owner`, `admin`, `contributor` or `viewer`.

Field notes:

- `unread` counts messages after the phone's read mark that this box didn't
  write. System notes don't count. Fetching messages moves the mark to the last
  message returned. On startup every existing chat is marked read.
- `preview` is the newest non-empty, non-system message, up to 140 characters.
- `/v1/proposals?status=open` lists open proposals that haven't been dismissed
  or superseded. `all` lists every proposal.
- Votes and notes go through the same core calls as the desktop
  (`ProposalVoteCast` and `ProposalRefined` deltas, signed by this box at its
  real roster role), so they sync to every device and merge with concurrent
  ones. A role below the content floor gets `403` and nothing is written.
- `request-changes` bundles the thread's change requests (all notes if there
  are none; `400` if the thread is empty), records "Sent to the agent to
  revise." on the thread and posts the instruction as an `@agent` message to
  the agent that produced the proposal, so whichever device hosts it answers.
  When that agent files its next proposal in the chat, this box links it
  (`parentId`) and marks the original `supersededBy`, and the original leaves
  the open list. `409` if the proposal is already revised or applied, or if it
  came from the chat's primary runtime and no `HIVE_SERVE_DEFAULT_AGENT` names
  an agent to send it to.
- An agent with no host runs on its owner's device. It has no `hostId`, and its
  `hostLabel` is "Owner's device". For an agent on this box, `hostLabel` is
  "<label> (this host)".
- Avatars: `avatarUrl` is the picture a member or agent uploaded on the
  desktop, a bounded `data:image/…` URL (about 96 KB at most) that rides the
  synced roster. Without one, draw initials on `avatarColorHex` (agents) or a
  color derived from the name. Avatars are sent only by `/v1/people`,
  `/v1/agents` and chat details, never on chat rows, so a client caches them
  by id.
- `hostKind` says where an agent runs, as the desktop's host badge does:
  `ownerDevice` (no host: its owner's machine), `thisHost` (this box),
  `worker` or `device` (another registered host). `hostOnline` is present
  for an agent with a host.
- `Person = {actorId, displayName, kind, role: "owner"|"admin"|"contributor"|"viewer", title?, avatarUrl?, isSelf: bool}`.
  `isSelf` marks this box's own identity (the one the phone writes as).
- `channelId` files a chat under a channel (`""` = unfiled). A channel's
  `defaultChatId` is its drop-in chat. `POST /v1/chats` files the new chat
  under the first channel, as the desktop does.
- `Participant = {kind: "person"|"agent"|"primary", id, name}`: who spoke in the
  chat, newest first, at most 5. `id` is an actor id (`person`), an agent id
  (`agent`) or `hive` (`primary`, the chat's own `@hive`).
- `GET /v1/chats/{id}/details` answers
  `{chat, channel?, people: [Person + inChat], agents: [Agent + inChat], defaultResponder, context, workflowRuns, skills, vaults}`.
  `inChat` is true for whoever spoke there (or an agent added to the chat).
  `defaultResponder = {kind: "primary"|"agent", name, via: "untagged"|"serveDefault", runtimeLabel?, model?, agentId?, hostLabel?, hostKind?}`
  is who answers an untagged message: the chat's `@hive` on its runtime, or the
  agent named by `hive serve --default-agent`.
  `context = {estimated: true, windowTokens, usedTokens, historyTokens, skillTokens, messageCount, compacting, model?}`
  is a coarse estimate from the transcript, the loaded skills and the model's
  window (the desktop's Context pane plans the real prompt).
  `workflowRuns = [{id, name, status: "running"|"awaitingGate"|"completed"|"failed"|"halted"|"canceled", startedAt, updatedAt, stepsDone, stepsTotal, initiator?}]`,
  newest first, at most 10. `skills = [{id, name, summary, sourceUrl?, agentIds?}]`
  and `vaults = [{label, url, agentIds?}]` (`agentIds` absent = every agent).
- `/v1/dashboard` is what the host owner configures in `dashboard.toml` in
  the data dir (format, panel kinds and examples in [dashboard.md](dashboard.md)).
  `Panel = {id, title, kind: "stat"|"gauge"|"timeseries"|"bar"|"list"|"status"|"text", unit?, value?, text?, level?: "ok"|"warn"|"critical", sparkline?: [{t, v}], series?: [{name, points: [{t, v}]}], rows?: [{label, value?, text?, up?, level?}], min?, max?, thresholds?: [{value, level}], refreshSeconds, updatedAt, error?}`.
  The host evaluates every panel (Prometheus or a JSON URL on its network,
  never a command) with a per-panel timeout and cache; a failing panel carries
  `error` and the rest still render. With no file, a host with
  `HIVE_PROMETHEUS_URL` set serves a generated lab dashboard (the old
  `/v1/lab/status` content), and any other host an empty one
  (`sections: []`). The app hides its dashboard tab when the dashboard is
  empty, and falls back to `/v1/lab/status` only when `/v1/dashboard` is 404
  (an older host).
- `/v1/appearance` resolves each field on its own: serve config first
  (`HIVE_SERVE_PALETTE`, `HIVE_SERVE_APPEARANCE`), then the desktop app's
  `appearance.json` on the same machine (`HIVE_DESKTOP_DATA_DIR`, default the
  OS app-data dir of `com.hive.desktop`), which the desktop rewrites whenever
  its theme changes. The file is read per request. A client treats `null` (or a
  404 from an older server) as "use the default palette".
- `/v1/lab/status` (deprecated) is best-effort. Prometheus (`HIVE_PROMETHEUS_URL`, default
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
| `proposal.updated` | `{proposal}`, when a known proposal's status, votes, thread, revision link or dismissal changes (from this phone or any device) |
| `turn.status` | `{chatId, agentId, state: "queued"\|"running"\|"done"\|"error", detail?}` |

### Pairing and links

`hive serve --pair` prints a terminal QR code and the text link when it
starts. `hive pair` does the same without starting a server, which is handy
while the systemd unit runs. Run it with the server's settings
(`HIVE_SERVE_LISTEN` or `HIVE_SERVE_PUBLIC_URL`, and `HIVE_DATA_DIR`): it
refuses to print a link to a loopback address, and it never creates a token of
its own, so it can't hand the phone one the server doesn't accept. The link is
`hive://pair?url=<base64url(serverURL), unpadded>&token=<token>`, where
`serverURL` is `HIVE_SERVE_PUBLIC_URL` or `http://<listen>`. The app's URL
scheme is `hive`: `hive://pair?…` and `hive://chat/<id>`. ntfy notifications
carry `Click: hive://chat/<chatId>`.

### Remote access through a relay (optional, off by default)

`hive serve` can also be reached without Tailscale, through a relay, over an
end-to-end encrypted and mutually authenticated tunnel (protocol:
[`relay-tunnel.md`](relay-tunnel.md), `hive-tunnel/1`). The host keeps one
outbound WebSocket to the relay and opens no inbound port. Each phone request
is dispatched into the same axum router in process, so the API and SSE
contract above is unchanged. The relay sees only ciphertext.

```sh
hive remote enable --relay https://relay.example.com [--reg-token T]
#   or per run: hive serve --remote-relay URL   (env HIVE_REMOTE_RELAY;
#   HIVE_REMOTE_RELAY=off overrides relay.json; HIVE_REMOTE_REG_TOKEN)
hive pair                  # v2 link: url/token plus v=2&hid&hs&relay&ps&exp
hive pair --relay-only     # no url/token (also automatic when the direct URL is loopback)
hive remote status         # relay, host id, live connection, phone count
hive phones list
hive phones revoke <pid|name> [--keep-token]
hive remote disable | reset   # reset = new host keys: every phone re-pairs
```

- State is in `<HIVE_DATA_DIR>/tunnel/` (directory 0700, files 0600):
  `keys.json` (Ed25519 signing key and X25519 Noise static), `phones.json`,
  `relay.json`, `status.json`, and `pending/` for the hand-off from `hive pair`
  and `hive phones revoke` to the running server (picked up within 2 s).
- The relay URL must be `https`; `--insecure-relay` /
  `HIVE_REMOTE_INSECURE_RELAY=1` allows `http` (tests, a loopback relay).
- Each v2 link opens a one-time pairing window of 10 minutes. The phone gets a
  per-phone credential (`hpc1`, 90 days) and renews it daily through
  `POST /v1/tunnel/credential`. That route and `PUT /v1/tunnel/push
  {"enabled": bool}` exist only through the tunnel and return 404 with a
  bearer token.
- End-to-end encrypted push notifications (replies, mentions, reviews,
  workflow approvals) and how to enable them with your own APNs key:
  [`push-notifications.md`](push-notifications.md).
- `hive phones revoke` takes effect at once: the server ends the phone's live
  sessions (GOAWAY), refuses its requests and handshakes, and tells the relay
  (retried until acked). It also rotates `serve.token` unless `--keep-token`,
  because a stolen phone holds that too. A running server picks up the new
  token within 2 s.
- Through the tunnel, requests carry no bearer token: the phone is
  authenticated by its Noise static key, and every request re-checks that the
  phone is still paired.

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
  token (the token isn't per-device). Phones paired over the relay are
  per-device and can be revoked one by one.
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
- A pending "send to agent to revise" is remembered in memory: if the box
  restarts before the revision arrives, the new version isn't linked to the
  old one (both stay listed).
- Approving doesn't apply a diff by default; the desktop's Implement does that.
  Set `HIVE_SERVE_APPLY_ON_APPROVE=1` to apply on the home host.

## Running and testing

```sh
hive serve --listen 100.x.y.z:8787 --label sff --pair
hive pair                                     # show the pairing QR code again
cargo test -p hive-cli                        # unit + integration (test relay, fake model, fake ntfy)
HIVE_TUNNEL_INTEROP_RELAY=http://127.0.0.1:8443 \
  cargo test -p hive-cli interop_against_a_real_relay   # tunnel against a relay run with HIVE_RELAY_TUNNEL=open
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
replay, ntfy pushes, and the pairing link format. The tunnel tests check the
`relay-tunnel/vectors.json` vectors, the mux and flow control, and run an end
to end pairing, GET, SSE stream, renewal and revocation through a fake relay
and a fake phone. The live test runs one turn
per model against the real gateway, in which the agent has to `read_file` a
planted note to answer.

Deployment: [`deploy/hive-serve/README.md`](../deploy/hive-serve/README.md).
