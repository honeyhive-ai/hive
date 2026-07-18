# Wire protocols

Hive's state is an append-only log of **signed event envelopes**. The same
envelope is the unit of local storage, of relay sync, and (eventually) of
direct peer transfer — so this one shape is the contract an independent
implementation would need.

## 1. Event envelope + local store

State lives in **SQLite** (`hive.db` in the app data dir): an append-only
`events` table, one signed envelope per row (`envelope_json` TEXT), projected
into a `ChatSession` on read. Each envelope serializes to JSON with `camelCase`
keys:

```json
{
  "id": "uuid",
  "eventId": "uuid",
  "sessionId": "uuid",
  "workspaceId": "uuid",
  "sequence": 42,
  "timestamp": "ISO8601",
  "actorStamp": { "…": "…" },
  "payload": { "kind": "messageAppended", "…": "…" },
  "scope": "session",
  "signerDeviceId": "uuid",
  "signature": "base64-Ed25519"
}
```

`payload.kind` is the tagged event variant — e.g. `messageAppended`,
`messageChunkReceived`, `messageCompleted`, `sessionTitleChanged`,
`agentRosterUpdated`, `proposalUpserted`, `messageReactionAdded`. The projector
folds them in order; a `sessionSnapshot` seeds/bounds replay.

The signature is **Ed25519** over a canonical preimage (sorted-key JSON,
ISO8601 dates) of the envelope's identifying fields + payload. See
`crates/hive-core/src/{crypto,events}.rs`. Envelopes that fail verification are
quarantined, not applied.

## 2. Relay sync (HTTP/JSON)

The relay (`crates/hive-relay/`) is a per-workspace forwarding mailbox keyed by
a **room id**. Devices on the same relay URL + room converge.

- `GET  /v1/health` → `ok`
- `POST /v1/workspaces/<room>/envelopes` → append signed envelopes to the room
- `GET  /v1/workspaces/<room>/envelopes?after=<sequence>` → drain everything
  newer than `<sequence>`

Clients push their unsynced envelopes and pull new ones on a short interval,
deduping by `eventId` (so restarts / double-sends are harmless). When a
**workspace key** is set, each envelope body is sealed with ChaCha20-Poly1305
*before* upload — the relay only ever stores ciphertext. See
[Self-hosting a relay](../networking/self-host.md).

## 3. MCP (subprocess or HTTP)

Standard [Model Context Protocol](https://modelcontextprotocol.io). Hive
implements `initialize`, `tools/list`, and `tools/call`, over two transports:

- **stdio** — Hive spawns the binary; JSON-RPC over stdin/stdout.
- **http** — JSON-RPC POSTs to the configured endpoint.

An installed server stays inert until you enable it. Wire types and bridges
live in `crates/hive-runtime/src/mcp/`.

## Canonical JSON conventions

Every signed payload uses **sorted keys**, **no escaped slashes**, and
**ISO8601** dates. This is the wire contract: an independent implementation MUST
produce byte-identical preimages or signatures won't match. A stability test
guards against accidental drift.

## Roadmap (not in the current build)

The relay also exposes `/v1/workspaces/<room>/{candidates,presence}` as
groundwork, but the following are tracked follow-ups and **not wired** in the
current Rust build — relay forwarding (above) is the multiuser path today:

- **Direct peer-to-peer** — STUN / UDP hole-punch / TURN fallback and a
  length-prefixed peer-link frame protocol.
- **LAN discovery** — zero-config local peer announcement.
- **Collaborative-text CRDT** — concurrent editing of proposal bodies.

## Versioning

Protocols carry an explicit version: the relay namespaces its routes under
`/v1/…` (a breaking change bumps to `/v2/…` and both coexist during the upgrade
window), and the envelope's `scope` defaults to `session` when missing, so new
fields can be added without breaking old logs.
