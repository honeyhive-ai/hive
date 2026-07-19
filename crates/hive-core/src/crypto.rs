//! Signing identity + signed envelopes — ported from swift-crypto usage in
//! `Identity.swift` / `SignedEnvelopeBuilder.swift` / `EnvelopeVerifier.swift`.
//!
//! Identity uses **Ed25519** (`ed25519-dalek`). Each human account owns a
//! signing keypair; each device owns its own keypair plus a `DeviceCertificate`
//! signed by the account key (so a relying party who trusts the account key can
//! transitively trust the device key). Every workspace event envelope is signed
//! by the writing device and verified on read.
//!
//! The x25519/ChaCha20-Poly1305/HPKE *sealing* primitives for the end-to-end
//! encrypted workspace key are introduced with the relay in Phase 7; this
//! module is the signing/authenticity half.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::events::SessionEventEnvelope;
use crate::identity::{ActorIdentity, ActorKind};
use crate::time_util::Timestamp;

/// Length of an Ed25519 public key / private seed.
pub const KEY_LEN: usize = 32;
/// Length of an Ed25519 signature.
pub const SIG_LEN: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("random source failure")]
    Random,
    #[error("malformed key (expected {KEY_LEN} bytes)")]
    BadKey,
    #[error("malformed signature (expected {SIG_LEN} bytes)")]
    BadSignature,
    #[error("envelope is not signed")]
    Unsigned,
    #[error("signature verification failed")]
    VerifyFailed,
}

/// An Ed25519 signing keypair. The private seed never leaves the owning device
/// (persisted via the OS keystore in `hive-runtime`).
pub struct SigningKeypair {
    signing: SigningKey,
}

impl SigningKeypair {
    /// Generate a fresh keypair from the OS CSPRNG.
    pub fn generate() -> Result<Self, CryptoError> {
        let mut seed = [0u8; KEY_LEN];
        getrandom::getrandom(&mut seed).map_err(|_| CryptoError::Random)?;
        Ok(Self {
            signing: SigningKey::from_bytes(&seed),
        })
    }

    /// Reconstruct from a stored 32-byte seed.
    pub fn from_seed(seed: &[u8]) -> Result<Self, CryptoError> {
        let seed: [u8; KEY_LEN] = seed.try_into().map_err(|_| CryptoError::BadKey)?;
        Ok(Self {
            signing: SigningKey::from_bytes(&seed),
        })
    }

    /// The 32-byte secret seed — store this securely, never transmit it.
    pub fn seed_bytes(&self) -> [u8; KEY_LEN] {
        self.signing.to_bytes()
    }

    /// The 32-byte public key, safe to publish.
    pub fn public_key_bytes(&self) -> [u8; KEY_LEN] {
        self.signing.verifying_key().to_bytes()
    }

    pub fn sign(&self, message: &[u8]) -> [u8; SIG_LEN] {
        self.signing.sign(message).to_bytes()
    }
}

/// Verify a detached signature against a raw public key.
pub fn verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
    let key_bytes: [u8; KEY_LEN] = public_key.try_into().map_err(|_| CryptoError::BadKey)?;
    let sig_bytes: [u8; SIG_LEN] = signature.try_into().map_err(|_| CryptoError::BadSignature)?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|_| CryptoError::BadKey)?;
    let sig = Signature::from_bytes(&sig_bytes);
    key.verify(message, &sig).map_err(|_| CryptoError::VerifyFailed)
}

// ---------------------------------------------------------------------------
// Canonical preimages (deterministic bytes that get signed)
// ---------------------------------------------------------------------------

/// Deterministic bytes signed for an event envelope. Covers the routing fields,
/// the ordering key (`lamport`), the claimed authorship (`actor_stamp`,
/// `timestamp`, `scope`), and the payload — so none can be tampered with. In
/// particular, binding `lamport` and `actor_stamp` closes two gaps: a peer can no
/// longer forge another member's authorship, nor rewrite an event's position in
/// the canonical order. JSON encodings are stable because serde serializes
/// struct fields and tagged enums in declaration order.
pub fn envelope_preimage(env: &SessionEventEnvelope) -> Vec<u8> {
    let mut out = Vec::with_capacity(160);
    // v2: added lamport + timestamp + actor_stamp + scope to the signed bytes.
    out.extend_from_slice(b"hive-envelope-v2\0");
    out.extend_from_slice(env.workspace_id.as_bytes());
    out.extend_from_slice(env.session_id.as_bytes());
    out.extend_from_slice(&env.sequence.to_le_bytes());
    out.extend_from_slice(&env.lamport.to_le_bytes());
    out.extend_from_slice(env.event_id.as_bytes());
    out.extend_from_slice(&serde_json::to_vec(&env.scope).unwrap_or_default());
    out.extend_from_slice(&serde_json::to_vec(&env.timestamp).unwrap_or_default());
    out.extend_from_slice(&serde_json::to_vec(&env.actor_stamp).unwrap_or_default());
    out.extend_from_slice(&serde_json::to_vec(&env.payload).unwrap_or_default());
    out
}

/// Sign an envelope in place with the writing device's keypair, stamping the
/// device id and signature.
pub fn sign_envelope(env: &mut SessionEventEnvelope, device_id: Uuid, keypair: &SigningKeypair) {
    let sig = keypair.sign(&envelope_preimage(env));
    env.signer_device_id = Some(device_id);
    env.signature = Some(sig.to_vec());
}

/// Verify an envelope's signature against the writing device's public key.
pub fn verify_envelope(env: &SessionEventEnvelope, device_public_key: &[u8]) -> Result<(), CryptoError> {
    let signature = env.signature.as_ref().ok_or(CryptoError::Unsigned)?;
    verify(device_public_key, &envelope_preimage(env), signature)
}

// ---------------------------------------------------------------------------
// Account / device identity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Platform {
    Macos,
    Windows,
    Linux,
    Ios,
}

impl Platform {
    /// The platform this binary was compiled for.
    pub fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            Platform::Macos
        }
        #[cfg(target_os = "windows")]
        {
            Platform::Windows
        }
        #[cfg(target_os = "ios")]
        {
            Platform::Ios
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "ios")))]
        {
            Platform::Linux
        }
    }
}

/// Public record of a human account (its signing public key is the root of
/// trust for the account's devices).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanAccount {
    pub id: Uuid,
    pub display_name: String,
    pub handle: String,
    pub signing_public_key: Vec<u8>,
    #[serde(default)]
    pub created_at: Timestamp,
}

impl HumanAccount {
    /// An `ActorIdentity` view of this account, for stamping messages/events.
    pub fn actor(&self) -> ActorIdentity {
        ActorIdentity {
            id: self.id.to_string(),
            display_name: self.display_name.clone(),
            kind: ActorKind::Human,
            account_id: Some(self.id),
            device_id: None,
            git_email: None,
            key_agreement_public: None,
        }
    }
}

/// A certificate binding a device key to an account, signed by the account key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCertificate {
    pub device_id: Uuid,
    pub device_public_key: Vec<u8>,
    pub account_id: Uuid,
    #[serde(default)]
    pub issued_at: Timestamp,
    pub signature: Vec<u8>,
}

impl DeviceCertificate {
    fn preimage(
        device_id: Uuid,
        device_public_key: &[u8],
        account_id: Uuid,
        issued_at: Timestamp,
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(96);
        out.extend_from_slice(b"hive-device-cert-v1\0");
        out.extend_from_slice(device_id.as_bytes());
        out.extend_from_slice(device_public_key);
        out.extend_from_slice(account_id.as_bytes());
        let issued = serde_json::to_value(issued_at)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        out.extend_from_slice(issued.as_bytes());
        out
    }

    /// Issue a certificate for `device_id`/`device_public_key`, signed by the
    /// account keypair.
    pub fn issue(
        account_keypair: &SigningKeypair,
        account_id: Uuid,
        device_id: Uuid,
        device_public_key: &[u8],
        issued_at: Timestamp,
    ) -> Self {
        let preimage = Self::preimage(device_id, device_public_key, account_id, issued_at);
        let signature = account_keypair.sign(&preimage).to_vec();
        Self {
            device_id,
            device_public_key: device_public_key.to_vec(),
            account_id,
            issued_at,
            signature,
        }
    }

    /// Verify the certificate against the account's public key.
    pub fn verify(&self, account_public_key: &[u8]) -> Result<(), CryptoError> {
        let preimage = Self::preimage(
            self.device_id,
            &self.device_public_key,
            self.account_id,
            self.issued_at,
        );
        verify(account_public_key, &preimage, &self.signature)
    }
}

/// Public record of one device belonging to an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceIdentity {
    pub id: Uuid,
    pub account_id: Uuid,
    pub platform: Platform,
    pub device_name: String,
    pub signing_public_key: Vec<u8>,
    pub certificate: DeviceCertificate,
    #[serde(default)]
    pub created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<Timestamp>,
}

impl DeviceIdentity {
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{ChatMessage, MessageRole};
    use crate::events::SessionEvent;

    #[test]
    fn sign_and_verify_roundtrip() {
        let kp = SigningKeypair::generate().unwrap();
        let msg = b"the quick brown fox";
        let sig = kp.sign(msg);
        assert!(verify(&kp.public_key_bytes(), msg, &sig).is_ok());
        // tampered message fails
        assert!(verify(&kp.public_key_bytes(), b"a different message!", &sig).is_err());
    }

    #[test]
    fn seed_roundtrip_reproduces_key() {
        let kp = SigningKeypair::generate().unwrap();
        let restored = SigningKeypair::from_seed(&kp.seed_bytes()).unwrap();
        assert_eq!(kp.public_key_bytes(), restored.public_key_bytes());
    }

    fn sample_envelope() -> SessionEventEnvelope {
        SessionEventEnvelope::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            7,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "Mara", "signed hello"),
            },
        )
    }

    #[test]
    fn envelope_signature_detects_tampering() {
        let kp = SigningKeypair::generate().unwrap();
        let device = Uuid::new_v4();
        let mut env = sample_envelope();
        sign_envelope(&mut env, device, &kp);

        assert_eq!(env.signer_device_id, Some(device));
        assert!(verify_envelope(&env, &kp.public_key_bytes()).is_ok());

        // mutate the sequence -> signature no longer matches
        let mut tampered = env.clone();
        tampered.sequence = 8;
        assert!(verify_envelope(&tampered, &kp.public_key_bytes()).is_err());

        // v2 preimage: mutating the ordering key (lamport) breaks the signature,
        // so a peer can't rewrite an event's position in the canonical order.
        let mut reordered = env.clone();
        reordered.lamport = env.lamport.wrapping_add(1);
        assert!(verify_envelope(&reordered, &kp.public_key_bytes()).is_err());

        // v2 preimage: forging the claimed author (actor_stamp) breaks it too.
        let mut spoofed = env.clone();
        spoofed.actor_stamp = Some(crate::identity::ActorStamp {
            actor: crate::identity::ActorIdentity::new("mallory", "Mallory", crate::identity::ActorKind::Human),
            recorded_at: crate::time_util::Timestamp::epoch(),
        });
        assert!(verify_envelope(&spoofed, &kp.public_key_bytes()).is_err());

        // wrong key -> fails
        let other = SigningKeypair::generate().unwrap();
        assert!(verify_envelope(&env, &other.public_key_bytes()).is_err());
    }

    #[test]
    fn unsigned_envelope_reports_unsigned() {
        let env = sample_envelope();
        assert!(matches!(
            verify_envelope(&env, &[0u8; KEY_LEN]),
            Err(CryptoError::Unsigned)
        ));
    }

    #[test]
    fn device_certificate_chains_to_account_key() {
        let account = SigningKeypair::generate().unwrap();
        let account_id = Uuid::new_v4();
        let device = SigningKeypair::generate().unwrap();
        let device_id = Uuid::new_v4();

        let cert = DeviceCertificate::issue(
            &account,
            account_id,
            device_id,
            &device.public_key_bytes(),
            Timestamp::epoch(),
        );
        assert!(cert.verify(&account.public_key_bytes()).is_ok());

        // a different account key cannot have issued it
        let impostor = SigningKeypair::generate().unwrap();
        assert!(cert.verify(&impostor.public_key_bytes()).is_err());
    }
}
