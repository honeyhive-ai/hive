//! Noise construction (§7.1): `Noise_IK_25519_ChaChaPoly_SHA256` for
//! sessions, `Noise_IKpsk1_…` (psk from the pairing secret) for enrollment.
//! Phone = initiator, host = responder, prologue = `ctx("noise", host_id)`.
//!
//! snow's default X25519 doesn't reject an all-zero DH output (a low-order
//! peer key); §7.1 requires aborting, so DH goes through [`CheckedResolver`].

use snow::params::{CipherChoice, DHChoice, HashChoice};
use snow::resolvers::{CryptoResolver, DefaultResolver};
use snow::types::{Cipher, Dh, Hash, Random};
use snow::{Builder, HandshakeState};

use super::ctx;

pub const IK: &str = "Noise_IK_25519_ChaChaPoly_SHA256";
pub const IK_PSK1: &str = "Noise_IKpsk1_25519_ChaChaPoly_SHA256";
/// Max Noise message (§7.1).
pub const MAX_MSG: usize = 65_535;

pub fn prologue(host_id: &str) -> Vec<u8> {
    ctx("noise", &[host_id.as_bytes()])
}

struct CheckedDh(Box<dyn Dh>);

impl Dh for CheckedDh {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn pub_len(&self) -> usize {
        self.0.pub_len()
    }
    fn priv_len(&self) -> usize {
        self.0.priv_len()
    }
    fn set(&mut self, privkey: &[u8]) {
        self.0.set(privkey)
    }
    fn generate(&mut self, rng: &mut dyn Random) {
        self.0.generate(rng)
    }
    fn pubkey(&self) -> &[u8] {
        self.0.pubkey()
    }
    fn privkey(&self) -> &[u8] {
        self.0.privkey()
    }
    fn dh(&self, pubkey: &[u8], out: &mut [u8]) -> Result<(), snow::Error> {
        self.0.dh(pubkey, out)?;
        let n = self.0.pub_len().min(out.len());
        // Constant-time-ish: fold every byte before deciding.
        if out[..n].iter().fold(0u8, |acc, b| acc | b) == 0 {
            return Err(snow::Error::Dh);
        }
        Ok(())
    }
}

/// snow's default primitives, with DH outputs checked for all-zero.
pub struct CheckedResolver;

impl CryptoResolver for CheckedResolver {
    fn resolve_rng(&self) -> Option<Box<dyn Random>> {
        DefaultResolver.resolve_rng()
    }
    fn resolve_dh(&self, choice: &DHChoice) -> Option<Box<dyn Dh>> {
        DefaultResolver.resolve_dh(choice).map(|d| Box::new(CheckedDh(d)) as Box<dyn Dh>)
    }
    fn resolve_hash(&self, choice: &HashChoice) -> Option<Box<dyn Hash>> {
        DefaultResolver.resolve_hash(choice)
    }
    fn resolve_cipher(&self, choice: &CipherChoice) -> Option<Box<dyn Cipher>> {
        DefaultResolver.resolve_cipher(choice)
    }
}

fn builder(psk: Option<&[u8; 32]>) -> Builder<'_> {
    let name = if psk.is_some() { IK_PSK1 } else { IK };
    Builder::with_resolver(name.parse().expect("valid Noise name"), Box::new(CheckedResolver))
}

/// The host's responder. `psk` = enrollment mode.
pub fn responder(static_priv: &[u8; 32], host_id: &str, psk: Option<&[u8; 32]>) -> Result<HandshakeState, snow::Error> {
    responder_with(static_priv, host_id, psk, None)
}

/// [`responder`] with an injected ephemeral (test vectors only).
pub fn responder_with(
    static_priv: &[u8; 32],
    host_id: &str,
    psk: Option<&[u8; 32]>,
    ephemeral: Option<&[u8; 32]>,
) -> Result<HandshakeState, snow::Error> {
    let prologue = prologue(host_id);
    let mut b = builder(psk).local_private_key(static_priv).prologue(&prologue);
    if let Some(p) = psk {
        b = b.psk(1, p);
    }
    if let Some(e) = ephemeral {
        b = b.fixed_ephemeral_key_for_testing_only(e);
    }
    b.build_responder()
}

/// A phone-side initiator (the tests' fake phone; the real phone is Swift).
pub fn initiator(
    static_priv: &[u8; 32],
    host_static_pub: &[u8; 32],
    host_id: &str,
    psk: Option<&[u8; 32]>,
    ephemeral: Option<&[u8; 32]>,
) -> Result<HandshakeState, snow::Error> {
    let prologue = prologue(host_id);
    let mut b = builder(psk).local_private_key(static_priv).remote_public_key(host_static_pub).prologue(&prologue);
    if let Some(p) = psk {
        b = b.psk(1, p);
    }
    if let Some(e) = ephemeral {
        b = b.fixed_ephemeral_key_for_testing_only(e);
    }
    b.build_initiator()
}
