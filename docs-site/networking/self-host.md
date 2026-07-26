# Self-hosting a relay

Hive's relay is a small, content-blind service that forwards encrypted event
envelopes between the devices in a workspace — no database needed. You host
**one** relay that your peers point at. It lives in its own repo,
**[github.com/honeyhive-ai/relay](https://github.com/honeyhive-ai/relay)** (MIT),
which carries the Docker image, `deploy/fly.toml`, and the full deploy README.

**Fastest path:** follow that repo's README (Docker / Fly.io) and the
[small-team deployment guide](../ops/deployment.md). The shortest local smoke
test:

```bash
docker build -t hive-relay https://github.com/honeyhive-ai/relay.git && \
  docker run -p 8443:8443 hive-relay
```

Health check: `curl http://localhost:8443/v1/health` → `ok`.

The rest of this page covers a from-scratch deploy on a VM you control.

You need:

- Linux (Ubuntu 22.04+ / Debian 12+) **or** macOS on a host reachable from your
  peers.
- A domain with TLS (Let's Encrypt via Caddy is easiest), or use a PaaS that
  terminates TLS for you.
- One TCP port open.

## Step 1 — Build

```bash
git clone https://github.com/honeyhive-ai/hive.git
cd hive
cargo build -p hive-relay --release
```

The binary lands at `target/release/hive-relay`. It has no resource
dependencies — copy it anywhere.

## Step 2 — Run

```bash
./target/release/hive-relay
```

The bind address is chosen in this order:

1. `$PORT` (set by Render / Cloud Run / Railway / Heroku) → `0.0.0.0:$PORT`
2. `$HIVE_RELAY_ADDR` (full `host:port`)
3. default `0.0.0.0:8443`

```bash
HIVE_RELAY_ADDR=0.0.0.0:9000 ./hive-relay      # custom port
```

### Optional: gate the relay with access tokens

By default the relay is **open** — anyone with the URL + room may connect (this
is the normal self-host mode; a workspace key keeps traffic private regardless).
If you want to restrict *who can connect at all* (e.g. you're running a relay for
a paid group), set `HIVE_RELAY_ACCESS_TOKENS` to a comma-separated allowlist:

```bash
HIVE_RELAY_ACCESS_TOKENS="tok_alice,tok_bob" ./hive-relay
```

Now only requests bearing `Authorization: Bearer <one-of-those>` are admitted
(`/v1/health` stays open). Each peer pastes their token into **Settings → Team
sync → Relay access token** (or sets `HIVE_RELAY_ACCESS_TOKEN`). Unset/empty ⇒
open, as before. This is a coarse on/off gate that requires a redeploy to
change — for durable, no-redeploy management, use the admin API below.

## Managing relay access

Beyond the static `HIVE_RELAY_ACCESS_TOKENS` allowlist, the relay has a durable
**user + token store** with an admin API — the model behind Hive's
**Settings → Team → Team members** panel. Users and tokens persist (in Postgres
when `DATABASE_URL` is set, else the snapshot volume), tokens are stored only as
hashes, and revoking one is instant with no redeploy.

Managing users requires an **admin authorizer**. The reference relay leaves the
admin API disabled by default; a downstream build (such as the hosted enterprise
relay) enables it and gates it on a GitHub-admin allowlist — set
`HIVE_RELAY_ADMIN_LOGINS` to a comma-separated list of GitHub logins permitted
to manage users. Those admins then manage members either from Hive's Team panel
or directly over the API:

```bash
U=https://your-relay.example
GH=<a github token for an admin login>

# Create a user + first token → returns {"user":…, "raw":"<token>"} (shown once)
curl -sX POST -H "x-hive-github-token: $GH" -H 'content-type: application/json' \
  "$U/v1/admin/users" -d '{"name":"Alice","login":"alice"}'

# List users + their tokens (hashes never returned)
curl -s  -H "x-hive-github-token: $GH" "$U/v1/admin/users"

# Revoke one token immediately (no restart)
curl -sX DELETE -H "x-hive-github-token: $GH" "$U/v1/admin/tokens/<tokenId>"
```

When the store gates access, an unknown token is **rejected** (the relay does
not silently fall back to open); the signed-token / static-allowlist policies
still work as a fallback when explicitly configured.

## Step 3 — TLS termination

The relay speaks plain HTTP; put TLS in front. A two-line Caddyfile:

```caddy
# /etc/caddy/Caddyfile
relay.example.com {
    reverse_proxy localhost:8443
}
```

Caddy fetches and rotates a Let's Encrypt cert automatically. Nginx + Certbot
works too. (PaaS hosts like Fly.io / Render terminate TLS for you — no proxy
needed there.)

## Step 4 — Run as a service (Linux / systemd)

`/etc/systemd/system/hive-relay.service`:

```ini
[Unit]
Description=Hive Relay
After=network-online.target

[Service]
ExecStart=/usr/local/bin/hive-relay
Environment=HIVE_RELAY_ADDR=0.0.0.0:8443
Restart=always
User=hive
Group=hive

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now hive-relay
```

## Step 5 — Point Hive at it

In every peer's app: **Settings → Team sync** — set the **Relay URL**
(`https://relay.example.com`), a shared **Room** id, and (strongly recommended) a
shared **Workspace key**, then **Save**. If you gated the relay with
`HIVE_RELAY_ACCESS_TOKENS` (above), also paste each peer's **Relay access token**;
leave it blank for an open relay. Changes apply within a few seconds, no restart.
Devices on the same relay URL + room converge.

> Environment variables (`HIVE_RELAY_URL` / `HIVE_WORKSPACE` /
> `HIVE_WORKSPACE_KEY` / `HIVE_RELAY_ACCESS_TOKEN`) still work as first-launch
> seeds, but the in-app settings are the source of truth after that.

## Step 6 — Verify

```bash
curl https://relay.example.com/v1/health      # → ok
```

Create a chat on one device; within a few seconds it appears on the others
(transcript, agents, proposals, reactions, and skills all flow through the same
path).

## Security

By default the relay is **open** at the *connection* layer — anyone with the URL
+ room id can reach that room. Content privacy is a separate, mandatory layer;
see [Security & trust](../concepts/security.md) for the full model. Three things
control access:

- **The workspace key is required to sync.** E2EE is mandatory for relay sync:
  Hive refuses to push plaintext, so a workspace with no key does not sync (the
  app shows a Sync error until you set one). With a key, every envelope is sealed
  with ChaCha20-Poly1305 *before* it leaves the device; the relay only ever
  stores ciphertext, and the key never transits the relay. Settings shows
  `🔒 encrypted` when it's on.
- **Use an unguessable room id.** To revoke read access, rotate the key and/or
  room.
- **Optionally gate connections** with `HIVE_RELAY_ACCESS_TOKENS` (see above) if
  you want to restrict who can reach the relay at all.

> The access-token gate controls *connection*, not per-member roles. Workspace
> membership/removal is enforced client-side today (removal re-keys so an ejected
> member can't read new traffic); server-enforced membership is on the roadmap
> for managed/paid relays.

## Operations

- **Single instance, in-memory.** Run one machine; don't scale out. A restart
  drops anything peers haven't pulled yet (durable storage is a tracked
  follow-up).
- Memory grows with `(workspaces × devices × queued events)` — tiny for a small
  team. CPU is negligible.

### Retention & memory bounds

The memory store keeps recent E2EE envelopes so a peer can catch up after being
offline. Three env vars bound it:

| Env var | Default | Effect |
|---------|---------|--------|
| `HIVE_RELAY_MAX_ENVELOPES` | `50000` | Per-workspace retained-envelope cap — the primary memory bound. `0` = unbounded. |
| `HIVE_RELAY_RETENTION_DAYS` | `0` (off) | Also prune envelopes older than N days. |
| `HIVE_RELAY_MAX_BODY_BYTES` | `4194304` (4 MiB) | Max request body size. |

The relay is a **cache of recent E2EE traffic**, not a system of record. A device
offline past the retention window re-syncs the gap from a peer that still has it,
or from the Postgres backend (`DATABASE_URL`) if you run one for unbounded
history. Size the cap for your longest expected offline gap.

### Health probes

`/v1/health` round-trips the store, so it reports real liveness/readiness, not
just process-up: `200 ok` when the store answers, `503` when it doesn't. Point
your orchestrator's liveness/readiness probes at it.

```bash
curl -fsS https://relay.example.com/v1/health      # → ok (200), or 503 if unhealthy
```

Pairing-code redemption is **rate-limited** to blunt code enumeration.

## Where the source lives

`crates/hive-relay/` (the axum service + routes). It depends only on
`hive-core` types + a small async/HTTP stack, so lifting it into a standalone
repo is a mechanical move when that fits your workflow.
