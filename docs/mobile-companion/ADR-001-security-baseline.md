# ADR-001 — Mobile Companion v1 Security Baseline

- **Status:** Accepted (binding), amended by **MC-002-A** (2026-08-10), whose curve erratum and
  platform-assurance scope were ratified the same day — see [Amendments](#amendments)
- **Date:** 2026-08-10
- **Deciders:** Owner (proposal approved by quorum), Reviewer (threat model), Hive (coordination)
- **Applies to:** Hive Mobile Companion app (React Native + Expo) and the desktop companion service

## Context

The Hive desktop runtime is configured as an **unauthenticated plaintext HTTP endpoint on
localhost** (`hive.config.toml`, `.hive/runtimes.json`: `http://localhost:11434/...`, no token,
no TLS). Its only security boundary is the loopback interface.

The mobile companion's justifying feature is an **approvals inbox**. Approving a proposal
dispatches an action that an agent executes on the owner's machine with the owner's privileges.
The phone is therefore not a read API with a write endpoint attached — it is an authenticated
remote command channel into a shell on someone's laptop, and every control below is sized to
that fact.

These three constraints are adopted **before any mobile code is written**. They are binding on
the API contract, the client, and the desktop companion service.

---

## 1. Pairing — pinned key, no bearer tokens

**QR code carries:** desktop static X25519 public key (or its hash), a single-use pairing nonce
with a ~90 s TTL, and candidate addresses. It **must never carry a long-lived bearer token.**

- **Handshake:** Noise IK against the pinned key from the QR. The phone's key is registered on
  the desktop under the nonce. Thereafter both sides authenticate by mutual static key — no PKI.
- **Direction:** desktop displays, phone scans. Desktop must be actively in pairing mode.
- **Human confirmation:** both devices derive a short authentication string; the human confirms
  it **on the desktop**. This is the mitigation for QR leakage (screen shares, shoulder surfing,
  a screenshot pasted into a bug report), which is the real pairing risk — not network MITM.
- **MITM on hostile wifi is out of scope by construction:** the QR is an out-of-band
  authenticated channel and an on-path attacker cannot forge a pinned key. Note the corollary:
  LAN-only is not inherently safer than relay, because hostile wifi *is* a LAN.
- **Revocation:** named device registry with last-seen and per-device revoke. No long-lived,
  offline-valid tokens — access derives per-session from the handshake so revocation bites
  immediately. A panic control rotates the desktop static key and forces universal re-pair.

## 2. Approval tiers — assigned and enforced on the desktop

**The tier classifier lives on the desktop. A tier claimed by the phone is untrusted input.**
The desktop stamps each pending item with its tier and **re-enforces the tier at execution
time**; a phone asserting Tier 1 for a Tier 3 action is rejected server-side.

| Tier | Scope | Phone behaviour |
| --- | --- | --- |
| 1 | Conversational gates, "continue", **stop a run** | Approve directly, no extra auth |
| 2 | File diffs inside workspace root; allowlisted commands | **Per-action biometric re-auth, proven by a hardware-signed assertion** the desktop verifies (amended by MC-002-A), full body rendered |
| 3 | Secrets/credentials, network egress, package install, `git push`/force ops, changes to permissions/hooks/`settings.json`, anything outside workspace root | **View and defer only** — "requires desktop" |

- **Stop is always Tier 1 and is never gated.** Making it harder to halt a runaway agent than to
  approve one is backwards.
- **Uniform biometric gating is rejected** as security theatre — it decays into reflexive
  thumbprinting until the prompt carries no signal. Biometrics are Tier 2 only.
- **Tier 2 re-auth is proven, not claimed (amendment MC-002-A).** The first contract draft carried
  the biometric result as a client-set boolean, which contradicts this section's own rule that a
  tier claim from the phone is untrusted input: a build with the client checks stripped sets it true
  and Tier 2 degrades to Tier 1. Superseded by a signature from a per-device key held in Secure
  Enclave / StrongBox, biometry-gated and auth-per-use, registered at pairing and **verified on the
  desktop**. No valid signature, no Tier 2 — no boolean and no fallback path. This defends against a
  tampered app on an intact OS, not against a rooted or jailbroken device, which is why Tier 3 stays
  desktop-only. The guarantee is **attested** on Android and **ceremony-anchored** on iOS; those are
  not equivalent, they must never be described as if they were, and the device registry and audit
  view record which one applies (see [Amendments](#amendments)). See
  `mobile-companion/DECISIONS.md` § MC-002-A and `mobile-companion/spec/device-approval-key.md`.
- **Display integrity:** the phone renders the exact canonical record from the desktop,
  content-hashed. Never an agent-authored summary — an agent that writes its own proposal text
  can social-engineer an approver on a six-inch screen.
- **Truncation in the approval UI is a security bug.** If the body does not fit on screen, there
  is no approve button.
- **Audit:** every phone-originated approval is logged with the device name and surfaced in a
  desktop "what did my phone approve" view.

## 3. Transport — LAN-only in v1

- **v1 is LAN-only** (mDNS discovery, direct connection).
- **Relay ships in v1.1:** default off, self-hostable address, with frame-size padding
  (256 B buckets) and ~250 ms batching on the relay path to close the token-stream side channel.
  Published logging and retention policy.
- **The transport interface is designed now** so the relay is a drop-in, and the UI **always
  surfaces which path is live**. Never silently downgrade: security is equal across both paths
  but *exposure* is not, because relay means a third party observes that you are active.
- **Accepted tradeoff, stated plainly:** v1 does not deliver off-network approvals. That is a
  material reduction of the product thesis — v1 is "unblock from the couch," not "unblock from
  the train." The reasoning is operational and policy-level, not cryptographic: running a relay
  creates logging, retention, uptime, abuse-handling and subpoena obligations that a local-first
  tool cannot quietly retract once users depend on them. **Reopen this if leadership wants the
  off-network story in v1** — better to spec the relay properly now than have it arrive
  half-designed in a point release.

---

## 4. Binding, non-negotiable: no passthrough

The companion service exposes **only allowlisted, typed operations**. There is no generic
proxy or forwarding endpoint to the local runtime.

One `/runtime/*` passthrough publishes an open, unauthenticated LLM-and-tools endpoint to the
local network. The companion service:

- binds its own port with its own auth, and stays **off until pairing has happened**;
- rejects any request carrying `Origin` or `Referer`, validates `Host`, and requires an auth
  header a browser cannot attach cross-origin (DNS rebinding against an off-loopback listener
  is the specific attack being defended against).

## 5. Supporting controls

Adopted alongside the three constraints; lower severity, same review gate.

- **Push is a hint, never content.** Ship a visible, templated, payload-free alert
  ("Approval needed — Coder2 wants to run a command"): category-level information only, never
  transcript text, file paths, or command strings. This gives users a reason to leave
  notifications enabled. The dominant leak is notification *mirroring* (Android
  notification-listener apps, Wear/Auto, lockscreen previews), not the transport. iOS silent
  push is throttled and unreliable — **no v1 behaviour may depend on silent-push delivery for
  correctness**; reconcile on foreground.
- **Key storage:** Keychain / Android Keystore, hardware-backed, non-exportable,
  `ThisDeviceOnly`. **iCloud Keychain sync explicitly disabled** — a synced key makes iCloud
  compromise equivalent to desktop RCE. No soft-storage fallback: if hardware backing is
  unavailable, the app degrades to view-only. Under MC-002-A there are **two** keys with different
  homes and different rules: the Noise static (Keychain/Keystore, device-unlock) and the approval
  key (Secure Enclave/StrongBox, biometry-gated per use, destroyed rather than replaced on
  invalidation). They are never the same key.
- **Screen capture:** `FLAG_SECURE` on Android, blur in the iOS app switcher, for transcript and
  approval views.
- **OTA updates must be signed and verified.** Expo OTA is a code-push channel into an app
  holding a key that executes commands on a laptop. This does not reverse the React Native
  decision, but it prices it.
- **Replay:** every approval carries a nonce bound to its specific pending item, single-use with
  an idempotency key. The desktop rejects the second approve. This also covers the stale-approval
  race in the sync model.

## Consequences

- Coder2's API contract must be reviewed against `conformance-checklist.md` in this directory
  before implementation begins. Endpoint shapes are where "no passthrough" is won or lost.
- If we are unwilling to pay for controls at this level, the honest alternative is to cut
  approvals from v1 and ship a read-only viewer. Under MC-002-A the narrower form of that choice is
  to cut Tier 2 and let in-root diffs and allowlisted commands walk to the desktop — which stays the
  correct fallback if the native-module cost is later refused.

## Amendments

### MC-002-A — Tier 2 approval assertion (2026-08-10, approved by quorum)

Amends §2. Tier 2 re-auth is proven by a hardware-signed assertion the desktop verifies, replacing
the client-attested `biometricOk` boolean, which contradicted §2's own untrusted-input rule. The
phone generates a per-device approval signing key at pairing — separate from the Noise static,
non-exportable, biometry-gated, auth-per-use, invalidated on biometric enrollment — and its public
key is registered during the SAS-confirmed ceremony. No valid signature, no Tier 2; no boolean and
no fallback. §1, §3, §4, Tier 1, Tier 3 and stop-a-run are unchanged.

Three consequences reach beyond §2:

- **§5 key storage** now covers *two* keys per device. The approval key is additionally
  user-authentication-required and enrollment-invalidated, and its recovery path is a full
  re-pairing at the desktop — deliberately, since a self-service re-key over the live session would
  hand Tier 2 to an attacker who enrolled their own biometric on an unlocked phone.
- **MC-001 §1 (React Native + Expo)** is priced further: `expo-local-authentication` returns a
  boolean, which is the same defect in a library wrapper, so signing requires a native module behind
  a config plugin and therefore **dev-client builds rather than Expo Go**.
- **§5 OTA** is narrowed for Tier 2 only. A pushed JS bundle can change what the client displays and
  what it asks the user for, but it cannot produce an approval signature: the key is in the secure
  element and the verifier is on the desktop. Every other OTA risk in §5 stands unchanged, and the
  render rule in particular is still bundle-controlled.

Full record: `mobile-companion/DECISIONS.md` § MC-002-A. Mechanism and merge gates:
`mobile-companion/spec/device-approval-key.md`. Adopted algorithm is **ECDSA P-256 / SHA-256**
(P1363 `r || s`), not the Ed25519 named in the approved text — the iOS Secure Enclave is P-256 only.
Recorded as an erratum in DECISIONS.md and **ratified 2026-08-10**; the curve is binding.

### MC-002-A platform assurance — the guarantee is not equal on both platforms (2026-08-10, approved by quorum)

Amends §2 and the amendment above. MC-002-A does **not** give the same guarantee on iOS and Android,
and this ADR says so in the words the registry uses:

- **Android — attested.** `setAttestationChallenge` yields a certificate chain to Google's hardware
  attestation root. At registration the desktop verifies, against a challenge *it* chose, that the
  key is hardware-backed, `noAuthRequired == false`, auth type biometric-strong, timeout `0`. The
  desktop knows these properties; it is not told them. Without this the amendment is client-attested
  and does not fix the defect it was raised for, so **attestation is mandatory** — part of the cost,
  alongside the native module and dev-client build.
- **iOS — ceremony-anchored.** There is no equivalent. App Attest attests app and device integrity,
  **not** the `SecAccessControl` flags of a given Secure Enclave key; the desktop cannot verify that
  `.biometryCurrentSet` was set. The anchor is the pairing ceremony itself: physical possession, a
  desktop-displayed QR, and SAS confirmed on the desktop.

Of the three options put to the vote — (A) accept the asymmetry and record it per device, (B) Tier 2
on Android only in v1, (C) cut Tier 2 from v1 per the §Consequences fallback — **A was approved**.
Tier 2 therefore ships on both platforms, and:

- the device registry stores `platform` and `approval_key.assurance` (`attested` | `ceremony`) per
  device, and the desktop audit view shows which anchor each Tier 2 decision rested on;
- **no document, UI string, release note or product copy may describe the two as equivalent** — that
  was ruled out under every option, including the one that won. Claiming parity would be the same
  category of error as the `biometricOk` field this amendment removed.

B and C stay on the record as the fallbacks: B if the iOS ceremony anchor is later judged
insufficient, C if the native-module cost is refused.

Full record: `mobile-companion/DECISIONS.md` § MC-002-A → Platform assurance scope. Mechanism:
`mobile-companion/spec/device-approval-key.md` §6.

## Open

- Off-network story in v1 — reopenable by leadership (see §3).
