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
open, as before. This is a coarse on/off gate, not per-member roles — see the
managed-service plan in the repo for the finer-grained direction.

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

By default the relay is **open** — anyone with the URL + room id can join that
room. Three things control access:

- **Set a workspace key.** With it, every envelope is sealed with
  ChaCha20-Poly1305 *before* it leaves the device; the relay only ever stores
  ciphertext. Settings shows `🔒 encrypted` when it's on. This is what keeps your
  data private even on an open relay.
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

## Where the source lives

`crates/hive-relay/` (the axum service + routes). It depends only on
`hive-core` types + a small async/HTTP stack, so lifting it into a standalone
repo is a mechanical move when that fits your workflow.
