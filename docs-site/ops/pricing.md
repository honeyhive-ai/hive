# Tiers

Hive's tiers are distinguished by **which relay you connect to**, not by which
app you run. The client is the same OSS binary everywhere; moving between tiers
is a connection change in **Settings → Multiuser sync** (and, for paid tiers, an
entitlement on your org).

| Tier | Who runs the relay | Cost | Notes |
|------|--------------------|------|-------|
| **Self-host** | you, on your hardware | free (MIT) | full core function |
| **Hive Cloud — Free** | Hive-operated | free | 1 team, ≤5 members, 30-day retention, best-effort forwarding |
| **Hive Cloud — Pro** | Hive-operated | ~$6/mo | more members, 1-year retention, guaranteed TURN |
| **Hive Cloud — Team** | Hive-operated | ~$4/user/mo | unlimited members, admin controls + audit, org/seat management |
| **Enterprise** | you, on-prem | site license | SSO, audit export, DLP hooks, data residency, support |

## Self-host (free, MIT)

Run `hive-relay` yourself — the full source is in this repo. See
[Self-hosting a relay](../networking/self-host.md) and the
[small-team deployment guide](deployment.md).

## Hive Cloud

A managed, multi-tenant relay. Same protocol as self-host — the client doesn't
change; you just connect to a Hive-operated relay (and, for paid plans, paste a
**relay access token** in Settings → Team sync).

### What the relay actually does (why it's cheap)

The relay is **sync + broker only**. It forwards end-to-end-encrypted event
envelopes, brokers short pairing codes, holds sealed key-rotations, and resolves
GitHub-handle → device lookups. **LLM inference never goes through the relay** —
the client talks to your model provider directly. So Hive Cloud has *no token
cost*; pricing reflects convenience and scale, not compute.

The only resources that scale with use are **relay-forwarded bandwidth when peers
can't connect directly (TURN fallback)** and **history retention** (how far back
the relay can replay events to a brand-new device). Plans meter those.

### Plans

| | **Free** | **Pro** | **Team** |
|---|---|---|---|
| Price | $0 | ~$6/mo flat | ~$4/user/mo |
| Hosted team workspaces | 1 | a few | unlimited |
| Members per team | up to 5 | up to ~15 | unlimited |
| Devices per account | unlimited | unlimited | unlimited |
| Backfill retention | 30 days | 1 year | unlimited |
| Invite by GitHub `@handle` | ✓ | ✓ | ✓ |
| Direct P2P (always free) | ✓ | ✓ | ✓ |
| Guaranteed TURN forwarding | best-effort | ✓ | ✓ |
| Team admin controls + audit | — | — | ✓ |
| Org/seat management, policy templates | — | — | ✓ |

> **Solo / local workspaces are always free and unlimited** — they never touch a
> relay. Retention limits only affect how far back the *relay* can replay to a
> *new* device; every existing device keeps its full local history forever.
>
> **We don't meter devices.** Using the same account on your laptop and your
> desktop is core to how Hive identity works, not an upsell.

Numbers above are indicative, not a committed price sheet. The Team tier's admin
controls (fine-grained roles, server-enforced membership, audit trail) are the
main paid surface and are still being built — see the
[managed-service plan](managed-service.md).

## Enterprise

An on-prem relay with **full role-based access control** and a support contract.
The value-add sits next to the same protocol — your team runs the same OSS
client.

- **RBAC** — capability-based roles (not just owner/admin/contributor/viewer):
  define custom roles from grants like *invite*, *remove member*, *rotate key*,
  *approve execution*, *manage integrations*, *view audit*.
- **GitHub Enterprise Cloud (GHEC)** — sign in through your GHEC org's SSO
  (SAML/OIDC via Okta / Entra / Google); map GitHub **teams → Hive roles**, so
  provisioning and deprovisioning follow GitHub. GitHub Enterprise **Server**
  (self-hosted) is supported too.
- **Governance for agent actions** — policy over what agents may do to a shared
  tracker (file issues but not close them), gated execution, plus an **audit
  trail** of every membership, key-rotation, and agent-made change, exportable to
  S3/SIEM.
- **On-prem + offline licensing** — the relay verifies a Hive-signed license with
  a public key, with no callback to Hive. Data-residency follows wherever you run
  it. DLP hooks available.

## What stays free forever

- The Hive client (all OSes).
- The reference relay (MIT) — envelope forwarding + end-to-end encryption.
- The protocol specifications.

A team that never touches Hive Cloud can run its own relay forever.

---

*For the engineering model — how each feature gates (server capability vs runtime
entitlement vs build flag) and where it lives — see [tiering](tiering.md).*
