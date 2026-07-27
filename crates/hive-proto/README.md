# `hive-proto`

The **IPC contract** shared between the Rust backend
([`hive-runtime`](../hive-runtime/README.md) / [`app/`](../../app/README.md)) and
the TypeScript frontend ([`web/`](../../web/README.md)).

Every type is a flat, string-keyed, **camelCase** presentation **DTO** that
derives `serde` (for Tauri command (de)serialization) and `ts-rs::TS`. These are
deliberately decoupled from the richer `hive-core` domain types — the backend
converts core values into these DTOs at the IPC boundary.

## Generated TypeScript bindings

The `export_bindings` test writes the matching TypeScript into
[`web/src/bindings/`](../../web/src/bindings/). Regenerate after changing a DTO
and commit the result:

```bash
cargo test -p hive-proto export_bindings
```

CI fails if `web/src/bindings/` drifts from these definitions, so the frontend
can never get out of sync with the backend contract.
