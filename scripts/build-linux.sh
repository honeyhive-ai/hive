#!/usr/bin/env bash
#
# Build the Hive desktop app for x86_64 Linux (Ubuntu/Debian) via Docker,
# producing a .deb. iroh builds fine on Linux (netlink, not the Windows `wmi`
# crate), so this includes FULL direct-P2P (default features).
#
# The frontend (web/dist) is built on the HOST first and reused in the
# container (beforeBuildCommand blanked), so the image needs no JS toolchain.
#
# Output: target/x86_64-unknown-linux-gnu/release/bundle/deb/*.deb
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

# Separate registry cache from the Windows build so concurrent runs don't
# contend on the cargo registry lock.
cache_vol="hive-linux-cargo-registry"
docker volume inspect "$cache_vol" >/dev/null 2>&1 || docker volume create "$cache_vol" >/dev/null

echo "[docker] building Linux .deb (x86_64, full P2P)…"
docker run --rm --platform linux/amd64 \
    -v "$repo_root":/work \
    -v "$cache_vol":/usr/local/cargo/registry \
    -w /work \
    rust:bookworm \
    bash -c '
        set -euo pipefail
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends \
            libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev patchelf \
            libayatana-appindicator3-dev libsoup-3.0-dev \
            build-essential pkg-config libssl-dev file >/dev/null
        cargo install tauri-cli --version "^2" --locked 2>/dev/null || true
        export PATH="/usr/local/cargo/bin:$PATH"
        # Cap concurrent codegen so the release link does not OOM the container
        # (the big binary + iroh/tauri can spike past the Docker memory ceiling).
        export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
        # P2P is on by default; iroh builds fine on Linux (netlink, not the
        # Windows `wmi` crate), so the Linux .deb ships WITH direct P2P.
        cargo tauri build \
            --target x86_64-unknown-linux-gnu \
            --bundles deb \
            --config "{\"build\":{\"beforeBuildCommand\":\"\"}}"
    '

echo ""
echo "Done. Package:"
ls -la target/x86_64-unknown-linux-gnu/release/bundle/deb/ 2>/dev/null || \
    echo "  (expected under target/x86_64-unknown-linux-gnu/release/bundle/deb/)"
