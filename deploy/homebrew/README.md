# Homebrew cask (staging)

`Casks/hive.rb` is the Homebrew cask for installing the desktop app. It lives
here as a staging copy; Homebrew discovers casks from a **tap repo**, so it has
to be published to one.

## Publish it to a tap

A tap is just a GitHub repo named `homebrew-<tap>`:

1. Create `github.com/honeyhive-ai/homebrew-hive`.
2. Copy this cask into it at `Casks/hive.rb`.
3. Cut a GitHub Release tagged `v<version>` with the two macOS DMGs attached:
   `Hive_<version>_aarch64.dmg` and `Hive_<version>_x64.dmg`.
4. Fill in the `sha256` values and bump `version`:
   ```bash
   shasum -a 256 Hive_0.1.0_aarch64.dmg   # → arm
   shasum -a 256 Hive_0.1.0_x64.dmg       # → intel
   ```
5. Replace the `honeyhive-ai/hive` placeholders with the real repo.

Users then install with:

```bash
brew tap honeyhive-ai/hive
brew install --cask hive
```

`brew upgrade --cask hive` picks up new releases (the `livecheck` block watches
GitHub releases).

## Unsigned builds caveat

Until the DMGs are **signed + notarized** (Developer ID — see
`docs/packaging.md`), Gatekeeper will block first launch even when installed
via Homebrew. Document the one-time workaround for users, or notarize:

```bash
xattr -dr com.apple.quarantine "/Applications/Hive.app"
```

Notarization is also a hard requirement for the **official** `homebrew/cask`
repo (`brew install --cask hive` with no tap). Use a personal tap until the app
is notarized and the project meets homebrew-cask's notability criteria.

## Automating sha256 + version

If you adopt the `bundles.yml` release workflow, add a step that, on tag, writes
the cask's `version` + `sha256` from the built DMGs and opens a PR against the
tap — so a tagged release updates the cask without manual edits.
