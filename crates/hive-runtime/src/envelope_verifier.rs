//! Verify-on-read with quarantine — ported from `EnvelopeVerifier.swift`.
//!
//! Before projecting a workspace's event stream, each signed envelope is
//! checked against the writing device's public key (resolved from the device
//! roster). Envelopes that fail — bad signature, unknown/revoked device, or
//! (under the clean-replacement default) unsigned — are diverted to a
//! quarantine list and excluded from projection, rather than poisoning state.

use hive_core::crypto::verify_envelope;
use hive_core::{CryptoError, DeviceIdentity, SessionEventEnvelope};
use std::collections::HashMap;
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
}
