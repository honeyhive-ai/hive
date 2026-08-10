# Transport

Implements MC-002 §3 and MC-001 §3. Normative.

## v1: LAN-only

v1 ships exactly one transport implementation, `LanTransport`. The relay ships in **v1.1**, default
off, self-hostable. The interface below is designed now so the relay is a drop-in — a second
`CompanionTransport` implementation and nothing else.

## The invariant that makes the relay cheap later

**The Noise session is established end-to-end between phone and desktop, over whatever carrier is
in use.** Encryption is never a property of the carrier. A transport moves opaque frames and knows
nothing about their contents.

This is why the relay can be added without touching approvals, sync, or the API layer, and why the
relay — when it arrives — is a dumb forwarder that cannot read, alter or selectively drop
application traffic without detection.

## Interface

See [`../contracts/transport.ts`](../contracts/transport.ts) for the authoritative TypeScript.

```
CompanionTransport
  discover(signal)          -> AsyncIterable<Candidate>
  connect(candidate, keys)  -> Promise<SecureSession>

SecureSession
  request(op, params)       -> Promise<Result>      // typed, allowlisted ops only
  subscribe(topic)          -> AsyncIterable<Event> // live stream
  close(reason)
  readonly info: { transport: 'lan' | 'relay', deviceId, since }
```

Rules for any implementation:

- Frames are fixed-header + opaque ciphertext. No transport parses application payloads.
- A transport may not add an operation, alter a payload, or expose a raw send. `request` is the only
  write path.
- `info.transport` is informational — for a status indicator — and must not gate any security
  decision. Approval tiers are enforced on the desktop and do not vary by carrier.

## LAN discovery

- mDNS / DNS-SD, service type `_hive-companion._tcp`, plus the `addrs` hints from the pairing QR.
- TXT records carry a protocol version and a **key fingerprint hint** only. Discovery data is never
  trusted: a candidate is just an address to attempt a handshake against, and the pinned static key
  decides whether that handshake succeeds.
- Multiple candidates are attempted concurrently; the first completed handshake to the pinned key
  wins and the rest are cancelled.
- iOS requires the local-network permission and `NSBonjourServices`; Android requires
  `NSD`/`NsdManager` plus a multicast lock. Both are requested at pairing, not at launch.

## Reconnect and backfill

- WebSocket carrier with exponential backoff, jittered, capped at 30 s; immediate retry on network
  or foreground transitions.
- Every stream is cursor-based. The phone persists the last applied cursor per topic and resumes
  with it; the desktop replays from that cursor or answers `cursor_too_old`, which forces a full
  refetch of that view. Gap-free-or-refetch — the phone never renders a transcript it knows has a
  hole in it.
- Live-stream loss degrades the UI to "last synced HH:MM", never to stale content presented as
  current. Pending approvals are re-fetched on every reconnect before the inbox is interactive.

## Offline behaviour

- Reads are served from cache with an explicit staleness marker.
- Composed messages queue locally and send on reconnect, marked *queued* in the transcript.
- **Approvals never queue.** An approval submitted against an unverifiable current record hash is
  meaningless; the control is disabled offline. Nothing decides in the background on the human's
  behalf.

## Relay (v1.1, specified now, not built)

Recorded so the interface does not have to change later:

- Default off. Self-hostable. Opt-in per device, with the transport shown in the UI whenever active.
- Dumb forwarder of opaque end-to-end-encrypted frames. Holds no key material and can decrypt
  nothing.
- **Size padding and batching are mandatory on the relay path.** Without them, frame sizes and
  timing leak the shape of activity — how many approvals, how large a diff, when a human is at the
  keyboard — to an operator who cannot read a byte of content.
- Relay-specific work not started in v1: rendezvous authentication, abuse limits, operator
  threat model, and whether relay presence alone justifies further restricting Tier 2.
