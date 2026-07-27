# `hive-runtime`

**Orchestration over [`hive-core`](../hive-core/README.md).** Everything that
touches the outside world — disk, network, model providers, subprocesses — lives
here. Where `hive-core` is pure domain logic, this is the layer the desktop
[`app/`](../../app/README.md) and the [`hive` CLI](../hive-cli/README.md) drive.

## Key modules

- **Event store** (`event_store.rs`) — the durable **SQLite** log of signed
  events; the identity store (`identity_store.rs`) keeps keys/certs on disk.
- **`ChatService`** (`chat_service.rs`) — the orchestrator: create/load chats,
  post turns, resolve pending mentions and workspace credentials.
- **Sync** (`sync_engine.rs`, `relay_client.rs`, `peer.rs`, `peer_iroh.rs`) — the
  sync engine plus the relay client and P2P (iroh) transport; envelope
  verification and quarantine live in `envelope_verifier.rs`.
- **Providers** (`provider/`) — adapters for Anthropic, OpenAI-compatible
  endpoints, the `claude` CLI, and generic subprocess agents, behind
  `provider/dispatch.rs`.
- **Agent loop** — `tool_loop.rs` (model ↔ tool_use ↔ tool_result), `mcp.rs` /
  `mcp_oauth.rs` (Model Context Protocol), `prompt.rs` (system-prompt assembly),
  `context.rs` (windowing/compaction), `mentions.rs`, and `directives.rs`.
- **Support** — `vault_fetcher.rs` (GitHub/GitLab/HTTPS knowledge sources),
  `git_attribution.rs`, `github.rs`, `remote_manifest.rs`.

## Notes

- Depends on `hive-core`; the desktop app and CLI depend on this crate.
- Uses [`hive-relay`](../hive-relay/README.md) as a **dev-dependency** in-process
  fixture for sync tests — the production relay is the separate Go repo.
