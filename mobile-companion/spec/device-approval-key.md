# Device approval key — binding, invalidation, recovery

Status: **adopted, normative**. Amendment MC-002-A was approved and is recorded in
[../DECISIONS.md](../DECISIONS.md). §8 below is now a merge gate, not a proposal.

Replaces the client-attested `biometricOk` field, which is deleted from `contracts/api.ts`.

**Erratum against the approved amendment text — ratified 2026-08-10.** The approved proposal named
an **Ed25519** key. The iOS Secure Enclave generates **P-256 only**
(`kSecAttrTokenIDSecureEnclave`, and §4 below already said so), so Ed25519 is unimplementable on one
of the two target platforms. The adopted algorithm is therefore **ECDSA P-256 / SHA-256**,
signatures in IEEE P1363 fixed-width `r || s`, which both the Enclave and Android StrongBox support.
This changes the curve, not the security argument: the guarantee comes from the key being
non-exportable and biometry-gated, not from the signature algorithm. Ratified as binding — the
curve is settled, and reopening it requires a new proposal.

Verification note: the Android API facts below were read out of `android.jar` (API 31) on the
review machine — constant values, method signatures, exception hierarchy. They are not recalled.
The iOS facts were not machine-checked; no Apple toolchain is present here. That asymmetry is
flagged again in §6 because it is load-bearing, not incidental.

---

## 1. What this can and cannot prove

A signature from a biometry-gated hardware key proves: **a key that the OS will not release
without a fresh biometric match, held on the registered device, signed this transcript.**

It does not prove a human read the diff. The scroll-to-end render rule in `spec/approvals.md`
stays client-side UX and stays unenforceable. A fully compromised OS defeats all of this.

Coder2 stated both limits unprompted and they are correct. They belong in the amendment text, not
in a footnote, because the failure mode of this whole area is a spec that quietly upgrades
"cryptographically attested" into "the human consented".

---

## 2. The finding that matters most: recovery, not invalidation

`setInvalidatedByBiometricEnrollment(true)` / `.biometryCurrentSet` are the right flags. They are
not where this design will fail.

Invalidation is observed **only at use time**. On Android the signal is
`KeyPermanentlyInvalidatedException` (a checked `InvalidKeyException`) thrown from
`Signature.initSign()`. There is no callback, no broadcast, and nothing the desktop learns at the
moment enrollment changes.

So the entire guarantee reduces to a single question: **what happens in that catch block.**

If the app generates a fresh key and re-registers it over the live Noise session, invalidation
buys nothing. The attacker's sequence is then: unlock the device, enroll their own biometric
(which kills the key exactly as designed), open Hive, tap through the "re-enroll your approval
key" prompt, and inherit Tier 2 bound to their own finger. The invalidation fired correctly and
the recovery path refunded it.

**Normative:**

- On `KeyPermanentlyInvalidatedException` (Android) or `errSecAuthFailed` /
  `-25293` from a key whose ACL no longer validates (iOS), the client **destroys the key,
  drops to Tier 1 + view/defer, and stops.** It does not generate a replacement.
- Binding a new approval key to an existing device record requires the **full pairing ceremony**:
  a fresh desktop-displayed QR, a new single-use nonce, and desktop-side SAS confirmation
  (`spec/pairing.md`). Physical presence at the desktop is the anchor.
- The desktop **rejects approval-key registration arriving over an established session,
  unconditionally** — whether or not the registry row currently holds a key. The rule must *not* be
  conditioned on "this device already has a key": `device.approvalKey.invalidate` exists precisely to
  clear that field, so a key-conditional rule lets the attacker's own invalidation open the re-key
  path this section exists to close. Sequence: enrol own biometric → key invalidates → client (or a
  forged report) clears the row → row now has no key → register over the live session → Tier 2
  inherited. Registration is reachable only from inside the pairing ceremony; there is no re-key path
  that does not pass the desk.
- Key invalidation is an audit event, logged when the phone reports it *and* inferred when a
  Tier 2 attempt arrives from a device whose key fails verification.

This costs a walk to the desktop after every legitimate biometric enrollment — including iOS
"add an alternate appearance", which counts as a database change. That is the correct price and
it is the line most likely to be filed as a UX papercut and quietly removed. It should be a
conformance gate.

---

## 3. Android — the flags fail *silently wide*, not closed

Read from `android.jar`, API 31:

```
BiometricManager.Authenticators:  BIOMETRIC_STRONG = 15   BIOMETRIC_WEAK = 255   DEVICE_CREDENTIAL = 32768
KeyProperties:                    AUTH_BIOMETRIC_STRONG = 2   AUTH_DEVICE_CREDENTIAL = 1
```

`BIOMETRIC_WEAK` is `255` — its bitmask is a **superset** of `BIOMETRIC_STRONG`'s `15`. A build
that passes `BIOMETRIC_WEAK` still accepts Class 3 sensors, so on any test device with a good
fingerprint reader it behaves identically and the mistake never surfaces. Every hazard in this
section has that shape: wrong value, no error, weaker guarantee.

| Setting | Required | Failure if wrong |
| --- | --- | --- |
| `setUserAuthenticationParameters(0, AUTH_BIOMETRIC_STRONG)` | timeout **0** = auth-per-use | any non-zero timeout is a time-window "remember for this session", forbidden for Tier 2 |
| `setUserAuthenticationValidityDurationSeconds(int)` | **never used** | deprecated form of the same hazard; also admits device credential |
| auth type | `AUTH_BIOMETRIC_STRONG` (2) alone | `AUTH_DEVICE_CREDENTIAL` (1), or `3`, makes the device **PIN** unlock the approval key |
| `setAllowedAuthenticators` | exactly `BIOMETRIC_STRONG` (15) | `BIOMETRIC_WEAK` silently widens; `\| DEVICE_CREDENTIAL` restores PIN |
| `setUserAuthenticationValidWhileOnBody(false)` | explicit | with a paired wearable, auth stays valid unprompted — a session cache under a name nobody greps for |
| `setUnlockedDeviceRequired(true)` | explicit | key usable while locked |
| `setInvalidatedByBiometricEnrollment(true)` | explicit | default is `true`, but state it — it is the subject of the amendment |
| `setIsStrongBoxBacked(true)` | guarded | `StrongBoxUnavailableException` extends `ProviderException` → **unchecked**. Unguarded, it crashes rather than degrades. Catch, fall back to TEE, and record which in the registry. |

`spec/approvals.md` §Tier 2 promises "ten diffs is ten prompts". That promise is implemented by
the timeout being `0` and by nothing else.

---

## 4. iOS — the two lines that undo it

Not machine-verified here. Both are the change a UX bug report will ask for.

- **Access control must be `[.privateKeyUsage, .biometryCurrentSet]` and nothing else.** Adding
  `.or` + `.devicePasscode` — the standard fix for "users get locked out" — restores passcode
  fallback and defeats the whole mechanism. `.biometryAny` likewise: it survives enrollment
  changes, which is the precise property being defended against.
- **`LAContext.touchIDAuthenticationAllowableReuseDuration` must never be set**, and a **fresh
  `LAContext` must be created per signature**. Reusing an already-evaluated context via
  `kSecUseAuthenticationContext` signs with no prompt. This is the iOS mirror of a non-zero
  Android timeout, and it is what someone will reach for when ten diffs means ten Face ID scans.

Also: generate with `kSecAttrTokenIDSecureEnclave` (P-256 only) and **fail pairing** if the
Enclave is unavailable rather than falling back to a software key; accessibility
`kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly`; `NSFaceIDUsageDescription` in Info.plist.

Consequence to carry into UX: with `WhenPasscodeSetThisDeviceOnly`, a user who *removes* their
device passcode has the item destroyed. Tier 2 is gone until they re-pair at the desktop. Correct,
and surprising enough to need saying in the product copy.

---

## 5. Transcript and freshness

Coder2's transcript is record hash + decision + nonce. Insufficient in one specific way.

Sign over canonical CBOR of `{ id, tier, decision, record_hash, device_id, server_nonce,
session_binding }`, where:

- **`server_nonce` is desktop-issued, per-approval, single-use, short-lived.** `approvals.md`
  step 4 has the *phone* supplying `nonce`. That is adequate as a replay guard and inadequate as
  a freshness proof: a phone-chosen nonce lets a client pre-sign approvals in bulk while biometry
  is available and spend them later — reintroducing session caching through the back door, past
  every flag in §3 and §4.
- **`session_binding`** is the Noise handshake hash, so a captured signature cannot be replayed
  into a different session.
- **`tier` is included** so a signature produced for a Tier 2 record cannot be presented against a
  record that has since been re-classified. This composes with the existing stale-hash rejection
  rather than adding a second mechanism.

---

## 6. The asymmetry that must not be papered over

**Android can prove these properties to the desktop. iOS cannot.**

`setAttestationChallenge(byte[])` exists and yields a certificate chain to Google's hardware
attestation root whose extension carries the security level, `origin`, `noAuthRequired`, and the
user-auth type and timeout. The desktop can verify, at registration, against a challenge it chose,
that the key really is hardware-backed and really is auth-per-use biometric-strong. On API 31
`setDevicePropertiesAttestationIncluded(true)` is also available.

Without it, the client calls `KeyInfo.isInsideSecureHardware()` /
`isUserAuthenticationRequirementEnforcedBySecureHardware()` / `isInvalidatedByBiometricEnrollment()`
locally and *tells* the desktop the answer — which is `biometricOk: boolean` again, one layer down
and better dressed. **Android attestation is therefore mandatory, not optional**, or the amendment
does not actually fix the defect it was raised for.

iOS has no equivalent. App Attest attests app and device integrity; it does not attest the
`SecAccessControl` flags of a given Enclave key. On iOS the desktop's only anchor is the pairing
ceremony — physical possession, desktop-displayed QR, SAS confirmed on the desktop — plus App
Attest for app genuineness.

So the guarantee is **attested on Android, ceremony-anchored on iOS**, and the amendment must
say that in those words. Claiming parity would be the same category of error as the field being
replaced.

### Settled by vote, 2026-08-10

This section was put to the team as an explicit scope question — is ceremony-anchored iOS acceptable
for Tier 2 — with three options: (A) accept the asymmetry and record it per device, (B) Tier 2 on
Android only in v1, (C) cut Tier 2 from v1 entirely. **Option A was approved.**

Consequently, and binding rather than advisory:

- Tier 2 ships on **both** platforms in v1. iOS is not degraded to Tier 1 + view/defer, and Tier 2 is
  not cut.
- The registry and the desktop audit view carry the distinction per device and per decision
  (`assurance: "attested" | "ceremony"`), so a reader can always tell which anchor a given approval
  rested on. Gates 11 and 12 below.
- **Android attestation being mandatory is now a vote result, not this spec's recommendation.**
  Without it the amendment is client-attested and does not fix the defect it was raised for.
  Attestation is part of the accepted cost, alongside the native module and dev-client build.
- Describing the two platforms as equivalent — in docs, UI, release notes or product copy — is
  ruled out under every option that was on the table, including the one that won.

---

## 7. Interaction with existing controls

- The panic control in `spec/pairing.md` rotates keys. It must revoke **approval keys** as well as
  Noise statics, or a panicked user rotates the transport and leaves the approval key live.
- Revocation in the device registry is checked before signature verification, not after. A revoked
  device's valid signature is still a rejection.
- Nothing here weakens the rule that **stop is never gated** (`approvals.md` §Tier 1). A dead or
  invalidated approval key must not be able to block a stop.

---

## 8. Lines that become conformance gates on adoption

1. No code path generates an approval key except during the SAS-confirmed pairing ceremony.
2. The desktop rejects approval-key registration over an established session **unconditionally** —
   no branch anywhere tests whether the device currently holds a key before deciding. Clearing
   `approvalKey` (via `device.approvalKey.invalidate`, inferred invalidation, or panic) must widen
   nothing: a row with no key is a row that cannot approve, never a row that may register one.
3. Android registration is refused unless a valid attestation chain to the Google hardware root
   verifies the desktop's challenge, `noAuthRequired == false`, auth type biometric-strong, timeout 0.
4. `setUserAuthenticationParameters` timeout argument is `0` at every call site; zero uses of
   `setUserAuthenticationValidityDurationSeconds`.
5. `setAllowedAuthenticators` is exactly `BIOMETRIC_STRONG`; `DEVICE_CREDENTIAL` appears nowhere.
6. `setUserAuthenticationValidWhileOnBody(false)` and `setUnlockedDeviceRequired(true)` are explicit.
7. `setIsStrongBoxBacked` is inside a `try`/`catch (StrongBoxUnavailableException)`.
8. iOS access control flags are exactly `[.privateKeyUsage, .biometryCurrentSet]`; zero occurrences
   of `.devicePasscode`, `.or`, `.biometryAny`.
9. Zero occurrences of `touchIDAuthenticationAllowableReuseDuration`; a new `LAContext` is
   constructed per signature.
10. The signed transcript includes a desktop-issued single-use nonce and the Noise session binding.
11. The registry records, per device, platform and whether the guarantee is attested or
    ceremony-anchored — and the desktop audit view shows it, per device and per Tier 2 decision.
12. No user-facing string, document or release note presents the iOS and Android Tier 2 guarantees
    as equivalent. The adopted words are **attested** and **ceremony-anchored** (§6, settled by vote
    2026-08-10).
