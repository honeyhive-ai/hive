//! Identity bootstrap + private-key storage — ported from `Identity.swift`,
//! `IdentityStore.swift`, and `KeychainIdentityStore`/`FileIdentityStore`.
//!
//! On first run we generate an Ed25519 **account** keypair and a per-device
//! keypair, issue a [`DeviceCertificate`] chaining the device key to the
//! account key, and persist the **public** records as JSON. The **private**
//! seeds go to a [`KeyVault`].
//!
//! Phase 2 ships a [`FileKeyVault`] (raw seed bytes on disk) — cross-platform
//! and CI-portable. An OS-keystore-backed vault (the `keyring` crate, with its
//! per-OS backend features) drops in behind the same trait at packaging time;
//! it needs platform-specific backends that don't exist in headless CI, so it
//! is intentionally not the default here.

use std::path::{Path, PathBuf};

use hive_core::crypto::{DeviceCertificate, DeviceIdentity, HumanAccount, Platform, SigningKeypair};
use hive_core::Timestamp;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("crypto error: {0}")]
    Crypto(#[from] hive_core::CryptoError),
    #[error("key vault: {0}")]
    Vault(String),
    #[error("stored seed for {0} is missing or malformed")]
    MissingSeed(String),
}

/// Stores/loads 32-byte private signing seeds, keyed by a stable id.
pub trait KeyVault {
    fn store_seed(&self, key_id: &str, seed: &[u8; 32]) -> Result<(), IdentityError>;
    fn load_seed(&self, key_id: &str) -> Result<Option<[u8; 32]>, IdentityError>;
}

/// File-backed key vault: one `<key_id>.seed` file of raw bytes per key.
pub struct FileKeyVault {
    dir: PathBuf,
}

impl FileKeyVault {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().join("keys"),
        }
    }

    fn path_for(&self, key_id: &str) -> PathBuf {
        // key ids look like "account:<uuid>" / "device:<uuid>"; keep them
        // filesystem-safe.
        self.dir.join(format!("{}.seed", key_id.replace(':', "_")))
    }
}

impl KeyVault for FileKeyVault {
    fn store_seed(&self, key_id: &str, seed: &[u8; 32]) -> Result<(), IdentityError> {
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(self.path_for(key_id), seed)?;
        Ok(())
    }

    fn load_seed(&self, key_id: &str) -> Result<Option<[u8; 32]>, IdentityError> {
        match std::fs::read(self.path_for(key_id)) {
            Ok(bytes) => {
                let seed: [u8; 32] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| IdentityError::MissingSeed(key_id.to_string()))?;
                Ok(Some(seed))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// The persisted public identity for this install: one account + this device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredIdentity {
    pub account: HumanAccount,
    pub device: DeviceIdentity,
}

fn account_key_id(account_id: Uuid) -> String {
    format!("account:{account_id}")
}

fn device_key_id(device_id: Uuid) -> String {
    format!("device:{device_id}")
}

/// Loads or bootstraps the local identity, backed by a [`KeyVault`] for private
/// seeds and a JSON file for the public records.
pub struct IdentityStore<V: KeyVault> {
    records_path: PathBuf,
    vault: V,
}

impl<V: KeyVault> IdentityStore<V> {
    pub fn new(app_data_dir: impl AsRef<Path>, vault: V) -> Self {
        Self {
            records_path: app_data_dir.as_ref().join("identity.json"),
            vault,
        }
    }

    pub fn load(&self) -> Result<Option<StoredIdentity>, IdentityError> {
        match std::fs::read_to_string(&self.records_path) {
            Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Load the existing identity or generate one (account keypair + device
    /// keypair + device certificate). Idempotent across launches.
    pub fn bootstrap(
        &self,
        display_name: &str,
        handle: &str,
        device_name: &str,
    ) -> Result<StoredIdentity, IdentityError> {
        if let Some(existing) = self.load()? {
            return Ok(existing);
        }

        let now = Timestamp::now();

        // Account root of trust.
        let account_kp = SigningKeypair::generate()?;
        let account_id = Uuid::new_v4();
        self.vault
            .store_seed(&account_key_id(account_id), &account_kp.seed_bytes())?;
        let account = HumanAccount {
            id: account_id,
            display_name: display_name.to_string(),
            handle: handle.to_string(),
            signing_public_key: account_kp.public_key_bytes().to_vec(),
            created_at: now,
        };

        // This device, certified by the account key.
        let device_kp = SigningKeypair::generate()?;
        let device_id = Uuid::new_v4();
        self.vault
            .store_seed(&device_key_id(device_id), &device_kp.seed_bytes())?;
        let certificate = DeviceCertificate::issue(
            &account_kp,
            account_id,
            device_id,
            &device_kp.public_key_bytes(),
            now,
        );
        let device = DeviceIdentity {
            id: device_id,
            account_id,
            platform: Platform::current(),
            device_name: device_name.to_string(),
            signing_public_key: device_kp.public_key_bytes().to_vec(),
            certificate,
            created_at: now,
            revoked_at: None,
        };

        let stored = StoredIdentity { account, device };
        self.save(&stored)?;
        Ok(stored)
    }

    /// Update the account display name (keys unchanged) and persist.
    pub fn update_display_name(&self, display_name: &str) -> Result<StoredIdentity, IdentityError> {
        let mut stored = self
            .load()?
            .ok_or_else(|| IdentityError::Vault("no identity to update".into()))?;
        stored.account.display_name = display_name.to_string();
        self.save(&stored)?;
        Ok(stored)
    }

    fn save(&self, stored: &StoredIdentity) -> Result<(), IdentityError> {
        if let Some(parent) = self.records_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.records_path, serde_json::to_vec_pretty(stored)?)?;
        Ok(())
    }

    /// Reconstruct the account signing keypair from the vault.
    pub fn account_keypair(&self, account_id: Uuid) -> Result<SigningKeypair, IdentityError> {
        let seed = self
            .vault
            .load_seed(&account_key_id(account_id))?
            .ok_or_else(|| IdentityError::MissingSeed(account_key_id(account_id)))?;
        Ok(SigningKeypair::from_seed(&seed)?)
    }

    /// Reconstruct this device's signing keypair from the vault — used to sign
    /// event envelopes.
    pub fn device_keypair(&self, device_id: Uuid) -> Result<SigningKeypair, IdentityError> {
        let seed = self
            .vault
            .load_seed(&device_key_id(device_id))?
            .ok_or_else(|| IdentityError::MissingSeed(device_key_id(device_id)))?;
        Ok(SigningKeypair::from_seed(&seed)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::crypto::verify_envelope;
    use hive_core::{ChatMessage, MessageRole, SessionEvent, SessionEventEnvelope};

    fn store(dir: &Path) -> IdentityStore<FileKeyVault> {
        IdentityStore::new(dir, FileKeyVault::new(dir))
    }

    #[test]
    fn bootstrap_is_idempotent_and_certificate_chains() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let first = s.bootstrap("Mara", "mara", "Mara's Mac").unwrap();
        let again = s.bootstrap("ignored", "ignored", "ignored").unwrap();
        assert_eq!(first, again, "identity must be stable across launches");

        // device certificate chains to the account key
        assert!(first
            .device
            .certificate
            .verify(&first.account.signing_public_key)
            .is_ok());
    }

    #[test]
    fn device_keypair_signs_envelopes_verifiable_by_public_record() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let id = s.bootstrap("Mara", "mara", "Mara's Mac").unwrap();

        let kp = s.device_keypair(id.device.id).unwrap();
        let mut env = SessionEventEnvelope::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            1,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "Mara", "signed by my device"),
            },
        );
        hive_core::sign_envelope(&mut env, id.device.id, &kp);

        // the public device record verifies what the private seed signed
        assert!(verify_envelope(&env, &id.device.signing_public_key).is_ok());
    }
}
