# Tiering: open-source core, paid, and enterprise

How Hive's free / paid / enterprise features are drawn, **where each feature
lives**, and how it ships. This is the engineering model; the user-facing tier
summary is [`docs-site/ops/pricing.md`](pricing.md).

## Principles

1. **One client build for everyone.** The desktop app is a single OSS (MIT)
   binary. We do **not** fork the client per tier — that fragments testing,
   trust, and the install story.
2. **The relay is the wedge.** Most paid value is operational and server-side
   (managed hosting, org auth, retention, audit, residency). It attaches to
   *which relay/org a client connects to*, not to a different client.
3. **Gate at runtime, not at compile time** — wherever possible. Premium
   client-side UI unlocks from an **entitlement** the client fetches; moving
   tiers never requires a reinstall.
4. **What's free is free forever.** The client + the reference relay + the
   protocol specs stay fully functional standalone. Upper tiers add managed
   value, never remove core function (no crippleware).

## The layers

```
OSS client (1 build, MIT) ──connects to──▶ reference relay (OSS)      self-host, free
                                           Hive Cloud relay (managed) free-tier / paid
                                           Enterprise relay (on-prem) licensed
        entitlement/license unlocks any client-side premium UI at runtime
```

| Tier | Who runs the relay | Client | What's added |
|------|--------------------|--------|--------------|
| **Self-host** | you | OSS | nothing — full core |
| **Cloud (free)** | Hive | OSS | hosted relay, rate-limited, no SLA |
| **Cloud (paid)** | Hive | OSS + entitlement | SLA, retention, org auth (SSO/SAML/OIDC), seats |
| **Enterprise** | you (on-prem) | OSS + entitlement | enterprise relay: SSO, audit export, DLP hooks, data residency, support |

## Where a feature lives — the decision tree

For any new paid/enterprise feature, place it in the first bucket that fits:

1. **Server/relay capability (preferred).** Does it concern hosting, multi-tenant
   auth, retention, audit, DLP, or residency? → it lives in the **relay/cloud**,
   and the OSS client needs *zero changes* to benefit. Most enterprise asks land
   here.
2. **Runtime-gated client UI.** Does it need client UI but no proprietary code
   (e.g. an org dashboard, seat management surface)? → ship it in the OSS client,
   **hidden behind an entitlement** the client reads at runtime. Default
   entitlement = free.
3. **Compile-time feature (last resort).** Is the code itself proprietary or
   contractually un-shippable (a vendor SSO/DLP SDK)? → put it behind a Cargo
   `--features enterprise` flag, and build it into the **enterprise *relay*
   binary**, not the client.

### Examples

| Feature | Bucket | Lives in |
|---|---|---|
| Managed hosting, SLA | server | Cloud relay (closed deploy) |
| Retention / history beyond memory | server | relay storage backend |
| Signed entitlement tokens (plan + caps) | server | `hive-relay` (built — `token.rs`) |
| Server-enforced workspace membership/roles | server | private `hive-relay-enterprise` crate via the OSS `WriteGuard` seam (source absent from OSS) |
| RBAC capabilities + custom roles | server (+ runtime UI) | gated/enterprise relay; editor in OSS client, gated |
| GHEC SSO + org/team → role mapping | server (+ proprietary connectors) | enterprise relay, `--features enterprise` |
| Add-collaborator by GitHub username + presence (hosted graph) | server | Cloud relay — free-capped / prosumer paid; see [`hive-social-graph-plan.md`](https://github.com/honeyhive-ai/hive/blob/main/docs/hive-social-graph-plan.md) |
| SSO / SAML / OIDC | server (+ proprietary connectors) | enterprise relay, `--features enterprise` |
| Audit-log export (S3/SIEM) | server | enterprise relay |
| DLP scan on forwarded frames | server | enterprise relay hook |
| Data-residency guarantees | server/ops | where the relay runs |
| Org/seat management UI | runtime entitlement | OSS client, gated |
| Per-tool policy templates for orgs | runtime entitlement | OSS client, gated |
| Issue/PR tracker connectors (GitHub/Linear/Jira) | free core | OSS client (MCP) |
| Governance over agent writes to a tracker (audit, policy) | server | gated/enterprise relay |
| The desktop app, BYO runtimes, MCP | free core | OSS client |
| Reference relay (forwarding + E2EE) | free core | `crates/hive-relay` |

> **Enterprise = full RBAC + GHEC, on-prem.** The Enterprise tier is the
> fully-RBAC profile: custom capability-based roles, GitHub Enterprise Cloud SSO
> with org/team→role mapping, audit export, run on the customer's own
> hardware. It verifies a Hive-signed license **offline** via the public-key
> token scheme (`HIVE_RELAY_TOKEN_PUBKEY`) — no callback to Hive. Design detail
> in [`managed-service-plan.md`](managed-service.md).

## The relay entitlement gate (built)

The **server-side** half of the wedge exists as of 2026-06-15. `hive-relay`'s
`EntitlementPolicy` admits connections based on a bearer token, not a hidden URL:

- `HIVE_RELAY_ACCESS_TOKENS` **unset/empty ⇒ `Open`** — the self-host default;
  anyone with the URL + room may connect (unchanged behavior).
- `HIVE_RELAY_ACCESS_TOKENS=tokA,tokB ⇒ `Tokens`** — a `require_entitlement`
  middleware admits only requests whose `Authorization: Bearer` is in the set.
  `/v1/health` stays ungated.

The client carries the token in **Settings → Team sync → Relay access token**
(or `HIVE_RELAY_ACCESS_TOKEN`); `RelayClient::with_auth` attaches it to every
request. This is the seam a billing system drives: **Stripe issues/revokes
tokens**, the relay just checks set membership. The token is a coarse on/off
today; the next step is a *signed* token carrying the plan's limits (member cap,
retention, TURN flag) for the relay to enforce — see
[`managed-service-plan.md`](managed-service.md) (P4 / RBAC).

> Note: this gate controls *connection*. It does **not** yet enforce per-workspace
> *membership/roles* server-side — that's the load-bearing item for a paid admin
> tier, tracked in the managed-service plan.

## The entitlement seam (client-side; design, not yet built)

A single place the client asks *"what am I allowed to show?"*:

- The client resolves an **entitlement set** (capability flags) from one of:
  the connected managed relay (returns the org's plan), a signed license key, or
  — by default — the **built-in OSS default = everything-free-enabled**.
- Premium UI checks a capability flag, never a build constant. Absent/free
  entitlement → the feature is simply hidden; nothing is disabled or nagged.
- This keeps offline/self-host fully functional (the OSS default grants the full
  free feature set with no network check).

Implementation note for when we build it: put the resolver in `hive-runtime`
behind a small `Entitlements` type with an OSS default, expose it over IPC, and
have the React layer gate on it. No client code path should *require* a remote
check to function.

## Should we split the codebase?

**Not yet — and probably never for the client.** Recommendation:

- **Keep the OSS client + reference relay in this (public) monorepo.** They are
  the free product; splitting them buys nothing and costs contributor clarity.
- **When the first truly-proprietary code appears** (a vendor SSO/DLP SDK we
  can't open-source), put **only that** in a separate **private** repo — an
  `hive-relay-enterprise` crate that depends on the OSS `hive-core`/`hive-relay`
  as published crates and adds the closed connectors behind `--features
  enterprise`. The OSS relay stays the default; the enterprise relay is a
  superset build.
- **Do not** create an "open-source edition" by stripping the client. The client
  is already the free edition; entitlements (above) handle the rest without a
  second build or a code fork.

Trigger for action: the day a contract requires shipping code we can't license
under MIT. Until then, this repo *is* the open-source free version, and the
tiering is entirely a runtime/deployment concern.

## What stays free forever

- The desktop client (all OSes).
- The reference relay (`crates/hive-relay`, MIT) — forwarding + E2EE.
- The protocol/wire specs.

A team can run its own relay and never touch a paid tier.
