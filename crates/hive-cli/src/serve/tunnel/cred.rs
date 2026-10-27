//! Phone credentials, `hpc1` (§5).
//!
//! `hpc1.<b64u(claims_json)>.<b64u(sig)>`, the signature over the exact ASCII
//! `"hpc1." + b64u(claims_json)`. The host issues them; the relay (and the
//! tests) verify them with [`verify`].

use std::collections::BTreeMap;
use std::fmt;

use ed25519_dalek::{Signature, VerifyingKey};
use serde::de::{Deserializer, MapAccess, Visitor};
use serde::Deserialize;
use serde_json::Value;

use super::{b64u, b64u_decode, HostKeys, CRED_LIFETIME_SECS, SKEW_SECS};

/// Relay cap on `exp − iat` (§5 step 5).
pub const MAX_LIFETIME_SECS: u64 = 400 * 86_400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    pub hid: String,
    pub pid: String,
    pub pk: [u8; 32],
    pub iat: u64,
    pub exp: u64,
}

impl Claims {
    /// The claims JSON, keys in the spec's order.
    pub fn to_json(&self) -> String {
        format!(
            "{{\"hid\":{},\"pid\":{},\"pk\":{},\"iat\":{},\"exp\":{}}}",
            Value::from(self.hid.as_str()),
            Value::from(self.pid.as_str()),
            Value::from(b64u(self.pk)),
            self.iat,
            self.exp
        )
    }
}

/// Sign `claims` as an `hpc1` token.
pub fn sign(keys: &HostKeys, claims: &Claims) -> String {
    let signed = format!("hpc1.{}", b64u(claims.to_json()));
    let sig = keys.sign(signed.as_bytes());
    format!("{signed}.{}", b64u(sig))
}

/// Issue a credential for `(pid, pk)` at `iat` with the default lifetime.
/// Returns the token and its `exp`.
pub fn issue(keys: &HostKeys, pid: &str, pk: &[u8; 32], iat: u64) -> (String, u64) {
    let claims = Claims { hid: keys.host_id().to_string(), pid: pid.to_string(), pk: *pk, iat, exp: iat + CRED_LIFETIME_SECS };
    (sign(keys, &claims), claims.exp)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredError {
    /// 4001
    Unauthorized,
    /// 4004
    HostUnknown,
    /// 4002
    Expired,
}

impl CredError {
    pub fn code(self) -> &'static str {
        match self {
            CredError::Unauthorized => "unauthorized",
            CredError::HostUnknown => "host_unknown",
            CredError::Expired => "credential_expired",
        }
    }
}

impl fmt::Display for CredError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// A JSON object that refuses duplicate keys (§2: verifiers of signed JSON
/// MUST reject duplicates; serde_json silently keeps the last one).
struct NoDupObject(BTreeMap<String, Value>);

impl<'de> Deserialize<'de> for NoDupObject {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = NoDupObject;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object without duplicate keys")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<NoDupObject, A::Error> {
                let mut out = BTreeMap::new();
                while let Some((k, v)) = map.next_entry::<String, Value>()? {
                    if out.insert(k.clone(), v).is_some() {
                        return Err(serde::de::Error::custom(format!("duplicate key {k}")));
                    }
                }
                Ok(NoDupObject(out))
            }
        }
        d.deserialize_map(V)
    }
}

/// Verify `token` for `host` (the `host` query parameter) against the host
/// signing key, in the order of §5. `None` = host not registered.
pub fn verify(token: &str, host: &str, host_key: Option<(&str, &VerifyingKey)>, now: u64) -> Result<Claims, CredError> {
    use CredError::*;
    // 1. Shape.
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 || parts[0] != "hpc1" {
        return Err(Unauthorized);
    }
    let sig_bytes = b64u_decode(parts[2]).map_err(|_| Unauthorized)?;
    let sig = Signature::from_slice(&sig_bytes).map_err(|_| Unauthorized)?;
    let claims_raw = b64u_decode(parts[1]).map_err(|_| Unauthorized)?;
    // 2. hid == host, and the host is registered. The hid is read before the
    //    signature check only to route the lookup; nothing else is trusted yet.
    let obj: NoDupObject = serde_json::from_slice(&claims_raw).map_err(|_| Unauthorized)?;
    let hid = obj.0.get("hid").and_then(Value::as_str).ok_or(Unauthorized)?;
    if hid != host {
        return Err(Unauthorized);
    }
    let Some((registered_hid, key)) = host_key else { return Err(HostUnknown) };
    if registered_hid != host {
        return Err(HostUnknown);
    }
    // 3. Signature over the exact transmitted bytes.
    let signed = format!("hpc1.{}", parts[1]);
    key.verify_strict(signed.as_bytes(), &sig).map_err(|_| Unauthorized)?;
    // 4. Claims well-formed.
    let s = |k: &str| obj.0.get(k).and_then(Value::as_str).map(str::to_string).ok_or(Unauthorized);
    let n = |k: &str| obj.0.get(k).and_then(Value::as_u64).ok_or(Unauthorized);
    let pid = s("pid")?;
    if pid.len() != 22 || b64u_decode(&pid).map(|b| b.len()).ok() != Some(16) {
        return Err(Unauthorized);
    }
    let pk: [u8; 32] = super::b64u_fixed(&s("pk")?).map_err(|_| Unauthorized)?;
    let (iat, exp) = (n("iat")?, n("exp")?);
    // 5. Times.
    if iat > now + SKEW_SECS {
        return Err(Unauthorized);
    }
    if exp <= now {
        return Err(Expired);
    }
    if exp.saturating_sub(iat) > MAX_LIFETIME_SECS {
        return Err(Unauthorized);
    }
    Ok(Claims { hid: hid.to_string(), pid, pk, iat, exp })
}
