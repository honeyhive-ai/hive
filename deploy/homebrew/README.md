# Homebrew cask (staging)

`Casks/hive.rb` is the Homebrew cask for installing the desktop app. It lives
here as a staging copy; Homebrew discovers casks from a **tap repo**, so it has
to be published to one.

## Publish it to a tap

A tap is just a GitHub repo named `homebrew-<tap>`:

1. Create `github.com/honeyhive-ai/homebrew-hive`.
2. Copy this cask into it at `Casks/hive.rb`.
3. Cut a GitHub Release tagged `v<version>` with the macOS DMG attached
   (`Hive_<version>_aarch64.dmg`). The app is **Apple-Silicon-only**, so the cask
   is arm-only (`depends_on arch: :arm64`); add an Intel slice only if/when an
   `x64` DMG is built.
4. Bump `version` and set the `sha256` from the released DMG:
   ```bash
   shasum -a 256 Hive_1.1.1_aarch64.dmg   # → sha256 in the cask
   ```

Users then install with:

```bash
brew tap honeyhive-ai/hive
brew install --cask hive
```

`brew upgrade --cask hive` picks up new releases (the `livecheck` block watches
GitHub releases).

## Signing

The macOS DMGs are **signed + notarized** (Developer ID) in CI, so Gatekeeper
opens them normally — no `xattr` workaround needed. Notarization also satisfies
the signing prerequisite for the **official** `homebrew/cask` repo; a personal
tap (above) is still the pragmatic path until the project meets homebrew-cask's
notability criteria.

## Automating sha256 + version

If you adopt the `bundles.yml` release workflow, add a step that, on tag, writes
the cask's `version` + `sha256` from the built DMGs and opens a PR against the
tap — so a tagged release updates the cask without manual edits.
