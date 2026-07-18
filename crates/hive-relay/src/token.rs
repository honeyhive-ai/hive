//! Signed entitlement tokens.
//!
//! A billing/license backend holds an **Ed25519 private key** and issues compact
//! tokens; the relay verifies them with the matching **public key**
//! (`HIVE_RELAY_TOKEN_PUBKEY`). Asymmetric on purpose:
//!
//! - the relay never holds an issuing secret — it can only *verify*, not *mint*;
//! - an on-prem **enterprise** relay can verify a Hive-issued license offline,
//!   with no callback to Hive;
//! - revocation-before-expiry is a future concern (short `exp` + reissue covers
//!   most cases; an explicit revocation list can be layered on later).
//!
//! Wire format (compact, three `.`-separated parts):
//!
//! ```text
//! hrt1.<b64url(claims_json)>.<b64url(ed25519_sig)>
//! ```
//!
//! The signature covers the ASCII bytes of `hrt1.<b64url(claims_json)>` (the
//! first two parts joined by `.`), so claims can't be altered without the key.
//!
//! The plain-allowlist policy (`HIVE_RELAY_ACCESS_TOKENS`) and the open default
//! both still work; signed tokens are an upgrade for when the relay needs to
//! read per-plan *limits* (member cap, retention, TURN) and RBAC capabilities
//! out of the token rather than just checking set membership.

use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

const PREFIX: &str = "hrt1";

/// Claims carried by a signed entitlement token. Forward-compatible: unknown
/// fields are ignored on decode, and the relay ignores any capability it does
/// not yet enforce — so the issuer can add claims before the relay learns them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct TokenClaims {
    /// Subject — the account or org id this token entitles.
    pub sub: String,
    /// Plan name: `"free" | "pro" | "team" | "enterprise"`.
    #[serde(default)]
    pub plan: String,
    /// Expiry, unix seconds. `0` = never expires.
    #[serde(default)]
    pub exp: u64,
    /// Max members the relay should admit to a gated workspace. `None` =
    /// unlimited.
    #[serde(default)]
    pub max_members: Option<u32>,
    /// Backfill retention window in days. `None` = unlimited.
    #[serde(default)]
    pub retention_days: Option<u32>,
    /// Whether guaranteed TURN-style forwarding is granted.
    #[serde(default)]
    pub turn: bool,
    /// RBAC capabilities granted to the subject (e.g. `"remove_member"`,
    /// `"rotate_key"`, `"view_audit"`). Forward-compatible: the relay ignores
    /// capabilities it does not enforce yet.
    #[serde(default)]
    pub caps: Vec<String>,
}

impl TokenClaims {
    /// True if `exp` is set and now-or-past.
    pub fn is_expired(&self, now_unix: u64) -> bool {
        self.exp != 0 && now_unix >= self.exp
    }

    /// Whether the subject holds a named RBAC capability.
    pub fn has_cap(&self, cap: &str) -> bool {
        self.caps.iter().any(|c| c == cap)
    }
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

/// Mint a signed `hrt1.…` token (issuer side — needs the secret SigningKey).
/// The relay never calls this; it's for the `hive-relay issue` operator command
/// / a billing backend.
pub fn issue(signing: &SigningKey, claims: &TokenClaims) -> String {
    let json = serde_json::to_vec(claims).expect("serialize claims");
    let body = format!("{PREFIX}.{}", b64().encode(json));
    let sig = signing.sign(body.as_bytes());
    format!("{body}.{}", b64().encode(sig.to_bytes()))
}

/// Generate a fresh Ed25519 issuer keypair (operator tooling — `hive-relay
/// keygen`). The private key stays with the issuer; only its public key goes on
/// the relay as `HIVE_RELAY_TOKEN_PUBKEY`.
pub fn generate_signing_key() -> SigningKey {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).expect("OS RNG");
    SigningKey::from_bytes(&seed)
}

/// Parse a 64-char-hex Ed25519 secret key into a SigningKey (issuer side).
pub fn parse_signing_key(hex: &str) -> Option<SigningKey> {
    let s = hex.trim();
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    let arr: [u8; 32] = bytes.try_into().ok()?;
    Some(SigningKey::from_bytes(&arr))
}

/// Lowercase-hex encode 32 bytes (for printing keys).
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Verify a compact `hrt1.…` token against the issuer public key. Returns the
/// claims iff the signature is valid. Expiry is **not** checked here (kept pure
/// and clock-free for testing) — the caller checks [`TokenClaims::is_expired`].
pub fn verify(token: &str, pubkey: &VerifyingKey) -> Option<TokenClaims> {
    let mut parts = token.split('.');
    let p0 = parts.next()?;
    let p1 = parts.next()?;
    let p2 = parts.next()?;
    if parts.next().is_some() || p0 != PREFIX {
        return None;
    }
    let sig_bytes = b64().decode(p2).ok()?;
    let sig = Signature::from_slice(&sig_bytes).ok()?;
    let signed = format!("{p0}.{p1}");
    pubkey.verify(signed.as_bytes(), &sig).ok()?;
    let claims_bytes = b64().decode(p1).ok()?;
    serde_json::from_slice(&claims_bytes).ok()
}

/// Parse an Ed25519 public key from hex (64 chars) or base64 (std or url-safe).
pub fn parse_pubkey(s: &str) -> Option<VerifyingKey> {
    let s = s.trim();
    let bytes: Vec<u8> = if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        (0..32)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
            .collect::<Option<Vec<u8>>>()?
    } else {
        b64()
            .decode(s)
            .ok()
            .or_else(|| base64::engine::general_purpose::STANDARD.decode(s).ok())?
    };
    let arr: [u8; 32] = bytes.try_into().ok()?;
    VerifyingKey::from_bytes(&arr).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    #[test]
    fn roundtrip_valid_token() {
        let sk = keypair();
        let claims = TokenClaims {
            sub: "org_acme".into(),
            plan: "team".into(),
            exp: 0,
            max_members: Some(50),
            retention_days: None,
            turn: true,
            caps: vec!["remove_member".into(), "view_audit".into()],
        };
        let tok = issue(&sk, &claims);
        let got = verify(&tok, &sk.verifying_key()).expect("valid");
        assert_eq!(got, claims);
        assert!(got.has_cap("view_audit"));
        assert!(!got.has_cap("manage_billing"));
        assert!(!got.is_expired(9_999_999_999));
    }

    #[test]
    fn rejects_tampered_claims() {
        let sk = keypair();
        let tok = issue(&sk, &TokenClaims { sub: "a".into(), plan: "pro".into(), ..Default::default() });
        // Flip the claims segment to a different (validly-encoded) payload.
        let mut parts: Vec<&str> = tok.split('.').collect();
        let forged = b64().encode(br#"{"sub":"a","plan":"enterprise"}"#);
        parts[1] = &forged;
        let tampered = parts.join(".");
        assert!(verify(&tampered, &sk.verifying_key()).is_none());
    }

    #[test]
    fn rejects_wrong_key() {
        let sk = keypair();
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let tok = issue(&sk, &TokenClaims { sub: "a".into(), ..Default::default() });
        assert!(verify(&tok, &other.verifying_key()).is_none());
    }

    #[test]
    fn rejects_malformed() {
        let sk = keypair();
        assert!(verify("nope", &sk.verifying_key()).is_none());
        assert!(verify("hrt1.only-two", &sk.verifying_key()).is_none());
        assert!(verify("hrt2.a.b", &sk.verifying_key()).is_none()); // wrong prefix
    }

    #[test]
    fn expiry_check() {
        let c = TokenClaims { exp: 1000, ..Default::default() };
        assert!(!c.is_expired(999));
        assert!(c.is_expired(1000));
        assert!(c.is_expired(1001));
        let never = TokenClaims { exp: 0, ..Default::default() };
        assert!(!never.is_expired(u64::MAX));
    }

    #[test]
    fn pubkey_parsing_hex_and_b64() {
        let sk = keypair();
        let vk = sk.verifying_key();
        let hex: String = vk.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(parse_pubkey(&hex).unwrap(), vk);
        let b64s = base64::engine::general_purpose::STANDARD.encode(vk.to_bytes());
        assert_eq!(parse_pubkey(&b64s).unwrap(), vk);
        assert!(parse_pubkey("garbage").is_none());
    }
}
