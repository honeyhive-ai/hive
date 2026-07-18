# Deploying for a small team (free tier)

This is the practical, end-to-end recipe for getting a handful of developers
sharing Hive workspaces — on free or near-free hosting, in about 15 minutes.

## What you're actually deploying

Hive is a desktop app; there's no central server to run for the app itself.
The **only** thing a team self-hosts is the **relay** — a tiny stateless
service that forwards encrypted events between devices. One relay serves your
whole team.

```
  ┌────────────┐         ┌──────────────┐         ┌────────────┐
  │ Dev A (app)│ ──push─▶│  hive-relay  │◀─pull── │ Dev B (app)│
  │            │ ◀─pull──│ (you host 1) │ ──push─▶│            │
  └────────────┘         └──────────────┘         └────────────┘
        every device with the same relay URL + room id converges
```

What you do **not** pay for centrally:

- **LLM inference.** Hive is bring-your-own: each developer uses their own
  Claude Code CLI subscription (the default runtime) or their own API key /
  local Ollama. There's no shared inference bill to provision.
- **A database.** The relay keeps state in memory; each device keeps its own
  SQLite event log. Nothing to back up on the relay.

So "deployment" for a small team is really just **"host one small relay and
share three values with your teammates."**

## Step 1 — Host one relay (free tier)

Pick a host. For a small team, two good free-ish options:

| | Fly.io *(recommended)* | Render |
|---|---|---|
| Stays always-on | yes (keep 1 machine) | **no** — free instances sleep when idle |
| First-request after idle | instant | slow (cold start), and **queued events are lost on sleep** |
| Setup | one CLI command | click through a Blueprint |
| TLS / HTTPS URL | automatic | automatic |

Because the relay holds queued events **in memory**, a host that sleeps will
drop anything a peer hasn't pulled yet. That's fine for an evening of pairing,
but for a team that's online at different hours, prefer Fly.io's always-on
single machine (its free allowance covers one `shared-cpu-1x` / 256 MB machine,
which is all the relay needs).

The relay lives in its own repo — **[github.com/honeyhive-ai/relay](https://github.com/honeyhive-ai/relay)**
(MIT) — which ships the Dockerfile + `deploy/fly.toml`. Clone it, then:

### Fly.io

```bash
git clone https://github.com/honeyhive-ai/relay && cd relay
fly auth signup                     # or: fly auth login
fly launch --copy-config --no-deploy   # first time — pick a unique app name + region
fly volumes create hive_data --size 1 --region <your-region>
fly deploy                          # this + later deploys
fly status                          # → https://<your-app>.fly.dev
```

### Render / Railway / Cloud Run / a VPS

Point the platform at the relay repo's Dockerfile (New → Web Service → Docker),
attach a persistent disk at `/data`, set `HIVE_RELAY_DATA_DIR=/data`, and expose
port 8443. Or build + run the image anywhere:

```bash
docker build -t hive-relay https://github.com/honeyhive-ai/relay.git
docker run -d -p 8443:8443 --restart unless-stopped hive-relay
# put Caddy/nginx in front for HTTPS, or use the platform's TLS
```

The full deploy reference (env, TLS, token gating, Postgres/HA) lives in the
relay repo's README. For the from-scratch / control build, see
[Self-hosting a relay](../networking/self-host.md).

## Step 2 — Verify it's up

```bash
curl https://<your-relay-url>/v1/health      # → ok
```

If you get `ok`, the relay is ready. There's nothing else to configure on it.

## Step 3 — Agree on three values

Decide these once and share them with the team over a trusted channel (your
password manager, a DM — **not** a public channel):

| Value | What it is | Example |
|---|---|---|
| **Relay URL** | the URL from Step 1 | `https://acme-hive.fly.dev` |
| **Room** | any shared string; same room = same workspace | `acme-backend` |
| **Workspace key** | shared secret → end-to-end encryption | a long passphrase |

The **workspace key is your security model** (see Step 5) — treat it like a
password and don't skip it.

## Step 4 — Each teammate connects (no restart, no env vars)

In Hive: **Settings → Multiuser sync**, fill in the Relay URL, Room, and
Workspace key, then **Save**. The background sync picks it up within a few
seconds — no relaunch needed. The panel should show:

```
● connected · relay https://acme-hive.fly.dev · room acme-backend · 🔒 encrypted
```

Now create a chat on one machine; it shows up on the others within ~3 seconds.
Replies, agents, proposals, reactions, and skills all propagate the same way.

> Environment variables (`HIVE_RELAY_URL`, `HIVE_WORKSPACE`,
> `HIVE_WORKSPACE_KEY`) still work, but they only **seed** the settings on first
> launch. After that the in-app Settings are the source of truth. Most teams
> don't need the env vars at all.

Each developer also points their own runtime at their own credentials — the
default `claude` CLI needs nothing beyond being logged in (`claude`); an
Anthropic/OpenAI key or a local Ollama goes in the same Settings panel. See
[Configuring a runtime](../getting-started/configuring-a-runtime.md).

## Step 5 — Security for a small team

The relay has **no built-in login** — anyone with the URL **and** the room id
can join that room. Two things make that safe:

1. **Always set a workspace key.** With it, every event is sealed with
   ChaCha20-Poly1305 *before* it leaves the device; the relay only ever stores
   ciphertext, and a peer with the wrong key can't read anything. Settings shows
   `🔒 encrypted` when it's on, `⚠ plaintext` when it isn't.
2. **Use an unguessable room id.** Not `team` or `dev` — something no one would
   stumble onto.

To remove someone's access, **rotate**: pick a new workspace key (and/or room),
and re-share it with everyone who should stay. The old key can no longer decrypt
new traffic.

If a teammate enables a higher Claude permission mode (Settings → Multiuser
sync → *Claude permissions*: `acceptEdits` / `bypassPermissions`), that lets the
`claude` agent edit files / run commands on **their** machine without asking —
it's local and off by default. Leave it on the safe default unless the
workspace is trusted.

## Free-tier expectations & when to upgrade

For a team of a few developers, the relay's load is negligible — events are
small and infrequent, memory is ~1 KB per queued item, CPU is trivial. A single
free `shared-cpu-1x` machine is plenty.

Move up when you hit one of these:

| Symptom | Why | Fix |
|---|---|---|
| Peers miss events after quiet periods | free host slept and dropped the in-memory queue | always-on machine (Fly.io min 1) |
| You want events to survive a relay restart | state is in-memory only today | keep the machine up; durable storage is a tracked follow-up |
| You need real accounts / SSO / audit | relay has no auth by design | a managed/enterprise relay — see [Pricing tiers](pricing.md) |

## Caveats (today's relay)

- **Single instance.** State is in memory — run exactly one machine, don't scale
  out, and know that a restart drops anything peers haven't pulled yet.
- **No authentication.** Security comes from the workspace key + an unguessable
  room id (Step 5), not from the relay.
- **Direct P2P / TURN** are tracked follow-ups; relay forwarding is the path that
  works today and is all a small team needs.

## Checklist

- [ ] One relay deployed, `/v1/health` returns `ok`
- [ ] Relay URL + room + workspace key shared over a trusted channel
- [ ] Every teammate's Settings → Multiuser sync shows `● connected · 🔒 encrypted`
- [ ] Each developer has their own runtime credentials (claude login / API key / Ollama)
- [ ] Room id is unguessable; everyone has the workspace key
