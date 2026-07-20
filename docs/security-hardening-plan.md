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

## S1 — Wire signature verification into ingest (highest value, has a prerequisite)

**Finding.** `verify_stream` / `DeviceRoster` exist (`crates/hive-runtime/src/envelope_verifier.rs`) but have **zero production callers** — the live ingest path (`sync_engine::apply_fetched` → `event_store::ingest`) never verifies signatures or revocation. So a peer can inject events signed by an unknown/revoked device, or unsigned events, and they are projected. (Authorship spoofing is already *provable* against — #35's v2 preimage binds `actor_stamp` — but nothing *checks* the signature yet.)

**Prerequisite (the real work): device-key distribution.** `verify_stream` needs a `DeviceKeyResolver` mapping `signer_device_id → signing public key (+ revoked)`. Today that roster is built only in tests (`DeviceRoster::with_device`). Production has no way to learn *other* devices' signing keys. Until it does, turning on verification would quarantine **everything** as `UnknownDevice` and break sync.

**Design.**
1. **Publish device certificates into the event log.** Add a `DeviceCertificateAdded { certificate: DeviceCertificate }` event (already forward-compat-safe thanks to phase 1). The cert (`crates/hive-core/src/crypto.rs::DeviceCertificate`) binds `device_id → signing key` and is signed by the **account** key, chaining trust to an account already in the roster. Emit it when a device joins / on first sync.
2. **Build the roster during projection.** As the workspace stream projects, accumulate a `DeviceRoster` from `DeviceCertificateAdded` (and revocations, S-rev below). This is the membership→verification coupling the review flagged: the roster must be built from the *authenticated* subset. Bootstrap trust from the workspace creator's account key (known from the genesis snapshot / invite).
3. **Gate ingest/projection on verification.** In `apply_fetched`, run `verify_stream(fetched, &roster, allow_unsigned=false)`; ingest only `valid`, and persist `quarantined` to a separate table for observability + retry (a cert may arrive after the event it signs — do **not** discard; re-verify when the roster grows, mirroring the phase-7 "don't skip undecodable" rule).

**New invariants.** Only events whose signature verifies against a device whose cert chains to an account in the roster are projected. Unsigned/unknown/revoked → quarantined, never silently dropped.

**Migration.** Legacy events were signed with the **v1** preimage (pre-#35) and lack `lamport` in the signed bytes. Verification must accept a v1 fallback for events authored before the v2 cutover (detect by a version tag or attempt-both), else all historical events quarantine. Add an explicit `preimage_version` to the envelope, or grandfather by timestamp.

**Tests.** Extend the existing `envelope_verifier` tests to the ingest path: a store that ingests an unknown-device event drops it; a cert arriving *after* its event promotes the quarantined event on re-verify; a revoked device's event is rejected.

**Risk / ordering.** Do this **first** among security items but **only after** the cert-distribution design is reviewed — turning verification on prematurely bricks sync. Depends on #35 (v2 preimage) being merged ✅.

---

## S2 — Point-in-time authorization

**Finding.** `chat_service::append_signed` authorizes against the **current** roster (`actor_role`, `crates/hive-runtime/src/chat_service.rs`), and `actor_role` returns **`Owner` for any non-member** (a deliberate local-creator affordance, but a footgun). Synced foreign events are **never** re-authorized (`authorization.rs` doc comment). So authorization is advisory client-side gating on the *writer*, at *write time*.

**Design.** Authorization must be evaluated against the roster **as of the event's canonical position**, on **read**, not just trusted from the author:
1. During projection, evaluate each governance/content event with `authorize(payload, role_of(signer) as-of this point, state-so-far)`. Because projection now folds in canonical order (phase 2), "state so far" is well-defined and deterministic.
2. Replace the non-member→Owner fallback with: non-member ⇒ **deny** for governance events; the local-creator case is handled by the genesis snapshot seeding the creator as an `Owner` member (already true in `create_chat`).
3. An event that fails point-in-time authz is quarantined like a bad signature (S1's mechanism).

**Risk.** The non-member→Owner change is load-bearing for local-only workspaces; must verify the creator is always a member before flipping the default. Medium risk — needs the S1 quarantine machinery first.

---

## S3 — Key epochs (rotation must not strand history)

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
