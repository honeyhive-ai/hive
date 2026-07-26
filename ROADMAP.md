# Hive roadmap

Working notes on where Hive is heading. Not a promise — a prioritized sketch.

## Now / near-term

- **Real-time push (kill the 3s poll).** The relay sync is HTTP request/response
  with client-side polling every ~3s, so the app feels up-to-3s laggy for a
  chat-shaped experience. Add server→client push so events land instantly.
  - **Step 1 — SSE.** Keep `POST` to send; add a one-way server-sent-events
    stream (`relay → client`) for new sealed events. Plain HTTP, auto-reconnect,
    proxy-friendly, far simpler than a full WebSocket protocol. Drops latency to
    ~instant. Lives in the Go relay + `sync_engine`.
  - **Step 2 — WebSocket (if needed).** Move to bidirectional WS + subscriptions
    only if duplex flows (presence, huddles) warrant it.
  - **Compatible with E2EE:** the relay fans out **opaque sealed blobs** by room;
    it never reads content. This is a transport upgrade, not a Nostr rewrite.

- **Chat-layer UI redesign.** Adopt the refreshed chat layer (role-tinted turns —
  human warm / agent cool / shared muted; refined avatars; composer with a
  runtime "route" pill + attachment chips). Design tokens map onto the existing
  Studio / Harbor / Midnight themes.

## Exploring

- **Subdomain communities (multi-tenant on one relay).** One relay hosts many
  communities addressed by subdomain (`acme.hive.example.com` → community `acme`),
  the URL authoritative for the workspace, tenant-scoped state. The relay is
  already multi-room; the work is (1) subdomain→community routing + per-community
  scoping in the **Go relay**, (2) client treats the community URL as the
  workspace root, (3) a model decision: *community-of-channels* vs the current
  *workspace-per-room*. Open questions: one shared community key vs per-channel
  keys; the directory becoming per-community (today it's per-relay).

## Deliberately not chasing

- Feature-parity with general team-chat platforms (mobile, voice huddles, git
  hosting, server-side search). Hive is **dev-first and content-blind (E2EE)** —
  the search/agent-over-history features that need a content-reading relay are a
  non-goal by design.

## Deferred security follow-ups

See [`docs/security-hardening-plan.md`](docs/security-hardening-plan.md): S1
pin-at-invite + p2p-path verification; S2 point-in-time authz; S4 relay-remove
rotation; S5 relay read-auth + metadata + key-at-rest.
