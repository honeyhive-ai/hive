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

## Other platforms

Homebrew on Linux can install CLI formulae, but the desktop app is distributed
as `.AppImage` / `.deb` / `.rpm` there, and as an installer on Windows. See
[Building the dist](../ops/build.md) for all the bundle formats.
