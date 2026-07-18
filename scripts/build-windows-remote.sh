#!/usr/bin/env bash
#
# Drive a NATIVE Windows build on your Windows box over SSH, from your Mac.
# Native MSVC compiles iroh's `wmi` crate fine, so this produces a FULL-P2P
# installer — the thing the cargo-xwin cross-compile (build-windows.sh) can't.
#
# ── One-time setup on the Windows box ─────────────────────────────────────────
#   1. Enable the OpenSSH Server:
#        Settings → System → Optional features → Add → "OpenSSH Server"
#        then in an elevated PowerShell:  Start-Service sshd; Set-Service sshd -StartupType Automatic
#   2. Install the build toolchain (once):
#        - Rust:  https://rustup.rs  (defaults to the MSVC toolchain — correct)
#        - "Microsoft C++ Build Tools" → workload "Desktop development with C++"
#        - Bun:   https://bun.sh        (frontend build)
#        - WebView2 runtime (preinstalled on Win11; Win10 may need it)
#        - tauri-cli:  cargo install tauri-cli --version "^2" --locked
#   3. Pick how source gets there (WIN_SYNC below).
#
# ── Usage ─────────────────────────────────────────────────────────────────────
#   WIN_HOST=you@192.168.1.50 \
#   WIN_REPO='C:/Users/you/hive' \
#   ./scripts/build-windows-remote.sh
#
#   WIN_SYNC controls how the source reaches the Windows box:
#     rsync (default) — incremental copy of the working tree (needs rsync on the
#                       Windows box: `scoop install rsync`, or Git-for-Windows/MSYS2)
#     git             — run `git pull` in WIN_REPO on the Windows box (you cloned
#                       it there from a remote both machines can reach)
#     none            — assume the source is already in place; just build
#
# Output: ./dist-windows/Hive_*-setup.exe  (copied back to the Mac)
#
set -euo pipefail

: "${WIN_HOST:?set WIN_HOST=user@host (the Windows box)}"
: "${WIN_REPO:?set WIN_REPO=repo path on the Windows box, e.g. C:/Users/you/hive}"
WIN_SYNC="${WIN_SYNC:-rsync}"

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
ps() { ssh "$WIN_HOST" "powershell -NoProfile -Command \"$1\""; }

echo "==> [1/3] syncing source to $WIN_HOST:$WIN_REPO  (mode: $WIN_SYNC)"
case "$WIN_SYNC" in
    rsync)
        rsync -az --delete \
            --exclude 'target/' --exclude 'web/node_modules/' \
            --exclude 'web/dist/' --exclude '.git/' --exclude 'dist-windows/' \
            "$repo_root"/ "$WIN_HOST:$WIN_REPO/"
        ;;
    git)
        ps "Set-Location -LiteralPath '$WIN_REPO'; git pull --ff-only"
        ;;
    none) echo "    (skipped)";;
    *) echo "unknown WIN_SYNC=$WIN_SYNC (rsync|git|none)" >&2; exit 2;;
esac

echo "==> [2/3] building natively on Windows (full P2P)…"
# cargo tauri build runs the frontend (beforeBuildCommand) then the app + NSIS.
ps "Set-Location -LiteralPath '$WIN_REPO'; cargo tauri build"

echo "==> [3/3] copying the installer back…"
mkdir -p "$repo_root/dist-windows"
scp "$WIN_HOST:$WIN_REPO/target/release/bundle/nsis/Hive_*-setup.exe" \
    "$repo_root/dist-windows/"

echo ""
echo "Done — full-P2P Windows installer:"
ls -la "$repo_root/dist-windows/"
