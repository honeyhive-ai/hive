# Conformance checklist — ADR-001

Applied as a review gate to the companion API contract and to every PR touching the companion
service or the mobile client. A "no" on any line blocks merge.

Derived from [ADR-001](./ADR-001-security-baseline.md) as amended by **MC-002-A**, plus the two
normative specs it defers to (noted per section). It adds no new policy — every line traces to one
of those documents.

**Two gates cannot be checked by a type-checker, a test, or a grep, and need a named human reviewer
on the PR** — they are marked 🔎 below:

- the **anti-parity** gate covers UI strings, docs and release notes, where the failure is a
  sentence describing the two platforms as equivalent; and
- the **unconditional-rejection** gate is the absence of a branch, and absence does not show up in
  a diff of the file that should not contain it — it shows up in the file someone adds later.

Reviewer owns both. A PR touching product copy, or any approval-key registration path, does not
merge on a green build alone.

## Pairing

- [ ] QR payload contains **only** desktop static public key (or hash), single-use nonce, candidate addresses.
- [ ] No bearer token, API key, or long-lived credential appears in the QR or in any pairing response.
- [ ] Pairing nonce is single-use with a ~90 s TTL, and pairing requires the desktop to be in an explicit pairing mode.
- [ ] Noise IK handshake pins the key from the QR; no fallback path accepts an unpinned key.
- [ ] Short authentication string is derived on both sides and confirmed **on the desktop**.
- [ ] Device registry supports naming, last-seen, per-device revoke, and a key-rotating panic control.
- [ ] Revoking a device denies access on the next request — no offline-valid credential outlives it.
- [ ] The approval key is registered inside the SAS-confirmed session and is a **separate key** from the Noise static; a device that cannot produce one still pairs, with `approval_key: null` and no Tier 2, and the desktop says so before the human confirms.

## Approvals

- [ ] Tier is computed on the desktop and is present on every pending item sent to the phone.
- [ ] Tier is **re-checked at execution time**; no code path trusts a tier supplied by the client.
- [ ] Tier 3 actions have no approve path reachable from the phone at all (not merely a hidden button).
- [ ] Stop-a-run is Tier 1 and reachable with no additional auth in every state.
- [ ] Tier 2 requires a **hardware approval assertion** verified on the desktop; no "remember for this session" affordance exists, and per-action auth is enforced by the key's access control rather than by client code.
- [ ] `biometricOk`, or any other client-set claim about the client's own compliance, appears **nowhere** in the contract, the client, or the service.
- [ ] A Tier 2 decide with a missing, malformed or unverifiable assertion returns `tier_forbidden` — never a downgrade to Tier 1, never a soft accept, never a retry that succeeds with less.
- [ ] There is **no passcode, PIN or device-credential fallback** on any Tier 2 path.
- [ ] A device with no registered approval key gets view/defer at Tier 2 — the approve control does not exist for it, rather than existing and failing.
- [ ] The Tier 2 challenge is desktop-issued, bound to one device and one record, single-use and short-lived; it is spent on failed attempts too, so a rejected decide cannot be retried against the same nonce.
- [ ] Approval bodies are canonical desktop records with a content hash the client verifies.
- [ ] No agent-authored summary is rendered in place of the canonical body.
- [ ] The client withholds the approve button when the body cannot be fully displayed.
- [ ] Every approval carries a single-use nonce bound to that pending item plus an idempotency key; replays are rejected.
- [ ] Phone-originated approvals are logged with device name and appear in the desktop audit view.

## Tier 2 approval key (MC-002-A)

Derived from [spec/device-approval-key.md](../../mobile-companion/spec/device-approval-key.md) §8,
which is normative. These are the lines most likely to be "fixed" by a UX bug report; each one
fails **silently wide** rather than closed, so none of them can be verified by testing on a
working device.

- [ ] The approval key is **ECDSA P-256 / SHA-256** on both platforms, with signatures in IEEE P1363 fixed-width `r || s` (ratified erratum, DECISIONS.md § MC-002-A) — the verifier accepts exactly one algorithm and does not negotiate one from a client-supplied field, and no DER/ASN.1 signature path exists.
- [ ] No code path generates an approval key except during the SAS-confirmed pairing ceremony.
- [ ] 🔎 The desktop rejects approval-key registration over an established session **unconditionally** — no branch tests whether the device currently holds a key first. There is no re-key path that does not pass the desk.
- [ ] Clearing a device's approval key — by `device.approvalKey.invalidate`, by inferred invalidation, or by panic — **widens nothing**. A row with no key cannot approve and cannot register; the "no key yet" state reached by removal is not more permissive than the state before it.
- [ ] On key invalidation the client **destroys the key and stops** at Tier 1 + view/defer; it never self-issues a replacement.
- [ ] Android registration is refused unless a valid attestation chain to the Google hardware root verifies the desktop's challenge, with `noAuthRequired == false`, auth type biometric-strong, timeout `0`.
- [ ] `setUserAuthenticationParameters` timeout argument is `0` at every call site; zero uses of `setUserAuthenticationValidityDurationSeconds`.
- [ ] `setAllowedAuthenticators` is exactly `BIOMETRIC_STRONG` (15); `BIOMETRIC_WEAK` and `DEVICE_CREDENTIAL` appear nowhere.
- [ ] `setUserAuthenticationValidWhileOnBody(false)` and `setUnlockedDeviceRequired(true)` are explicit.
- [ ] `setIsStrongBoxBacked` is inside a `try`/`catch (StrongBoxUnavailableException)` — it is unchecked and crashes rather than degrades.
- [ ] iOS access control flags are exactly `[.privateKeyUsage, .biometryCurrentSet]`; zero occurrences of `.devicePasscode`, `.or`, `.biometryAny`.
- [ ] Zero occurrences of `touchIDAuthenticationAllowableReuseDuration`; a fresh `LAContext` is constructed per signature.
- [ ] The signed transcript is domain-separated and includes a **desktop-issued** single-use nonce, the record hash, the tier, the decision, and the Noise session binding.
- [ ] Revocation is checked **before** signature verification — a revoked device's valid signature is still a rejection.
- [ ] The panic control clears approval keys as well as Noise statics.
- [ ] A dead or invalidated approval key cannot block **stop-a-run**.
- [ ] The registry records, per device, platform and whether the guarantee is attested or ceremony-anchored; the desktop device list shows it, and every Tier 2 audit line carries the `assurance` its decision rested on.
- [ ] 🔎 No user-facing string, document or release note describes the iOS and Android Tier 2 guarantees as equivalent — the adopted words are **attested** (Android) and **ceremony-anchored** (iOS).

## Path classification (Tier 2/3 boundary)

Derived from [spec/workspace-root-resolution.md](../../mobile-companion/spec/workspace-root-resolution.md).

- [ ] Containment is treated as **necessary, not sufficient** — the in-root deny-list is applied after it.
- [ ] Paths containing `..`, `:`, trailing dot/space, reserved device names, or `~N` short-name shapes are **rejected**, not normalised.
- [ ] Any symlink / junction / reparse point on the walked path rejects to Tier 3; no code path resolves one and compares.
- [ ] Containment compares path **components** against a resolved root, never a string prefix.
- [ ] Case sensitivity matches the filesystem; unknown filesystems compare case-insensitively.
- [ ] Regular files with link count > 1 are rejected (hardlink escape is invisible to path checks).
- [ ] `.git/**`, `.hive/**`, `.claude/**`, `settings*.json`, `.env*`, `package.json`, CI workflow files and `**/.bin/**` are Tier 3 even when inside the root.
- [ ] Mode / executable-bit changes are Tier 3 at any path.
- [ ] Classification is re-run **at execution time**, in the same operation as the write; a tier change rejects the decision rather than re-tiering it.
- [ ] Rendered paths escape Unicode bidi control characters.

## Transport and service surface

- [ ] Every endpoint is an allowlisted, typed operation. **Zero** generic proxy/forward routes to the runtime.
- [ ] Companion service binds its own port with its own auth and is inert until pairing has occurred.
- [ ] Requests carrying `Origin` or `Referer` are rejected; `Host` is validated; auth uses a header a browser cannot attach cross-origin.
- [ ] Transport is abstracted behind one interface so the v1.1 relay is a drop-in.
- [ ] The active path (LAN / relay) is visible in the UI; no silent downgrade.
- [ ] No relay code path is reachable in v1.

## Client and platform

- [ ] Keys are hardware-backed, non-exportable, `ThisDeviceOnly`, with iCloud Keychain sync disabled.
- [ ] App degrades to view-only when hardware-backed storage is unavailable — no soft-storage fallback.
- [ ] `FLAG_SECURE` (Android) and app-switcher blur (iOS) applied to transcript and approval views.
- [ ] Push payloads are templated and category-level only — no transcript text, file paths, or command strings.
- [ ] No correctness-critical behaviour depends on silent push; foreground reconcile covers every gap.
- [ ] OTA updates are signed and verified before application.
- [ ] Approval signing goes through the native module (`SecKeyCreateSignature` / `KeyStore` + `Signature`) — `expo-local-authentication` and any other boolean-returning auth wrapper are absent from the Tier 2 path, and the build is a dev client, not Expo Go.
