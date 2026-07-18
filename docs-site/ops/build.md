# Building the dist

Hive is a **Tauri v2** app: a Rust backend (`crates/` + `app/`) with a
React/TypeScript frontend (`web/`). The bundler is the Tauri CLI.

## Prerequisites

- **Rust** (stable) + **Bun** (frontend package manager).
- The Tauri CLI: `cargo install tauri-cli --version "^2"` (or use the dev
  dependency in `web/`).
- Platform toolchains for whatever you bundle (Xcode CLT on macOS; MSVC on
  Windows; GTK/WebKit dev packages on Linux — see below).

## Build the desktop app

From the repo root:

```bash
cargo tauri build
```

This runs the frontend build (`bun run --cwd web build`) and compiles the Rust
app in release, then bundles for the host OS. Output lands under
`target/release/bundle/` (or `target/<triple>/release/bundle/` for a cross
target):

| Host | Bundles |
|------|---------|
| macOS | `macos/Hive.app`, `dmg/Hive_*.dmg` |
| Windows | `nsis/Hive_*-setup.exe` (`.msi` needs the WiX toolset) |
| Linux | `appimage/*.AppImage`, `deb/*.deb`, `rpm/*.rpm` |

For a fast dev loop instead of a bundle: `cargo tauri dev`.

## Cross-building from one machine

You can produce most targets from a single Mac:

- **macOS Intel** (from Apple Silicon): native cross, no emulation —
  ```bash
  rustup target add x86_64-apple-darwin
  cargo tauri build --target x86_64-apple-darwin
  ```
  (Universal: `--target universal-apple-darwin`.)
- **Linux** (any host, via Docker): build in a `rust:bookworm` container with the
  Tauri deps (`libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
  libsoup-3.0-dev`); reuse the already-built `web/dist` and skip the JS rebuild
  with `--config '{"build":{"beforeBuildCommand":""}}'`.
- **Windows** (from macOS/Linux, via Docker): cross-compile with
  [`cargo-xwin`](https://github.com/rust-cross/cargo-xwin) to
  `x86_64-pc-windows-msvc` and let Tauri's NSIS bundler run on Linux —
  `cargo tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc
  --bundles nsis`. Produces the NSIS `.exe`; the `.msi` still needs a real
  Windows host.

## Linux system dependencies

On a Linux build host (or container):

```bash
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev \
  patchelf libsoup-3.0-dev
```

## After download (unsigned bundles)

Builds are **unsigned** unless you supply signing credentials, so first launch
needs a nudge:

- **macOS:** right-click → **Open** (or `xattr -dr com.apple.quarantine
  /Applications/Hive.app`).
- **Windows:** SmartScreen → **More info → Run anyway**.
- **Linux:** `chmod +x Hive_*.AppImage && ./Hive_*.AppImage` (add
  `--appimage-extract-and-run` on hosts without FUSE).

## Signing & notarization

Not wired into the default build — it needs real credentials and is a
release-engineering step:

- **macOS** — Developer ID cert + `notarytool` (set `APPLE_*` env vars; Tauri
  signs/notarizes during `tauri build`).
- **Windows** — Authenticode cert via `bundle.windows.certificateThumbprint` or
  a `bundle.windows.signCommand`.
- **Linux** — `.deb`/AppImage are usable unsigned; sign repos/packages as needed.

See `docs/packaging.md` in the repo for the full matrix.

## Building just the relay

The relay is a separate binary in the same workspace:

```bash
cargo build -p hive-relay --release      # → target/release/hive-relay
# or run it directly:
cargo run -p hive-relay
```

See [Self-hosting a relay](../networking/self-host.md).
