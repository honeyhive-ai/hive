# Pairing

Implements MC-002 §1. Normative.

## Goals

Bind one phone to one desktop such that: the phone knows the desktop's real static key (not one a
LAN attacker substituted), the desktop knows a human physically approved this specific phone, and
neither side holds a credential that keeps working after revocation.

## Non-goals

Pairing over the internet, pairing without physical co-presence, and multi-desktop pairing from one
QR. Each are separate flows, none in v1.

## QR payload

The **desktop displays**, the **phone scans**. Payload is CBOR, base45-encoded, in a QR shown in a
desktop modal that is dismissed on completion or expiry.

```
{
  v:     1,                    // payload version
  spk:   bytes(32),            // desktop static X25519 public key
  n:     bytes(16),            // single-use pairing nonce
  exp:   uint,                 // unix seconds, issued_at + 90
  addrs: [ "192.168.1.42:8787", "hive-desk.local:8787" ]   // candidates, hints only
}
```

Rules:

- **No bearer token, ever.** The QR is a key-pinning and rendezvous artifact. Possession of it does
  not authenticate anything on its own; it must be spent through a completed handshake.
- `n` is single-use. The desktop drops it from the pending set on first successful *or* failed
  handshake attempt that reaches the SAS step. Re-pairing requires a fresh QR.
- `exp` is 90 seconds. The desktop enforces it; the phone also refuses a stale payload so a
  photographed QR does not sit usable in a camera roll.
- `addrs` are **hints**, not trust. Connecting to an attacker-supplied address is harmless because
  the handshake authenticates the key, not the address. mDNS results may be used instead.

## Handshake

Noise **IK**, `Noise_IK_25519_ChaChaPoly_BLAKE2s`.

- Phone is initiator and already knows the responder static key (`spk`, pinned from the QR).
- Phone's static key is generated on-device at first launch, stored in iOS Keychain /
  Android Keystore, hardware-backed where available, non-exportable, never leaves the device.
- IK transmits the initiator static key encrypted to the responder in the first message, so the
  phone's identity is not disclosed to a passive LAN observer.
- Prologue = the canonical CBOR encoding of the QR payload. This binds the session to the exact QR
  displayed and makes any tampering with `spk`, `n` or `exp` a handshake failure rather than a
  downgrade.

Because `spk` is pinned out-of-band through a channel that requires line of sight to the desktop's
screen, there is no PKI, no TLS trust store and no certificate validation to get wrong.

## Short authentication string

After the handshake completes and before the desktop admits the device:

1. Both sides derive `sas = base10(BLAKE2s(handshake_hash)[0..4]) mod 10^6`, rendered as two groups
   of three digits.
2. The **phone displays** the SAS and a device name field, prefilled with the OS device name.
3. The phone generates its **approval key** (below) and offers the public key, its backing and, on
   Android, an attestation chain over a desktop-issued challenge.
4. The **human confirms the SAS on the desktop** — the desktop shows its own computed SAS with
   Confirm / Reject, and states whether Tier 2 will be available on this device. Confirm is the
   only path that writes a registry entry.
5. On reject, or after 60 s with no confirmation, the desktop tears down the session and burns `n`.

The confirmation lives on the desktop because the desktop is the side with authority. A phone
prompt that a phone-side attacker could satisfy proves nothing.

## Approval key registration (MC-002-A)

The approval key is a **second, separate** key from the Noise static: P-256, generated in the
Secure Enclave / StrongBox, non-exportable, and released by the OS only on a fresh strong-biometric
match, per use. Full flag list, platform hazards and the recovery rules are normative in
[device-approval-key.md](device-approval-key.md); what belongs here is the binding.

- The key is generated **during this ceremony and only during this ceremony**. Its public key is
  registered inside the SAS-confirmed session, so approval authority is bound to a pairing a human
  confirmed at the desk.
- The desktop **rejects approval-key registration arriving over an established session,
  unconditionally** — including for a device whose registry row holds no key. There is no re-key
  path that does not pass the desk. Otherwise an attacker who enrols their own biometric — which
  correctly invalidates the old key — simply taps through a re-enrol prompt and inherits Tier 2;
  and if the rejection were conditioned on the row *already holding* a key, the invalidation that
  clears the row would be what unlocks the prompt.
- **Android:** registration is refused unless a valid attestation chain to the Google hardware root
  verifies over the desktop's challenge and shows `noAuthRequired == false`, auth type
  biometric-strong, timeout `0`. Without attestation the phone would just be *telling* the desktop
  that its key is hardware-backed and per-use — the replaced boolean, one layer down.
- **iOS:** there is no equivalent. App Attest attests app and device integrity, not the
  `SecAccessControl` flags of an Enclave key. The anchor is the ceremony itself. The registry
  records this as `assurance: "ceremony"` and the desktop UI says so; claiming parity with Android
  here would be the same category of error as the field this amendment removes. Shipping Tier 2 on
  both platforms on these terms was approved on 2026-08-10 over Android-only Tier 2 and no Tier 2 in
  v1 — see [device-approval-key.md](device-approval-key.md) §6.
- **No key is not a failure to pair.** A device with no biometric hardware, none enrolled, or no
  usable secure element pairs normally with `approval_key: null` and has **no Tier 2** — view/defer
  at that tier and above. The desktop states this at the confirm step, before the human commits.

## Device registry

Desktop-side, authoritative, persisted alongside runtime config.

```
{
  device_id:    uuid,
  name:         string,        // human-set at pairing, editable, shown in every audit line
  static_pk:    bytes(32),
  paired_at:    timestamp,
  last_seen_at: timestamp,
  last_addr:    string,        // for the human's benefit only
  revoked_at:   timestamp?,    // set = permanently dead
  platform:     "ios" | "android",
  approval_key: {              // null = no Tier 2 on this device, ever, until re-pairing
    pk:            bytes(65),  // P-256 uncompressed
    backing:       "secure-enclave" | "strongbox" | "tee",
    assurance:     "attested" | "ceremony",
    registered_at: timestamp
  } | null
}
```

- Desktop UI lists devices with name, last seen, and a one-tap **Revoke**.
- **Revocation is immediate**: the entry is marked revoked, all live sessions for that static key are
  closed mid-frame, and any queued push registration for it is deleted. There is no token to expire
  and no grace period.
- **Panic control** — a single "Unpair everything" action rotates the desktop static key. Every
  paired device's pin is now wrong, so every device fails at handshake rather than at authorization.
  This is the recovery path for a lost phone when the human cannot identify which entry to revoke.
  It **also clears every `approval_key`**: rotating the transport while leaving approval keys live
  would be a panic button that panics about the wrong thing.
- A revoked device that re-pairs gets a **new** `device_id`. Names may be reused; identity is not.

## Storage on the phone

| Item | Store |
|---|---|
| Phone static private key | Keychain / Keystore, hardware-backed, non-exportable, `WhenUnlockedThisDeviceOnly` |
| Approval private key | Secure Enclave / StrongBox, non-exportable, biometry-gated per use, invalidated by biometric enrolment — never the same key as the static above |
| Pinned desktop `spk` + `device_id` | Keychain / Keystore |
| Cached transcripts, approval records | App sandbox, encrypted at rest, cleared on unpair |

Unpairing on the phone deletes all three. It does not revoke on the desktop — the human must revoke
there too, and the desktop UI is where that is discoverable.
