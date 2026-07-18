# Testing multiuser across machines / OSes

Hive syncs a workspace by forwarding **signed event envelopes** through a relay:
every device that points at the same relay URL + room id converges to the same
projected state. (Direct P2P — STUN/hole-punch — is an optimization tracked
separately; relay forwarding is all you need to test multiuser.)

## 1. Run a relay reachable by both machines

```bash
# from the repo root, on a host both machines can reach
cargo run --release -p hive-relay          # binds 0.0.0.0:8443
# or: HIVE_RELAY_ADDR=0.0.0.0:9000 cargo run -p hive-relay
# or containerized:
docker build -t hive-relay https://github.com/honeyhive-ai/relay.git && \
  docker run -p 8443:8443 hive-relay
```

For two machines on a LAN, the relay host's LAN IP works
(`http://192.168.x.y:8443`). Over the internet, host it on a cloud provider and
use its public HTTPS URL — see **`docs/relay-deploy.md`** (Fly.io / Render /
any container host). Health check: `GET /v1/health` → `ok`.

## 2. Point each device at the same relay + room

The easiest way is **Settings → Multiuser sync**: type the relay URL, room, and
(optionally) a workspace key, then **Save**. Settings persist to `settings.json`
and the background sync loop picks up the change within a few seconds — **no
restart, no env vars**. You can connect, reconnect, or go local-only at any time.

Alternatively, seed the values from the environment **before first launch** (they're
written into `settings.json` on first run, after which the UI is the source of
truth):

```bash
export HIVE_RELAY_URL="http://<relay-host>:8443"
export HIVE_WORKSPACE="team-alpha"        # any shared string = the room
export HIVE_WORKSPACE_KEY="a shared secret"  # E2EE: relay sees only ciphertext
```

For chat replies, the **default runtime is the `claude` CLI** (your Claude
subscription, streamed via stream-json) — install and authenticate Claude Code
(`claude`) on each device; no API key needed. An API key is only needed for an
explicit Anthropic-API/OpenAI runtime, and you can enter it in **Settings →
Multiuser sync** (it overrides `ANTHROPIC_API_KEY`).

**Claude permissions.** By default the `claude` agent is **read-only** — it proposes
edits but asks before touching files. Settings → Multiuser sync → *Claude
permissions* lets you opt into `acceptEdits` (auto-apply file edits) or
`bypassPermissions` (also run commands) — these inject `--permission-mode` into the
`claude` invocation. Leave it on the safe default unless you trust the workspace.

With `HIVE_WORKSPACE_KEY` set (the **same** value on every device), envelopes are
sealed with ChaCha20-Poly1305 before they leave the device — the relay only ever
stores ciphertext, and a peer with the wrong key can't read anything. Settings →
Multiuser sync shows `🔒 encrypted`. Without it, sync still works but the relay
sees plaintext (`⚠ plaintext`).

Then run the app (dev or a built bundle):

```bash
cargo tauri dev                         # dev (from the repo root)
# or launch the bundled app (see docs/packaging.md)
```

Settings → **Multiuser sync** shows `● connected · relay … · room …` when a
relay is configured. The background task syncs every ~3s.

## 3. What you should see

- Create a chat and send a message on machine A. Within a few seconds it appears
  on machine B (the sidebar + transcript refresh on `workspace://synced`).
- Replies, reactions, proposals, agent roster changes, and skills all propagate —
  they're all event-sourced and flow through the same path.
- It's bidirectional and order-independent: either side can create chats; both
  converge. Events are deduped by id, so restarts/double-sends are harmless.

## Building bundles per OS

See `docs/packaging.md`. In short: `cargo tauri build` (from the repo root) on
each target OS emits the native installer (`.dmg` / `.msi` / `.deb`/`.AppImage`).
Signing/notarization needs platform credentials (documented there).

## Current limitations (tracked follow-ups, task #125)

- **E2EE is opt-in via a shared key.** With `HIVE_WORKSPACE_KEY` the relay sees
  only ciphertext (sealed + authenticated). Per-device key distribution (so you
  don't share one key out-of-band) uses the `hive-core::e2ee` HPKE path and is a
  follow-up.
- **No membership/verify-on-read across peers yet.** Ingested events aren't
  signature-verified against a synced device roster (no membership exchange).
  Use a private room id you trust.
- **Direct P2P** (STUN/UDP hole-punch/TURN) and the **collaborative-text CRDT**
  are not wired; relay forwarding + the commutative projector cover multiuser
  chat/agents/review.
