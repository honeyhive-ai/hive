# Mobile Companion — Binding Decisions

Status: **approved, binding**. Adopted before any mobile code is written.
Any change to the constraints below requires a new approved proposal, not a PR discussion.

Two proposals were approved together and are recorded here as one baseline, plus two amendments:

- **MC-001** — Stack, architecture and v1 scope (React Native + Expo, companion-not-peer, approvals-first)
- **MC-001-A** — Amendment to MC-001 §1: the app is on the New Architecture, and that is no longer optional
- **MC-002** — Security baseline (pinned-key pairing, desktop-enforced approval tiers, LAN-only)
- **MC-002-A** — Amendment to MC-002 §2: Tier 2 biometric is OS-enforced and desktop-verified, not a client-attested boolean

A third, independent of the security baseline, was approved on 2026-08-10 and is recorded below:

- **MC-003** — Visual scale: Tailwind rem resolves against a 14px root, so every desktop length is 0.875× nominal

Two follow-up votes on 2026-08-10 closed the questions MC-002-A left open, and are recorded inside
its section rather than as new amendments: the **curve erratum is ratified** (ECDSA P-256 / SHA-256,
not the Ed25519 of the approved text), and the **platform asymmetry is accepted** — Tier 2 ships on
both platforms with per-device `assurance` recorded as attested or ceremony-anchored, never as
parity.

MC-002 resolves the two open questions MC-001 deferred to the Coder2/Reviewer passes:
(a) v1 launches **LAN-only**, relay in v1.1; (b) per-action re-auth is required for **Tier 2 only**,
and a whole class of approvals (Tier 3) is **desktop-only**. MC-002-A then fixes *how* that re-auth
is proven — see below.

---

## MC-001 — Stack, architecture, scope

| # | Decision |
|---|---|
| 1 | **Stack** — React Native + Expo, one codebase for iOS and Android. v1 surface is lists, transcripts and buttons; OTA updates support fast iteration. Revisit native-per-platform only on real platform depth. **Amended by MC-002-A:** a custom native module and **dev-client builds** — not Expo Go — are now required. **Amended by MC-001-A:** the app runs on the **New Architecture**; the legacy renderer is no longer available at any version. |
| 2 | **Architecture** — the phone is a companion, **not a peer**. Desktop Hive is the sole source of authoritative state. The phone caches for read and queues for write. |
| 3 | **Transport** — desktop exposes a companion service: HTTP fetch + WebSocket live stream, two interchangeable paths behind one interface. |
| 4 | **Push** — wake-up ping only. No message content over APNs/FCM; content is fetched over the secure channel after wake. |
| 5 | **v1 scope** — pair with desktop; workspace list; conversation view with agent-attributed turns; compose/reply; approvals inbox (workflow gates + proposals); run status with stop. The approvals inbox is the primary justification for the app. |
| 6 | **Out of scope for v1** — workflow authoring, file editing, agent configuration. |

## MC-001-A — New Architecture is mandatory (amendment to MC-001 §1)

Status: **approved, binding**. Amends MC-001 §1. Does not change MC-001 §2–§6, and does not change
MC-002, MC-002-A or MC-003.

### What changed

MC-001 §1 chose React Native + Expo, and the app shipped with `newArchEnabled: false` — an explicit
choice of the legacy Paper renderer and the bridge.

The SDK 51 → 57 upgrade (commit `34aa3d4`) removes that choice. React Native 0.82 deleted the legacy
renderer: the `newArchEnabled` key no longer appears in `app.json`, and generated native projects
write `newArchEnabled=true` unconditionally. Verified 2026-08-10 — `expo prebuild` on SDK 57 writes
`newArchEnabled=true` into `android/gradle.properties`.

This is a change of fact rather than a change of intent, and nothing in `src/` is affected: the app's
only animation is React Native's own `Animated` API in `Turn.tsx`, and the upgrade commit records the
suite passing unchanged. It is recorded here rather than only in the app README because it reverses a
choice this log states, and because it constrains a milestone that has not been built yet.

The dependency tree had already foreclosed the alternative independently: `react-native-reanimated`
4.5.1, pulled in by React Navigation's drawer, dropped Paper support at v4 and is itself New
Architecture only. Even had React Native kept the legacy renderer, staying on it would have meant
giving up the navigation stack MC-001 §1 assumes.

### The consequence — MC-002-A

MC-002-A §1 requires a per-device approval signing key in Secure Enclave / StrongBox, which means a
custom native module over Keychain and Keystore. That module must now be **Fabric / TurboModule
compatible**, and any third-party native dependency considered for the approval path must likewise be
New Architecture ready.

**Bridge-only modules are not an option at any version.** This removes the schedule escape hatch: if
the TurboModule work runs long, shipping the approval key over the old bridge is not available as a
fallback, because there is no version of React Native still carrying it. The choice is a New
Architecture module or no Tier 2 — and MC-002-A §3 already fixes what "no Tier 2" means, which is
view/defer only, not a downgrade path.

## MC-002 — Security baseline

| # | Decision |
|---|---|
| 1 | **Pairing** — QR carries desktop static X25519 public key + single-use 90 s nonce + candidate addresses. Never a long-lived bearer token. Noise IK against the pinned key; human confirms a short authentication string on the desktop. Desktop displays, phone scans. Named device registry, immediate revocation, key-rotating panic control. See [spec/pairing.md](spec/pairing.md). |
| 2 | **Approval tiers** — tier is assigned and enforced on the **desktop**, never trusted from the phone. Phone renders the canonical content-hashed record, never an agent-authored summary. If the body does not fit on screen, there is no approve button. All phone-originated approvals are logged with the device name and surfaced in a desktop audit view. See [spec/approvals.md](spec/approvals.md). **Amended by MC-002-A** on how Tier 2 re-auth is proven. |
| 3 | **Transport** — v1 is **LAN-only**. Relay ships in v1.1, default off, self-hostable, with size padding and batching on the relay path. The transport interface is designed now so the relay is a drop-in. See [spec/transport.md](spec/transport.md). |
| 4 | **No passthrough** — the companion service exposes only allowlisted typed operations. No endpoint proxies, forwards or relays to the local runtime. See [spec/companion-service-api.md](spec/companion-service-api.md). |

### Why no passthrough is non-negotiable

`hive.config.toml` in this install points the default runtime at
`http://localhost:11434/v1/chat/completions` — unauthenticated plaintext HTTP with tool support
enabled. Any companion endpoint that forwards an arbitrary path, URL or body would turn a phone on
the LAN into an unauthenticated command channel against that runtime, and through its tools, the
shell. The companion service is therefore a **typed façade**, not a proxy.

## MC-002-A — Tier 2 approval assertion (amendment to MC-002 §2)

Status: **approved, binding**. Amends MC-002 §2. Does not change MC-002 §1, §3 or §4, and does not
change Tier 1, Tier 3, or stop-a-run.

### The defect

The contract sent `biometricOk?: boolean` from phone to desktop and the spec accepted it as "the
client attested biometric success". MC-002 §2's governing invariant is that *a phone build with all
client checks deleted must not be able to approve anything it could not approve before.* Both cannot
hold: a modified client sets the field to `true` and approves Tier 2 with no biometric at all, so
Tier 2 — in-root file diffs and allowlisted commands — collapses to Tier 1 against precisely the
attacker the tier exists to stop. The desktop cannot verify a self-report.

### The change

| # | Decision |
|---|---|
| 1 | **Second key at pairing** — the phone generates a per-device **approval signing key**, separate from the Noise static key, in Secure Enclave (iOS) / StrongBox (Android): non-exportable, user-authentication-required, auth-per-use, invalidated by biometric enrollment. Its public key is registered during the SAS-confirmed handshake. See [spec/pairing.md](spec/pairing.md). |
| 2 | **Tier 2 decisions carry a signature**, not a claim — over the domain-separated transcript `{ id, tier, decision, record_hash, device_id, server_nonce, session_binding }`. The nonce is **desktop-issued**, single-use and short-lived; a phone-chosen nonce would let a client pre-sign approvals in bulk while biometry is available. `biometricOk` is **deleted** from the contract. |
| 3 | **The desktop verifies** against the registered public key. No valid signature, no Tier 2 approval — no boolean, no fallback, no downgrade path. A device that cannot register an approval key has no Tier 2, and its Tier 2 records are view/defer only. |
| 4 | **No self-service re-key.** On key invalidation the client destroys the key, drops to Tier 1 + view/defer and stops. Re-binding an approval key requires the full pairing ceremony at the desktop. A recovery path over the live session would refund the entire guarantee to an attacker who enrolled their own biometric. |
| 5 | **Android key attestation is mandatory**, not optional — without it the client merely *tells* the desktop that the key is hardware-backed and auth-per-use, which is the same defect one layer down. |

Mechanism, platform flags and the failure modes of each are normative in
[spec/device-approval-key.md](spec/device-approval-key.md); its §8 is a merge gate.

### Why this actually works

The OS will not produce the signature without a fresh biometric match, and the key cannot leave the
secure element. Per-action biometric stops being a client promise and becomes a cryptographic proof
the desktop checks — which is what MC-002 §2 already required of everything else. Per-action comes
free: the key requires auth on every use, so ten diffs is ten prompts, enforced by the platform
rather than by app code that can be patched out.

### Recorded limits — not footnotes

- **This is not proof of human intent.** It proves a biometry-gated key on the paired device signed
  this decision. It does not prove the human who was shown the diff pressed the button, and it does
  nothing to make the scroll-to-end render rule enforceable. That rule stays client-side UX.
- **It defends against a tampered or repackaged app on an intact OS**, not against a jailbroken or
  rooted device with a compromised kernel. That residual risk is why **Tier 3 stays desktop-only**.
- **The guarantee is asymmetric: attested on Android, ceremony-anchored on iOS.** Android key
  attestation proves the key's properties to the desktop; iOS has no equivalent, so its anchor is
  physical presence at the desk during SAS. Claiming parity would be the same category of error as
  the field being replaced.

### Erratum — curve (ratified 2026-08-10)

The approved proposal text named **Ed25519**. The iOS Secure Enclave generates **P-256 only**, so
Ed25519 is unimplementable on one of the two target platforms. The adopted algorithm is **ECDSA
P-256 / SHA-256**, signatures in IEEE P1363 fixed-width `r || s`, supported by both the Enclave and
StrongBox. This changes the curve, not the security argument — the guarantee comes from the key
being non-exportable and gated by the OS keystore on a fresh biometric match, not from the signature
algorithm. Every property the amendment relies on — auth-per-use, enrollment invalidation, Android
attestation of key properties, no self-service re-key — holds identically under P-256.

**Ratified 2026-08-10.** The erratum now carries the same binding force as the rest of MC-002-A;
P-256 is the adopted algorithm, not a pending substitution. The alternatives it was weighed against
were dropping iOS from v1 or accepting a software-held approval key on iOS — the latter being
exactly the defect MC-002-A was raised to fix. Reopening the curve requires a new proposal.

### Review finding — invalidation must subtract only (closed 2026-08-10, Reviewer)

The open question was whether `device.approvalKey.invalidate` can only ever *remove* Tier 2. The op
itself does: its `reason` values are all removals, it clears `approvalKey` on the registry row, and
no inverse op is on the allowlist — forging it buys a self-inflicted downgrade.

The composition did not. Four documents wrote the registration rule as "the desktop rejects
approval-key registration over an established session **for a device that already has a key**". That
condition is exactly what `invalidate` falsifies. Read together, the two rules re-opened decision 4:
enrol own biometric → key invalidates → report the invalidation → the row no longer has a key → the
conditional no longer fires → register over the live session → Tier 2 bound to the attacker's
finger. Each rule was correct alone; the pair was the attack §2 of `spec/device-approval-key.md`
describes, reassembled out of two safe halves.

The rejection is now **unconditional** in all four places — no branch may test whether the row holds
a key before refusing — and the invariant is stated as such: *clearing an approval key widens
nothing; a row with no key cannot approve and cannot register.* No decision was reversed; decision 4
was restored to what it already said. Amended: `spec/device-approval-key.md` §2 and §8 gate 2,
`spec/companion-service-api.md`, `spec/pairing.md`, `contracts/api.ts`, plus a new conformance gate.

`device.approvalKey.invalidate` was also missing from the `spec/companion-service-api.md` operation
table while existing in `contracts/api.ts` — under that file's own rule ("anything not on this list
does not exist") the op was unreachable. It is now listed, marked remove-only.

### Cost, and why now

Pairing gains a second key registration; the client gains a sign-on-decide step; the desktop gains
signature verification and challenge issuance. No change to Tier 1, to stop-a-run, or to the
transport. It also forces a **dev-client build** — `expo-local-authentication` returns a boolean,
which is the same defect in a library wrapper, so signing needs a native module behind a config
plugin and the app cannot run in Expo Go. That is a real consequence of MC-001 §1, accepted here
explicitly. Cheap now; expensive after the client ships and the registry format is frozen.

### Platform assurance scope — **decided 2026-08-10**

A separate proposal asked whether ceremony-anchored iOS is acceptable for Tier 2 at all, since the
amendment does not give equal guarantees on both platforms. **Option A was approved: accept the
asymmetry and record it per device.**

| | Option | Outcome |
|---|---|---|
| **A** | Ship Tier 2 on both platforms; the registry and the desktop audit view record per device whether the guarantee is **attested** or **ceremony-anchored**, and ADR-001 states the difference in those words | **Adopted** |
| B | Tier 2 on Android only in v1; iOS gets Tier 1 + view/defer | Rejected — makes the flagship platform the degraded one, and reads as arbitrary to users |
| C | Cut Tier 2 from v1 entirely (the ADR-001 fallback) | Rejected — remains the correct fallback only if the native-module cost is later refused |

What was ruled out under every option: shipping on both platforms and describing them as
equivalent.

Also settled by this vote, and now binding rather than advisory: **without Android key attestation
the amendment is client-attested and does not fix the defect it was raised for.** Attestation is
part of the cost of MC-002-A, alongside the custom native module and dev-client build.

Binding consequences:

- The registry carries `platform` and `approval_key.assurance` (`"attested" | "ceremony"`) per
  device; the desktop device list and the audit view both display it, and each Tier 2 audit line
  records which anchor that specific decision rested on (`spec/approvals.md` § Audit).
- No document, UI string or product copy may present the two platforms as equivalent. The words are
  **attested** (Android, verified against a challenge the desktop chose) and **ceremony-anchored**
  (iOS, physical presence at the desk during SAS, plus App Attest for app genuineness only).
- iOS reasoning stands on the record: App Attest attests app and device integrity, not the
  `SecAccessControl` flags of a given Enclave key, so the desktop cannot verify `.biometryCurrentSet`
  was set.

### The alternative that was not taken

Cutting Tier 2 from v1 — phone gets Tier 1 plus view/defer at every higher tier, and in-root diffs
and allowlisted commands walk to the desktop. Put formally as Option C in the platform-assurance
vote above and rejected there. Recorded because it remains the correct fallback if
the native-module cost is later refused, and because it mirrors the fallback already in ADR-001.
What must **not** ship under either option is a boolean described in docs or UI as biometric
enforcement.

---

## Accepted tradeoff, recorded explicitly

v1 does **not** deliver off-network approvals. This is a material reduction of the product thesis —
the pitch was "unblock a run from the train", and on the train you are not on the LAN. What v1 ships
is "unblock a run from the couch".

The reduction was accepted to avoid shipping a relay, a metadata-leak surface and a remote command
channel in the same release as the first pairing implementation. **Reopen this if leadership wants
the off-network story in v1** — the transport interface is built to absorb it without a rewrite, so
the cost of reopening is the relay's own security work, not a refactor.

## Ordering constraint

Pairing and the approval-tier classifier land before the approvals inbox is wired to any live
decision path. An inbox that can approve before the tier classifier is enforced is the exact failure
this baseline exists to prevent.

MC-002-A extends this: approval-key registration and desktop-side signature verification land **with
pairing**, before any Tier 2 approve control is reachable. A Tier 2 approve path that ships ahead of
its verifier is the boolean again, in the form of a check nobody wrote yet.

---

## MC-003 — Visual scale is Tailwind rem × 14px

Status: **approved, binding** (2026-08-10). Applies to the base build and everything after it.
Full detail and provenance in [spec/visual-scale.md](spec/visual-scale.md); the implementation is
`app/src/theme/scale.ts`.

| # | Decision |
|---|---|
| 1 | **Root is 14px.** `web/src/styles.css:8` sets `:root { font-size: 14px }`, Tailwind v4 with no `@theme` override. Every rem-based utility on desktop renders at **0.875× nominal**. |
| 2 | **Normative radii** — `md` 5.25, `lg` 7, `xl` **10.5**, `2xl` 14, `3xl` 21. Spacing is Tailwind step × 3.5px (`p-4` = 14). Type is step × 14px (`text-sm` = 12.25, `text-base` = 14). |
| 3 | **The literal ramp ports unchanged.** `.tt-text` 14px, `.tt-time` 12px, `.tt-model` 11px and the rest of the chat layer are written in literal px in `styles.css` and are not rem-scaled. The two ramps do not align on the desktop either; **do not reconcile them**. |
| 4 | **Single source, enforced.** `rem(n) = n * 14` in `app/src/theme/scale.ts`. Raw px literals are forbidden for anything that maps to a Tailwind utility, enforced by a guard test, with a `// px-ok:` escape hatch for values that have no desktop equivalent. |

### Why this is binding rather than advisory

The base-build brief originally specified radii of 12 / 16 / 24. Those are the 16px-root values and
they are wrong for this app. A port built on them is uniformly ~14% oversized and over-rounded — it
looks entirely plausible in isolation and is detectable only side-by-side with the desktop, which is
the one comparison a phone build gate cannot make.

The enforcement clause matters as much as the numbers. The 0.875 factor does not get dropped all at
once; it gets dropped one component at a time by whoever types `borderRadius: 12` from memory. A
per-file guard catches that on the commit that introduces it, which is the only point at which it is
cheap to see.
