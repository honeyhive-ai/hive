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

This downloads the right build for your Mac automatically (Apple Silicon or
Intel) and installs `Hive.app` into `/Applications`.

## First launch (unsigned builds)

Until notarized builds are published, macOS Gatekeeper blocks the first launch
of an unsigned app even when installed via Homebrew. Clear it once:

```bash
xattr -dr com.apple.quarantine "/Applications/Hive.app"
```

…or right-click **Hive.app → Open** the first time.

## Uninstall

```bash
brew uninstall --cask hive          # remove the app
brew uninstall --zap --cask hive    # also remove local data/settings
```

## Other platforms

Homebrew on Linux can install CLI formulae, but the desktop app is distributed
as `.AppImage` / `.deb` / `.rpm` there, and as an installer on Windows. See
[Building the dist](../ops/build.md) for all the bundle formats.
