#!/usr/bin/env bash
#
# Cross-build the Hive desktop app for Windows (x86_64-pc-windows-msvc) from a
# macOS/Linux host, using Docker + cargo-xwin. Produces the NSIS installer.
#
# The frontend (web/dist) is built on the HOST first and reused inside the
# container (beforeBuildCommand is blanked), so the image needs no JS toolchain.
# Tauri's NSIS bundler runs on Linux but probes a few host GTK/appindicator
# libs during bundling, so those -dev packages are installed too.
#
# Output: target/x86_64-pc-windows-msvc/release/bundle/nsis/Hive_*-setup.exe
#
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

if ! docker info >/dev/null 2>&1; then
    echo "Docker is not running. Start Docker Desktop and retry." >&2
    exit 1
fi

if [[ ! -f web/dist/index.html ]]; then
    echo "[host] building frontend (web/dist missing)…"
    bun run --cwd web build
fi

# Cache cargo's registry/git across runs (NOT the toolchain dir — mounting over
# /usr/local/cargo would hide the image's rustup/cargo).
cache_vol="hive-win-cargo-registry"
docker volume inspect "$cache_vol" >/dev/null 2>&1 || docker volume create "$cache_vol" >/dev/null

echo "[docker] cross-building Windows NSIS installer…"
docker run --rm \
    -v "$repo_root":/work \
    -v "$cache_vol":/usr/local/cargo/registry \
    -w /work \
    rust:bookworm \
    bash -c '
        set -euo pipefail
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        # NSIS + clang/llvm for cargo-xwin, plus the host libs Tauri probes
        # while running the bundler on Linux.
        apt-get install -y -qq --no-install-recommends \
            nsis clang llvm lld \
            libwebkit2gtk-4.1-dev librsvg2-dev \
            libappindicator3-dev libayatana-appindicator3-dev \
            libsoup-3.0-dev pkg-config >/dev/null
        rustup target add x86_64-pc-windows-msvc
        cargo install cargo-xwin --locked 2>/dev/null || true
        cargo install tauri-cli --version "^2" --locked 2>/dev/null || true
        export PATH="/usr/local/cargo/bin:$PATH"
        # Dedicated build dir so a concurrent Linux/native build (sharing the
        # repo mount) doesn't make us block on the cargo build-directory lock.
        export CARGO_TARGET_DIR=/work/target/cross-windows
        # --no-default-features drops the (default-on) `p2p` feature so iroh —
        # whose `wmi` crate (Windows COM) can't cross-compile via cargo-xwin —
        # is excluded. Works because the workspace root declares hive-runtime
        # with default-features = false. The Windows installer therefore ships
        # WITHOUT direct P2P; build natively on Windows (or via CI) to include
        # it. The relay-based multiuser path still works.
        cargo tauri build \
            --runner cargo-xwin \
            --target x86_64-pc-windows-msvc \
            --bundles nsis \
            --config "{\"build\":{\"beforeBuildCommand\":\"\"}}" \
            -- --no-default-features
    '

echo ""
echo "Done. Installer:"
ls -la target/cross-windows/x86_64-pc-windows-msvc/release/bundle/nsis/ 2>/dev/null || \
    echo "  (expected under target/cross-windows/x86_64-pc-windows-msvc/release/bundle/nsis/)"
