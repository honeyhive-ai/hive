# LAN discovery

> **Roadmap — not in the current build.** Zero-config local peer discovery
> (mDNS / Bonjour `_hive._tcp`) is a tracked follow-up. It is **not implemented
> today.**

## What it will do

When two Hive instances are on the same network, they'll be able to find each
other without any server — advertising a small, opt-in record that carries
**identity material only** (workspace id/name + the inviter's account/device id,
display name, and signing public key — never workspace content, member lists, or
credentials). A discovered peer's join would go through the **same accept-invite
+ signature-verified envelope sync** as a pasted invite link, so the trust model
is identical to invites today.

## What works now

For sharing a workspace — whether peers are on the same LAN or across the
internet — use the **relay**: same trust model, works everywhere. See
[Self-hosting a relay](self-host.md) and [Invites & joining](../features/invites.md).
