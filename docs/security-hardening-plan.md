# Security hardening plan (post-convergence)

Status of the sync-correctness program and the vetted design for the **security
phase**, which is deliberately *not* shipped as rushed code: each item below
needs a foundational piece (device-key distribution, OS keychain, key epochs)
that must be reviewed before implementation. This doc is the executable plan.

## Where we are

Sync-convergence program — **done** (merged/open PRs):

| Phase | What | PR |
| --- | --- | --- |
| 1 | Forward-compatible event decoding (`Unknown` variant, resilient load) | #34 |
| 2 | Canonical device-independent fold order (Lamport + `(lamport,event_id)`) | #34 |
| 3 | Convergence property test (500-permutation) | #34 |
| 4 | Loss-free proposal votes (`ProposalVoteCast` delta) | #34 |
| 5 | Project the full canonical stream on load | #34 |
| 6 | Durable **causal** Lamport clock + v2 signature preimage (binds lamport/actor/timestamp/scope) | #35 |
| 7 | Loss-free relay delivery + fetch durability (cursor doesn't skip undecodable; push rollback) | #36 |
| 8 | Snapshot compaction safety (seed from earliest snapshot) | #37 |

The north-star invariant — *same event set → same projected state on every
device* — holds and is property-tested. What remains is **security**, the
weakest area per the architecture review. None of it is a quick fix; the
ordering below reflects hard dependencies.

---

## S1 — Wire signature verification into ingest (GitHub-anchored identity)

> **Status — implemented and activated (default: relay-vouched via GitHub).**
> Layers 1–3 (below) plus **Layer 4 activation**: `ChatService::publish_identity`
> emits this device's trust events (`AccountKeyRegistered` + a `DeviceCertificateAdded`
> re-issued under the account's *current* id) on chat create / join, idempotently,
> so peers verify its signatures. The account-id switch (GitHub sign-in) is handled
> — the cert re-issues under the new id and `ensure_self_member` re-adds the member
> under it. **Residual (narrow):** account keys are pinned first-registration in the
> log, so a malicious member could pre-register an *invited-but-not-yet-online*
> member's identity. Closed by **pin-at-invite** (the inviter already fetches the
> invitee from the GitHub-authenticated directory — record their signing key there),
> the immediate follow-up. Also still staged: p2p-path verification (`peer.rs`).
> Historical status:
> Done: the trust events (`AccountKeyRegistered`, `DeviceCertificateAdded`), the
> `WorkspaceRoster` + `build_roster` (folds trust events in canonical order,
> verifies each cert chains to its account key) and the 3-way `Verdict`
> (`hive-runtime::envelope_verifier`), and verify-on-ingest wired into
> `apply_fetched` with the **non-bricking policy** (reject only provably-bad —
> bad signature / revoked / impersonation; *hold* unsigned/unknown, never drop).
> It is purely additive: with no trust events in the log the roster is empty and
> everything is grandfathered, so behaviour is unchanged until emission ships.
> **Staged (Layer 4 — activation):** emitting each device's trust events, which
> requires reconciling the account-id lifecycle (the bootstrap device cert is
> issued under the local account id, but after GitHub sign-in the author stamps
> the GitHub-derived id — the cert must be re-issued under the current account,
> else a device's own events fail the impersonation check). Also staged: p2p-path
> verification (`peer.rs` ingests without `apply_fetched`), and the directory
> extension to carry signing keys (Option A). These need the trust-bootstrap
> decision below before implementation.


**Finding.** `verify_stream` / `DeviceRoster` exist (`crates/hive-runtime/src/envelope_verifier.rs`) but have **zero production callers** — the live ingest path (`sync_engine::apply_fetched` → `event_store::ingest`) never verifies signatures or revocation. So a peer can inject events signed by an unknown/revoked device, or unsigned events, and they are projected. (Authorship spoofing is already *provable* against — #35's v2 preimage binds `actor_stamp` — but nothing *checks* the signature yet.)

**What verification needs, and the two questions it must answer.** `verify_stream` needs a resolver mapping `signer_device_id → signing public key (+ revoked)`. To trust that mapping it must answer:
1. *What is device D's public signing key?* — from D's `DeviceCertificate` (`crates/hive-core/src/crypto.rs`), which binds `device_id → device_public_key → account_id`, **signed by the account key**.
2. *Does D legitimately belong to the account it stamps as author?* — which requires trusting **that account's public key**. This is the hard part: *how do we know an account key is really this person's?*

### Identity is already anchored on GitHub — leverage it

Two things already ship that make (2) tractable without a from-scratch web-of-trust PKI:

- **The account id is derived from the GitHub user id.** `account_id_for(github_id)` (`hive-runtime::github`) — a workspace member's stable identity *is* their immutable GitHub identity, not a random UUID. Invites add members keyed by it (`invite_by_handle`, `app/src/lib.rs:6065`).
- **A GitHub-authenticated key directory already exists.** Clients `directory_register` their device's **X25519 key-agreement** public key under their signed-in GitHub login (the call is authenticated by the GitHub token, so the relay only lets you list devices under *your* login). `directory_lookup(handle)` returns `DirectoryEntry { github_id, login, devices: [{device_id, ka_public}] }`. Invite-by-handle already resolves `@alice` this way and seals the workspace key to her registered devices (`relay_client.rs`, `invite_by_handle`).

> ⚠️ Scope of the existing mechanism: this directory lives **on the relay**, so invite-by-handle / directory resolution is **relay-dependent** — it needs a relay both parties are registered on, and it does **not** work in pure-P2P / no-relay mode (that path uses the passphrase + short-code join). It is a *per-relay* directory, not a global one.

So S1's "device-key distribution" is largely **extend what already ships**: add the device **signing** key + its `DeviceCertificate` to the same directory entry (today it only carries the KA/sealing key), and resolve `signer_device_id → signing key → GitHub identity` through it.

### The real fork: is the relay a trusted identity anchor?

The directory binding **trusts the relay** to assert "this key belongs to @alice." That is exactly the tension with the review's *"authorization must not depend on trusting the relay."* Three binding options span the trade-off:

- **Option A — relay-attested (lowest friction, relay semi-trusted).** Reuse the directory: the relay verified the GitHub token at registration, so it vouches `github_id → keys`. Almost no new user friction; **but a malicious relay could substitute a key.** Fine for a **relay you run / enterprise** (the enterprise relay already gates on GitHub via `HIVE_RELAY_ADMIN_LOGINS`); not fine for a hostile relay.
- **Option B — GitHub-hosted proof (relay untrusted, more friction).** The user posts a signed `@alice ↔ key K` statement to a GitHub-controlled location (gist/repo/signed commit); verifiers fetch it straight from `github.com` and check it's hosted under `@alice` **and** signed by K. GitHub becomes the CA; the relay is bypassed for identity. Keybase-style.
- **Option C — GitHub's native signing-key directory (relay untrusted, moderate friction).** Register the account public key as one of the user's GitHub **SSH signing keys**; verifiers fetch `api.github.com/users/<login>/ssh_signing_keys`. Reuses a first-class GitHub feature (also makes signed commits show "Verified"); one-time key-add friction.

**Recommended split:** Option **A** for trusted/enterprise relays (reuse the shipped directory), Options **B/C** as the relay-untrusted path for self-hosters who need identity to survive a hostile relay. In all cases **authorization** (who is a member, in what role) stays in Hive events (S2), and **device→account** stays the existing `DeviceCertificate` — GitHub only answers *identity*.

### Design

1. **Extend the directory + a `DeviceCertificateAdded { certificate }` event** so peers learn each device's signing key: the directory (Option A) and/or the event log (forward-compat-safe via phase 1). The cert chains device → GitHub-anchored account.
2. **Build the roster during projection** from members (GitHub-anchored account keys, bound per the chosen option) + device certs + revocations. This is the membership→verification coupling the review flagged: build from the *authenticated* subset, rooted in the workspace creator's GitHub identity (known from the genesis snapshot / invite).
3. **Wire `verify_stream` into `apply_fetched` with a safe enforcement policy:** ingest `valid`; **quarantine** provably-bad (bad signature, revoked device, or `actor_stamp` account ≠ the signing device's account); **hold** `UnknownDevice` for retry (a cert may arrive after the event — do not drop, re-verify when the roster grows, mirroring the phase-7 "don't skip undecodable" rule). Strict mode (reject unknown) is a later flag-flip once distribution is universal, so wiring it never bricks sync.

### New invariants (stated honestly per relay-trust assumption)

Only events whose signature verifies against a device whose cert chains to a **GitHub-anchored account in the roster** are projected; unsigned/unknown/revoked/impersonating → quarantined, never silently dropped. Under **Option A** the identity binding is *"as strong as trusting the relay's GitHub-gated directory"*; under **B/C** it is *"as strong as the member's GitHub account,"* independent of the relay.

### Migration

- Legacy events signed with the **v1** preimage (pre-#35, no `lamport` in the signed bytes) must be grandfathered — add an explicit `preimage_version` (or attempt-both), else all history quarantines.
- Directory entry gains a signing-key field additively (serde default); old clients ignore it.

### Tests

Extend `envelope_verifier` tests to the ingest path: an unknown-device event is held (not dropped) and **promoted** once its cert arrives; a revoked-device event is rejected; an event whose `actor_stamp` account ≠ the signer's account is rejected (impersonation); a v1-preimage legacy event is grandfathered.

### Risk / ordering + the open decision

Do this **first** among the security items, but the one decision to make before implementing is the relay-trust fork above: **is a self-hosted/trusted relay an acceptable identity anchor for the default deployment (Option A), or must identity survive a hostile relay (Options B/C)?** That is a product-security call. Depends on #35 (v2 preimage) ✅.

---

## S2 — Point-in-time authorization

**Finding.** `chat_service::append_signed` authorizes against the **current** roster (`actor_role`, `crates/hive-runtime/src/chat_service.rs`), and `actor_role` returns **`Owner` for any non-member** (a deliberate local-creator affordance, but a footgun). Synced foreign events are **never** re-authorized (`authorization.rs` doc comment). So authorization is advisory client-side gating on the *writer*, at *write time*.

**Design.** Authorization must be evaluated against the roster **as of the event's canonical position**, on **read**, not just trusted from the author:
1. During projection, evaluate each governance/content event with `authorize(payload, role_of(signer) as-of this point, state-so-far)`. Because projection now folds in canonical order (phase 2), "state so far" is well-defined and deterministic.
2. Replace the non-member→Owner fallback with: non-member ⇒ **deny** for governance events; the local-creator case is handled by the genesis snapshot seeding the creator as an `Owner` member (already true in `create_chat`).
3. An event that fails point-in-time authz is quarantined like a bad signature (S1's mechanism).

**Risk.** The non-member→Owner change is load-bearing for local-only workspaces; must verify the creator is always a member before flipping the default. Medium risk — needs the S1 quarantine machinery first.

---

## S3 — Key epochs (rotation must not strand history) — ✅ **implemented**

> Done: `SealedEnvelope` now carries a `version`; `SyncEngine` holds an epoch→key
> keyring (seals under the highest epoch, opens each body under its own); the app
> builds the ring from the passphrase key + every openable rotation. Migration is
> additive (unversioned bodies = epoch 0). Tested by `rotated_epochs_do_not_strand_history`.

**Finding.** `WorkspaceKeyRotation` has a `version` (`crates/hive-core/src/e2ee.rs`) but **events carry no epoch tag** (`SealedEnvelope` is just `{nonce, ciphertext}`), and the client holds a **single** key (`SyncEngine.key`). After a real rotation (`remove_and_revoke`), events sealed under the **old** key become undecryptable and are silently skipped (mitigated for *loss* by phase 7's cursor fix, but they still can't be read).

**Design.**
1. Stamp the sealing epoch on the wire: `SealedEnvelope { version: u32, nonce, ciphertext }`.
2. Client retains a **keyring** (epoch → key), not a single key; `decode` selects by `version`. Keys come from the existing rotation channel (`fetch_key_rotations`).
3. `set_key` (added in phase 7) generalizes to `add_key(version, key)`.

**Migration.** Unversioned sealed bodies = epoch 0. Additive; old clients ignore the new field (serde default).

**Tests.** Seal under epoch 1, rotate to epoch 2, assert a client with both keys reads both; a client with only epoch 2 reads epoch-2 events and cleanly skips (not loses) epoch-1 ones.

---

## S4 — Remove-path clarity + relay-membership rotation

**Finding.** Two removal affordances exist and are *intentionally* different: **"Remove"** (`remove_member`, roster-only, no rotation — `RightRail.tsx:656`) vs **"Remove & revoke"** (`remove_and_revoke`, rotates the key — `:667`). The genuine gap is `workspace_remove_member` (`app/src/lib.rs:6170`) — the **relay/enterprise** server-side membership removal — which does not rotate the E2EE key, so an admin who removes someone from relay membership doesn't revoke their **read** access (they keep the key, and open-relay reads are unauthenticated — see S5).

**Design.** After a successful `workspace_remove_member`, trigger a workspace-level key rotation sealed to the remaining members' devices — reusing `remove_and_revoke`'s rotation logic, but gathering recipients from the **workspace** roster (all members' `key_agreement_public`) rather than a single session's. Requires a workspace-level member/device directory (overlaps S1's roster). Until then, surface the gap in the UI: the relay-remove action should warn that it does not rotate the key.

**Risk.** Couples enterprise membership to E2EE rotation; low individually but depends on the workspace roster (S1).

---

## S5 — Relay + key-at-rest hardening

Independent, each self-contained but with its own dependency:

- **Open-relay reads are unauthenticated** (`crates/hive-relay/src/lib.rs` — `list_envelopes`/`list_keyring` have no membership check under `EntitlementPolicy::Open`): anyone who knows/guesses the room id fetches all ciphertext + the keyring. Mitigation: require a read token even on the open relay, and derive room ids with real entropy (today `room_workspace_id` is UUIDv5 of a possibly-guessable **name**). *Content stays confidential under E2EE, but metadata + ciphertext exposure is real.*
- **Metadata leakage.** The keyring's `sealed` map is keyed by cleartext `device_id` and carries a cleartext `version` → the relay learns the device roster + rotation cadence. Presence/candidate blobs leak per-device liveness. Mitigation: opaque per-device tags; document the residual (a content-blind relay still sees participant count + timing).
- **Private keys at rest are plaintext** (`identity_store.rs::FileKeyVault` writes raw Ed25519 seeds unencrypted; `ka_secret` is plaintext in `settings.json`). Mitigation: encrypt at rest under an OS-keychain-held KEK (macOS Keychain / Windows Credential Manager / Linux Secret Service). **Platform-specific and migration-sensitive** (a botched migration locks users out of their identity) — needs its own careful PR with a fallback + backup path.
- **Plaintext fallback.** With no passphrase set, sync runs in the clear (`workspace_key()` → `None`). Decide: refuse to sync a shared workspace without a key, or make E2EE mandatory for team rooms.

---

## Recommended execution order

1. **S1 cert-distribution design review** → implement device-cert events + roster build → wire `verify_stream` (with v1/v2 preimage grandfathering). *Everything else leans on the roster.*
2. **S2** point-in-time authz (reuses S1's quarantine).
3. **S3** key epochs (independent; unblocks safe rotation).
4. **S4** relay-remove rotation (needs S1's workspace roster).
5. **S5** relay read-auth + metadata + key-at-rest (independent; key-at-rest is its own platform-specific PR).

Each ships as its own PR with property/failure tests, per the review's "every material concern gets an executable test."
