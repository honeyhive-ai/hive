#!/usr/bin/env bash
#
# Unified build entrypoint. Just name the target OS — direct P2P (iroh) is
# included automatically on every platform that can build it:
#
#   ./scripts/build.sh mac        # .dmg  — full P2P (native)
#   ./scripts/build.sh linux      # .deb  — full P2P (Docker, x86_64)
#   ./scripts/build.sh windows    # .exe  — relay-only*  (Docker cross-compile)
#   ./scripts/build.sh all        # all of the above
#
# * Windows is the one exception: iroh depends on the `wmi` crate (Windows COM),
#   which cannot be cross-compiled by cargo-xwin from macOS/Linux. The Windows
#   installer therefore ships WITHOUT direct P2P (relay-based multiuser still
#   works). To get a full-P2P Windows build, build natively on Windows or via a
#   `windows-latest` CI runner — there iroh compiles and P2P is included.
#
# P2P is wired as the opt-in cargo feature `p2p`; this script passes
# `--features p2p` for you wherever it's buildable, so from your side P2P is
# simply on by default per OS.
#
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

build_mac() {
    echo "==> macOS (.dmg, full P2P)…"
    # Native build; tauri runs the frontend build itself (beforeBuildCommand).
    # P2P is on by default — no feature flag needed.
    cargo tauri build --bundles dmg
    echo "    -> $(ls -1 target/release/bundle/dmg/*.dmg 2>/dev/null | tail -1)"
}

build_linux() {
    echo "==> Linux (.deb, full P2P)…"
    "$repo_root/scripts/build-linux.sh"
}

build_windows() {
    echo "==> Windows (.exe, relay-only — iroh can't cross-compile)…"
    "$repo_root/scripts/build-windows.sh"
}

targets=("$@")
if [[ ${#targets[@]} -eq 0 ]]; then
    echo "usage: $0 <mac|linux|windows|all> [more...]" >&2
    exit 2
fi

for t in "${targets[@]}"; do
    case "$t" in
        mac|macos|darwin) build_mac ;;
        linux|ubuntu|deb) build_linux ;;
        win|windows|msvc) build_windows ;;
        all)
            build_mac
            build_linux
            build_windows
            ;;
        *)
            echo "unknown target: $t (expected mac|linux|windows|all)" >&2
            exit 2
            ;;
    esac
done
