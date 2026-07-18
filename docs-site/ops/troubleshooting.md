# Troubleshooting

## App won't launch (unsigned bundle)

Builds are unsigned, so the OS blocks the first launch:

- **macOS** — "can't be opened because the developer cannot be verified":
  right-click → **Open**, or clear quarantine once:
  ```bash
  xattr -dr com.apple.quarantine /Applications/Hive.app
  ```
- **Windows** — SmartScreen → **More info → Run anyway**.
- **Linux** — `chmod +x Hive_*.AppImage` (and add `--appimage-extract-and-run`
  on hosts without FUSE).

## Can't reach a local runtime (e.g. Ollama)

Runtime HTTP calls are made by the Rust backend (not the webview), so there's no
App Transport Security gate — `http://localhost:11434` works fine. If a local
endpoint isn't reachable:

- Confirm the service is running (`curl http://localhost:11434/api/tags`).
- Check the URL/port in the runtime config.
- For the `pi` agent → Ollama, set the **Ollama base URL** on the runtime (Hive
  bootstraps a provider config from it).

## A subprocess agent failed or hung

- **`spawn claude: No such file or directory`** (or the same for `aider`/`pi`)
  — the CLI isn't visible to the app. Hive repairs the minimal `PATH` that
  Finder/Dock launches inherit by probing your login shell at startup and
  merging in common bin dirs (`~/.local/bin`, `/opt/homebrew/bin`, …). If a
  CLI still isn't found, make sure your shell profile (`.zprofile`/`.zshrc`)
  exports its location, then relaunch Hive.
- **`pi exited with status 1` / "No API key"** — `pi` isn't configured. Run
  `pi login` (or set its provider/model), or point it at a local Ollama via the
  runtime's Ollama base URL.
- **The turn hangs** — subprocess agents are bounded by a wall-clock timeout;
  raise or lower it with `HIVE_AGENT_TIMEOUT_SECS` (default 300).
- The error text now includes the tool's own stderr, so check the agent reply
  for the underlying message.

## Peers aren't syncing

Multiuser goes through the relay. Check, on **every** device:

1. **Relay URL + Room match exactly** (Settings → Multiuser sync). The status
   line should read `● connected`.
2. The relay is up: `curl https://<relay>/v1/health` → `ok`.
3. If you set a **workspace key**, it must be identical on every device
   (mismatched keys can't decrypt each other's envelopes). Status shows
   `🔒 encrypted`.
4. The relay holds queued events **in memory** — a relay restart (or a free-tier
   host that slept) drops anything peers hadn't pulled yet.

## Agent says it did something but nothing happened

The agent may have narrated an action it didn't broker through Hive:

- The runtime may not support Hive's tool calls (e.g. plain Ollama).
- A subprocess agent (`claude` / `pi` / `aider`) can run tools through its **own**
  mechanism — the side-effect happens, but Hive doesn't surface it as a tool
  block. Check the workspace folder directly: file edits are on disk, commits
  are in `git log`.

## Tests fail after pulling main

```bash
cargo test --workspace      # Rust
cd web && bun run build     # frontend typecheck + build
```
