# Contributing to Hive

Hive is a **Tauri v2** app: a Rust backend with a React/TypeScript frontend, plus
a small relay. This is the quick orientation for contributors — deeper docs live
in [`docs/`](docs/README.md).

## Layout

```
crates/
  hive-core      pure domain: models, config, crypto, events/projector, authz
  hive-runtime   IO/orchestration: SQLite event store, providers, MCP, sync
  hive-proto     IPC DTOs shared with the frontend (ts-rs generates web/src/bindings)
  hive-relay     the axum relay (envelope forwarding)
app/       the Tauri shell: commands/events, app wiring
web/             React + TypeScript frontend (Vite + Bun)
docs/            developer/maintainer docs (this audience)
docs-site/       the published user docs (MkDocs Material)
```

## Build & test

```bash
cargo test --workspace          # Rust crates + the Tauri app
cd web && bun install           # first time
cd web && bun run build         # frontend typecheck (tsc) + build
cargo tauri dev                 # run the app (frontend + backend)
cargo run -p hive-relay         # run the relay locally (:8443)
```

## IPC type bindings (ts-rs)

DTOs in `hive-proto` generate the TypeScript bindings in `web/src/bindings/`.
After changing a DTO, regenerate and commit them:

```bash
cargo test -p hive-proto export_bindings
```

CI fails if `web/src/bindings/` is out of sync with the Rust types.

## Conventions

- Match the surrounding code's style, naming, and comment density.
- Keep `hive-core` pure (no IO); IO/orchestration belongs in `hive-runtime`.
- Events are append-only and signed; add new behavior as `SessionEvent` variants
  with projector + authorization coverage (see [`docs/architecture.md`](docs/architecture.md)).

## Docs

- User-facing changes → update [`docs-site/`](docs-site/) (the published site).
- Internals / architecture / release → update [`docs/`](docs/README.md).

See [`docs/README.md`](docs/README.md) for the full developer doc map.
