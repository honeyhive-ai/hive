//! End-to-end encryption for the workspace key — the sealing half of the crypto
//! story (the signing half is in `crypto`).
//!
//! Each device has an X25519 **key-agreement** keypair (distinct from its
//! Ed25519 signing key). The symmetric workspace key is sealed *per device*
//! using an HPKE-base-style scheme: ephemeral X25519 → ECDH → HKDF-SHA256 →
//! ChaCha20-Poly1305. The relay only ever forwards ciphertext; only a holder of
//! a recipient device's secret can open the seal. Key **rotation** issues a new
//! version sealed only for the still-trusted devices, so a removed device — even
//! with old blobs — cannot read anything sealed after its removal.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::crypto::CryptoError;

const HKDF_INFO: &[u8] = b"hive-workspace-key-seal-v1";

/// An X25519 key-agreement keypair for receiving sealed workspace keys.
pub struct KeyAgreementKeypair {
    secret: StaticSecret,
}

impl KeyAgreementKeypair {
    pub fn generate() -> Result<Self, CryptoError> {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).map_err(|_| CryptoError::Random)?;
        Ok(Self {
            secret: StaticSecret::from(seed),
        })
    }

    pub fn from_seed(seed: &[u8]) -> Result<Self, CryptoError> {
        let seed: [u8; 32] = seed.try_into().map_err(|_| CryptoError::BadKey)?;
        Ok(Self {
            secret: StaticSecret::from(seed),
        })
    }

    pub fn seed_bytes(&self) -> [u8; 32] {
        self.secret.to_bytes()
    }

    pub fn public_key_bytes(&self) -> [u8; 32] {
        PublicKey::from(&self.secret).to_bytes()
    }
}

/// A workspace key sealed for one recipient device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SealedBlob {
    /// Ephemeral X25519 public key for this seal.
    pub ephemeral_public: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// Derive a 32-byte key + 12-byte nonce from the ECDH shared secret, bound to
/// both public keys so the same shared secret can't be replayed across pairs.
fn derive_key_nonce(shared: &[u8], ephemeral_pub: &[u8], recipient_pub: &[u8]) -> ([u8; 32], [u8; 12]) {
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(ephemeral_pub);
    salt.extend_from_slice(recipient_pub);
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut okm = [0u8; 44];
    hk.expand(HKDF_INFO, &mut okm).expect("hkdf expand");
    let mut key = [0u8; 32];
    let mut nonce = [0u8; 12];
    key.copy_from_slice(&okm[..32]);
    nonce.copy_from_slice(&okm[32..]);
    (key, nonce)
}

/// Seal `plaintext` (the workspace key) for `recipient_public` (an X25519
/// public key).
pub fn seal(recipient_public: &[u8], plaintext: &[u8]) -> Result<SealedBlob, CryptoError> {
    let recipient: [u8; 32] = recipient_public.try_into().map_err(|_| CryptoError::BadKey)?;
    let recipient_pk = PublicKey::from(recipient);

    let mut eph_seed = [0u8; 32];
    getrandom::getrandom(&mut eph_seed).map_err(|_| CryptoError::Random)?;
    let ephemeral = StaticSecret::from(eph_seed);
    let ephemeral_pub = PublicKey::from(&ephemeral).to_bytes();

    let shared = ephemeral.diffie_hellman(&recipient_pk);
    let (key, nonce) = derive_key_nonce(shared.as_bytes(), &ephemeral_pub, &recipient);

    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| CryptoError::VerifyFailed)?;

    Ok(SealedBlob {
        ephemeral_public: ephemeral_pub.to_vec(),
        ciphertext,
    })
}

/// Open a [`SealedBlob`] with the recipient device's keypair.
pub fn open(recipient: &KeyAgreementKeypair, blob: &SealedBlob) -> Result<Vec<u8>, CryptoError> {
    let eph: [u8; 32] = blob
        .ephemeral_public
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::BadKey)?;
    let ephemeral_pub = PublicKey::from(eph);
    let recipient_pub = recipient.public_key_bytes();

    let shared = recipient.secret.diffie_hellman(&ephemeral_pub);
    let (key, nonce) = derive_key_nonce(shared.as_bytes(), &eph, &recipient_pub);

    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    cipher
        .decrypt(Nonce::from_slice(&nonce), blob.ciphertext.as_ref())
        .map_err(|_| CryptoError::VerifyFailed)
}

/// A versioned symmetric workspace key, sealed to each trusted device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceKeyRotation {
    pub version: u32,
    /// device id → the workspace key sealed for that device's X25519 key.
    pub sealed: std::collections::BTreeMap<String, SealedBlob>,
}

/// Generate a fresh 32-byte symmetric workspace key.
pub fn generate_workspace_key() -> Result<[u8; 32], CryptoError> {
    let mut key = [0u8; 32];
    getrandom::getrandom(&mut key).map_err(|_| CryptoError::Random)?;
    Ok(key)
}

/// Derive a stable 32-byte workspace key from a shared passphrase (HKDF-SHA256).
/// Lets peers agree on a key out-of-band without exchanging raw bytes — the
/// relay-forwarding E2EE path.
pub fn derive_workspace_key(passphrase: &str) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(b"hive-workspace-key"), passphrase.as_bytes());
    let mut key = [0u8; 32];
    hk.expand(b"hive-workspace-key-v1", &mut key).expect("hkdf expand");
    key
}

/// A symmetrically-sealed blob: what travels over the wire so the relay only
/// ever sees ciphertext. Sealed with the workspace key (ChaCha20-Poly1305) and
/// a fresh random nonce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SealedEnvelope {
    /// Key epoch this ciphertext was sealed under — the [`WorkspaceKeyRotation`]
    /// version, or `0` for the base/passphrase key. A client keyed with multiple
    /// epochs uses this to pick the right key, so a rotation no longer strands
    /// history: events sealed under an older epoch stay readable. Absent on
    /// pre-epoch bodies (`#[serde(default)]` → 0).
    #[serde(default)]
    pub version: u32,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// Seal arbitrary bytes (a serialized envelope) with the workspace key for epoch
/// `version` (stamped onto the output so a keyed reader can pick the right key).
pub fn seal_symmetric(
    key: &[u8; 32],
    version: u32,
    plaintext: &[u8],
) -> Result<SealedEnvelope, CryptoError> {
    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce).map_err(|_| CryptoError::Random)?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| CryptoError::VerifyFailed)?;
    Ok(SealedEnvelope {
        version,
        nonce: nonce.to_vec(),
        ciphertext,
    })
}

/// Open a [`SealedEnvelope`] with the workspace key. Also authenticates: only a
/// holder of the key could have produced openable ciphertext.
pub fn open_symmetric(key: &[u8; 32], sealed: &SealedEnvelope) -> Result<Vec<u8>, CryptoError> {
    if sealed.nonce.len() != 12 {
        return Err(CryptoError::BadKey);
    }
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(&sealed.nonce), sealed.ciphertext.as_ref())
        .map_err(|_| CryptoError::VerifyFailed)
}

impl WorkspaceKeyRotation {
    /// Seal `key` for each `(device_id, x25519_public)` recipient.
    pub fn seal_for_devices(
        version: u32,
        key: &[u8],
        recipients: &[(String, Vec<u8>)],
    ) -> Result<Self, CryptoError> {
        let mut sealed = std::collections::BTreeMap::new();
        for (device_id, public) in recipients {
            sealed.insert(device_id.clone(), seal(public, key)?);
        }
        Ok(Self { version, sealed })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let device = KeyAgreementKeypair::generate().unwrap();
        let secret = b"the-32-byte-symmetric-workspace!"; // 32 bytes
        let blob = seal(&device.public_key_bytes(), secret).unwrap();
        let opened = open(&device, &blob).unwrap();
        assert_eq!(opened, secret);
    }

    #[test]
    fn wrong_device_cannot_open() {
        let alice = KeyAgreementKeypair::generate().unwrap();
        let bob = KeyAgreementKeypair::generate().unwrap();
        let blob = seal(&alice.public_key_bytes(), b"workspace-key-payload-1234567890").unwrap();
        assert!(open(&bob, &blob).is_err());
    }

    #[test]
    fn symmetric_seal_open_roundtrip_and_tamper() {
        let key = derive_workspace_key("team-alpha secret");
        let plaintext = br#"{"kind":"messageAppended"}"#;
        let sealed = seal_symmetric(&key, 0, plaintext).unwrap();
        assert_ne!(sealed.ciphertext, plaintext);
        assert_eq!(open_symmetric(&key, &sealed).unwrap(), plaintext);

        // wrong key fails
        let other = derive_workspace_key("different");
        assert!(open_symmetric(&other, &sealed).is_err());

        // tampered ciphertext fails (AEAD)
        let mut bad = sealed.clone();
        bad.ciphertext[0] ^= 0xff;
        assert!(open_symmetric(&key, &bad).is_err());
    }

    #[test]
    fn sealed_envelope_stamps_and_defaults_epoch() {
        let key = derive_workspace_key("epoch test");
        let sealed = seal_symmetric(&key, 3, b"payload").unwrap();
        assert_eq!(sealed.version, 3, "epoch is stamped on the sealed body");

        // A legacy pre-epoch body (no `version`) deserializes to epoch 0.
        let legacy = serde_json::json!({ "nonce": sealed.nonce, "ciphertext": sealed.ciphertext });
        let parsed: SealedEnvelope = serde_json::from_value(legacy).unwrap();
        assert_eq!(parsed.version, 0, "missing version defaults to base epoch");
    }

    #[test]
    fn derive_workspace_key_is_stable() {
        assert_eq!(derive_workspace_key("x"), derive_workspace_key("x"));
        assert_ne!(derive_workspace_key("x"), derive_workspace_key("y"));
    }

    #[test]
    fn rotation_excludes_removed_device() {
        let alice = KeyAgreementKeypair::generate().unwrap();
        let bob = KeyAgreementKeypair::generate().unwrap();
        let key_v1 = generate_workspace_key().unwrap();

        // v1 sealed for both
        let v1 = WorkspaceKeyRotation::seal_for_devices(
            1,
            &key_v1,
            &[
                ("alice".into(), alice.public_key_bytes().to_vec()),
                ("bob".into(), bob.public_key_bytes().to_vec()),
            ],
        )
        .unwrap();
        assert_eq!(open(&bob, &v1.sealed["bob"]).unwrap(), key_v1);

        // rotate: new key sealed only for alice (bob removed)
        let key_v2 = generate_workspace_key().unwrap();
        let v2 = WorkspaceKeyRotation::seal_for_devices(
            2,
            &key_v2,
            &[("alice".into(), alice.public_key_bytes().to_vec())],
        )
        .unwrap();
        assert!(v2.sealed.get("bob").is_none(), "bob must not receive v2");
        assert_eq!(open(&alice, &v2.sealed["alice"]).unwrap(), key_v2);
        assert_ne!(key_v1, key_v2);
    }
}
