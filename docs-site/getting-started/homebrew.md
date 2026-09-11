# Install with Homebrew

On macOS, Hive can be installed as a [Homebrew](https://brew.sh) cask from the
project's tap:

```bash
brew tap honeyhive-ai/hive
brew install --cask hive
```

Upgrade later with:

```bash
brew upgrade --cask hive
```

This installs `Hive.app` into `/Applications`. The desktop app is **Apple
Silicon only** — the cask is arm-only, so `brew install` on an Intel Mac reports
no available build.

The DMGs are **signed + notarized** (Developer ID), so Gatekeeper opens the app
normally — no `xattr` workaround needed — and it **updates itself in place** from
then on.

## Uninstall

```bash
brew uninstall --cask hive          # remove the app
brew uninstall --zap --cask hive    # also remove local data/settings
```

## The `hive` CLI / daemon

The headless client + agent daemon ships as its own formula (works on macOS
**and** Linux, Apple Silicon and x86_64):

```sh
brew install honeyhive-ai/hive/hive-cli   # installs the `hive` binary
brew upgrade hive-cli                      # track releases
```

This is the same runtime as the desktop app without the window — for running
agents on a server, scripting Hive, or connecting a remote agent to a workspace.
See [Headless agents & setups](../concepts/headless-agents.md) for the full
workflow (`hive enroll` → `hive worker`).

## Other platforms

Homebrew installs the **CLI formula** on both macOS and Linux (above). The
**desktop app** is a macOS-only cask; on Linux it's distributed as
`.AppImage` / `.deb` / `.rpm`, and on Windows as an installer. See
[Building the dist](../ops/build.md) for all the bundle formats.
