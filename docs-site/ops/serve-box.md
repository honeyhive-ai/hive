# Running hive serve as your own device

`hive serve` is the always-on Hive device your [phone](../features/phone-app.md)
talks to. Out of the box it joins the workspace as **its own member**. Since
v1.16 you can instead enroll it as **one of your own devices**, so everything
you do from the phone is authored as you.

This page covers that enrollment, how the box's online status works, applying
approved changes on the box, and the v1.16 sync change every upgrading
workspace should know about. For reaching the box from the phone, see
[Remote access for your phone](../networking/remote-access.md).

## Its own member, or your device

| | Its own member (default) | Your device (`HIVE_SERVE_OWNER`, approved) |
|---|---|---|
| Phone messages show as | `HIVE_SERVE_NAME` (for example "Phone") | you |
| Votes and review notes | a second voter, at the box's role (Contributor when it enrolled itself) | yours: one person, one vote |
| Chats the phone creates | owned by the box; nobody answers `@hive` in them | owned by you; your desktop answers `@hive` |
| Who signs events | the box's own account | the box's device key, under a certificate from your account |

The box's keys never leave it, and your account key never leaves your desktop.
The desktop signs a certificate for the box's device key; the box can't create
that certificate itself.

## Enroll the box

1. **Name the owner.** In the box's environment (the service's env file), set
   `HIVE_SERVE_OWNER` to your display name in the workspace, or your actor id
   if the name isn't unique, and restart `hive serve`:

    ```sh
    HIVE_SERVE_OWNER=alice
    ```

2. **Get the code.** On start, `hive serve` prints an enrollment code
   (`hivedev1:…`) and a fingerprint. To print it again, run
   `hive serve enroll` with the same environment.
3. **Approve it on your desktop.** **Settings → Account → My devices → Add a
   device**, paste the code, check that the fingerprint matches what the box
   printed, and approve.

The box notices the certificate on its next sync and acts as you from then on.
The phone's `/v1/health` reports `identity.mode` `owner`. Approving also
removes the box's separate member, if it had one.

!!! info "Before approval"
    Until you approve it, a box with `HIVE_SERVE_OWNER` set that isn't a
    member yet waits read-only (`pending`) instead of joining as one. A box
    that is already a member keeps working as that member meanwhile, and if
    it's an Admin, desktops already treat it as your device for answering
    `@hive`.

!!! danger "A box can't make itself your device"
    An event the box stamps as you without a valid certificate is quarantined
    on every device as impersonation.

## What changes once it's your device

- **Messages, votes, comments and chats** from the phone are yours. A vote
  from your desktop replaces one from your phone, so you count once toward
  quorum.
- **Your desktop answers `@hive`** in chats the phone creates. Only a desktop
  runs a chat's primary runtime, so the desktop must be open. While it's
  closed, the turn waits and is answered when it opens. The phone's chat
  details show whether your desktop is available.
- **Older messages follow you.** Messages the box wrote as its own member
  before enrollment read as yours on the phone (for example "Alice via Phone")
  and as "*you* via *box*" on the desktop. Chats it created stay answered by
  your desktop.
- **No phantom person.** The retired box member isn't listed as another
  person in the chats it created.

### Revoke it

On the desktop, **Settings → Account → My devices → Revoke**. Updated desktops
stop accepting the box's events, and the box goes read-only
(`identity.mode` `revoked`). To enroll it again, make a new device key and
approve its new code:

```sh
hive serve enroll --new-device
# restart hive serve, then approve the new code on the desktop
```

!!! warning "Upgrade every desktop"
    Enrollment needs no new event type, so older desktops also accept the
    enrolled box's events as yours. But only updated desktops keep honoring a
    revocation after the sync batch it arrived in.

## Host liveness

Hosts (the `hive serve` box, `hive worker` boxes, desktops) show as online or
offline in the phone's Hosts list, agent host badges, the desktop's Hosts
list, `hive queue` and chat details. The same signal decides whether a desktop
defers an agent's turn to its host.

Since v1.16 this rides the relay's **presence channel**, not the event log.
Each running host sends a beat every 30 seconds to an ephemeral slot in the
room, sealed under the workspace key so the relay sees only ciphertext. A
host is online when it has beaten within the last 90 seconds.

Earlier versions re-wrote a signed host record about once a minute, which
added roughly 1,500 durable events a day per always-on box to every member's
log. The durable host record is now written only when something real changes
(a new host, or a new label, kind or owner), and a restart writes nothing.

!!! note "Mixed versions"
    Older clients read liveness from the durable record, which upgraded hosts
    no longer refresh. They show upgraded hosts as offline. Nothing breaks,
    but upgrade every device to see the right status.

## Apply on approve

By default, approving a file-diff proposal from the phone records the vote
and nothing else: applying it is the desktop's **Implement**. To apply on the
box instead, set:

```sh
HIVE_SERVE_APPLY_ON_APPROVE=1
```

An approved diff is then committed to the chat's branch (`hive/chat/<id>`) in
the checkout bound on the box (`hive bind-repo`), the way the desktop's
Implement does. Only a proposal whose status is `approved` applies; one that
was rejected by a qualifying reviewer, dismissed or superseded never does.

## v1.16: a relay room carries only its own workspace

**Every release with relay sync up to v1.15.1** uploaded the events of *all*
the workspaces on a device into whichever relay room it was connected to.
Those events were sealed under that room's key, so other members of that room
could read them.

v1.16.0 fixes this on the desktop, `hive serve` and `hive worker`:

- A device pushes only the events of the room's own workspace.
- Events of any other workspace that arrive from the relay (pushed by a device
  that hasn't upgraded) are dropped, never stored.
- Direct peer-to-peer sync carries only the connected room's workspace, both
  ways, and a headless worker answers mentions only in its own workspace.
- **One-time cleanup.** On first start after upgrading, each device deletes
  the events of any workspace it doesn't belong to and never wrote anything
  in. It backs up the database first, and deletes nothing unless the backup
  succeeds. A workspace with any event this device signed is always kept.

### What to expect and do when upgrading from 1.15 or earlier

- **Upgrade every device**, desktops, `hive serve` and `hive worker` boxes
  alike. An older client keeps leaking its other workspaces into the room
  until it's upgraded; upgraded peers just drop what it sends.
- **A cleanup message on first start.** If the cleanup removed anything, the
  device logs how many events of how many workspaces it removed, and where
  the backup is: `hive.db.bak-foreign-purge-<timestamp>` next to `hive.db` in
  the data dir. Once you're satisfied nothing you need is gone, you can delete
  the backup. If there was nothing to remove, there's no message and no
  backup.
- **Rejoining a dropped room starts from scratch.** If the cleanup removed a
  room's workspace, its sync position goes too, so joining that room again
  later pulls its history from the start.
- **Data already on a relay stays there.** Events that older versions
  uploaded remain on the relay, encrypted under that room's key. If a device
  of yours held other workspaces while connected to a room shared with other
  people, assume the members of that room could read those events.
- **Hosts may look offline** on devices that haven't upgraded yet (see
  [Host liveness](#host-liveness)).
