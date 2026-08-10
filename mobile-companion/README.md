# Hive Mobile Companion

Design baseline for the iOS + Android companion app. **No app code yet** — this directory is the
binding constraint set that the first implementation pass must satisfy.

| File | What it settles |
|---|---|
| [DECISIONS.md](DECISIONS.md) | The two approved decisions (MC-001 stack/scope, MC-002 security), the accepted tradeoff, and the ordering constraint |
| [spec/pairing.md](spec/pairing.md) | QR payload, Noise IK, SAS confirmation, device registry, revocation and panic control |
| [spec/approvals.md](spec/approvals.md) | Tier 1/2/3 definitions, desktop enforcement, canonical record + render rule, decision flow, audit |
| [spec/workspace-root-resolution.md](spec/workspace-root-resolution.md) | The Tier 2/3 path boundary: reject-don't-resolve admission rules, the deny-list inside the root, TOCTOU |
| [spec/device-approval-key.md](spec/device-approval-key.md) | **Normative (MC-002-A).** Hardware approval key: binding, invalidation, the recovery path, and what attestation can prove per platform. §8 is a merge gate |
| [spec/transport.md](spec/transport.md) | LAN-only v1, discovery, reconnect/backfill, offline behaviour, relay in v1.1 |
| [spec/companion-service-api.md](spec/companion-service-api.md) | The complete operation allowlist and why there is no passthrough |
| [contracts/api.ts](contracts/api.ts) | Operation names and payload types, shared by phone and desktop |
| [contracts/transport.ts](contracts/transport.ts) | `CompanionTransport` / `SecureSession`, the seam the v1.1 relay drops into |

## The four things to remember

1. **The phone is a companion, not a peer.** Desktop is the sole source of authoritative state.
2. **Tier is decided on the desktop.** Client-side tier logic is UX. Deleting it must not widen what
   a phone can approve.
3. **Approve means a human saw it.** Canonical body, content-hashed, scrolled to the end — or no
   approve button.
4. **No passthrough.** The local runtime is unauthenticated plaintext HTTP; the companion service is
   a typed façade over an allowlist, never a proxy.

## Status

The contracts under `contracts/` are unbuilt TypeScript — there is no toolchain in this workspace
yet, so they are type-checked for the first time when the Expo app is scaffolded. Treat any error
found then as a spec bug to fix here, not just in the app.

Not yet specified, and deliberately so: relay rendezvous authentication and abuse limits (v1.1),
and the allowlisted-command list backing Tier 2.

The workspace-root resolution rules — previously the highest-risk gap — are now settled in
[spec/workspace-root-resolution.md](spec/workspace-root-resolution.md). Two results from that pass
changed other files:

- **Done.** `allInsideWorkspaceRoot` is out of `contracts/api.ts`, and `allowlisted` with it — same
  defect, same fix. `CanonicalBody` now carries render content only; `tier` is the sole
  classification the phone receives.
- **Done — MC-002-A, approved and applied.** Per-action biometric was a client-attested boolean,
  which cannot coexist with the tier invariant in [spec/approvals.md](spec/approvals.md). It is now
  a signature from a biometry-gated hardware key, registered at pairing and **verified on the
  desktop**; `biometricOk` is deleted from the contract. Recorded in
  [DECISIONS.md](DECISIONS.md) § MC-002-A; mechanism and merge gates in
  [spec/device-approval-key.md](spec/device-approval-key.md), now normative.

  Three consequences worth knowing before you build against it: **Android key attestation is
  mandatory, not optional**, and iOS has no equivalent, so the guarantee is verified on one platform
  and ceremony-anchored on the other; **key invalidation has no self-service recovery** — a changed
  biometric enrollment costs a walk to the desktop, and that is the control, not a papercut; and the
  signing step needs a native module, so the app builds as a **dev client, not Expo Go**.

- **Done — two MC-002-A follow-up votes, 2026-08-10.** The curve **erratum is ratified**: the
  approval key is **ECDSA P-256 / SHA-256** (P1363 `r || s`), not the Ed25519 of the approved text,
  because the iOS Secure Enclave generates P-256 only. And the **platform asymmetry is accepted**:
  Tier 2 ships on iOS and Android both, with the registry and the desktop audit view recording per
  device whether the guarantee is **attested** (Android, an attestation chain the desktop verified
  against its own challenge) or **ceremony-anchored** (iOS, physical presence at the desk during
  SAS). Android-only Tier 2 and cutting Tier 2 from v1 were both on the ballot and rejected; they
  stay on the record as fallbacks. The one thing ruled out under every option: describing the two
  platforms as equivalent, in docs or in UI.
