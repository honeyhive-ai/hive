# `hive-core`

The pure, platform-agnostic **domain core**. No IO — all orchestration (SQLite,
network, providers, MCP) lives in [`hive-runtime`](../hive-runtime/README.md).
This crate is the source of truth that everything else folds and drives.

## What it holds

- **Event sourcing** (`events.rs`) — `SessionEvent` / `SessionEventEnvelope` and
  the deterministic **projector** (`project`, `project_with_founder`) that folds
  the append-only, signed event log into current state. Same events in ⇒ same
  state out, on every device, which is what makes sync converge.
- **Session state** (`session.rs`, `chat.rs`) — `ChatSession` and the transcript
  types (messages, tool calls/results, reactions) the projector produces.
- **Authorization** (`authorization.rs`, `policy.rs`) — the governance rules
  (`WorkspaceRole` = Owner/Admin/Contributor/Viewer) enforced both at authoring
  and, critically, again **on ingest during projection**, so a forged but
  validly-signed governance event is dropped everywhere.
- **E2EE & crypto** (`e2ee.rs`, `crypto.rs`) — workspace-key generation/derivation,
  sealed envelopes (ChaCha20-Poly1305), signing keypairs, and device
  certificates/identities.
- **Domain types** — proposals (`proposals.rs`), workflows (`workflow.rs`),
  skills (`skills.rs`), vaults (`vault.rs`), channels (`channel.rs`), agents
  (`agent.rs`), runtimes (`runtime.rs`), workspace hosts/runtimes, schedules, and
  identity.
- **Support** — config, context budgeting (`context_budget.rs`), and git context
  reading (`git_context.rs`).

## Notes

- Keep this crate **IO-free**. New behavior is added as `SessionEvent` variants
  with matching **projector** and **authorization** coverage — see
  [`docs/architecture.md`](../../docs/architecture.md) and
  [`CONTRIBUTING.md`](../../CONTRIBUTING.md).
