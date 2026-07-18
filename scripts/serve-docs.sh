#!/usr/bin/env bash
#
# Serve the documentation site locally with live-reload.
#
# First time: installs mkdocs-material into a venv at ./.venv-docs/.
# Subsequent runs: reuses the venv.
#
# Usage:
#   bash scripts/serve-docs.sh
#   open http://127.0.0.1:8000

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

venv_dir="$repo_root/.venv-docs"

if [[ ! -d "$venv_dir" ]]; then
    echo "Setting up docs venv at $venv_dir"
    python3 -m venv "$venv_dir"
    "$venv_dir/bin/pip" install --upgrade pip
    "$venv_dir/bin/pip" install mkdocs-material
fi

exec "$venv_dir/bin/mkdocs" serve --dev-addr 127.0.0.1:8000
