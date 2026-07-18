# Packaging & release (Rust + Tauri)

Cross-platform bundles are produced by the Tauri bundler. `app/tauri.conf.json`
has `bundle.active = true` and `targets = "all"`, so a release build emits the native
installer for the host OS.

## Build

```bash
cd web && bun install && cd ..  # once
cargo tauri build               # from repo root: builds web/ (Bun hook) then bundles
```

Per-OS artifacts:

| OS      | Artifacts                          | Signing |
|---------|------------------------------------|---------|
| macOS   | `.app`, `.dmg`                     | Developer ID + **notarization** |
| Windows | `.msi`, `.exe` (NSIS)              | **Authenticode** code signing |
| Linux   | `.deb`, `.AppImage`, `.rpm`        | (optional) GPG / repo signing |

## Signing (requires real credentials — not run in CI by default)

- **macOS** — set `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
  `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`; Tauri signs
  and notarizes during `tauri build`.
- **Windows** — provide a code-signing cert; configure `bundle.windows.certificateThumbprint`
  (or sign the produced `.msi`/`.exe` via `signtool`).
- **Linux** — unsigned AppImage/deb are usable; sign repos/packages as needed.

Tauri's built-in **updater** can be enabled later with an update signing keypair
(`tauri signer generate`) and a release feed.

## CI

`.github/workflows/rust-tauri.yml` runs `cargo build -p hive-app --release` on the
macOS/Windows/Linux matrix (validates the bundle compiles). Wiring real signing +
attaching artifacts to GitHub Releases is gated on the secrets above and is a release-
engineering step, not part of PR CI.

## Bundle-size note

The frontend bundle is large because Monaco ships every language worker. Before a
production release, trim Monaco to the languages actually used (diff/markdown/json/
plaintext) via `monaco-editor/esm` feature imports or a Vite `manualChunks` split.

## Relay

The relay deploys independently of the desktop app and lives in its own repo,
[github.com/honeyhive-ai/relay](https://github.com/honeyhive-ai/relay)
(`docker build -t hive-relay https://github.com/honeyhive-ai/relay.git`), bind via
`HIVE_RELAY_ADDR`. See that repo's README + `deploy/fly.toml` for hosting.
