//! Verify-on-read with quarantine — ported from `EnvelopeVerifier.swift`.
//!
//! Before projecting a workspace's event stream, each signed envelope is
//! checked against the writing device's public key (resolved from the device
//! roster). Envelopes that fail — bad signature, unknown/revoked device, or
//! (under the clean-replacement default) unsigned — are diverted to a
//! quarantine list and excluded from projection, rather than poisoning state.

use hive_core::crypto::verify_envelope;
use hive_core::{CryptoError, DeviceIdentity, SessionEvent, SessionEventEnvelope};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Why an envelope was quarantined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineReason {
    /// No signature present (rejected unless `allow_unsigned`).
    Unsigned,
    /// Signed by a device id not in the roster.
    UnknownDevice,
    /// Signed by a device whose key has been revoked.
    RevokedDevice,
    /// Signature did not verify against the device's public key.
    BadSignature,
    /// The stamped author's account is not the signing device's account — a
    /// member trying to attribute an event to someone else.
    Impersonation,
}

/// Resolves device public keys + revocation status for verification.
pub trait DeviceKeyResolver {
    fn public_key(&self, device_id: Uuid) -> Option<&[u8]>;
    fn is_revoked(&self, device_id: Uuid) -> bool;
}

/// A roster of known devices, the simplest [`DeviceKeyResolver`].
#[derive(Debug, Default, Clone)]
pub struct DeviceRoster {
    keys: HashMap<Uuid, Vec<u8>>,
    revoked: HashMap<Uuid, bool>,
}

impl DeviceRoster {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, device: &DeviceIdentity) {
        self.keys
            .insert(device.id, device.signing_public_key.clone());
        self.revoked.insert(device.id, device.is_revoked());
    }

    pub fn with_device(mut self, device: &DeviceIdentity) -> Self {
        self.insert(device);
        self
    }
}

impl DeviceKeyResolver for DeviceRoster {
    fn public_key(&self, device_id: Uuid) -> Option<&[u8]> {
        self.keys.get(&device_id).map(Vec::as_slice)
    }
    fn is_revoked(&self, device_id: Uuid) -> bool {
        self.revoked.get(&device_id).copied().unwrap_or(false)
    }
}

/// The split result of verifying an envelope stream.
#[derive(Debug, Default)]
pub struct VerificationOutcome {
    /// Envelopes that verified — safe to project.
    pub valid: Vec<SessionEventEnvelope>,
    /// Rejected envelopes paired with the reason.
    pub quarantined: Vec<(SessionEventEnvelope, QuarantineReason)>,
}

/// Verify each envelope against the resolver. `allow_unsigned` controls whether
/// envelopes without a signature pass through (Swift's back-compat behavior) or
/// are quarantined (the clean-replacement default, where everything is signed).
pub fn verify_stream<R: DeviceKeyResolver>(
    envelopes: impl IntoIterator<Item = SessionEventEnvelope>,
    resolver: &R,
    allow_unsigned: bool,
) -> VerificationOutcome {
    let mut outcome = VerificationOutcome::default();
    for env in envelopes {
        let Some(device_id) = env.signer_device_id else {
            if allow_unsigned {
                outcome.valid.push(env);
            } else {
                outcome.quarantined.push((env, QuarantineReason::Unsigned));
            }
            continue;
        };
        if resolver.is_revoked(device_id) {
            outcome
                .quarantined
                .push((env, QuarantineReason::RevokedDevice));
            continue;
        }
        let Some(public_key) = resolver.public_key(device_id) else {
            outcome
                .quarantined
                .push((env, QuarantineReason::UnknownDevice));
            continue;
        };
        match verify_envelope(&env, public_key) {
            Ok(()) => outcome.valid.push(env),
            Err(CryptoError::Unsigned) if allow_unsigned => outcome.valid.push(env),
            Err(_) => outcome
                .quarantined
                .push((env, QuarantineReason::BadSignature)),
        }
    }
    outcome
}

// ---------------------------------------------------------------------------
// Workspace roster + verdicts (S1) — a device-key resolver built by folding the
// trust events (`AccountKeyRegistered`, `DeviceCertificateAdded`, membership)
// out of a workspace's own event stream, so verification needs no network at
// ingest and every device derives the identical roster (it folds in canonical
// order). See docs/security-hardening-plan.md S1.
// ---------------------------------------------------------------------------

/// device_id → (signing public key, owning account) for trusted, non-revoked
/// devices, plus the set of devices whose owning account was removed.
#[derive(Debug, Default, Clone)]
pub struct WorkspaceRoster {
    devices: HashMap<Uuid, (Vec<u8>, Uuid)>,
    revoked: HashSet<Uuid>,
}

impl WorkspaceRoster {
    /// The account that owns `device_id`, if trusted.
    pub fn account_of(&self, device_id: Uuid) -> Option<Uuid> {
        self.devices.get(&device_id).map(|(_, a)| *a)
    }
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty() && self.revoked.is_empty()
    }
}

impl DeviceKeyResolver for WorkspaceRoster {
    fn public_key(&self, device_id: Uuid) -> Option<&[u8]> {
        self.devices.get(&device_id).map(|(k, _)| k.as_slice())
    }
    fn is_revoked(&self, device_id: Uuid) -> bool {
        self.revoked.contains(&device_id)
    }
}

/// Fold a workspace's events into a [`WorkspaceRoster`]. Trust rooted in the
/// members (an account is trusted while it's in the roster) and the cryptographic
/// cert chain (a device is trusted only if its certificate verifies against its
/// account's registered signing key). Account keys are pinned first-registration
/// wins in canonical order (deterministic); the GitHub-anchored binding of that
/// key to a real identity is established where the key *enters* the log (the
/// directory / GitHub proof — Options A/B/C in the plan). A removed account's
/// devices become revoked.
pub fn build_roster(envelopes: &[SessionEventEnvelope]) -> WorkspaceRoster {
    let mut ordered: Vec<&SessionEventEnvelope> = envelopes.iter().collect();
    ordered.sort_by_key(|e| e.canonical_key());

    let mut account_keys: HashMap<Uuid, Vec<u8>> = HashMap::new();
    let mut members: HashSet<Uuid> = HashSet::new();
    let mut member_account: HashMap<String, Uuid> = HashMap::new();
    let mut certs: Vec<hive_core::crypto::DeviceCertificate> = Vec::new();

    let mut note_member = |members: &mut HashSet<Uuid>,
                           member_account: &mut HashMap<String, Uuid>,
                           m: &hive_core::WorkspaceMember| {
        if let Some(aid) = m.actor.account_id {
            members.insert(aid);
            member_account.insert(m.id.clone(), aid);
        }
    };

    for env in &ordered {
        match &env.payload {
            SessionEvent::AccountKeyRegistered { account_id, signing_public_key } => {
                account_keys
                    .entry(*account_id)
                    .or_insert_with(|| signing_public_key.clone());
            }
            SessionEvent::SessionSnapshot { session } => {
                for m in &session.members {
                    note_member(&mut members, &mut member_account, m);
                }
            }
            SessionEvent::MemberAdded { member } => {
                note_member(&mut members, &mut member_account, member);
            }
            SessionEvent::MemberRemoved { member_id } => {
                if let Some(aid) = member_account.get(member_id) {
                    members.remove(aid);
                }
            }
            SessionEvent::DeviceCertificateAdded { certificate } => {
                certs.push(certificate.clone());
            }
            _ => {}
        }
    }

    let mut roster = WorkspaceRoster::default();
    for cert in &certs {
        let Some(account_key) = account_keys.get(&cert.account_id) else { continue };
        if cert.verify(account_key).is_err() {
            continue; // cert must chain to its account's key
        }
        if members.contains(&cert.account_id) {
            roster
                .devices
                .insert(cert.device_id, (cert.device_public_key.clone(), cert.account_id));
        } else {
            // The account was removed — its devices are revoked.
            roster.revoked.insert(cert.device_id);
        }
    }
    roster
}

/// The verification outcome for a single envelope, split so a caller can apply a
/// non-bricking policy: **quarantine** the provably-bad, but only **hold** the
/// merely-unverifiable (retry as the roster grows) instead of dropping it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Signed by a known, non-revoked device; signature valid; author matches.
    Valid,
    /// Provably bad — reject: bad signature, revoked device, or impersonation.
    Quarantine(QuarantineReason),
    /// Can't verify yet (unsigned, or a device whose cert we haven't seen).
    /// Safe to accept for now and re-check later; never a reason to drop.
    Unverifiable(QuarantineReason),
}

/// Classify an envelope against the roster (see [`Verdict`]).
pub fn verdict_for(roster: &WorkspaceRoster, env: &SessionEventEnvelope) -> Verdict {
    let Some(device_id) = env.signer_device_id else {
        return Verdict::Unverifiable(QuarantineReason::Unsigned);
    };
    if roster.is_revoked(device_id) {
        return Verdict::Quarantine(QuarantineReason::RevokedDevice);
    }
    let Some(signing_key) = roster.public_key(device_id) else {
        return Verdict::Unverifiable(QuarantineReason::UnknownDevice);
    };
    if verify_envelope(env, signing_key).is_err() {
        return Verdict::Quarantine(QuarantineReason::BadSignature);
    }
    // Impersonation: the stamped author's account must be the signing device's.
    if let (Some(stamp), Some(owner)) = (&env.actor_stamp, roster.account_of(device_id)) {
        if let Some(claimed) = stamp.actor.account_id {
            if claimed != owner {
                return Verdict::Quarantine(QuarantineReason::Impersonation);
            }
        }
    }
    Verdict::Valid
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity_store::{FileKeyVault, IdentityStore};
    use hive_core::{sign_envelope, ChatMessage, MessageRole, SessionEvent};

    fn signed_env(
        store: &IdentityStore<FileKeyVault>,
        device_id: Uuid,
        seq: i64,
    ) -> SessionEventEnvelope {
        let kp = store.device_keypair(device_id).unwrap();
        let mut env = SessionEventEnvelope::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            seq,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "Mara", "hi"),
            },
        );
        sign_envelope(&mut env, device_id, &kp);
        env
    }

    #[test]
    fn valid_signature_passes_unknown_and_tampered_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let store = IdentityStore::new(dir.path(), FileKeyVault::new(dir.path()));
        let id = store.bootstrap("Mara", "mara", "Mac").unwrap();
        let roster = DeviceRoster::new().with_device(&id.device);

        // 1) a properly signed envelope
        let good = signed_env(&store, id.device.id, 1);

        // 2) tampered after signing
        let mut tampered = signed_env(&store, id.device.id, 2);
        tampered.sequence = 999;

        // 3) signed by an unknown device
        let mut unknown = good.clone();
        unknown.signer_device_id = Some(Uuid::new_v4());

        // 4) unsigned
        let unsigned = SessionEventEnvelope::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            4,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "x", "y"),
            },
        );

        let outcome = verify_stream(
            vec![good, tampered, unknown, unsigned],
            &roster,
            false,
        );
        assert_eq!(outcome.valid.len(), 1);
        assert_eq!(outcome.quarantined.len(), 3);
        let reasons: Vec<_> = outcome.quarantined.iter().map(|(_, r)| *r).collect();
        assert!(reasons.contains(&QuarantineReason::BadSignature));
        assert!(reasons.contains(&QuarantineReason::UnknownDevice));
        assert!(reasons.contains(&QuarantineReason::Unsigned));
    }

    #[test]
    fn revoked_device_is_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let store = IdentityStore::new(dir.path(), FileKeyVault::new(dir.path()));
        let mut id = store.bootstrap("Mara", "mara", "Mac").unwrap();
        let env = signed_env(&store, id.device.id, 1);

        id.device.revoked_at = Some(hive_core::Timestamp::now());
        let roster = DeviceRoster::new().with_device(&id.device);

        let outcome = verify_stream(vec![env], &roster, false);
        assert_eq!(outcome.valid.len(), 0);
        assert_eq!(
            outcome.quarantined[0].1,
            QuarantineReason::RevokedDevice
        );
    }

    // --- S1 roster + verdicts ------------------------------------------------

    use hive_core::crypto::{DeviceCertificate, SigningKeypair};
    use hive_core::identity::{ActorIdentity, ActorKind, ActorStamp, WorkspaceMember, WorkspaceRole};
    use hive_core::{ChatSession, SessionEventEnvelope, Timestamp};

    /// A test principal: an account keypair + one device keypair, with the trust
    /// events (`AccountKeyRegistered`, `DeviceCertificateAdded`, `MemberAdded`)
    /// that put it in a roster.
    struct Principal {
        account_kp: SigningKeypair,
        account_id: Uuid,
        device_kp: SigningKeypair,
        device_id: Uuid,
    }

    fn principal() -> Principal {
        Principal {
            account_kp: SigningKeypair::generate().unwrap(),
            account_id: Uuid::new_v4(),
            device_kp: SigningKeypair::generate().unwrap(),
            device_id: Uuid::new_v4(),
        }
    }

    fn env(lamport: i64, payload: SessionEvent) -> SessionEventEnvelope {
        SessionEventEnvelope::new(Uuid::nil(), Uuid::nil(), lamport, payload)
    }

    fn trust_events(p: &Principal, lamport_base: i64) -> Vec<SessionEventEnvelope> {
        let cert = DeviceCertificate::issue(
            &p.account_kp,
            p.account_id,
            p.device_id,
            &p.device_kp.public_key_bytes(),
            Timestamp::epoch(),
        );
        let member = WorkspaceMember {
            id: p.account_id.to_string(),
            actor: ActorIdentity {
                id: p.account_id.to_string(),
                display_name: "P".into(),
                kind: ActorKind::Human,
                account_id: Some(p.account_id),
                device_id: Some(p.device_id),
                git_email: None,
                key_agreement_public: None,
            },
            role: WorkspaceRole::Contributor,
            title: String::new(),
            index: 1,
            joined_at: Timestamp::epoch(),
        };
        vec![
            env(lamport_base, SessionEvent::MemberAdded { member }),
            env(
                lamport_base + 1,
                SessionEvent::AccountKeyRegistered {
                    account_id: p.account_id,
                    signing_public_key: p.account_kp.public_key_bytes().to_vec(),
                },
            ),
            env(
                lamport_base + 2,
                SessionEvent::DeviceCertificateAdded { certificate: cert },
            ),
        ]
    }

    /// A content event signed by `signer`, stamping `claimed_account` as author.
    fn authored(
        signer_device: Uuid,
        signer_kp: &SigningKeypair,
        claimed_account: Uuid,
        lamport: i64,
    ) -> SessionEventEnvelope {
        let mut e = env(
            lamport,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "P", "hi"),
            },
        );
        e.actor_stamp = Some(ActorStamp {
            actor: ActorIdentity {
                id: claimed_account.to_string(),
                display_name: "P".into(),
                kind: ActorKind::Human,
                account_id: Some(claimed_account),
                device_id: Some(signer_device),
                git_email: None,
                key_agreement_public: None,
            },
            recorded_at: Timestamp::epoch(),
        });
        hive_core::sign_envelope(&mut e, signer_device, signer_kp);
        e
    }

    #[test]
    fn roster_accepts_valid_rejects_tamper_and_impersonation() {
        let alice = principal();
        let roster = build_roster(&trust_events(&alice, 1));

        // 1) Alice's device signs, stamping Alice → Valid.
        let good = authored(alice.device_id, &alice.device_kp, alice.account_id, 100);
        assert_eq!(verdict_for(&roster, &good), Verdict::Valid);

        // 2) Tampered after signing → BadSignature (provably bad, quarantined).
        let mut tampered = good.clone();
        tampered.sequence = 9999;
        assert_eq!(
            verdict_for(&roster, &tampered),
            Verdict::Quarantine(QuarantineReason::BadSignature)
        );

        // 3) Alice's device signs but stamps a DIFFERENT account → Impersonation.
        let spoof = authored(alice.device_id, &alice.device_kp, Uuid::new_v4(), 101);
        assert_eq!(
            verdict_for(&roster, &spoof),
            Verdict::Quarantine(QuarantineReason::Impersonation)
        );
    }

    #[test]
    fn unknown_and_unsigned_are_unverifiable_not_dropped() {
        let alice = principal();
        let roster = build_roster(&trust_events(&alice, 1));

        // A device with no cert in the roster → held, not rejected.
        let stranger = principal();
        let unknown = authored(stranger.device_id, &stranger.device_kp, stranger.account_id, 100);
        assert_eq!(
            verdict_for(&roster, &unknown),
            Verdict::Unverifiable(QuarantineReason::UnknownDevice)
        );

        // Unsigned → held, not rejected.
        let unsigned = env(101, SessionEvent::SessionTitleChanged { title: "x".into() });
        assert_eq!(
            verdict_for(&roster, &unsigned),
            Verdict::Unverifiable(QuarantineReason::Unsigned)
        );
    }

    #[test]
    fn cert_not_chaining_to_account_key_is_ignored() {
        let alice = principal();
        // Forge a cert for alice.device signed by a DIFFERENT (attacker) account key.
        let attacker = SigningKeypair::generate().unwrap();
        let forged = DeviceCertificate::issue(
            &attacker,
            alice.account_id, // claims Alice's account…
            alice.device_id,
            &alice.device_kp.public_key_bytes(),
            Timestamp::epoch(),
        );
        let events = vec![
            env(1, SessionEvent::AccountKeyRegistered {
                account_id: alice.account_id,
                signing_public_key: alice.account_kp.public_key_bytes().to_vec(), // real key
            }),
            env(2, SessionEvent::DeviceCertificateAdded { certificate: forged }),
        ];
        let roster = build_roster(&events);
        // The forged cert doesn't verify against Alice's real account key → device
        // never enters the roster.
        assert!(roster.public_key(alice.device_id).is_none());
    }

    #[test]
    fn removed_member_devices_are_revoked() {
        let alice = principal();
        let mut events = trust_events(&alice, 1);
        events.push(env(50, SessionEvent::MemberRemoved { member_id: alice.account_id.to_string() }));
        let roster = build_roster(&events);

        assert!(roster.is_revoked(alice.device_id), "removed account's device is revoked");
        let e = authored(alice.device_id, &alice.device_kp, alice.account_id, 100);
        assert_eq!(
            verdict_for(&roster, &e),
            Verdict::Quarantine(QuarantineReason::RevokedDevice)
        );
    }

    #[test]
    fn roster_is_order_independent() {
        let alice = principal();
        let bob = principal();
        let mut events = trust_events(&alice, 1);
        events.extend(trust_events(&bob, 10));

        let forward = build_roster(&events);
        let mut reversed = events.clone();
        reversed.reverse();
        let backward = build_roster(&reversed);

        for p in [&alice, &bob] {
            assert_eq!(
                forward.public_key(p.device_id),
                backward.public_key(p.device_id),
                "roster must be identical regardless of event order",
            );
            assert!(forward.public_key(p.device_id).is_some());
        }
        let _ = ChatSession::new("x", Uuid::nil(), "r"); // silence unused import in some cfgs
    }
}
