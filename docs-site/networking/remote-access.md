# Remote access for your phone

The [phone app](../features/phone-app.md) talks to **`hive serve`**, a Hive
device you run on an always-on box (a home server, a small PC, a VM). This
page covers getting the phone to that box, from anywhere, and keeping the
link healthy.

There are two ways in, and you can use both:

| Path | How the phone reaches the box | Needs |
|---|---|---|
| **Direct** | plain HTTP to the box's address, with a bearer token | the phone on the same LAN, or both on a VPN such as Tailscale |
| **Relay tunnel** | through your relay, end-to-end encrypted (`hive-tunnel/1`) | a relay with the tunnel turned on; no VPN and no inbound port on the box |

The relay tunnel is the "works from anywhere" option. The box keeps one
outbound WebSocket to the relay. The phone connects to the relay, the relay
checks that it holds a valid credential for that box, and then splices the
two connections together. Inside, the phone and the box run a Noise
handshake: the phone pins the box's key from the pairing QR code, so the relay
can't read, alter or impersonate either end. It sees only ciphertext, sizes
and timing. The full protocol is
[`docs/relay-tunnel.md`](https://github.com/honeyhive-ai/hive/blob/main/docs/relay-tunnel.md).

## Before you start

- The `hive` CLI on the box (`brew install honeyhive-ai/hive/hive-cli`, or see
  [Headless agents](../concepts/headless-agents.md#install-the-cli)). Check it
  with `hive --version`.
- The box joined to your workspace with `hive enroll "hivews1:…"`, using the
  invite from the desktop's **Copy invite**. The invite carries the workspace
  key, so move it to the box over a channel you trust, such as SSH.
- For the relay tunnel: a relay with the tunnel turned on. It is **off by
  default**; see [Using your own relay](#using-your-own-relay).

A ready-made systemd unit and env file are in
[`deploy/hive-serve/`](https://github.com/honeyhive-ai/hive/tree/main/deploy/hive-serve).

## 1. Run `hive serve`

```sh
hive serve --listen 127.0.0.1:8787 --label home-box
```

- `--listen` / `HIVE_SERVE_LISTEN` (default `127.0.0.1:8787`). The box binds
  only to loopback, a private LAN address or a VPN address, and refuses
  `0.0.0.0` and public addresses unless `HIVE_SERVE_ALLOW_ANY_BIND=1`.
  With the relay tunnel only, loopback is fine: the phone never connects to
  this port directly.
- `--label` / `HIVE_SERVE_LABEL` is the host name other members see.
- `HIVE_SERVE_PUBLIC_URL` is the URL the phone should use for the direct path
  when it differs from `http://<listen>` (a DNS name, say).

On its first run, `hive serve` creates the direct-path bearer token in
`<HIVE_DATA_DIR>/serve.token` (mode 0600).

## 2. Turn on the relay tunnel

```sh
hive remote enable --relay https://<relay-url>
# restart hive serve, e.g. systemctl --user restart hive-serve
```

This creates the box's host keys and saves the relay in
`<HIVE_DATA_DIR>/tunnel/relay.json`. The relay URL must be `https`.

| Option | Use |
|---|---|
| `--reg-token <token>` / `HIVE_REMOTE_REG_TOKEN` | a relay that only admits hosts with an access token |
| `hive serve --remote-relay <url>` / `HIVE_REMOTE_RELAY` | use a relay for this run only, without `relay.json` |
| `HIVE_REMOTE_RELAY=off` | turn remote access off for this run even when `relay.json` enables it |
| `--insecure-relay` / `HIVE_REMOTE_INSECURE_RELAY=1` | allow an `http` relay URL. For tests and a relay on loopback only |

## 3. Pair the phone

```sh
hive pair
```

`hive pair` prints a QR code and the `hive://pair?…` link. Scan it with the
phone's camera or from the app. With remote access on, it's a **v2 link**: it
carries the direct URL and token (when there's a usable direct path) plus the
box's host id, its public key, the relay URL and a one-time pairing secret.

- **Run it with the server's settings.** `hive pair` is a separate process
  from `hive serve`. Load the same environment (`HIVE_DATA_DIR`,
  `HIVE_SERVE_LISTEN` or `HIVE_SERVE_PUBLIC_URL`), for example from the
  service's env file:

    ```sh
    set -a; . ~/.config/hive-serve/hive-serve.env; set +a
    hive pair
    ```

- **It never creates a token of its own.** It reads the token `hive serve`
  created, so it can't hand the phone one the server doesn't accept. With no
  `serve.token` yet, it stops and tells you to start `hive serve` first or
  point `HIVE_DATA_DIR` / `--token-file` at the server's data dir.
- **It won't send the phone to loopback.** A direct URL such as
  `http://127.0.0.1:8787` would point the phone at itself. With remote access
  on, `hive pair` prints a relay-only link instead. Without it, `hive pair`
  refuses and asks for `--listen <address:port>` or `--public-url <url>`.
- `--relay-only` leaves the direct URL and token out of the link even when a
  direct path exists.
- The pairing secret works **once, for 10 minutes**, and only while
  `hive serve` is running (it picks up the new pairing window within 2
  seconds). Run `hive pair` again for a fresh one.
- `hive serve --pair` prints the same code when the server starts.

!!! warning "The link is a key"
    A link with a direct URL also carries the box's API token. Share it only
    with your own phone, and don't paste it into a chat or a ticket.

After pairing, the phone holds its own credential for this box. It is valid
for 90 days and the app renews it every day while you use it, so you only
re-pair a phone that has been unused for 90 days.

## Manage phones

```sh
hive phones list
hive phones revoke <pid|name> [--keep-token]
```

`hive phones list` shows each phone paired over the relay: its id, name,
state (`active` or `revoked`), the day it was paired and the day it was last
seen.

`hive phones revoke` takes effect at once. A running `hive serve` ends the
phone's live sessions, refuses its requests and handshakes, and tells the
relay, which then rejects that phone before it reaches the box. By default it
also **rotates the direct-path token** (`serve.token`), because a lost phone
holds that too; every phone that uses the direct path must then pair again.
`--keep-token` skips the rotation.

## Check the link

```sh
hive remote status
```

```text
relay:     https://<relay-url> (enabled)
host id:   <host id>
serve:     connected for 6h 33m (status active, push available, checked 12s ago)
phones:    1 active, 0 revoked
```

The `serve:` line comes from a status file that a running `hive serve`
refreshes every 30 seconds:

| `serve:` says | Meaning |
|---|---|
| `connected for …` | the box is connected to the relay; the time is how long the current connection has been up |
| `not connected (…, since … ago)` | the box is running but can't reach the relay, or the relay refused it; a `last error` follows when there is one |
| `unknown: hive serve last reported … ago … — is it running?` | the status file hasn't been refreshed for over 90 seconds: `hive serve` stopped or crashed |
| `status pending` | the relay registered the box but hasn't activated it yet, so phones can't connect through it |
| `push unavailable` | the relay has no APNs key; see [Push through your own relay](#push-through-your-own-relay) |

There is no `serve:` line until `hive serve` has run with remote access on.

Other commands:

- `hive remote disable`: stop using the relay. Keys and phones are kept, so
  enabling it again needs no re-pairing. Restart `hive serve` afterwards.
- `hive remote reset`: new host keys, so a new host id. **Every phone must
  pair again.** Use it if the box's keys may have leaked.
- `hive --version`: the CLI's version. The phone sees the same version on the
  box's `/v1/health`.

## Using your own relay

The tunnel is part of the open relay,
[honeyhive-ai/relay](https://github.com/honeyhive-ai/relay) (`cmd/hive-relay`),
and is free to self-host. It is **off by default**: a relay you upgrade does
not become a public tunnel provider on its own. Turn it on in the relay's
environment, not on the box:

| Relay env | Meaning |
|---|---|
| `HIVE_RELAY_TUNNEL=off` | default. The tunnel routes answer 404 |
| `HIVE_RELAY_TUNNEL=open` | any host may register and is active at once |
| `HIVE_RELAY_TUNNEL=allowlist` | only the host ids in `HIVE_RELAY_TUNNEL_HOSTS` (comma-separated; get a box's id from `hive remote status`) |

If the relay also gates access with tokens (`HIVE_RELAY_ACCESS_TOKENS` or
`HIVE_RELAY_TOKEN_PUBKEY`), a box must present one to register: pass it with
`hive remote enable --reg-token`.

Check the relay with `GET https://<relay-url>/v1/tunnel/info`. A relay with
the tunnel on answers `{"versions":[1],"push":…,"billing":"none"}`; one with
it off answers 404. A self-hosted relay always reports `"billing":"none"`, so
the app shows no subscription screen for it.

!!! note "Run one relay instance"
    The relay keeps each box's control connection in memory, so a phone's
    connection must reach the same relay process as the box's. Run a single
    instance (or route by host yourself).

See [Self-hosting a relay](self-host.md) for deploying the relay itself.

## Push through your own relay

Push notifications travel `hive serve` → relay → Apple (APNs) → phone,
encrypted end to end with a key the box and the phone agree on at pairing.
Apple accepts a push only when it is signed with an APNs key from the same
Apple developer team that signed the app. So with your own relay, push works
when you run the relay with **your** APNs key and build and sign the app
yourself under the same team.

1. **Create an APNs key.** In the Apple Developer portal, under
   **Certificates, Identifiers & Profiles → Keys**, create a key with Apple
   Push Notifications service enabled. Download the `.p8` file (Apple lets you
   download it once; treat it like a password) and note the Key ID and your
   Team ID. Enable the Push Notifications capability on the app's identifier.
2. **Give the relay the key.**

    ```sh
    HIVE_RELAY_TUNNEL=open
    HIVE_RELAY_APNS_KEY_FILE=/path/to/AuthKey_<KEYID>.p8   # or HIVE_RELAY_APNS_KEY=<inline PEM>
    HIVE_RELAY_APNS_KEY_ID=<Key ID>
    HIVE_RELAY_APNS_TEAM_ID=<Team ID>
    HIVE_RELAY_APNS_TOPIC=<the app's bundle id>
    HIVE_RELAY_APNS_ENV=auto        # optional: auto (default) | sandbox | production
    ```

    `auto` uses the environment each phone reports: sandbox for development
    builds, production for TestFlight and App Store builds. Keep the `.p8`
    readable only by the relay's user and out of version control.
    `/v1/tunnel/info` then reports `"push": true`.
3. **Point the box at the relay and pair**, as in steps 2 and 3 above.
   `hive remote status` shows `push available` once the box is connected.
4. **Build the app with push turned on**, signed with your team and bundle id,
   install it, and allow notifications when the app asks.

What gets pushed, and the per-phone previews setting, are on
[Hive on your phone](../features/phone-app.md#push-notifications). The
details, including the host-side rate limits, are in
[`docs/push-notifications.md`](https://github.com/honeyhive-ai/hive/blob/main/docs/push-notifications.md).

## Troubleshooting

**`hive pair` says the link would point the phone at the phone itself.**
It's reading the default loopback address. Load the server's environment
first, or pass `--listen <address:port>` / `--public-url <url>`, or turn on
remote access so it prints a relay-only link.

**`hive pair` says there's no serve token.** It's looking in the wrong data
dir, or `hive serve` has never run there. Set `HIVE_DATA_DIR` to the server's
data dir (or pass `--token-file`), or start `hive serve` first.

**`hive pair` says remote access is enabled but there are no host keys.**
Run `hive remote enable --relay …` (or start `hive serve` once) in the same
data dir.

**The phone can't pair: the link expired or was already used.** Pairing
secrets are one-time and last 10 minutes, and `hive serve` must be running to
accept them. Run `hive pair` again.

**`hive remote status` says `unknown: … is it running?`** `hive serve` has
stopped. Check the service (`systemctl --user status hive-serve`) and its log.

**`hive remote status` says `not connected`.** Check the `last error`. Common
causes:

- the relay's tunnel is off: `/v1/tunnel/info` answers 404. Set
  `HIVE_RELAY_TUNNEL` on the relay;
- the relay URL is wrong or isn't `https`;
- the relay requires a registration token: pass `--reg-token`;
- the box was replaced by a newer connection with the same keys (two
  `hive serve` processes on one data dir). Stop one.

**The phone says the host is offline.** The relay has no live connection from
the box. Check `hive remote status` on the box.

**The phone says it was revoked, or its credential expired.** Pair it again
with `hive pair`. A revoked phone stays revoked in `hive phones list`.

**The phone can't reach the box after `hive phones revoke`.** Revoking also
rotated `serve.token`, so phones on the direct path must pair again. Use
`--keep-token` next time if that isn't what you want.

**No notifications.** Check, in order: `hive remote status` shows
`push available`; the phone is listed `active` in `hive phones list`;
notifications are allowed for the app in iOS Settings; and you've backgrounded
the app (the box doesn't push while the app is open and live). Run
`hive serve` with `HIVE_PUSH_DEBUG=1` to print each dropped push and relay
refusal. Relay-side APNs errors (a wrong topic or a sandbox/production
mismatch) are covered in
[`docs/push-notifications.md`](https://github.com/honeyhive-ai/hive/blob/main/docs/push-notifications.md).
