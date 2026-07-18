# Hive Relay API

This document defines the client-side relay contract used by `RelayTransport`.

The relay is intentionally a dumb, ciphertext-only coordination service:
- it stores opaque encrypted envelopes
- it stores per-version sealed workspace-key snapshots
- it stores ephemeral presence
- it never needs plaintext session payloads

## Base URL

Relay clients are configured with a single endpoint, for example:

```toml
[transport]
kind = "relay"

[transport.relay]
endpoint = "wss://relay.hive.example/v1"
account_token_env = "HIVE_RELAY_TOKEN"
```

`RelayTransport` talks to the relay over **REST** at `https://.../v1`. The
relay has no streaming/WebSocket surface; clients discover new envelopes by
polling `GET .../envelopes?after=`.

## Auth

The relay uses **two independent** credentials, on **separate headers**, so a
gated/paid relay and a directory identity never collide:

```http
Authorization: Bearer <relay-access-token>      # entitlement: may I connect?
X-Hive-Github-Token: <github-oauth-token>        # identity: who am I (directory)?
```

### Entitlement (`Authorization: Bearer`)

Controls *whether a client may use the relay at all*. The server's policy comes
from the `HIVE_RELAY_ACCESS_TOKENS` env var:

- **unset / empty ⇒ `Open`** — every request is admitted (self-host default).
- **comma-separated list ⇒ `Tokens`** — only requests whose bearer is in the set
  are admitted; everything except `GET /v1/health` is behind a
  `require_entitlement` middleware that returns `401`/`403` otherwise.

The desktop client sends this from `relay_access_token` (Settings → Team sync, or
`HIVE_RELAY_ACCESS_TOKEN`); `RelayClient::with_auth(token)` attaches it to every
request. It is intentionally transport-agnostic — a billing system issues/revokes
tokens without changing the relay surface.

The relay's `EntitlementPolicy` resolves from the environment, most specific
first:

1. **`HIVE_RELAY_TOKEN_PUBKEY`** (hex/base64 Ed25519 public key) ⇒ **signed
   tokens**. The bearer is a compact `hrt1.<b64url(claims)>.<b64url(sig)>` minted
   by a billing/license backend holding the matching private key; the relay
   *verifies but never mints*. Claims carry `sub`, `plan`, `exp`, `max_members`,
   `retention_days`, `turn`, and RBAC `caps`. Verified claims are stashed in the
   request extensions for handlers to enforce per-plan limits. See
   `crates/hive-relay/src/token.rs` and
   [`managed-service`](../docs-site/ops/managed-service.md).
2. **`HIVE_RELAY_ACCESS_TOKENS`** (comma-separated) ⇒ a static **allowlist**
   (coarse on/off).
3. otherwise ⇒ **open** (self-host default).

### Identity (`X-Hive-Github-Token`)

Used **only** by the directory endpoints (register / lookup), where the relay
calls GitHub `/user` to bind a request to a GitHub account. It rides its own
header so it doesn't consume `Authorization` (which carries the entitlement). For
backward compatibility the relay falls back to the bearer if the header is
absent.

## Admin APIs (user/token management)

`/v1/admin/*` manages durable relay access users + tokens. These routes bypass
the entitlement gate and are instead authorized by an **admin authorizer** seam
(the reference relay leaves it disabled → `404`; a downstream build enables it,
e.g. gated on a GitHub-admin allowlist authenticated by `x-hive-github-token`).
Tokens are stored only as SHA-256 hashes; the raw value is returned once at
issue time.

| Method + path | Body | Returns |
|---|---|---|
| `POST /v1/admin/users` | `{"name","login?","label?"}` | `{user, token, raw}` — `raw` shown once |
| `GET /v1/admin/users` | — | `[{…user, tokens:[…]}]` (no hashes) |
| `POST /v1/admin/users/{id}/tokens` | `{"label?"}` | `{user, token, raw}` |
| `POST /v1/admin/users/{id}/disabled` | `{"disabled": bool}` | `{"ok":true}` |
| `DELETE /v1/admin/tokens/{id}` | — | `204` |

A token issued here resolves to entitlement claims (`sub` = the user, `plan` =
`team`); revoking a token or disabling its user takes effect immediately.

## Envelope APIs

### POST `/v1/workspaces/{workspaceID}/envelopes`

Appends one or more sealed envelopes.

Request body:

```json
{
  "envelopes": [
    {
      "workspaceID": "UUID",
      "sequence": 42,
      "signerDeviceID": "UUID",
      "signature": "base64",
      "keyVersion": 3,
      "nonce": "base64",
      "ciphertext": "base64"
    }
  ]
}
```

Response:
- `204 No Content` or `200 OK`

### GET `/v1/workspaces/{workspaceID}/envelopes?after={sequence}&limit={limit}`

Returns sealed envelopes after the provided sequence. This poll is the **only**
mechanism for discovering new envelopes — clients call it on an interval with
the last sequence they've seen.

Response body:

```json
{
  "envelopes": [
    {
      "workspaceID": "UUID",
      "sequence": 42,
      "signerDeviceID": "UUID",
      "signature": "base64",
      "keyVersion": 3,
      "nonce": "base64",
      "ciphertext": "base64"
    }
  ]
}
```

Servers may also return a bare array of envelopes; the current client accepts both forms.

## Workspace-key rotation APIs

These endpoints exist so a second client can import its sealed workspace-key material for a specific `keyVersion` before attempting to decrypt a relayed envelope tagged with that version.

### POST `/v1/workspaces/{workspaceID}/keyring`

Publishes a `WorkspaceKeyRotationSnapshot` to the workspace's keyring.

Request body:

```json
{
  "workspaceID": "UUID",
  "keyVersion": 3,
  "sealedKeysByDeviceID": {
    "DEVICE_UUID": {
      "keyVersion": 3,
      "createdAt": "2026-05-27T23:00:00Z",
      "ciphersuite": "Curve25519_SHA256_ChachaPoly",
      "encapsulatedKey": "base64",
      "ciphertext": "base64",
      "recipientPublicKey": "base64"
    }
  },
  "rotatedAt": "2026-05-27T23:00:00Z"
}
```

Response:
- `204 No Content` or `200 OK`

### GET `/v1/workspaces/{workspaceID}/keyring`

Returns **all** sealed workspace-key snapshots published to the workspace's
keyring. Clients pick the newest version they can decrypt for their device;
there is no per-version subpath.

Response:
- `200 OK` with an array of `WorkspaceKeyRotationSnapshot`

## Presence APIs

### POST `/v1/workspaces/{workspaceID}/presence`

Upserts the caller's current presence.

Request body:
- one `WorkspacePresence`

### GET `/v1/workspaces/{workspaceID}/presence`

Returns the current presence set.

Response body:

```json
{
  "presence": [
    {
      "actor": { "id": "alice", "displayName": "Alice", "kind": "human" },
      "client": { "id": "mac-client", "platform": "macOS", "deviceName": "Alice Mac" },
      "workspaceRole": "owner",
      "activeSessionID": "UUID",
      "activeRunID": "UUID",
      "reviewingProposalID": "UUID",
      "activePane": "Review",
      "state": "active",
      "lastSeenAt": "2026-05-27T23:00:00Z"
    }
  ]
}
```

Servers may also return a bare array of presence entries; the current client accepts both forms.

## Invariants

- The relay must not require or inspect plaintext `SessionEvent` payloads.
- `keyVersion` on `SealedSessionEventEnvelope` is authoritative for decryption.
- Clients are responsible for signature verification after decrypting the inner envelope.
- Relay storage may be self-hosted; nothing in this contract assumes an Apple-only client.
