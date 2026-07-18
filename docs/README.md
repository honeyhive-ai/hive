# Hive developer & maintainer docs

Docs for people working **on** Hive (architecture, internals, contributing).
If you're looking for how to **use** or **self-host** Hive, that's the user site
built from [`docs-site/`](../docs-site/) (the published MkDocs site).

> **Audience split.** `docs-site/` = usage + relay deployment (users/admins).
> `docs/` (here) = implementation, internals, release engineering (contributors).
> Rule of thumb: *"how do I use/run/deploy it"* → user site; *"how is it built /
> how do I change it"* → here.

## Start here

| Doc | What it covers |
|-----|----------------|
| [architecture.md](architecture.md) | Crate layout, event sourcing + projector, Tauri IPC + ts-rs, provider dispatch. |
| [multiuser.md](multiuser.md) | Relay-forwarded sync, E2EE, testing multiuser across machines. |
| [relay-deploy.md](relay-deploy.md) | Cloud-hosting the `hive-relay` binary. |
| [packaging.md](packaging.md) | Bundling + signing/notarization per OS. |
| [hive-server-side-membership.md](../docs-site/ops/server-side-membership.md) | Server-enforced workspace membership (the `WriteGuard` seam + the private enterprise crate). |

## Product & tiers

| Doc | What it covers |
|-----|----------------|
| [tiering.md](../docs-site/ops/tiering.md) | OSS core vs paid/enterprise — how features gate (entitlements vs build) and where they live. |
| [managed-service-plan.md](../docs-site/ops/managed-service.md) | GitHub-account identity, hosted relay, signed entitlement tokens, RBAC/GHEC roadmap. |
| [hive-social-graph-plan.md](hive-social-graph-plan.md) | Add-friend-by-GitHub-username, presence, P2P bootstrap; GitHub Teams for Enterprise; the prosumer paid tier. |
| [hive-issue-trackers.md](hive-issue-trackers.md) | Per-workspace issue/ticket integration via MCP (GitHub Issues/PRs, Linear, …). |

## Design intent & internals

Deeper design references — useful context for extending Hive; when one conflicts
with the code, the code wins.

- Agent model: [`hive-agent-architecture-matrix.md`](hive-agent-architecture-matrix.md) — agent modes, execution/state/tool/permission shape.
- Relay protocol: [`hive-relay-api.md`](hive-relay-api.md) — the `/v1` wire contract (client ↔ relay).
- Branding: [`hive-logo-system.md`](hive-logo-system.md) — logo/brand assets.

The user-facing tiering / managed-service / pricing docs live on the published
site under [`docs-site/ops/`](../docs-site/ops/).

## Building & running

See [CONTRIBUTING.md](../CONTRIBUTING.md) for the quick build/test loop. In short:

```bash
cargo test --workspace            # Rust
cd web && bun run build           # frontend typecheck + build
cargo tauri dev                   # run the app
cargo run -p hive-relay           # run the relay
```
