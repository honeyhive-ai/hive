# Social graph: friends, presence & P2P bootstrap (plan)

**Status:** implemented (P1–P6 shipped) · **Created:** 2026-06-20 · Kept as the
design reference for the friend/presence/P2P subsystem in `hive-relay::social`
and the runtime.

A plan for **adding a collaborator by GitHub username**: send a request, accept
it on any of your devices (dismissing it everywhere else), see each other's
online status, and have the accepted relationship **bootstrap a direct P2P link**
between the participating devices. It also lays out how the *same* identity
primitive scales up to **GitHub Teams** for the Enterprise tier, and how the
hosted version of the friend graph becomes a **lower paid (prosumer) tier**.

> One-line framing: *a friendship is a private, 2-person, GitHub-identified
> workspace.* We reuse the workspace/roster/peer-link machinery rather than
> inventing a parallel stack.

---

## 1. User stories

- **Add a collaborator.** I share Hive with a friend. We both sign in with
  GitHub. I type their GitHub username; they get a request.
- **Multi-device, single decision.** The request appears on *all* my friend's
  signed-in devices. They accept on one — it disappears on the others.
- **Presence.** Once connected, we each see whether the other is online.
- **Direct link.** Accepting establishes a P2P path between the device that sent
  the request and the device(s) that accepted, partially seeding the existing
  peer-link/iroh transport.
- **(Enterprise) Team-derived.** An org admin connects GitHub; org/team
  membership *auto-populates* the roster with roles — no manual add-friend.

---

## 2. What we already have (build on, don't reinvent)

| Primitive | Where | Reuse for |
|---|---|---|
| GitHub-anchored identity: `account_id = UUIDv5("github:<id>")` | runtime + relay | naming the friend you target; the request subject |
| Relay authenticates calls with the GitHub token (`X-Hive-Github-Token`) | relay | the relay can *vouch* a request truly came from `github:<id>` |
| Per-device keypairs; X25519 key-agreement pubkey published in the roster | runtime | sealing/auth between specific devices |
| Rendezvous + peer-link (iroh node-ids, UDP hole-punch, relay fallback) | `hive-runtime` transports | the actual direct data path once friends accept |
| Workspace = relay room + optional E2EE key + roster | runtime + relay | model a friendship as a 2-person workspace |
| `EntitlementPolicy` (Open/Tokens/SignedKey) + `WriteGuard` seam | `hive-relay` (+ private enterprise crate) | gate paid/enterprise behavior server-side |

The gap is **stateful, account-scoped social state** (who knows whom, which of my
devices are online) and a **realtime account channel** to fan requests/presence
out to every device. The room WS transport is per-room; we need a per-*account*
one.

---

## 3. New components

```
            ┌─────────────────────────── Relay (social service) ───────────────────────────┐
 client ───▶│  Account device registry   account_id → [{device_pubkey, node_id, last_seen}] │
 (signed in │  Friend graph store        pending requests + accepted edges (GitHub-authed)  │
  w/ GitHub) │  Account WS channel        per-account: deliver requests, dismiss, presence   │
            └───────────────────────────────────────────────────────────────────────────────┘
                    ▲  request/accept/reject (Authorization: github token)
                    │  WS subscribe: my account (all my devices) + my friends' presence
 client A ──────────┘
   │  on accept: exchange node-ids / ka-pubkeys ──▶ existing peer-link (iroh) ──▶ client B
```

1. **Account device registry (relay).** On sign-in each device registers under
   its `account_id`: `{device_pubkey, iroh_node_id, last_seen, label}`. This is
   what makes "appears on all my devices" and "dismiss everywhere" possible —
   state is keyed by *account*, not device. Devices heartbeat to refresh
   `last_seen`.
2. **Friend graph store (relay).** Edges (`accepted`) and `pending` requests,
   each row authenticated by the requester's GitHub token so the relay records a
   trustworthy `from = github:<id>`. Operations: `request(to_username)`,
   `accept(request_id)`, `reject(request_id)`, `remove(friend)`, `list()`.
3. **Account WS channel (relay).** A websocket scoped to the authenticated
   account. Pushes: incoming request → all my devices; `request_resolved` →
   dismiss on the others; `presence` deltas for my friends. Same transport family
   as room sync, new routing key.
4. **GitHub username → id resolution (client).** `GET /users/{username}` with the
   signed-in token → numeric id → `account_id`. The request targets the account,
   not the typed string (rename-safe).
5. **P2P bootstrap on accept (runtime).** Acceptance triggers a node-id /
   ka-pubkey exchange (via the relay, E2EE-sealed) between the sender's device and
   the accepting device, then hands off to the existing peer-link dialer. Other
   devices of either user can "promote" themselves later by registering +
   exchanging.

---

## 4. Data model (sketch)

Relay-side (new tables / store):

```
account_device(account_id, device_pubkey, node_id, label, last_seen)        PK(account_id, device_pubkey)
friend_request(id, from_account, to_account, from_github_login, created_at, state)   state ∈ {pending, accepted, rejected, cancelled}
friend_edge(account_a, account_b, since)                                    canonical-ordered pair, unique
```

Wire DTOs (ts-rs, surfaced over IPC):

```rust
struct FriendRequestDto { id: String, from_login: String, from_account: String, created_at: String }
struct FriendDto        { account_id: String, login: String, display_name: Option<String>, avatar_url: Option<String>, presence: Presence }
enum   Presence         { Online, Away, Offline }   // serde camelCase
```

Client IPC commands: `friend_request(login)`, `friend_requests()` (incoming),
`friend_accept(id)`, `friend_reject(id)`, `friend_list()`, `friend_remove(id)`;
events: `friend_request_received`, `friend_request_resolved`, `friend_presence`.

---

## 5. Security & privacy

- **Accept-gated.** A lookup only sends a signed request. **No** chat, presence,
  or device info flows to the other party until they accept. Pre-accept, the
  requester learns only "delivered/seen", never the target's device list.
- **GitHub-vouched origin.** The relay stamps `from = github:<id>` from the
  verified token, so requests can't be spoofed to look like another user.
- **Presence is opt-in & friends-only.** You only expose presence to accepted
  edges; default to account-level ("online") not device-level to avoid leaking
  activity patterns. A "appear offline" toggle.
- **E2EE on the P2P path.** Node-id/ka-pubkey exchange is sealed to the specific
  device keys (reuse HPKE-per-device); the relay sees ciphertext for the
  handshake, never the friendship's content.
- **Abuse controls.** Rate-limit outbound requests; block / report; a hard cap on
  pending requests; requests expire.
- **Self-host parity.** The endpoints are part of the open relay *protocol* so a
  self-hosted relay can implement them; the hosted convenience is the paid wedge
  (§7), not a locked protocol.

---

## 6. Phases

Each phase is independently demoable and testable (mirror the existing relay
integration-test style: two clients against a local relay).

- **P1 — Account device registry + account channel.** ✅ *Relay + client SDK
  done (2026-06-20).* `hive-relay::social`: per-account device registry
  (`device_id → {node_id, label, last_seen}`) keyed by `github:<id>`, a login→
  account index, and an append-only **inbox polled with an `after` cursor** as
  the account channel — the idiomatic fit here (everything else polls), delivering
  the same fan-out/dismiss behavior; a websocket push layer can sit over the same
  inbox later. Endpoints: `POST /v1/account/{register,heartbeat}`,
  `GET /v1/account/{inbox,devices}` (GitHub-token auth, behind the entitlement
  gate). `RelayClient::account_*` mirror them. Unit-tested in `social.rs`
  (shared inbox across two devices, monotonic seq, heartbeat, empties).
  *Deferred to P3:* wiring the app to call `account_register` on GitHub sign-in
  and run the heartbeat loop (the heartbeat cadence belongs with presence).
- **P2 — Friend request lifecycle.** ✅ *Relay + client SDK done (2026-06-20).*
  `hive-relay::social` friend graph: `create/accept/close(reject|cancel)` +
  `remove` + `list_friends`/`incoming_requests`; canonical-ordered edges;
  idempotent duplicate requests; self/already-friends guards; **free-tier cap
  via `with_friend_cap(Some(5))`** (unlimited when unset = self-host). Origin is
  GitHub-vouched (relay stamps `from` from the verified token); target resolved
  by `@username` against the login index (404 if they haven't signed in →
  username-only discovery). Accept/reject push a `friendResolved` event to *both*
  accounts' inboxes, so the pending UI dismisses on every device. Endpoints under
  `/v1/friends*`; `CapReached` → HTTP 402 (upgrade prompt). `RelayClient`
  friend_* methods + `FriendRequestOutcome`/`Friend`/`IncomingFriendRequest`.
  13 unit tests in `social.rs` (symmetric edge, recipient-only accept, idempotency,
  reject vs cancel, remove, cap blocks request *and* accept, event shape).
- **P3 — Presence + abuse controls.** ✅ *Relay + client SDK done (2026-06-20).*
  Account-level presence (§9) derived from the freshest device heartbeat:
  `online` ≤70s, `away` ≤300s, else `offline`; an "appear offline" flag forces
  offline. `GET /v1/friends/presence`, `POST /v1/account/visibility`;
  `RelayClient::friend_presence` / `set_visibility` + `FriendPresence`. Abuse
  controls: pending requests **auto-expire after 14 days** (dropped from incoming
  + no longer block re-requests) and a **50 simultaneously-pending-outbound cap**
  (`TooManyPending` → HTTP 429). 5 new unit tests (state mapping, heartbeat→
  online/away/offline, appear-offline override, expiry, outbound cap). *Deferred
  to P5 integration:* the app's register-on-sign-in + background heartbeat loop +
  the visibility toggle UI (wired with the rest of the app/IPC/UI in one pass).
  *Remaining P3 follow-ups:* block/report + per-account send rate-limit.
- **P4 — P2P bootstrap (discovery).** ✅ *Relay + client SDK done (2026-06-20).*
  Friend-gated discovery of a friend's dialable node ids: `GET
  /v1/friends/:account/devices` returns the friend's `{device_id, node_id}` only
  when the caller and target are accepted friends (else 403), so node ids aren't
  disclosed to strangers. `RelayClient::friend_devices`. Unit-tested (visible
  only between friends; strangers and third parties get `None`). *Deferred to P5
  integration:* feeding those node ids to the existing iroh `PeerLinkService`
  dialer on accept, with relay-forward fallback when hole-punch fails — this is
  app wiring over transport that already exists, not new protocol.
- **P5 — UI + app integration.** ✅ *Done (2026-06-20).* App IPC commands
  (`friends_overview`, `friend_send_request`/`accept`/`reject`/`remove`,
  `friend_set_visibility`) over the relay client; device **registers on GitHub
  sign-in** (piggybacked on `directory_register`) and `friends_overview`
  heartbeats (re-registering if unknown) so presence stays fresh. `FriendsView`
  (rail "☺" button → friends view): add-by-username, incoming requests with
  accept/decline, friends list with presence dots (green/amber/muted), remove,
  and an "appear offline" toggle; the cap (402) shows an upgrade hint. React
  polls `friends_overview` every 20s. ipc.ts wrappers + `FriendsView.test.tsx`
  (presence colors, outcome messages). tsc clean, 45 web tests green.
- **P6 — Friendship = workspace (DMs).** ✅ *Done (2026-06-20).* "Message" on a
  connected friend lazily provisions a private 2-person workspace: a
  **deterministic room** (`dm-<loId>-<hiId>` from the sorted GitHub ids, so both
  sides converge) where the lexicographically-smaller account "owns" the DM — it
  mints the E2EE key and seals it to the friend's devices via the relay keyring
  (the proven `invite_by_handle` path); the other side receives the key from the
  keyring (the existing invitee path, no passphrase). DMs carry a `dm_account`
  flag, are **excluded from the workspace rail** (rail stays workspace-only) and
  listed via `list_dms`; opening one switches the active workspace so the chat
  list scopes to it. `friend_open_dm`/`list_dms` IPC + a "Message" button in the
  Friends section that navigates to the chat. *Not runtime-verified headless:*
  the DM key handshake reuses verified invite/keyring code, but the end-to-end
  two-device DM exchange should be smoke-tested in-app.

Abuse-control hardening (rate limits, expiry, block/report) rides alongside
P2–P3.

---

## 7. Tiering: where each piece lives

Following [`tiering.md`](../docs-site/ops/tiering.md)'s decision tree — the social graph is
**stateful server state**, so it's a relay/cloud capability; the client UI and
protocol are free.

| Layer | Tier | Notes |
|---|---|---|
| Friend/presence **protocol** + client UI | **Free / OSS** | shipped in the one OSS client + reference-relay endpoints; self-hosters get it by running their own relay |
| **Hosted** friend graph (sign in with GitHub, add anyone, no relay setup), capped | **Cloud (free)** | the on-ramp; **≤ 5** active collaborators, presence, 1:1 only |
| Unlimited collaborators, **friend groups**, persistent presence/history, priority P2P relays (TURN) | **Cloud (paid) — the lower/prosumer tier** | individual/prosumer monetization *before* an org needs full RBAC; driven by the existing signed-entitlement token (member cap / TURN flag already modeled) |
| **GitHub Teams → roster + roles** (org/team auto-membership, GHEC SSO) | **Enterprise** | the same identity primitive at org scale — see §8; lives in the private `hive-relay-enterprise` crate behind `--features enterprise` |

Why this is clean: **one primitive, three price points.** "Add a person by their
GitHub identity" is free as a protocol; *hosted+unlimited* is the prosumer paid
wedge; *org/team-derived+RBAC* is enterprise. No crippleware — self-host always
works; paying buys hosting, scale, and org automation.

The entitlement token already carries `max_members` and capability flags
(`token.rs`), so the paid-tier caps (collaborator count, TURN access) are
enforced server-side with the mechanism that exists — no new gate type.

---

## 8. Leveraging GitHub Teams for Enterprise

The consumer flow is "I name one friend." The enterprise flow inverts it: **the
org's GitHub structure *is* the roster**, no manual adds.

- **Org/team sync.** With GHEC SSO, the enterprise relay reads `GET /orgs/{org}/teams`
  and team membership (GitHub App / OAuth with `read:org`). Each GitHub **team**
  maps to a Hive **role group**; each member's `github:<id>` is auto-enrolled into
  the workspaces that team owns. Joining/leaving the GitHub team adds/removes the
  Hive collaborator automatically (webhook `membership`/`team` events → roster
  delta).
- **Team → role mapping.** A config maps `@org/eng-leads → admin`,
  `@org/contractors → contributor (read+propose, no merge)`, etc. This is the
  "GHEC org/team → role mapping" row already anticipated in `tiering.md`, realized
  on top of the same `friend_edge`/membership store — an edge whose *source* is a
  team rule rather than a hand-sent request.
- **Presence at org scale.** The same presence channel shows "who on the team is
  online", scoped by team visibility rules.
- **Governance.** Because membership derives from GitHub, deprovisioning is
  automatic (offboard in GitHub → access revoked in Hive), which is the audit/SOC2
  story enterprises want. Pairs with the existing audit-export / `WriteGuard`
  enforcement.
- **Where it lives.** The GitHub-org connector (App, webhook handler, team→role
  policy) is the kind of proprietary integration that belongs in the private
  `hive-relay-enterprise` crate (`--features enterprise`), depending on the OSS
  `hive-relay` membership/`WriteGuard` seam. The OSS relay keeps the manual
  friend flow; enterprise adds the automatic team-derived one.

**Migration story:** a solo user on the prosumer tier who later joins/forms an
org keeps the same `github:<id>` identity and friend graph; the org simply layers
team-derived membership on top. Nobody re-onboards.

---

## 9. Decisions (settled 2026-06-20)

1. **Presence granularity → account-level.** Surface "online/away/offline" per
   account, not per device. Avoids leaking activity patterns and keeps presence
   deltas small.
2. **Friendship model → reuse-as-workspace.** A friendship is provisioned as a
   private 2-person workspace (room + E2EE key), so "message a friend" reuses the
   existing chat/roster/sync/E2EE stack (§6 P6) — no parallel DM stack.
3. **Free-cloud cap → 5 active collaborators.** Cloud-free allows up to **5**
   accepted collaborators; the prosumer paid tier lifts the cap (and adds groups /
   persistent presence / TURN). Enforced via the entitlement token's `max_members`.
4. **Discovery → username-only.** You can only reach someone by their exact
   GitHub username; no email/handle enumeration. (Revisit if there's demand.)
5. **Self-host store → same `hive-relay` process, feature-flagged off.** The
   social tables live in the relay process behind a flag (default off), so minimal
   relays stay minimal and self-hosters opt in without a second service.

---

## 10. Relationship to existing docs

- Identity & hosted-relay roadmap: [`managed-service-plan.md`](../docs-site/ops/managed-service.md)
- Tier placement rules: [`tiering.md`](../docs-site/ops/tiering.md)
- Server-side membership & the `WriteGuard` seam: [`hive-server-side-membership.md`](../docs-site/ops/server-side-membership.md)
- P2P transport internals: [`architecture.md`](architecture.md), [`multiuser.md`](multiuser.md)
