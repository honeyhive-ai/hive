# Approval tiers

Implements MC-002 §2. Normative.

## The rule everything else follows from

**Tier is assigned and enforced on the desktop.** The phone is told a tier so it can render the
right UI. It is never believed when it reports one. A phone that claims Tier 1 for a `git push
--force` is rejected by the desktop classifier, not by phone-side UI logic.

Corollary: every tier check exists twice — once on the phone for UX, once on the desktop for
enforcement — and only the desktop one is load-bearing. A phone build with all client checks
deleted must not be able to approve anything it could not approve before.

## Tiers

### Tier 1 — direct approve, no extra auth

Conversational gates, `continue`, and **stop-a-run**.

- **Stop is never gated.** No biometric, no confirm sheet, no tier check that can fail closed. The
  ability to halt a misbehaving run is a safety control; adding friction to it makes the product
  less safe, not more. Stop is idempotent and always available while a run is live.

### Tier 2 — hardware approval assertion, full body rendered

File diffs whose every touched path resolves inside the workspace root; allowlisted commands.

- Every Tier 2 approval carries an **approval assertion**: a signature from a per-device key held
  in the Secure Enclave / StrongBox that the OS will not use without a fresh biometric match. The
  desktop verifies it against the public key registered during the SAS-confirmed pairing ceremony.
  See [device-approval-key.md](device-approval-key.md) for generation, flags, invalidation and
  recovery.
- Authentication is **per action**, not per session — enforced by the key's own access control
  (auth timeout `0` on Android, a fresh `LAContext` per signature on iOS), not by app code. Ten
  diffs is ten prompts because the platform demands it, not because the client remembered to ask.
- **No boolean, no fallback.** The phone never reports biometric success; it produces a signature or
  it does not. A missing, malformed or unverifiable assertion is `tier_forbidden`, never a
  downgrade to Tier 1 and never a soft accept. There is no passcode or PIN path — a fallback is the
  self-report again with extra steps.
- **Fail closed on capability.** A device with no biometric hardware, no enrolled biometric, or no
  usable secure element cannot register an approval key at pairing. Tier 2 is then simply
  unavailable on that device: those records are view/defer only, exactly as Tier 3 is.
- **The guarantee is not equal on both platforms, and Tier 2 ships on both anyway** (approved
  2026-08-10). On Android the desktop *verified* the key's properties through an attestation chain
  over a challenge it chose — `assurance: "attested"`. On iOS there is no equivalent, so the anchor
  is the pairing ceremony — `assurance: "ceremony"`, ceremony-anchored. The registry, the device
  list, and every Tier 2 audit line carry which one applies; nothing in the UI or the docs may
  present them as the same thing. See [device-approval-key.md](device-approval-key.md) §6.
- Biometric failure, key invalidation, or an expired challenge degrades to **defer**, never approve.
- The full canonical body must be rendered before the approve control exists — see below.

> The assertion proves that a biometry-gated key on the paired device signed this exact decision.
> It does not prove that the human who was shown the body is the one who approved it, and a rooted
> or jailbroken OS still defeats it. It raises the attack from "edit the JS bundle" to "defeat the
> TEE", which is the point, and it is not a proof of human intent. Tier 3 stays desktop-only for
> exactly that residual risk; MC-002-A does not change Tier 3.

### Tier 3 — view and defer only, desktop required

Secrets; network egress; package install; `git push` and any force-push; changes to permissions,
hooks, or `settings.json`; anything resolving outside the workspace root.

- The phone shows the record and a **Defer** / **Open on desktop** control. There is no approve
  path in the protocol — the desktop rejects a Tier 3 decision from a phone device regardless of
  what the client sent.
- Classification is by **union**: a change touching one Tier 3 path is Tier 3 in full. There is no
  partial approval and no per-hunk splitting in v1.
- Unclassifiable actions are Tier 3. The classifier fails closed.

## Canonical record and the render rule

The phone renders the **canonical content-hashed record** — the actual diff, the actual command,
the actual gate body. It never renders an agent-authored summary as the basis for a decision. An
agent that can write the text a human approves against can talk that human into anything.

```
record_hash = BLAKE2s(canonical_cbor({ id, kind, tier, body, workspace, created_at }))
```

**If the body does not fit on screen, there is no approve button.** Concretely: the approve control
is disabled until the user has scrolled the full body to its end. A truncated or elided body
(over the render budget, binary content, unrenderable encoding) yields Defer-only, at any tier.
"Approve" must always mean "a human saw this".

## Decision flow

1. Desktop classifies on creation, stamps `tier` and `record_hash`, stores both.
2. Phone fetches the record, verifies `record_hash` against the body it received, renders it.
3. For Tier 2 only: phone calls `approval.challenge` and receives a **desktop-issued, single-use,
   short-lived** nonce bound to this device and this record. The phone then signs, inside the OS
   biometric prompt, the domain-separated transcript
   `"hive-approval-v1" || canonical_cbor({ id, tier, decision, record_hash, device_id,
   server_nonce, session_binding })`, where `session_binding` is the Noise handshake hash. The
   transcript is reconstructed by both sides and never transmitted.
4. Phone submits `{ id, decision, record_hash, device_id, nonce }`, plus `approval_sig` for Tier 2.
5. Desktop verifies, in order:
   - device is registered and **not revoked** — revocation is checked before signature
     verification, so a revoked device's valid signature is still a rejection;
   - `nonce` unseen for this device (replay guard, retained 24 h);
   - the record is still **pending** — not already decided, superseded, or expired;
   - submitted `record_hash` equals the current stored hash;
   - the record's stored tier permits phone approval, and for Tier 2 the assertion verifies:
     challenge is one this desktop issued to this device for this record, unspent and unexpired;
     the reconstructed transcript matches; the signature verifies against the approval public key
     in the registry for this `device_id`. Missing, malformed or invalid → `tier_forbidden`.
6. Any failure → the decision is rejected with a typed reason and the record stays pending. The
   challenge is spent either way, so a failed attempt cannot be retried against the same nonce.

The desktop-issued challenge is what makes the assertion a **freshness** proof rather than only a
replay guard. A client-chosen nonce would let a build pre-sign a batch of approvals while biometry
is available and spend them later — session caching reintroduced behind the very platform flags
that forbid it.

**Stale approvals** fall out of step 4 for free: if the underlying action changed, its hash changed,
and the phone's decision no longer matches what the human saw. It is rejected, and the phone
re-fetches and re-renders rather than silently approving new content under an old consent.

## Audit

Every phone-originated decision is appended to a desktop audit log:

```
{ ts, device_id, device_name, record_id, kind, tier, decision, record_hash, transport, outcome,
  assurance }   // assurance: "attested" | "ceremony" | null — Tier 2 only
```

`assurance` records what the Tier 2 guarantee actually rested on for that decision: an Android
attestation chain the desktop verified, or the iOS pairing ceremony it took on trust. The two are
not equal and the audit view must not render them as if they were.

Surfaced in a desktop audit view — filterable by device, defaulting to the last 30 days, with
rejected attempts shown alongside accepted ones. A rejected replay or a hash mismatch is a signal
worth seeing; hiding failures would hide exactly the events that matter.

## Push interaction

Push is a content-free wake (MC-001 §4). Notification text is fixed and generic —
`Hive: something needs your attention` — with no record body, no path, no command, no agent text.
Notification content is mirrored to lock screens, watches, car displays and notification-history
stores outside the app's control; anything placed there has left the secure channel. Content is
fetched over the Noise session after the app is opened and unlocked.
