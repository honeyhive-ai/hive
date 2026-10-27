//! The paired-phone registry, `tunnel/phones.json` (0600).
//!
//! `hive serve` holds it in memory and writes it through on every change;
//! `hive phones list|revoke` read and edit the file from another process (a
//! revocation also goes through `pending/`, so `hive serve` re-applies it even
//! if one of its own writes raced the CLI's).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{b64u_fixed, read_json, write_json};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhoneRecord {
    pub pid: String,
    pub name: String,
    /// b64u(phone auth Ed25519 public key): the credential's `pk`.
    pub auth_pub: String,
    /// b64u(phone Noise static public key).
    pub static_pub: String,
    /// b64u(push key) = HKDF(h_enroll, "push-key", 32).
    pub push_key: String,
    pub enrolled_at: u64,
    /// The latest `exp` issued to this phone (what a revocation lasts until).
    pub cred_exp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<u64>,
    #[serde(default)]
    pub push_enabled: bool,
    /// Last push `seq` sent (strictly increasing, survives restarts).
    #[serde(default)]
    pub push_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<u64>,
}

impl PhoneRecord {
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }

    pub fn static_key(&self) -> Option<[u8; 32]> {
        b64u_fixed(&self.static_pub).ok()
    }
}

#[derive(Default, Serialize, Deserialize)]
struct PhonesFile {
    #[serde(default)]
    v: u32,
    #[serde(default)]
    phones: Vec<PhoneRecord>,
}

pub struct Registry {
    path: PathBuf,
    inner: Mutex<Vec<PhoneRecord>>,
}

impl Registry {
    pub fn file(dir: &Path) -> PathBuf {
        dir.join("phones.json")
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let path = Self::file(dir);
        let phones = read_json::<PhonesFile>(&path)?.map(|f| f.phones).unwrap_or_default();
        Ok(Self { path, inner: Mutex::new(phones) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<PhoneRecord>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, phones: &[PhoneRecord]) -> Result<()> {
        write_json(&self.path, &PhonesFile { v: 1, phones: phones.to_vec() })
    }

    pub fn list(&self) -> Vec<PhoneRecord> {
        self.lock().clone()
    }

    pub fn get(&self, pid: &str) -> Option<PhoneRecord> {
        self.lock().iter().find(|r| r.pid == pid).cloned()
    }

    pub fn is_active(&self, pid: &str) -> bool {
        self.lock().iter().any(|r| r.pid == pid && r.is_active())
    }

    /// The active record whose Noise static is `static_pub` (S1).
    pub fn active_by_static(&self, static_pub: &[u8; 32]) -> Option<PhoneRecord> {
        self.lock().iter().find(|r| r.is_active() && r.static_key().as_ref() == Some(static_pub)).cloned()
    }

    /// Find by pid, or by name when it names exactly one phone.
    pub fn resolve(&self, pid_or_name: &str) -> Option<PhoneRecord> {
        let phones = self.lock();
        if let Some(r) = phones.iter().find(|r| r.pid == pid_or_name) {
            return Some(r.clone());
        }
        let named: Vec<_> = phones.iter().filter(|r| r.name == pid_or_name && r.is_active()).collect();
        (named.len() == 1).then(|| named[0].clone())
    }

    /// E3/E4: store an enrolled phone. An active record with the same static
    /// is replaced (the caller reused its pid).
    pub fn activate(&self, rec: PhoneRecord) -> Result<()> {
        let mut phones = self.lock();
        phones.retain(|r| !(r.pid == rec.pid || (r.is_active() && r.static_pub == rec.static_pub)));
        phones.push(rec);
        self.save(&phones)
    }

    /// Mark `pid` revoked. Returns its latest credential `exp`, or `None` when
    /// there is no such phone.
    pub fn revoke(&self, pid: &str) -> Result<Option<u64>> {
        let mut phones = self.lock();
        let Some(r) = phones.iter_mut().find(|r| r.pid == pid) else { return Ok(None) };
        let exp = r.cred_exp;
        if r.revoked_at.is_none() {
            r.revoked_at = Some(super::now());
            self.save(&phones)?;
        }
        Ok(Some(exp))
    }

    /// Record a newly issued credential `exp` (never moves backwards).
    pub fn set_cred_exp(&self, pid: &str, exp: u64) -> Result<()> {
        let mut phones = self.lock();
        if let Some(r) = phones.iter_mut().find(|r| r.pid == pid) {
            r.cred_exp = r.cred_exp.max(exp);
            self.save(&phones)?;
        }
        Ok(())
    }

    pub fn set_push(&self, pid: &str, enabled: bool) -> Result<bool> {
        let mut phones = self.lock();
        let Some(r) = phones.iter_mut().find(|r| r.pid == pid && r.is_active()) else { return Ok(false) };
        r.push_enabled = enabled;
        self.save(&phones)?;
        Ok(true)
    }

    pub fn touch(&self, pid: &str) {
        let mut phones = self.lock();
        if let Some(r) = phones.iter_mut().find(|r| r.pid == pid) {
            r.last_seen = Some(super::now());
            let _ = self.save(&phones);
        }
    }

    /// Bump and persist the push `seq` for `pid`; returns it with the key.
    pub fn next_push_seq(&self, pid: &str) -> Option<(u64, [u8; 32])> {
        let mut phones = self.lock();
        let r = phones.iter_mut().find(|r| r.pid == pid)?;
        let key = b64u_fixed(&r.push_key).ok()?;
        r.push_seq += 1;
        let seq = r.push_seq;
        self.save(&phones).ok()?;
        Some((seq, key))
    }
}
