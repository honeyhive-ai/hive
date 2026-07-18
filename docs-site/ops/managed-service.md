# Hive as a managed service (GitHub-account identity)

Status: planning + P1 in progress. This captures the target and the staged path,
including the **one account, many devices** model (e.g. the same person on a
MacBook *and* a Windows desktop).

## Why

Today Hive is local-first: self-sovereign device keys + bring-your-own relay.
That's powerful but the friction is real — every device must agree on a relay
URL, the relay must be up and current, and you pair by codes. A managed service
removes that:

- **Always-on hosted relay** → sync / short codes / revocation "just work", no
  babysitting, no version skew.
- **GitHub identity** → sign in once; invite teammates by `@handle` instead of
  codes; commits an agent makes are attributed to the real person for free.
- **Onboarding** = "Sign in with GitHub" instead of generating keys.

The server stays **content-blind**: it authenticates *who* and brokers
ciphertext, but never holds message keys. E2EE is preserved via the per-device
X25519 sealing + key rotation already built (see `hive-core::e2ee`).

## One account, many devices

The GitHub user is the **account**; each machine is a **device** under it.

- `account_id = UUIDv5(ns, "github:<github_user_id>")` — deterministic, so the
  MacBook and the Windows desktop compute the **same** account id once both sign
  in to the same GitHub account. They show up as **one member**, two devices.
- Each device keeps its **own** keypairs: an Ed25519 signing key (already) and an
  X25519 key-agreement key (`ka_secret`, already). Different `device_id`s.
- The roster/`ActorIdentity` keys off the **account** (`actor.id = account_id`),
  while sealing/rotation targets **every device** of an account:
  - The directory (below) maps `account → [device key-agreement pubkeys]`.
  - When an owner seals/rotates a workspace key, they seal it to **all** of a
    member's devices, so both your Mac and your Windows box can decrypt.
  - `DeviceCertificate` (already) chains each device key to the account key, so
    other members can trust "these devices really belong to that account."

## Phases

- **P0 — Hosted relay (deploy).** Run the relay ([github.com/honeyhive-ai/relay](https://github.com/honeyhive-ai/relay))
  somewhere always on — Docker/Fly per its README (`deploy/fly.toml`). For a
  tailnet, run it as a launchd/systemd service. Bake a default relay URL into the
  app so there's zero config. *(This alone makes short codes/sync reliable.)*
- **P1 — GitHub sign-in (device flow).** Desktop OAuth **device flow** (no client
  secret): show a code, user authorizes at github.com/login/device, app gets a
  token, fetches the profile, and binds the account id + name + email. Each
  device signs in independently and resolves to the same account id.
- **P2 — Directory + invite-by-handle.** Authenticated relay endpoints:
  `register(account, device, signing_pub, ka_pub, device_cert)` and
  `lookup(handle|account) → devices`. Pairing becomes "invite `@handle`" → seal
  the workspace key to all their devices. Short codes stay as a fallback.
- **P3 — Teams as servers.** Server-side workspace membership + roles (owner
  removes → rotate to remaining devices), presence, unread, push.
- **P4 — Productionize.** Multi-tenant isolation, durable storage (relay state is
  in-memory today), authz middleware, rate limits, billing, self-host parity.

### P4 status — relay entitlement gate (done)

The first slice of P4 — distinguishing a **paid/managed** relay from a **free
self-hosted** one — is built (2026-06-15):

- `hive-relay` has an `EntitlementPolicy` (`Open` | `Tokens`). `HIVE_RELAY_ACCESS_TOKENS`
  unset ⇒ `Open` (self-host default, anyone may connect); set (comma-separated)
  ⇒ only requests bearing a listed bearer token are admitted. A `require_entitlement`
  middleware fronts every route except `/v1/health`.
- The client carries a `relay_access_token` (Settings → Team sync, or
  `HIVE_RELAY_ACCESS_TOKEN`); `RelayClient::with_auth` attaches it as
  `Authorization: Bearer` on every call. The GitHub identity token moved to its
  own `X-Hive-Github-Token` header so the two never collide.

**Why tokens, not URL secrecy:** the relay endpoint is not a secret — gating by a
hidden URL is trivially defeated (it leaks in configs, logs, packet traces). The
entitlement token is the actual access control, and self-host stays
zero-friction (no token ⇒ open).

**Still open in P4:** per-token *plans/limits* (member cap, retention window,
TURN-forwarding flag) embedded in a signed token; relay-side enforcement of those
limits; durable storage; Stripe → token issue/revoke; rate limits. See
[`tiering.md`](tiering.md) for the gate-placement model and
[`docs-site/ops/pricing.md`](pricing.md) for the tier shape.

## Fine-grained admin controls (RBAC) — needed for a paid "team management" tier

The owner remove-user + auto re-key flow exists, and four roles
(`Owner`/`Admin`/`Contributor`/`Viewer`) are modeled in `hive-core`
(`WorkspaceRole`) with `add_member` / `remove_member` / `set_member_role`
commands gated by the `AuthorizationEvaluator`. But this is **not yet strong
enough to sell as a paid admin feature**, for one structural reason:

> **Enforcement is client-side.** The authorization evaluator runs in the client
> *before* it persists/forwards an event. The relay is content-blind and does
> **not** enforce membership or roles. Removal today means *re-keying* so an
> ejected member can't *read* new traffic — but a modified client could still
> *write* ciphertext into the room, and role checks are advisory.

For paid "team management" to be real, the gap work is:

1. **Server-side membership enforcement.** *(first slice built — see
   [`hive-server-side-membership.md`](server-side-membership.md).)* The relay
   now authenticates the caller (cached GitHub identity) and **rejects writes**
   (`envelopes`/`keyring`/`presence`/`candidates`) from non-members /
   under-privileged actors on *managed* workspaces; membership is opt-in per
   workspace (`claim` → caller becomes `Owner`), unmanaged workspaces stay open
   for self-host. Admin endpoints (`claim`/list/`upsert`/remove) enforce the
   `hive-core` role rules. **Lives in the private `hive-relay-enterprise` crate**,
   layered on the OSS relay's generic `WriteGuard` seam — the open/reference relay
   contains no membership source at all (just the hook). Still open: client
   wiring, `max_members` enforcement, durable storage, and capability gating. This
   was the load-bearing change; everything else is granularity on top.
2. **Capabilities, not just 4 roles.** Decompose roles into per-capability grants
   so orgs can compose policy: `invite`, `remove_member`, `rotate_key`,
   `manage_agents`, `manage_runtimes`, `approve_execution`, `manage_billing`,
   `view_audit`. Roles become named bundles of capabilities; custom roles are an
   enterprise add-on.
3. **Org layer above workspaces.** Map to GitHub orgs: an **org admin** (distinct
   from a per-workspace **owner**) who can manage seats, default policy, and SSO
   across all the org's workspaces.
4. **Auditable admin actions.** Every membership/role/key-rotation/removal event
   is already a signed envelope; surface them as an **admin audit trail** (and,
   for enterprise, export to S3/SIEM). This is the natural paid surface — admins
   buy *accountability*, not just buttons.
5. **Quorum/role floors for execution.** The review model already has
   `requiredApprovals` / `approvalRoleFloor`; expose org-level policy templates
   (e.g. "edits to `main` need 2 approvals incl. one Admin") behind an
   entitlement.

**Tier placement** (per [`tiering.md`](tiering.md)): membership *enforcement* and
*audit export* are **server/relay** capabilities (bucket 1 — the OSS client needs
no change to benefit). Org/seat/policy *dashboards* are **runtime-entitlement**
client UI (bucket 2 — shipped in the OSS client, hidden unless the connected
managed relay grants the capability). No part of this should fork the client or
gate the free self-host path.

## Keep self-host first-class

Managed (sign in with GitHub, default hosted relay) is the easy path; a
self-hosted relay + key-only identity stays supported for offline/private use.
GitHub is the first identity provider; GitLab/Google/SSO can follow. Teams can
map to GitHub orgs.

## Deploy notes

Cloud (Fly), once you have a Fly account:

```sh
git clone https://github.com/honeyhive-ai/relay && cd relay
brew install flyctl            # or: curl -L https://fly.io/install.sh | sh
fly auth login                 # interactive
fly launch --copy-config --no-deploy   # first time, pick a name (uses deploy/fly.toml)
fly volumes create hive_data --size 1 --region <region>
fly deploy
# → https://<app>.fly.dev  — use that as the relay URL on every device
```

Tailnet (persistent local relay on macOS) — build the relay from
[its repo](https://github.com/honeyhive-ai/relay) and run it under launchd/tmux,
bound to `0.0.0.0:8443` so it's reachable at your Tailscale IP:8443.

## Signed entitlement tokens (built — claims + verification)

The relay's entitlement gate now supports **signed tokens**, not just a static
allowlist, so the relay can read a subscriber's *plan limits and capabilities*
out of the token itself (implemented in `crates/hive-relay/src/token.rs`).

**Asymmetric on purpose.** A billing/license backend holds an **Ed25519 private
key** and mints tokens; the relay is configured with only the **public key**
(`HIVE_RELAY_TOKEN_PUBKEY`, hex or base64) and can *verify but never mint*. This
is also exactly what an **on-prem enterprise relay** needs: it verifies a
Hive-issued license offline, with no callback to Hive.

**Wire format** (compact, three `.`-separated parts):

```text
hrt1.<b64url(claims_json)>.<b64url(ed25519_sig)>
```

The signature covers `hrt1.<b64url(claims_json)>`, so claims can't be edited
without the key. **Claims:**

| Claim | Meaning |
|---|---|
| `sub` | account / org id the token entitles |
| `plan` | `free` \| `pro` \| `team` \| `enterprise` |
| `exp` | unix-seconds expiry (`0` = never); short `exp` + reissue is the revocation story for now |
| `max_members` | member cap the relay should enforce per gated workspace (`null` = unlimited) |
| `retention_days` | backfill window (`null` = unlimited) |
| `turn` | guaranteed TURN-forwarding granted |
| `caps` | RBAC capability strings (below); forward-compatible — the relay ignores ones it doesn't enforce yet |

**Policy resolution** (`EntitlementPolicy::from_env`, most specific first):
`HIVE_RELAY_TOKEN_PUBKEY` ⇒ verify signed tokens → `HIVE_RELAY_ACCESS_TOKENS`
⇒ static allowlist → otherwise **open**. The client sends the token as
`Authorization: Bearer` (Settings → Team sync, or `HIVE_RELAY_ACCESS_TOKEN`).

**Enforcement points** (where each claim bites — `max_members`/`retention`
require the server-side membership work, so they're staged):

1. *Connection* — `require_entitlement` admits only a valid, unexpired token;
   verified claims are stashed in request extensions. **(done)**
2. *Member cap* — reject `keyring` / directory writes that would exceed
   `max_members`. *(needs server-side membership — see RBAC §1.)*
3. *Retention* — the mailbox sweep keeps `retention_days` of envelopes per
   workspace. *(needs durable storage.)*
4. *TURN* — refuse sustained relay-forwarding (vs. direct P2P) unless `turn`.
5. *Capabilities* — once membership is server-enforced, gate privileged ops
   (remove member, rotate key, manage policy) on `caps`.

## Enterprise tier — RBAC + GitHub Enterprise Cloud (GHEC)

The Enterprise tier is the **fully-RBAC** profile, runnable **on-prem** and
integrated with **GHEC** (and GitHub Enterprise Server for air-gapped orgs).

**RBAC (capabilities, not just 4 roles).** Decompose the `Owner`/`Admin`/
`Contributor`/`Viewer` roles into per-capability grants so orgs compose policy:
`invite`, `remove_member`, `rotate_key`, `manage_agents`, `manage_runtimes`,
`approve_execution`, `manage_billing`, `view_audit`, `manage_integrations`.
Roles become named bundles; **custom roles** are the enterprise add-on. The
capabilities ride in the signed token's `caps` (relay-enforced) and/or are
resolved from the org directory at connect time.

**GHEC integration:**

- **SSO / identity.** Reuse the existing GitHub device-flow sign-in, but scope to
  the customer's **GHEC org**: GHEC's SAML/OIDC is the IdP, so access follows the
  org's existing SSO (Okta/Entra/Google) with no separate Hive login. GitHub
  Enterprise **Server** (self-hosted GitHub) is supported by pointing the OAuth
  app + API base URL at the GHES host.
- **Org → team → workspace mapping.** A GHEC **org** maps to a Hive tenant; GHEC
  **teams** map to Hive role bundles (e.g. `@org/eng` → `Contributor`,
  `@org/admins` → `Admin`); membership is derived from GitHub team membership,
  so deprovisioning in GitHub deprovisions in Hive.
- **Org admin vs workspace owner.** A GHEC **org admin** (distinct from a
  per-workspace owner) sets default policy, seat allocation, and SSO across all
  the org's workspaces.
- **Audit + compliance.** Membership / role / key-rotation / removal events are
  already signed envelopes; the enterprise relay exports them to S3/SIEM, and
  (per [`tiering.md`](tiering.md)) any proprietary SSO/DLP connectors live in the
  closed `hive-relay-enterprise` build behind `--features enterprise`, never in
  the OSS client.

**Placement:** SSO, org/team sync, audit export, membership enforcement are
**relay/server** capabilities (the OSS client is unchanged). Custom-role and
policy-template *editors* are runtime-entitlement client UI. See
[`docs-site/ops/pricing.md`](pricing.md) for the tier shape and
[`hive-issue-trackers.md`](https://github.com/honeyhive-ai/hive/blob/main/docs/hive-issue-trackers.md) for the per-workspace issue/PR
integration (also capability-gated).

## GitHub OAuth app (needed for P1 sign-in)

Create a GitHub OAuth App (Settings → Developer settings → OAuth Apps), enable
**Device Flow**, and set its **client ID** as `HIVE_GITHUB_CLIENT_ID` (env) or in
Settings. No client secret is needed for the device flow.
