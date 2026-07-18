# Server-side workspace membership

Status: **first slice built** (relay enforcement + admin API + tests). Client
wiring is the next step.

> **Lives in the private `hive-relay-enterprise` crate** (repo
> `honeyhive-ai/hive-relay-enterprise`), **not in this OSS repo**. The open
> `hive-relay` keeps only a generic [`WriteGuard`] extension seam (a hook that
> does nothing unless a guard is set) — no membership code, no `/members` routes.
> The enterprise crate depends on the OSS relay and installs a guard that
> enforces membership; it is a *superset build*, never a fork. So the paid
> controls aren't merely compiled out of the open binary — their **source isn't
> in the public repo at all**. See [`tiering.md`](tiering.md).

## Why this is the load-bearing piece

The relay is content-blind: it forwards opaque (E2EE) envelopes. Until now,
membership lived only in the client — the authorization evaluator ran in the
client *before* it persisted an event, and the relay accepted any envelope POST
to any room. So "removing a member" only meant **re-keying** (they can't *read*
new traffic), but a modified client could keep **writing** ciphertext into the
room, and role checks were advisory.

Member caps, capability RBAC, and "eject a bad actor" only become real once the
**relay itself** authenticates the caller and enforces who may write. That's what
this adds.

## Design (as built)

**Authentication — cached GitHub identity.** The relay authenticates a caller as
a GitHub account and caches the verification, so GitHub `/user` is hit at most
once per token per 10-minute window (`SESSION_TTL_SECS`), not on every write:

```text
write request carries:  X-Hive-Github-Token: <gh token>
  cache hit  -> account = "github:<id>"        (cheap)
  cache miss -> verify via GitHub /user, cache, then account
```

`account_id = "github:<numeric id>"` — stable across GitHub renames; the client
can compute the same string from its own id. (`Authorization: Bearer` stays
reserved for the relay *entitlement* token; identity rides `X-Hive-Github-Token`,
as the directory already did.)

**Membership is opt-in per workspace.** A workspace with no membership record is
*unmanaged* and behaves exactly as before (open / self-host). The first
authenticated **claim** makes the caller `Owner` and turns enforcement on:

```text
POST /v1/workspaces/:id/members/claim      -> caller becomes Owner (409 if claimed)
GET  /v1/workspaces/:id/members            -> list (must be a member)
POST /v1/workspaces/:id/members            -> add / set role  {account, login, role}  (Admin+)
DELETE /v1/workspaces/:id/members/:account -> remove                                   (Admin+)
```

**Roles** mirror `hive-core::authorization`: `Viewer < Contributor < Admin <
Owner`. `Contributor`+ may write; `Admin`+ may manage membership; only an `Owner`
may grant/affect `Owner`, and the last `Owner` can't be removed or demoted.

**Enforcement (writes + admin first; reads unchanged).** For a *managed*
workspace, these `POST`s require a member with write capability — `403` otherwise,
`401` if unauthenticated:

- `POST …/envelopes`, `POST …/keyring`, `POST …/presence`, `POST …/candidates`

`GET …/envelopes` is **unchanged** — a non-member only ever sees ciphertext, so
read-gating is deferred (it would break open-read + new-device backfill). The
admin endpoints enforce `Admin`+ via the role rules above.

**Eject-a-bad-actor = remove + rotate.** `DELETE …/members/:account` stops their
*writes* immediately; pair it with a workspace-key rotation (already built) so
they also lose *read* access to new traffic. Both together = full removal.

## Where it lives

**OSS (`crates/hive-relay`, this repo):** only the seam — a public `WriteGuard`
trait + `RelayState::with_write_guard`, and `enforce_write` which delegates to the
guard (or no-ops when none is set). No membership logic. `cargo test -p hive-relay`
= 14 tests (incl. the seam: no-guard allows writes, a guard can reject them).

**Private (`honeyhive-ai/hive-relay-enterprise`):**
- `src/membership.rs` — roles + guarded `claim`/`upsert`/`remove`.
- `src/lib.rs` — `EnterpriseState` implements `hive_relay::WriteGuard`
  (`caller_account` cached GitHub auth → role write-enforcement), the `/members`
  admin API, and `router()` composing the OSS relay with the guard installed.
- `cargo test` = 11 (membership logic + enforcement: unmanaged stays open,
  managed requires identity, viewer denied, non-member denied, removed member
  loses write).

## How it composes with entitlement tokens

Two orthogonal checks, on two headers:

| Check | Header | Question |
|---|---|---|
| **Entitlement** | `Authorization: Bearer` | may this client use the relay at all? (plan/caps — see [signed tokens](managed-service.md)) |
| **Membership** | `X-Hive-Github-Token` | is this account a member of *this workspace*, with what role? |

A signed entitlement token's `caps` will later refine membership actions (e.g.
require a `remove_member` capability), and `max_members` will cap the membership
table — see [`managed-service-plan.md`](managed-service.md).

## Next steps

1. **Client wiring** — send `X-Hive-Github-Token` on writes; call `claim` when
   creating a team workspace; surface add/remove/role in the People rail (the
   IPC + UI already exist client-side — point them at the relay endpoints).
2. **Enforce `max_members`** from the entitlement token against the membership
   table.
3. **Durable membership** — persist alongside the (planned) durable event store;
   today it's in-memory like the rest of the reference relay.
4. **Session pruning** + optional `/v1/session` warm endpoint that returns the
   caller's `account_id` to the client.
5. **Capability gating** — once `caps` ship, gate privileged ops on them rather
   than role rank alone.
