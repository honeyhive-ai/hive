//! Host long-term keys (§3): the Ed25519 signing key (credentials, control
//! challenge) and the X25519 Noise static `hs`, in `tunnel/keys.json` (0600).

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::{b64u, b64u_fixed, ctx, random_bytes, read_json, sha256, write_json};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeysFile {
    v: u32,
    /// b64u(Ed25519 seed).
    sign_seed: String,
    /// b64u(X25519 private key).
    static_priv: String,
    created_at: u64,
}

pub struct HostKeys {
    sign: SigningKey,
    static_priv: [u8; 32],
    static_pub: [u8; 32],
    host_id: String,
}

/// `b64u(SHA256(ctx("host-id", host_sign_pub))[0..16])`.
pub fn host_id_for(sign_pub: &[u8; 32]) -> String {
    b64u(&sha256(&ctx("host-id", &[sign_pub]))[..16])
}

/// X25519 public key for a raw private key (clamped inside X25519).
pub fn x25519_pub(private: &[u8; 32]) -> [u8; 32] {
    x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*private)).to_bytes()
}

impl HostKeys {
    pub fn from_parts(sign_seed: [u8; 32], static_priv: [u8; 32]) -> Self {
        let sign = SigningKey::from_bytes(&sign_seed);
        let host_id = host_id_for(sign.verifying_key().as_bytes());
        Self { sign, static_priv, static_pub: x25519_pub(&static_priv), host_id }
    }

    pub fn generate() -> Self {
        Self::from_parts(random_bytes(), random_bytes())
    }

    pub fn file(dir: &Path) -> PathBuf {
        dir.join("keys.json")
    }

    pub fn load(dir: &Path) -> Result<Option<Self>> {
        let path = Self::file(dir);
        let Some(f) = read_json::<KeysFile>(&path)? else { return Ok(None) };
        if f.v != 1 {
            bail!("{}: unsupported keys version {}", path.display(), f.v);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)?.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                eprintln!("remote: {} was mode {mode:o}; tightening to 0600", path.display());
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            }
        }
        Ok(Some(Self::from_parts(b64u_fixed(&f.sign_seed)?, b64u_fixed(&f.static_priv)?)))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        write_json(
            &Self::file(dir),
            &KeysFile {
                v: 1,
                sign_seed: b64u(self.sign.to_bytes()),
                static_priv: b64u(self.static_priv),
                created_at: super::now(),
            },
        )
    }

    pub fn load_or_create(dir: &Path) -> Result<Self> {
        if let Some(k) = Self::load(dir)? {
            return Ok(k);
        }
        let k = Self::generate();
        k.save(dir)?;
        Ok(k)
    }

    pub fn host_id(&self) -> &str {
        &self.host_id
    }

    pub fn sign_pub(&self) -> VerifyingKey {
        self.sign.verifying_key()
    }

    pub fn static_pub(&self) -> &[u8; 32] {
        &self.static_pub
    }

    pub fn static_priv(&self) -> &[u8; 32] {
        &self.static_priv
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.sign.sign(msg).to_bytes()
    }

    /// `Ed25519(host_sign_key, ctx("host-auth", hostId, nonce))` (§6.2).
    pub fn sign_challenge(&self, nonce: &[u8; 32]) -> [u8; 64] {
        self.sign(&ctx("host-auth", &[self.host_id.as_bytes(), nonce]))
    }
}
