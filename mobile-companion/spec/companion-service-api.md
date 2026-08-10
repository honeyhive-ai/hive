# Companion service API

Implements MC-002 §4 and MC-001 §3/§5. Normative.

## Shape

A **typed façade**, not a proxy. The complete set of operations a paired phone may invoke is the
allowlist below. There is no generic endpoint, no path parameter that becomes a filesystem path, no
field that becomes a URL, no field that becomes a command string, and no route that forwards to the
local runtime.

`hive.config.toml` points the default runtime at `http://localhost:11434/v1/chat/completions` —
unauthenticated, plaintext, tools enabled. A passthrough endpoint would hand any paired phone, and
anything that compromises one, direct unauthenticated access to that runtime and through its tools
to the shell. Hence: no passthrough, at any tier, for any caller.

Every operation runs over the established Noise session. There is no unauthenticated route except
the pairing listener.

## Operations

| Op | Kind | Returns |
|---|---|---|
| `workspaces.list` | read | id, name, agent count, unread count, pending approval count |
| `workspace.get` | read | workspace detail + participant roster |
| `conversation.history` | read | page of turns, cursor-based, agent-attributed |
| `conversation.subscribe` | stream | live turns, run state changes, approval created/decided |
| `message.send` | write | accepted turn id (idempotent on client `nonce`) |
| `approvals.list` | read | pending records: id, kind, **tier**, `record_hash`, created_at |
| `approval.get` | read | the canonical body + `record_hash` |
| `approval.challenge` | read | desktop-issued single-use nonce for one Tier 2 decision on this device |
| `approval.decide` | write | accepted \| typed rejection (see approvals.md §Decision flow) |
| `runs.list` | read | live runs with state and elapsed |
| `run.stop` | write | accepted; **idempotent, never gated** |
| `device.self` | read | this device's registry entry as the desktop sees it |
| `device.approvalKey.invalidate` | write | accepted; **removes only** — clears this device's approval key and with it its Tier 2. No inverse op exists |
| `push.register` | write | binds an APNs/FCM token to this `device_id` |

Anything not on this list does not exist. Adding an operation is a spec change and a security
review, not an implementation detail.

## Explicitly excluded from v1

Matching MC-001 §6, and enforced by absence rather than by permission checks: no workflow
authoring, no file read or write, no agent configuration, no settings mutation, no runtime
selection, no shell, no arbitrary fetch. An operation that does not exist cannot be misused.

## Envelope

```
request:  { v:1, op, params, nonce, ts }
response: { v:1, ok:true, result } | { v:1, ok:false, error: { code, message } }
event:    { v:1, topic, cursor, payload }
```

- `nonce` is client-generated, unique per device, retained desktop-side for 24 h. Replays are
  rejected, not deduplicated into success.
- Writes are idempotent on `nonce`, so a reconnect mid-send cannot double-post a message or
  double-decide an approval.
- Errors are typed codes (`revoked`, `replay`, `stale_hash`, `tier_forbidden`, `cursor_too_old`,
  `not_pending`, `rate_limited`), never free-form strings the phone parses.

## Server-side enforcement points

Independent of anything the client sends:

1. Device is registered and not revoked — checked per request, not per session, so revocation takes
   effect on the next frame. Checked **before** signature verification, so a revoked device's valid
   assertion is still a rejection.
2. Tier permits the operation for a phone device (`approval.decide` only).
3. `record_hash` matches the current stored record (`approval.decide` only).
4. Nonce is unseen.
5. For Tier 2 (`approval.decide` only): the approval assertion verifies against the approval public
   key registered for this device, over a challenge this desktop issued to this device for this
   record, unspent and unexpired. Missing, malformed or invalid → `tier_forbidden`; never a
   downgrade. See [approvals.md](approvals.md) §Decision flow and
   [device-approval-key.md](device-approval-key.md).
6. Rate limits per device on writes.
7. Every `approval.decide` outcome — accepted **and** rejected — is written to the audit log.

Approval-key **registration** is deliberately not in the operation table: it happens only inside the
pairing ceremony ([pairing.md](pairing.md)), and the desktop rejects it over an established session
**unconditionally** — not merely for a device that already holds a key. The distinction is
load-bearing, because `device.approvalKey.invalidate` clears exactly that field: gating the
rejection on "already holds a key" would mean an attacker who enrols their own biometric, kills the
key and reports the invalidation arrives at a registry row with no key and a registration path now
open. A re-key op reachable from a paired phone would hand Tier 2 back to whoever holds the unlocked
device, and one reachable *only after invalidation* hands it to the one attacker the mechanism was
built for. See [device-approval-key.md](device-approval-key.md) §2 and §8 gate 2.

`device.approvalKey.invalidate` is on the list because it is the phone's way of surrendering
authority early, and it is safe to accept without proof of intent for one reason only: it strictly
subtracts. That property holds only while no operation, and no state reachable from it, can add
authority back.

## Binding to the phone client

`../contracts/api.ts` is the single source of truth for operation names and payload types, shared by
the phone client and the desktop service. A drift between the two should be a type error, not a
runtime surprise.
