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
    /// Option C (github-signing-keys identity mode): GitHub was fetched and the
    /// account's registered signing key is **not** among the login's
    /// `ssh_signing_keys` — a provably-bad identity binding. See
    /// `identity_verifier` and docs/security-hardening-plan.md S1 Option C.
    GithubKeyNotListed,
    /// Two different signing keys were registered for the same account id — an
    /// ambiguous/contested identity binding. Fail closed: don't trust events
    /// stamped as a disputed account (a forged `AccountKeyRegistered` under a
    /// victim's account id can no longer silently win first-registration and
    /// hijack the victim). See `build_roster`.
    DisputedAccountKey,
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

/// device_id → (signing public key, the set of accounts that have cert-verified
/// this device) for trusted, non-revoked devices, plus the set of devices whose
/// owning account was removed. A device can be certified to more than one
/// account — e.g. a local identity that later signs into GitHub is the same
/// person, and each cert is signed by its own account key — so an event is
/// authentic if its stamp matches ANY of the device's certified accounts.
#[derive(Debug, Default, Clone)]
pub struct WorkspaceRoster {
    devices: HashMap<Uuid, (Vec<u8>, HashSet<Uuid>)>,
    revoked: HashSet<Uuid>,
    /// account_id → the account's pinned signing public key, for member accounts
    /// (first-registration wins, canonical order). Retained so the Option-C
    /// identity gate can check this exact key against the account's GitHub
    /// `ssh_signing_keys`. Not used by Option A (relay-trusted) verification.
    account_keys: HashMap<Uuid, Vec<u8>>,
    /// Accounts for which two *different* signing keys were registered — a
    /// contested binding. Events stamped as one of these are quarantined (fail
    /// closed), so a forged registration under a victim's account can't hijack it.
    disputed: HashSet<Uuid>,
}

impl WorkspaceRoster {
    /// Whether `device_id` is cert-verified to sign as `account` (any of its
    /// certified accounts). False for an unknown device.
    pub fn device_signs_for(&self, device_id: Uuid, account: Uuid) -> bool {
        self.devices.get(&device_id).is_some_and(|(_, accts)| accts.contains(&account))
    }
    /// Whether `device_id` is a trusted (cert-verified, non-revoked) device.
    pub fn is_certified(&self, device_id: Uuid) -> bool {
        self.devices.contains_key(&device_id)
    }
    /// One account this device is certified to (deterministic: smallest by id),
    /// for callers needing a single account when an event carries no stamp. Use
    /// `device_signs_for` for authorization — a device may hold several.
    pub fn account_of(&self, device_id: Uuid) -> Option<Uuid> {
        self.devices.get(&device_id).and_then(|(_, accts)| accts.iter().min().copied())
    }
    /// The pinned account signing key for a member account, if known. Used by the
    /// Option-C identity gate to compare against GitHub `ssh_signing_keys`.
    pub fn account_signing_key(&self, account_id: Uuid) -> Option<&[u8]> {
        self.account_keys.get(&account_id).map(Vec::as_slice)
    }
    /// Whether `account_id` has a contested key binding (two different keys
    /// registered). Events stamped as it are quarantined — fail closed.
    pub fn is_disputed(&self, account_id: Uuid) -> bool {
        self.disputed.contains(&account_id)
    }
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty() && self.revoked.is_empty()
    }
}

impl DeviceKeyResolver for WorkspaceRoster {
    fn public_key(&self, device_id: Uuid) -> Option<&[u8]> {
        self.devices.get(&device_id).map(|(k, _)| k.as_slice())
    }
    // (see below for is_revoked)
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
    let mut disputed: HashSet<Uuid> = HashSet::new();
    let mut explicit_revoked: HashSet<Uuid> = HashSet::new();
    let mut members: HashSet<Uuid> = HashSet::new();
    let mut member_account: HashMap<String, Uuid> = HashMap::new();
    let mut certs: Vec<hive_core::crypto::DeviceCertificate> = Vec::new();

    let note_member = |members: &mut HashSet<Uuid>,
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
                match account_keys.get(account_id) {
                    // A different key than the one already pinned for this account:
                    // a contested binding. Mark disputed (fail closed in verdict_for)
                    // instead of silently keeping first-wins — which let a forged
                    // registration under a victim's account id, sorted to lamport 0,
                    // deterministically win and hijack the victim's identity.
                    Some(existing) if existing != signing_public_key => {
                        disputed.insert(*account_id);
                    }
                    // Idempotent re-registration of the same key — not a conflict.
                    Some(_) => {}
                    None => {
                        account_keys.insert(*account_id, signing_public_key.clone());
                    }
                }
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
            SessionEvent::DeviceRevoked { device_id } => {
                explicit_revoked.insert(*device_id);
            }
            _ => {}
        }
    }

    let mut roster = WorkspaceRoster::default();
    roster.disputed = disputed;
    // Explicitly-revoked devices (a `DeviceRevoked` event) are never trusted, even
    // if they still hold a valid member cert — verdict_for checks is_revoked first.
    roster.revoked.extend(explicit_revoked);
    // Retain the pinned account signing keys for accounts that are members — the
    // Option-C gate checks these against GitHub. (Non-member account keys are
    // irrelevant; their devices are revoked below.)
    for (account_id, key) in &account_keys {
        if members.contains(account_id) {
            roster.account_keys.insert(*account_id, key.clone());
        }
    }
    for cert in &certs {
        let Some(account_key) = account_keys.get(&cert.account_id) else { continue };
        if cert.verify(account_key).is_err() {
            continue; // cert must chain to its account's key
        }
        if members.contains(&cert.account_id) {
            // Accumulate every member account this device is certified to (a
            // device may transition identities — e.g. local → GitHub sign-in — and
            // hold a valid cert under each). Any of them is a valid signing account.
            let entry = roster
                .devices
                .entry(cert.device_id)
                .or_insert_with(|| (cert.device_public_key.clone(), HashSet::new()));
            entry.1.insert(cert.account_id);
        }
    }
    // A device whose *only* verified certs are to removed accounts is revoked —
    // but a device that also holds a current member cert (above) stays trusted.
    for cert in &certs {
        if roster.devices.contains_key(&cert.device_id) {
            continue;
        }
        if let Some(account_key) = account_keys.get(&cert.account_id) {
            if cert.verify(account_key).is_ok() && !members.contains(&cert.account_id) {
                roster.revoked.insert(cert.device_id);
            }
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
        // A genuine event is ALWAYS signed — `append_signed` sets the signature and
        // the actor stamp together — so an unsigned envelope that nonetheless
        // carries an `actor_stamp` is provably forged: it claims an author with no
        // signature to prove it. It must be QUARANTINED, not merely held/accepted;
        // otherwise the default (Relay) ingest accepts it and projection-authz +
        // seed-selection trust the unverified stamped author — a workspace takeover
        // (a forged `MemberRoleChanged`/`MemberRemoved` or founder-stamped snapshot
        // stamped as the real Owner). An unsigned event with NO stamp claims no
        // author, so it stays the lenient `Unverifiable`.
        return if env.actor_stamp.is_some() {
            Verdict::Quarantine(QuarantineReason::Unsigned)
        } else {
            Verdict::Unverifiable(QuarantineReason::Unsigned)
        };
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
    // Impersonation: the stamped author's account must be one the signing device
    // is cert-verified to. A device legitimately certified to more than one
    // account (an identity transition) may sign as any of them — each cert is
    // signed by that account's own key, so this can't be forged. The device is
    // known here (public_key resolved above), so a stamp matching none of its
    // certified accounts is provably an impersonation.
    if let Some(stamp) = &env.actor_stamp {
        if let Some(claimed) = stamp.actor.account_id {
            // Contested identity: two different keys were registered for this
            // account, so we can't tell which is genuine. Fail closed — this is
            // what stops a forged `AccountKeyRegistered` under a victim's account
            // id from hijacking it (the victim's real registration now makes the
            // account disputed rather than silently losing first-registration).
            if roster.is_disputed(claimed) {
                return Verdict::Quarantine(QuarantineReason::DisputedAccountKey);
            }
            if !roster.device_signs_for(device_id, claimed) {
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
                avatar_url: None,
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
                avatar_url: None,
            },
            recorded_at: Timestamp::epoch(),
        });
        hive_core::sign_envelope(&mut e, signer_device, signer_kp);
        e
    }

    #[test]
    fn explicitly_revoked_device_is_quarantined_even_with_a_valid_cert() {
        // P1-3: a DeviceRevoked event revokes a single device without removing its
        // account — its signed events are quarantined even though its cert still
        // chains to a current member account.
        let p = principal();
        let mut events = trust_events(&p, 1);
        // Sanity: without revocation the device's event is Valid.
        let before = build_roster(&events);
        let signed = authored(p.device_id, &p.device_kp, p.account_id, 10);
        assert_eq!(verdict_for(&before, &signed), Verdict::Valid);

        // Revoke the device; now the same signed event is quarantined.
        events.push(env(20, SessionEvent::DeviceRevoked { device_id: p.device_id }));
        let after = build_roster(&events);
        assert_eq!(
            verdict_for(&after, &signed),
            Verdict::Quarantine(QuarantineReason::RevokedDevice)
        );
    }

    #[test]
    fn conflicting_account_key_registration_disputes_the_account_and_fails_closed() {
        // P0-2 PoC: an attacker forges AccountKeyRegistered{victim_account, attacker_key}
        // (victim's account id is a guessable UUIDv5), racing the victim's genuine
        // registration. First-wins previously let the forgery pin the attacker's key
        // and impersonate the victim. Now the two different keys make the account
        // DISPUTED, and events stamped as it are quarantined — fail closed, no hijack.
        let victim = principal();
        let attacker_key = SigningKeypair::generate().unwrap();

        // Genuine trust events for the victim (registers victim's real key at
        // lamport 2), PLUS a forged registration of the SAME account id under the
        // attacker's key at a later lamport — a second, different key.
        let mut events = trust_events(&victim, 1);
        events.push(env(
            5,
            SessionEvent::AccountKeyRegistered {
                account_id: victim.account_id,
                signing_public_key: attacker_key.public_key_bytes().to_vec(),
            },
        ));
        let roster = build_roster(&events);
        assert!(roster.is_disputed(victim.account_id), "conflicting keys must dispute the account");

        // The victim's OWN genuinely-signed event (valid signature, certified
        // device) stamped as the victim is now quarantined because the account is
        // disputed — fail closed. Ambiguity is resolved as "trust neither," which
        // denies the attacker the takeover (their forged key can't be relied on).
        let genuine =
            authored(victim.device_id, &victim.device_kp, victim.account_id, 10);
        assert_eq!(
            verdict_for(&roster, &genuine),
            Verdict::Quarantine(QuarantineReason::DisputedAccountKey)
        );
    }

    #[test]
    fn unsigned_event_with_a_forged_stamp_is_quarantined_not_accepted() {
        // P0-1 PoC: a malicious member forges an UNSIGNED governance event stamped
        // as the real Owner. Genuine events are always signed (append_signed signs
        // + stamps together), so this is provably forged and MUST be quarantined —
        // not accepted (Relay mode), where projection-authz would trust the stamped
        // Owner and apply the takeover on every device.
        use crate::identity_verifier::{gate_ingest, IdentityCache, IdentityMode, IngestAction};

        let owner = principal();
        let roster = build_roster(&trust_events(&owner, 1));

        // Forged: unsigned (signer_device_id stays None) but stamped as the owner,
        // carrying a self-promotion to Owner.
        let mut forged = env(
            10,
            SessionEvent::MemberRoleChanged {
                change: hive_core::MemberRoleChange {
                    member_id: owner.account_id.to_string(),
                    old_role: WorkspaceRole::Contributor,
                    new_role: WorkspaceRole::Owner,
                },
            },
        );
        forged.actor_stamp = Some(ActorStamp {
            actor: ActorIdentity {
                id: owner.account_id.to_string(),
                display_name: "P".into(),
                kind: ActorKind::Human,
                account_id: Some(owner.account_id),
                device_id: Some(owner.device_id),
                git_email: None,
                key_agreement_public: None,
                avatar_url: None,
            },
            recorded_at: Timestamp::epoch(),
        });
        assert!(forged.signer_device_id.is_none(), "PoC envelope must be unsigned");

        // The verdict must be Quarantine, and Relay-mode ingest must DROP it.
        assert_eq!(
            verdict_for(&roster, &forged),
            Verdict::Quarantine(QuarantineReason::Unsigned)
        );
        let cache = IdentityCache::new();
        assert!(
            matches!(
                gate_ingest(IdentityMode::Relay, verdict_for(&roster, &forged), None, None, &cache),
                IngestAction::Quarantine(_)
            ),
            "Relay-mode ingest must quarantine a forged unsigned stamped event, not Accept it",
        );

        // Sanity: an unsigned event with NO stamp claims no author, so it stays the
        // lenient Unverifiable — genuinely unauthored events aren't newly dropped.
        let unstamped = env(
            11,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "x", "y"),
            },
        );
        assert_eq!(
            verdict_for(&roster, &unstamped),
            Verdict::Unverifiable(QuarantineReason::Unsigned)
        );
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
    fn device_certified_to_multiple_accounts_may_sign_as_any() {
        // Identity transition: the SAME device is certified to an old account and
        // a new one (e.g. a local id, then a GitHub sign-in). Each cert is signed
        // by its own account key, so events stamped with EITHER account are
        // authentic — but a third, uncertified account is still impersonation.
        let p = principal();
        let old_account = p.account_id;
        let new_account = Uuid::new_v4();
        let new_account_kp = SigningKeypair::generate().unwrap();

        let old_cert = DeviceCertificate::issue(
            &p.account_kp, old_account, p.device_id, &p.device_kp.public_key_bytes(), Timestamp::epoch(),
        );
        let new_cert = DeviceCertificate::issue(
            &new_account_kp, new_account, p.device_id, &p.device_kp.public_key_bytes(), Timestamp::epoch(),
        );
        let mk_member = |acct: Uuid| WorkspaceMember {
            id: acct.to_string(),
            actor: ActorIdentity {
                id: acct.to_string(),
                display_name: "P".into(),
                kind: ActorKind::Human,
                account_id: Some(acct),
                device_id: Some(p.device_id),
                git_email: None,
                key_agreement_public: None,
                avatar_url: None,
            },
            role: WorkspaceRole::Contributor,
            title: String::new(),
            index: 1,
            joined_at: Timestamp::epoch(),
        };
        let events = vec![
            env(1, SessionEvent::MemberAdded { member: mk_member(old_account) }),
            env(2, SessionEvent::MemberAdded { member: mk_member(new_account) }),
            env(3, SessionEvent::AccountKeyRegistered {
                account_id: old_account,
                signing_public_key: p.account_kp.public_key_bytes().to_vec(),
            }),
            env(4, SessionEvent::AccountKeyRegistered {
                account_id: new_account,
                signing_public_key: new_account_kp.public_key_bytes().to_vec(),
            }),
            env(5, SessionEvent::DeviceCertificateAdded { certificate: old_cert }),
            env(6, SessionEvent::DeviceCertificateAdded { certificate: new_cert }),
        ];
        let roster = build_roster(&events);

        // Signing as the new account → Valid (the device is certified to it).
        let as_new = authored(p.device_id, &p.device_kp, new_account, 100);
        assert_eq!(verdict_for(&roster, &as_new), Verdict::Valid);
        // Signing as the old account → still Valid.
        let as_old = authored(p.device_id, &p.device_kp, old_account, 101);
        assert_eq!(verdict_for(&roster, &as_old), Verdict::Valid);
        // Signing as a third, uncertified account → Impersonation.
        let as_other = authored(p.device_id, &p.device_kp, Uuid::new_v4(), 102);
        assert_eq!(
            verdict_for(&roster, &as_other),
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
