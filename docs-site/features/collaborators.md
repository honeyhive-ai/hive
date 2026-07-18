# Collaborators, presence & DMs

Beyond sharing a workspace, Hive lets you connect with people directly by their
**GitHub username** — see who's online, and message them one-to-one. It's the
consumer-grade side of Hive's identity model; the workspace-level sharing flow is
covered in [Invites & joining](invites.md).

![Collaborators](../images/collaborators.png){ width="820" }

## Add someone by GitHub username

1. Sign in with GitHub (Settings → **Account**) — this is your identity across
   every device.
2. Open the **Friends** view from the **☺** button on the left workspace rail.
3. Type a teammate's GitHub username and **Add**.

They receive a request on **every device** they're signed in on. Accepting on one
device dismisses it on the rest. Nothing about you syncs until they accept —
looking someone up only sends a signed request, stamped with your verified GitHub
identity so it can't be spoofed.

> **Username-only.** You can only reach someone by their exact GitHub handle (no
> email/enumeration), and they must have signed into Hive at least once.

## Presence

Once connected, each friend shows a presence dot:

- **● online** — active in the last ~minute
- **● away** — seen in the last few minutes
- **○ offline** — older, or appearing offline

Presence is **account-level** (not per-device) and only visible to accepted
friends. Flip **Appear offline** in the Friends view to stay dark regardless of
activity.

## Direct messages

Click **Message** on a connected friend to open a private 1:1 conversation. Under
the hood a DM is just a **two-person workspace** with its own end-to-end
encryption key, so it reuses the same chat sync you already know — the relay only
ever sees ciphertext. DMs live in the Friends section, not the workspace rail.

## How it syncs

- **Identity** is your GitHub account (`github:<id>`), shared across your devices;
  commits Hive makes on your behalf are attributed to you.
- **Requests, presence, and DM keys** flow through a relay you can
  [self-host](../networking/self-host.md) (or Hive Cloud). Everything sensitive is
  end-to-end encrypted.
- Once two devices are connected, Hive will also try a **direct peer-to-peer**
  link (see [Rendezvous](../networking/rendezvous.md) and
  [TURN fallback](../networking/turn.md)), falling back to relay forwarding.

## Free-tier limit

On the hosted free tier you can have up to **5** active collaborators; paid plans
lift the cap (see [pricing](../ops/pricing.md)). Self-hosting your own relay has
no cap.

## Where this differs from workspace invites

| | Collaborators (this page) | Workspace invites |
|---|---|---|
| Reach by | GitHub username | a shared link / short code |
| Scope | 1:1 (presence + DMs) | a whole shared workspace + roster |
| Set up in | the **Friends** view (rail ☺) | the **People** pane (right rail) |

See also: [Identity & devices](../concepts/identity.md),
[Invites & joining](invites.md).
