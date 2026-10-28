# Push notifications (iPhone)

`hive serve` can wake the Hive iPhone app with a notification when an agent
finishes a reply, someone mentions you, a change is ready for review, or a
workflow waits for your approval. The notification content is end-to-end
encrypted: the relay and Apple see only ciphertext. The wire format is
[`relay-tunnel.md` §14](relay-tunnel.md#14-end-to-end-encrypted-push); this
page covers what the host sends, when, and how to turn it on for your own
setup.

Push needs remote access through a relay ([`ios-app.md`](ios-app.md#remote-access-through-a-relay-optional-off-by-default)).
Without it, `hive serve` can still notify through a self-hosted ntfy server
(`--ntfy-url` / `--ntfy-topic`), which sends no message content at all.

## How it works

```
hive serve ──(control WebSocket)──▶ relay ──(HTTP/2, APNs key)──▶ APNs ──▶ iPhone
  encrypts per phone                 forwards ct                       Notification Service
  with that phone's push key         + the phone's APNs token          Extension decrypts and
                                                                       shows title/body
```

1. When a phone pairs, both ends derive a 32-byte **push key** from the
   enrollment handshake. The host keeps it in `tunnel/phones.json`; the app
   keeps it in a Keychain access group shared only with its Notification
   Service Extension.
2. On an event, `hive serve` builds the plaintext below and seals it for each
   phone that wants pushes: `ChaCha20-Poly1305(push_key, nonce, plaintext,
   ctx("push", host_id, pid))`. It sends `{"t":"push","pid","ct","prio","collapse"}`
   on its control connection.
3. The relay looks up the APNs token the phone registered when it connected,
   and sends APNs a generic alert ("Hive" / "New activity on your host") with
   `mutable-content: 1` and the ciphertext.
4. The app's Notification Service Extension decrypts it with the push key,
   checks `ts` (within 1 h) and `seq` (strictly increasing, so a replay shows
   only the generic text), and replaces the title and body. Tapping it opens
   `hive://chat/<chatId>`. If anything fails, the generic text stays.

## Events and payload

Plaintext (JSON) of every push:

| Field | Type | Present | Meaning |
|---|---|---|---|
| `v` | `1` | always | format version |
| `seq` | u64 | always | per (host, phone), strictly increasing, survives restarts |
| `ts` | seconds | always | when the host sent it |
| `kind` | string | always | see below |
| `title` | ≤ 64 bytes | always | names the sender and the event, **never** message text |
| `body` | ≤ 512 bytes | always | the preview, or `""` with previews off |
| `chatId` | UUID | chat events | deep link `hive://chat/<chatId>`; also the collapse key (HMACed) |
| `sender` | ≤ 64 bytes | `message.done`, `mention` | the agent or member name alone |
| `proposalId` | UUID | `proposal.created`, `workflow.approval` | the proposal to review |
| `workflowRunId` | UUID | `workflow.approval` | the run waiting on the gate |
| `workflowId` | UUID | `workflow.approval` | the run's workflow definition |
| `preview` | `true` | when `body` is a content preview | |

Unknown fields and kinds must be ignored by the app (an unknown kind shows
`title`/`body` and links `chatId` if present).

| `kind` | When | `title` | `body` (previews on) |
|---|---|---|---|
| `message.done` | an agent reply finished (not your messages, not a reply that mentions you) | `coder replied`, or `coder hit an error` for a failed turn | first 120 characters of the reply |
| `mention` | a message from an agent or another member `@mentions` you | `Ana mentioned you` | first 120 characters of the message |
| `proposal.created` | a proposal is waiting for review (not a workflow gate's) | `Change ready for review` | the proposal title, cut the same way |
| `workflow.approval` | a workflow run's approval gate is waiting (again on each rerouted attempt) | `<workflow name> needs approval` | the gate's stage name |
| `phone.paired` | a new phone was paired with this host | `New phone paired` | the phone's name (always shown) |
| `test` | reserved for a test push | | |

Rules:

- **One notification per event.** A reply that both finishes and mentions you
  is one `mention`. A workflow gate is announced once as `workflow.approval`,
  not also as `proposal.created` (its backing proposal is recognised by the
  run's `proposalId`, or as an authorless decision when the run hasn't synced
  in yet).
- **"You"** is this host's identity (the one the phone posts as) and any
  member that shares its account or its display name, such as your desktop.
  A mention is `@<your name>` (or `@<name>#<n>`), `@you` / `@all`, or a role
  group you're in (`@owners`, `@admins`, …). Your own messages never notify.
- **Only news.** Replies, proposals and gates created more than 10 minutes
  before `hive serve` started (history that synced in late) don't push, and a
  restart never replays.

## Per-phone settings

Each paired phone has two preferences, set by the app through the tunnel-only
route `PUT /v1/tunnel/push {"enabled": bool, "previews"?: bool}`:

- `enabled` (default off until the app asks for permission): pushes go only
  to active (not revoked) phones with `enabled` on.
- `previews` (default on): with previews off, `body` is empty and `preview`
  is absent, so the notification says only who did what ("coder replied").
  The phone's settings screen has the toggle. If an encrypted push with its
  preview would exceed 2 048 bytes, the host sends it without the preview.

## Host-side limits

On top of the relay's limits (§11), `hive serve` drops a push to a phone when:

| Rule | Limit |
|---|---|
| Foreground | the phone has an open `/v1/events` stream through the tunnel. The app keeps that stream open only while it's in the foreground and sees the event live. The stream counts until the phone cancels it (RESET), the session ends, or the relay drops the connection |
| Coalescing | one push per (phone, kind, chat) per 10 s: the first of a burst goes, the rest are dropped |
| Per phone | 20 per minute, 500 per day |
| Host-wide | 1 per second sustained, burst of 5 (the relay's "pushes ≤ 1/s") |

A dropped push doesn't count against any limit. Drops are logged at debug
level only: set `HIVE_PUSH_DEBUG=1` to print each drop and relay refusal on
stderr.

The relay acks each push. On `ok:false` the host:

- `push_unavailable`: turns push off until the next relay connection (whose
  `welcome` reports `push`) and says so once on stderr;
- `no_push_token`: holds pushes to that phone for 10 minutes, or until the app
  connects again (it registers its token when it connects);
- `revoked`: holds pushes to that phone for 1 hour;
- `rate_limited`: holds all pushes for 60 s;
- `push_failed`: nothing; that one push is lost.

Nothing is retried: a notification is a hint, and the app catches up through
`/v1/events` when it opens.

## Turning push on for yourself

APNs only accepts a push for an app when it is signed with an APNs key from
the Apple developer team that publishes that app. So push works when you run
the relay with your own key **and** build and sign the iOS app yourself under
the same team.

### 1. Apple Developer portal

1. In **Certificates, Identifiers & Profiles → Keys**, create a key with
   **Apple Push Notifications service (APNs)** enabled. Download the
   `AuthKey_<KEYID>.p8` file. Apple lets you download it only once: keep it
   secret, like a password. Note the **Key ID** and your **Team ID** (top
   right of the portal, or Membership details).
2. On the app's identifier(s) (your development bundle id, and the release
   bundle id if you ship one), enable the **Push Notifications** capability
   (and register the extension's id, `<bundle id>.notifications`). With
   automatic signing in Xcode, signed in with an Apple ID of that team
   (Xcode → Settings → Accounts), this happens for development builds when
   you build with push turned on (step 4). Without an account, Xcode falls
   back to a wildcard profile, which can't carry Push, and the push build
   fails to sign.

### 2. Run the relay with the key

Use the open relay ([honeyhive-ai/relay](https://github.com/honeyhive-ai/relay),
`cmd/hive-relay`) with tunnels on and the APNs settings:

```sh
HIVE_RELAY_TUNNEL=open                      # or allowlist + HIVE_RELAY_TUNNEL_HOSTS=<host id>
HIVE_RELAY_APNS_KEY_FILE=/path/to/AuthKey_KEYID.p8   # or HIVE_RELAY_APNS_KEY=<inline PEM>
HIVE_RELAY_APNS_KEY_ID=<Key ID>
HIVE_RELAY_APNS_TEAM_ID=<Team ID>
HIVE_RELAY_APNS_TOPIC=<the app's bundle id>
HIVE_RELAY_APNS_ENV=auto                    # optional: auto | sandbox | production
```

`auto` uses the environment each phone reports: `development` for Xcode
(Debug) builds, `production` for TestFlight and App Store builds. Keep the
`.p8` file readable only by the relay's user, and don't commit it.

Check it: `GET /v1/tunnel/info` on the relay reports `"push": true`.

### 3. Point the host at the relay and pair

```sh
hive remote enable --relay https://<your relay>
hive serve                  # or restart the service
hive pair                   # scan with the phone
hive remote status          # "push available" once connected to a relay with a key
```

### 4. Build the app with push on

In the iOS app's repository (`hive-ios`), set in `Config/Local.xcconfig`:

```
HIVE_PUSH_NOTIFICATIONS = YES
```

plus your own team and bundle id overrides (see `Config/Local.xcconfig.example`).
Build and install, open the app, and allow notifications when asked. The app
then registers its APNs token with the relay and turns `enabled` on for this
host. **Settings → Notifications** in the app toggles previews.

### 5. Troubleshooting

- `GET /v1/tunnel/info` says `"push": false`: the relay has no APNs key
  configured, or failed to load it (check its log). The host then turns push
  off and falls back to ntfy if configured.
- **`DeviceTokenNotForTopic`** in the relay log: `HIVE_RELAY_APNS_TOPIC` isn't
  the bundle id of the build on the phone. The relay forgets that token; fix
  the topic, then open the app so it registers again.
- **`BadDeviceToken`**: sandbox vs production mismatch, e.g. an Xcode build
  sent to the production endpoint. Use `HIVE_RELAY_APNS_ENV=auto`, or match it
  to the build.
- Nothing arrives while the app is open: expected, the foreground stream
  suppresses pushes. Background the app and try again.
- Nothing arrives at all: check that the phone is paired and active
  (`hive phones list`), that notifications are allowed for the app in iOS
  Settings, and run `hive serve` with `HIVE_PUSH_DEBUG=1` to see dropped pushes
  and relay refusals.

## Limitation: other people's setups

Because APNs ties pushes to the publisher's key, a self-hosted relay can push
only to builds of the app signed by the same Apple team as the relay's key.
Someone running their own relay can't push to the App Store build of the app;
they need to build and sign the app themselves with their own team, bundle id
and key (steps 1–4). A hosted push gateway that self-hosted relays could
forward ciphertext to is described in §14 as a possible later addition and is
not built.
