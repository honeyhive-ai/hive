# `app` — Hive desktop shell

The **[Tauri v2](https://tauri.app) desktop app**: a thin Rust shell that owns
the [`hive-runtime`](../crates/hive-runtime/README.md) services and bundles the
[`web/`](../web/README.md) UI into one native binary for macOS, Linux, and
Windows.

## Structure

- `src/lib.rs` — the shell. Constructs `AppState` (the runtime services:
  `ChatService`, `EventStore`, identity, MCP registry, providers), registers the
  **IPC commands** the frontend calls, and pushes streaming chat updates back as
  **Tauri events**.
- `src/workflows.rs` — IPC surface for the workflow engine (definitions/runs).
- `src/main.rs` — binary entry point.
- `tauri.conf.json`, `capabilities/`, `icons/`, `build.rs` — Tauri bundling,
  permission capabilities, app icons, and build glue.

## How IPC maps to the frontend

The frontend never talks to the runtime directly. It **invokes IPC commands**
(via `web/src/lib/ipc.ts`) whose request/response types are the DTOs in
[`hive-proto`](../crates/hive-proto/README.md); streaming updates (chat tokens,
sync events) arrive as **emitted Tauri events** the UI subscribes to. Keep the
Rust command signatures and the `hive-proto` DTOs in step — the generated
`web/src/bindings/` are the shared source of truth.

## Run

```bash
cargo tauri dev        # frontend + backend, from the repo root
```
