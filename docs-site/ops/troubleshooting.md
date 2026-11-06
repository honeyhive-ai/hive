# Troubleshooting

## Which version am I running?

- **Desktop app**: **Settings → Updates & data** shows the version you're on.
- **`hive` CLI** (on a `hive serve` or `hive worker` host):
  ```bash
  hive --version     # prints: hive <version>   (-V and `hive version` work too)
  ```

Include both when you report a problem.

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

- **`spawn claude: No such file or directory`** (or the same for `aider`, `pi`,
  `agy` or `copilot`)
  — the CLI isn't visible to the app. Hive repairs the minimal `PATH` that
  Finder/Dock launches inherit by probing your login shell at startup and
  merging in common bin dirs (`~/.local/bin`, `/opt/homebrew/bin`, …). If a
  CLI still isn't found, make sure your shell profile (`.zprofile`/`.zshrc`)
  exports its location, then relaunch Hive.
- **`pi exited with status 1` / "No API key"** — `pi` isn't configured. Run
  `pi login` (or set its provider/model), or point it at a local Ollama via the
  runtime's Ollama base URL.
- **Google Antigravity or GitHub Copilot says it isn't signed in** — sign in
  in a terminal on the machine that runs the agent: run `agy` once and choose
  Sign in with Google, or run `copilot login` (on a headless host you can set
  `COPILOT_GITHUB_TOKEN` to a fine-grained token with the *Copilot Requests*
  permission; a classic `ghp_` token doesn't work). **Settings → Models →
  Subscription CLIs** shows the sign-in state; click **Re-scan** (or **Check
  Google sign-in** for `agy`) afterwards. A usage-limit error means the
  subscription's quota or premium requests ran out. See
  [Configuring a runtime](../getting-started/configuring-a-runtime.md#subscription-clis-google-antigravity-and-github-copilot).
- **The turn hangs** — subprocess agents are bounded by a wall-clock timeout;
  raise or lower it with `HIVE_AGENT_TIMEOUT_SECS` (default 300).
- The error text now includes the tool's own stderr, so check the agent reply
  for the underlying message.

## Peers aren't syncing

Multiuser goes through the relay. **Start with the status pill** — it is now
real, not cosmetic. It shows **live / error / offline** (older builds always
said "Live" whenever a URL was set). A wrong workspace key, a dead relay, or an
expired token surfaces as a visible **Sync error** with the reason, so read that
first. Then check, on **every** device:

1. **Relay URL + Room match exactly** (Settings → Multiuser sync). A healthy
   connection reads **live**; a problem reads **error** (with the reason) or
   **offline**.
2. The relay is up: `curl https://<relay>/v1/health` → `ok` (`200`; a `503`
   means the relay is running but its store is unhealthy).
3. If you set a **workspace key**, it must be identical on every device — a
   mismatch now shows up as a **Sync error** rather than silently dropping the
   remote messages it can't decrypt. Status shows `🔒 encrypted` when a key is
   set. Note that **sync requires a key**: a workspace pointed at a relay with no
   key won't push at all and shows a Sync error until you configure one.
4. The relay holds queued events **in memory** — a relay restart (or a free-tier
   host that slept) drops anything peers hadn't pulled yet.

> A single bad or undecryptable envelope no longer halts sync — the client skips
> the corrupt/foreign body and keeps applying the rest, logging the reason. So
> "everything stopped" is unlikely; look for a **Sync error** on the pill and the
> specific reason it names.

## The phone can't reach the box (`hive remote status`)

On the `hive serve` host, run `hive remote status` with the server's
environment (the same `HIVE_DATA_DIR`), since it reads the status file the
running server keeps in its data directory:

```text
relay:     https://relay.example.com (enabled)
host id:   <host id>
serve:     connected for 6h 33m (status active, push available, checked 12s ago)
phones:    1 active, 0 revoked
```

How to read the `serve:` line:

- **`connected for …`** — how long the relay link has been up, and when the
  running server last checked in (it refreshes every 30 seconds).
- **`not connected (…, since …)`** — the server is running but has no relay
  link; a `last error:` at the end says why.
- **`unknown: hive serve last reported … ago (…) — is it running?`** — the
  server stopped refreshing its status (no check-in for over 90 seconds), so
  it has most likely stopped or crashed. Check its service and logs and
  restart it.
- `relay: not configured` means remote access was never enabled
  (`hive remote enable --relay <url>`). No `serve:` line means `hive serve`
  hasn't reported a relay status yet (for example, it hasn't been started
  since remote access was enabled).

See [Remote access](../networking/remote-access.md) and
[Running hive serve as your own device](serve-box.md).

## An MCP tool is never offered to a local model

Ollama and `custom` (OpenAI-compatible) runtimes get **at most 16 tools a
turn**. If you enable many MCP tools, some aren't offered, and Hive logs a
warning naming them: `tool cap reached: these tools are NOT offered this
turn`. Find it in the log file (**Settings → Diagnostics**) on the desktop,
or in the log output of `hive serve` / `hive worker`. To have a tool offered,
name it in the agent's instructions, a skill it uses, or your message, or
disable servers the agent doesn't need. See
[MCP servers](../addons/mcp.md#local-models-at-most-16-tools-a-turn).

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
