//! Enrolling another machine as one of **your own devices** — the `hive serve`
//! box your phone talks through — so everything it does is authored as you.
//!
//! Hive identity is account-scoped: an account's signing key certifies each of
//! its devices (`DeviceCertificate`), and every envelope is signed by a device
//! key and stamped with the account it acts for. The verifier
//! (`envelope_verifier::verdict_for`) already accepts an event stamped as
//! account `A` from device `D` iff a valid `A → D` certificate is in the log
//! and `A` is a member — for every desktop, old or new. So making the box "one
//! of the owner's devices" needs no new event: the owner's desktop (the only
//! holder of the owner's account key) signs a certificate for the box's device
//! key, and the box starts stamping its events as the owner.
//!
//! The flow:
//! 1. On the box, `hive serve enroll` prints an **enrollment code**
//!    (`hivedev1:…`): the workspace, the box's device id and public signing key,
//!    its label, and a signature by that device key (proof of possession, so a
//!    code can't name someone else's key). Also printed by `hive serve` while it
//!    waits for approval.
//! 2. The owner pastes it into the desktop ("Add my device"), checks the
//!    fingerprint against the box's terminal, and approves. The desktop appends a
//!    `DeviceCertificateAdded` signed by the owner's account key. Without that
//!    approval the box cannot claim the owner: an event it stamps as the owner is
//!    quarantined everywhere as impersonation.
//! 3. The box sees the certificate after its next pull ([`standing`]) and from
//!    then on authors as the owner. The desktop also retires the box's old
//!    member, which becomes a *retired alias* of the owner
//!    ([`retired_aliases`], `hive_core::DeviceLinks::link_retired`).
//! 4. Revoking: the owner's desktop appends `DeviceRevoked` for the box's
//!    device. Every desktop quarantines that device's later events; the box sees
//!    it and stops writing. Re-enrolling takes a fresh device key
//!    (`hive serve enroll --new-device`), since a revoked device id stays revoked.

use std::collections::{HashMap, HashSet};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hive_core::crypto::{verify, verify_envelope, SigningKeypair};
use hive_core::{ActorIdentity, ActorKind, ChatSession, SessionEvent, SessionEventEnvelope};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::envelope_verifier::{build_roster, DeviceKeyResolver};

/// Prefix of an enrollment code.
pub const CODE_PREFIX: &str = "hivedev1:";
/// Longest label carried in a code, in bytes.
pub const MAX_LABEL: usize = 64;
const PREIMAGE_TAG: &[u8] = b"hive-device-enroll-v1\0";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnrollError {
    #[error("not a Hive device enrollment code (expected {CODE_PREFIX}...)")]
    NotACode,
    #[error("the enrollment code is damaged or incomplete")]
    Malformed,
    #[error("the enrollment code's signature doesn't match its device key")]
    BadSignature,
}

/// What a box asks to be enrolled as: its device id + public signing key, in
/// one workspace, under a label. Signed by that device key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEnrollment {
    pub workspace_id: Uuid,
    pub device_id: Uuid,
    pub device_public_key: [u8; 32],
    pub label: String,
    pub signature: [u8; 64],
}

fn clamp_label(label: &str) -> String {
    let mut out = String::new();
    for c in label.trim().chars() {
        if out.len() + c.len_utf8() > MAX_LABEL {
            break;
        }
        out.push(c);
    }
    out
}

fn preimage(workspace_id: Uuid, device_id: Uuid, key: &[u8; 32], label: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(PREIMAGE_TAG.len() + 64 + label.len());
    out.extend_from_slice(PREIMAGE_TAG);
    out.extend_from_slice(workspace_id.as_bytes());
    out.extend_from_slice(device_id.as_bytes());
    out.extend_from_slice(key);
    out.extend_from_slice(label.as_bytes());
    out
}

impl DeviceEnrollment {
    /// Build and sign a request for `device_id` (whose key is `device_key`).
    pub fn new(workspace_id: Uuid, device_id: Uuid, device_key: &SigningKeypair, label: &str) -> Self {
        let label = clamp_label(label);
        let key = device_key.public_key_bytes();
        let signature = device_key.sign(&preimage(workspace_id, device_id, &key, &label));
        Self { workspace_id, device_id, device_public_key: key, label, signature }
    }

    /// The `hivedev1:` code: base64url of ws(16) ‖ device(16) ‖ key(32) ‖
    /// sig(64) ‖ label (UTF-8).
    pub fn encode(&self) -> String {
        let mut b = Vec::with_capacity(128 + self.label.len());
        b.extend_from_slice(self.workspace_id.as_bytes());
        b.extend_from_slice(self.device_id.as_bytes());
        b.extend_from_slice(&self.device_public_key);
        b.extend_from_slice(&self.signature);
        b.extend_from_slice(self.label.as_bytes());
        format!("{CODE_PREFIX}{}", URL_SAFE_NO_PAD.encode(b))
    }

    /// Parse and verify a code (whitespace and line breaks from a terminal
    /// copy are ignored).
    pub fn decode(code: &str) -> Result<Self, EnrollError> {
        let compact: String = code.chars().filter(|c| !c.is_whitespace()).collect();
        let body = compact.strip_prefix(CODE_PREFIX).ok_or(EnrollError::NotACode)?;
        let b = URL_SAFE_NO_PAD.decode(body.as_bytes()).map_err(|_| EnrollError::Malformed)?;
        if b.len() < 128 || b.len() > 128 + MAX_LABEL {
            return Err(EnrollError::Malformed);
        }
        let uuid = |r: std::ops::Range<usize>| Uuid::from_slice(&b[r]).map_err(|_| EnrollError::Malformed);
        let label = std::str::from_utf8(&b[128..]).map_err(|_| EnrollError::Malformed)?.to_string();
        let e = Self {
            workspace_id: uuid(0..16)?,
            device_id: uuid(16..32)?,
            device_public_key: b[32..64].try_into().map_err(|_| EnrollError::Malformed)?,
            signature: b[64..128].try_into().map_err(|_| EnrollError::Malformed)?,
            label,
        };
        e.verify()?;
        Ok(e)
    }

    /// Proof of possession: the device key signed this request.
    pub fn verify(&self) -> Result<(), EnrollError> {
        let pre = preimage(self.workspace_id, self.device_id, &self.device_public_key, &self.label);
        verify(&self.device_public_key, &pre, &self.signature).map_err(|_| EnrollError::BadSignature)
    }

    /// Short fingerprint of the device key, shown on both ends so the owner can
    /// check they approve the box they're looking at.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.device_public_key)
    }
}

/// `XXXX-XXXX-XXXX`: the first 6 bytes of SHA-256(key), upper-case hex.
pub fn fingerprint(key: &[u8]) -> String {
    let d = Sha256::digest(key);
    let hex: String = d[..6].iter().map(|b| format!("{b:02X}")).collect();
    format!("{}-{}-{}", &hex[0..4], &hex[4..8], &hex[8..12])
}

/// Where a device stands in a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// No other account has certified it: it acts as its own identity.
    Own,
    /// A member's account certified it: it acts as that member (the owner).
    OwnerDevice(ActorIdentity),
    /// The device was revoked: it must not write.
    Revoked,
}

/// Every account key ever registered (or rotated to), per account.
fn account_keys(trust: &[SessionEventEnvelope]) -> HashMap<Uuid, Vec<Vec<u8>>> {
    let mut keys: HashMap<Uuid, Vec<Vec<u8>>> = HashMap::new();
    for env in trust {
        match &env.payload {
            SessionEvent::AccountKeyRegistered { account_id, signing_public_key } => {
                keys.entry(*account_id).or_default().push(signing_public_key.clone());
            }
            SessionEvent::AccountKeyRotated { account_id, new_signing_public_key, .. } => {
                keys.entry(*account_id).or_default().push(new_signing_public_key.clone());
            }
            _ => {}
        }
    }
    keys
}

/// device → (its public key, the accounts with a cert for it that verifies
/// against a key that account registered).
fn certified_devices(trust: &[SessionEventEnvelope]) -> HashMap<Uuid, (Vec<u8>, HashSet<Uuid>)> {
    let keys = account_keys(trust);
    let mut out: HashMap<Uuid, (Vec<u8>, HashSet<Uuid>)> = HashMap::new();
    for env in trust {
        let SessionEvent::DeviceCertificateAdded { certificate: c } = &env.payload else { continue };
        let valid = keys.get(&c.account_id).is_some_and(|ks| ks.iter().any(|k| c.verify(k).is_ok()));
        if valid {
            out.entry(c.device_id)
                .or_insert_with(|| (c.device_public_key.clone(), HashSet::new()))
                .1
                .insert(c.account_id);
        }
    }
    out
}

/// This device's standing in a workspace, from its trust history (`trust`: the
/// workspace's key/cert/revocation/membership events) and roster (`config`).
///
/// `own_account_key` is the box's own account public key: a cert under it is
/// the box's own, not an owner's. With several owners certifying the device
/// (unusual), `prefer` (an actor id or display name) picks one, else the
/// smallest account id wins (deterministic).
pub fn standing(
    trust: &[SessionEventEnvelope],
    config: Option<&ChatSession>,
    device_id: Uuid,
    own_account_key: &[u8],
    prefer: Option<&str>,
) -> Standing {
    let roster = build_roster(trust);
    if roster.is_revoked(device_id) {
        return Standing::Revoked;
    }
    let Some(config) = config else { return Standing::Own };
    let mut owners: Vec<&hive_core::WorkspaceMember> = config
        .members
        .iter()
        .filter(|m| m.actor.kind == ActorKind::Human)
        .filter(|m| {
            m.actor.account_id.is_some_and(|a| {
                roster.device_signs_for(device_id, a)
                    && !roster.is_disputed(a)
                    && roster.account_signing_key(a).is_some_and(|k| k != own_account_key)
            })
        })
        .collect();
    owners.sort_by_key(|m| m.actor.account_id);
    let preferred = prefer.and_then(|p| {
        owners.iter().find(|m| m.actor.id == p || m.actor.display_name.eq_ignore_ascii_case(p)).copied()
    });
    match preferred.or_else(|| owners.first().copied()) {
        Some(m) => Standing::OwnerDevice(m.actor.clone()),
        None => Standing::Own,
    }
}

/// Retired aliases: `(alias actor id, owner actor id)` for each actor that is
/// no longer a member of `config` but whose device — one that **signed events
/// as that actor** — the owner's account has certified. Signing as the alias
/// proves the device was the alias's own; the owner's cert proves the owner
/// claims it. A former member that certifies someone else's device under its
/// own account gets nothing: that device never signed as it.
pub fn retired_aliases(trust: &[SessionEventEnvelope], config: &ChatSession) -> Vec<(String, String)> {
    let devices = certified_devices(trust);
    // (device, account) pairs proven by a signature.
    let mut signed_as: HashSet<(Uuid, Uuid)> = HashSet::new();
    for env in trust {
        let (Some(dev), Some(acct)) =
            (env.signer_device_id, env.actor_stamp.as_ref().and_then(|s| s.actor.account_id))
        else {
            continue;
        };
        if let Some((key, _)) = devices.get(&dev) {
            if !signed_as.contains(&(dev, acct)) && verify_envelope(env, key).is_ok() {
                signed_as.insert((dev, acct));
            }
        }
    }
    let member_of_account = |a: Uuid| {
        config.members.iter().find(|m| m.actor.kind == ActorKind::Human && m.actor.account_id == Some(a))
    };
    let is_member = |a: Uuid| {
        let id = a.to_string();
        config.members.iter().any(|m| m.actor.account_id == Some(a) || m.actor.id == id || m.id == id)
    };
    let mut out = Vec::new();
    for (dev, (_, accounts)) in &devices {
        for alias in accounts {
            if is_member(*alias) || !signed_as.contains(&(*dev, *alias)) {
                continue;
            }
            let mut owners: Vec<_> = accounts.iter().filter(|o| *o != alias).filter_map(|o| member_of_account(*o)).collect();
            owners.sort_by_key(|m| m.actor.account_id);
            if let Some(owner) = owners.first() {
                out.push((alias.to_string(), owner.actor.id.clone()));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// One device certified to an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnDevice {
    pub device_id: Uuid,
    pub fingerprint: String,
    pub revoked: bool,
    /// Other accounts that also certified it (a box's own bootstrap account).
    pub also_certified_by: Vec<Uuid>,
}

/// The devices `account` has certified in this workspace, revoked ones included.
pub fn devices_of(trust: &[SessionEventEnvelope], account: Uuid) -> Vec<OwnDevice> {
    let revoked: HashSet<Uuid> = trust
        .iter()
        .filter_map(|e| match &e.payload {
            SessionEvent::DeviceRevoked { device_id } => Some(*device_id),
            _ => None,
        })
        .collect();
    let mut out: Vec<OwnDevice> = certified_devices(trust)
        .into_iter()
        .filter(|(_, (_, accts))| accts.contains(&account))
        .map(|(device_id, (key, accts))| {
            let mut also: Vec<Uuid> = accts.into_iter().filter(|a| *a != account).collect();
            also.sort();
            OwnDevice { device_id, fingerprint: fingerprint(&key), revoked: revoked.contains(&device_id), also_certified_by: also }
        })
        .collect();
    out.sort_by_key(|d| d.device_id);
    out
}

/// Accounts (other than `except`) holding a valid certificate for `device`.
pub fn accounts_certifying(trust: &[SessionEventEnvelope], device: Uuid, except: Uuid) -> Vec<Uuid> {
    let mut v: Vec<Uuid> = certified_devices(trust)
        .remove(&device)
        .map(|(_, a)| a.into_iter().filter(|a| *a != except).collect())
        .unwrap_or_default();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_roundtrips_and_verifies() {
        let kp = SigningKeypair::generate().unwrap();
        let (ws, dev) = (Uuid::new_v4(), Uuid::new_v4());
        let e = DeviceEnrollment::new(ws, dev, &kp, "  home box  ");
        assert_eq!(e.label, "home box");
        let code = e.encode();
        assert!(code.starts_with(CODE_PREFIX));
        // A terminal copy may wrap the code across lines.
        let wrapped = format!("{}\n  {}", &code[..40], &code[40..]);
        let back = DeviceEnrollment::decode(&wrapped).unwrap();
        assert_eq!(back, e);
        assert_eq!(back.fingerprint(), fingerprint(&kp.public_key_bytes()));
        assert_eq!(back.fingerprint().len(), 14);
    }

    #[test]
    fn code_tampering_is_rejected() {
        let kp = SigningKeypair::generate().unwrap();
        let e = DeviceEnrollment::new(Uuid::new_v4(), Uuid::new_v4(), &kp, "box");
        // Someone else's key under this signature.
        let mut swapped = e.clone();
        swapped.device_public_key = SigningKeypair::generate().unwrap().public_key_bytes();
        assert_eq!(DeviceEnrollment::decode(&swapped.encode()), Err(EnrollError::BadSignature));
        // A relabel.
        let mut relabeled = e.clone();
        relabeled.label = "Alex's MacBook".into();
        assert_eq!(DeviceEnrollment::decode(&relabeled.encode()), Err(EnrollError::BadSignature));
        // Another device id.
        let mut other = e.clone();
        other.device_id = Uuid::new_v4();
        assert_eq!(DeviceEnrollment::decode(&other.encode()), Err(EnrollError::BadSignature));
        assert_eq!(DeviceEnrollment::decode("hivews1:abc"), Err(EnrollError::NotACode));
        assert_eq!(DeviceEnrollment::decode("hivedev1:abc"), Err(EnrollError::Malformed));
    }

    #[test]
    fn long_labels_are_clamped_on_a_char_boundary() {
        let kp = SigningKeypair::generate().unwrap();
        let e = DeviceEnrollment::new(Uuid::new_v4(), Uuid::new_v4(), &kp, &"é".repeat(100));
        assert!(e.label.len() <= MAX_LABEL);
        assert!(DeviceEnrollment::decode(&e.encode()).is_ok());
    }
}

/// The whole lifecycle across real devices: an owner's desktop, their `hive
/// serve` box and a teammate, synced through the in-process relay, every pull
/// verified on ingest exactly as in production.
#[cfg(test)]
mod flow_tests {
    use std::path::{Path, PathBuf};

    use hive_core::{workspace_config_session_id, ActionProposal, MessageRole, ProposalKind, WorkspaceHost, WorkspaceRole};

    use super::*;
    use crate::{ChatService, EventStore, FileKeyVault, IdentityStore, RelayClient, SyncEngine};

    async fn spawn_relay() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, crate::test_relay::router()).await.unwrap() });
        format!("http://{addr}")
    }

    struct Dev {
        svc: ChatService,
        dir: PathBuf,
        engine: SyncEngine,
    }

    impl Dev {
        fn open(dir: &Path, name: &str, relay: &str, ws: Uuid) -> Self {
            std::fs::create_dir_all(dir).unwrap();
            let ids = IdentityStore::new(dir, FileKeyVault::new(dir));
            let me = ids.bootstrap(name, &name.to_lowercase(), name).unwrap();
            let svc = ChatService::new(
                EventStore::open(dir.join("hive.db")).unwrap(),
                me.device.id,
                ids.device_keypair(me.device.id).unwrap(),
                ids.account_keypair(me.account.id).unwrap(),
                me.account.actor(),
            );
            let key = hive_core::derive_workspace_key("enroll flow passphrase");
            let engine = SyncEngine::new(RelayClient::new(relay), ws.to_string()).with_key(key);
            Self { svc, dir: dir.to_path_buf(), engine }
        }

        async fn sync(&mut self) {
            let mut store = EventStore::open(self.dir.join("hive.db")).unwrap();
            self.engine.push_new(&store).await.unwrap();
            self.engine.pull(&mut store).await.unwrap();
        }

        fn id(&self) -> String {
            self.svc.author().id.clone()
        }

        fn bodies(&self, chat: Uuid) -> Vec<(String, String)> {
            self.svc
                .load(chat)
                .unwrap()
                .unwrap()
                .messages
                .iter()
                .filter(|m| m.role == MessageRole::User)
                .map(|m| (m.body.clone(), m.actor_identity.as_ref().map(|a| a.id.clone()).unwrap_or_default()))
                .collect()
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn box_enrolled_as_the_owners_device_authors_as_the_owner_until_revoked() {
        let relay = spawn_relay().await;
        let tmp = tempfile::tempdir().unwrap();
        let ws = Uuid::new_v4();
        let cfg = workspace_config_session_id(ws);

        // Alex's desktop founds the workspace and opens a chat with a proposal.
        let mut mac = Dev::open(&tmp.path().join("mac"), "Alex", &relay, ws);
        mac.svc.ensure_self_member(cfg, ws).unwrap();
        let chat = mac.svc.create_chat("plans", ws, "").unwrap().id;
        let proposal = ActionProposal::new("ship it", ProposalKind::Decision, mac.id());
        let pid = proposal.id;
        mac.svc.upsert_proposal(chat, ws, proposal).unwrap();
        mac.sync().await;

        // A teammate joins.
        let mut taylor = Dev::open(&tmp.path().join("taylor"), "Taylor", &relay, ws);
        taylor.sync().await;
        taylor.svc.ensure_self_member(cfg, ws).unwrap();
        taylor.sync().await;

        // The box joins the old way: its own member.
        let mut sff = Dev::open(&tmp.path().join("sff"), "hive-cli", &relay, ws);
        sff.sync().await;
        sff.svc.ensure_self_member(cfg, ws).unwrap();
        let box_actor = sff.id();
        sff.svc.post_user_message(chat, ws, "before enrollment").unwrap();
        sff.sync().await;
        assert_eq!(sff.svc.device_standing(ws, None).unwrap(), Standing::Own);

        // Unapproved, the box tries to claim Alex: stamped as Alex, signed by its
        // own device. Everyone else drops it as impersonation.
        let alex = mac.svc.author().clone();
        let mut rogue = Dev::open(&tmp.path().join("sff"), "hive-cli", &relay, ws);
        rogue.svc.act_as_owner_device(alex.clone());
        rogue.svc.post_user_message(chat, ws, "forged as Alex").unwrap();
        rogue.sync().await;
        drop(rogue);
        mac.sync().await;
        taylor.sync().await;
        for dev in [&mac, &taylor] {
            let bodies = dev.bodies(chat);
            assert!(bodies.contains(&("before enrollment".into(), box_actor.clone())), "{bodies:?}");
            assert!(!bodies.iter().any(|(b, _)| b == "forged as Alex"), "{bodies:?}");
        }
        // A delegated box can't approve devices or publish an identity as Alex.
        let mut delegated = Dev::open(&tmp.path().join("sff"), "hive-cli", &relay, ws);
        delegated.svc.act_as_owner_device(alex.clone());
        let any = DeviceEnrollment::new(ws, Uuid::new_v4(), &SigningKeypair::generate().unwrap(), "x");
        assert!(delegated.svc.certify_device(&any).is_err());
        assert!(!delegated.svc.publish_identity(cfg, ws).unwrap());
        drop(delegated);

        // Alex approves the box's code on the desktop.
        let code = sff.svc.enrollment_request(ws, "sff").encode();
        let request = DeviceEnrollment::decode(&code).unwrap();
        assert_eq!(request.fingerprint(), fingerprint(&sff.svc.device_public_key()));
        mac.svc.certify_device(&request).unwrap();
        // It's listed as Alex's device, also certified by the box's own account.
        let mine = mac.svc.own_devices(ws).unwrap();
        let listed = mine.iter().find(|d| d.device_id == sff.svc.device_id()).unwrap();
        assert!(!listed.revoked);
        assert_eq!(listed.also_certified_by, vec![Uuid::parse_str(&box_actor).unwrap()]);
        mac.sync().await;

        // The box sees the certificate and switches to Alex.
        sff.sync().await;
        let Standing::OwnerDevice(owner) = sff.svc.device_standing(ws, None).unwrap() else { panic!("not enrolled") };
        assert_eq!(owner.id, alex.id);
        sff.svc.act_as_owner_device(owner);
        sff.svc.post_user_message(chat, ws, "from the phone").unwrap();
        sff.svc.vote_on_proposal(chat, ws, pid, sff.id(), WorkspaceRole::Owner, true).unwrap();
        sff.svc.refine_proposal(chat, ws, pid, sff.id(), WorkspaceRole::Owner, "looks good", false).unwrap();
        let mut host = WorkspaceHost::new(sff.svc.device_id().to_string(), hive_core::HostKind::Worker, "sff");
        host.owner_actor_id = alex.id.clone();
        sff.svc.register_host(ws, host).unwrap();
        sff.sync().await;

        // Accepted everywhere, authored as Alex (authz on ingest included: the
        // host upsert is a governance event).
        mac.sync().await;
        taylor.sync().await;
        for dev in [&mac, &taylor] {
            let bodies = dev.bodies(chat);
            assert!(bodies.contains(&("from the phone".into(), alex.id.clone())), "{bodies:?}");
            let s = dev.svc.load(chat).unwrap().unwrap();
            let p = s.proposals.iter().find(|p| p.id == pid).unwrap();
            assert_eq!(p.approvals.len(), 1);
            assert_eq!(p.approvals[0].actor_id, alex.id);
            assert_eq!(p.refinements[0].actor_id, alex.id);
            let hosts = dev.svc.list_hosts(ws).unwrap();
            assert!(hosts.iter().any(|h| h.id == sff.svc.device_id().to_string() && h.owner_actor_id == alex.id));
        }

        // One person, one vote: Alex votes again on the desktop; still one vote.
        mac.svc.vote_on_proposal(chat, ws, pid, alex.id.clone(), WorkspaceRole::Owner, false).unwrap();
        mac.sync().await;
        sff.sync().await;
        for dev in [&mac, &sff] {
            let s = dev.svc.load(chat).unwrap().unwrap();
            let p = s.proposals.iter().find(|p| p.id == pid).unwrap();
            assert_eq!(p.approvals.len(), 1, "{:?}", p.approvals);
            assert!(!p.approvals[0].approved);
        }

        // Alex retires the box's old member. The box keeps working as Alex, and
        // its old member becomes Alex's alias (history + chats it created).
        mac.svc.remove_member(cfg, ws, box_actor.clone()).unwrap();
        mac.sync().await;
        sff.sync().await;
        assert!(matches!(sff.svc.device_standing(ws, None).unwrap(), Standing::OwnerDevice(_)));
        sff.svc.post_user_message(chat, ws, "after retirement").unwrap();
        sff.sync().await;
        mac.sync().await;
        taylor.sync().await;
        for dev in [&mac, &taylor] {
            assert!(dev.bodies(chat).contains(&("after retirement".into(), alex.id.clone())));
            let links = dev.svc.device_links(ws).unwrap();
            assert_eq!(links.person(&box_actor), alex.id);
        }
        // A former member that certifies Alex's desktop under its own account
        // gains nothing: that device never signed as it.
        let taylor_links = taylor.svc.device_links(ws).unwrap();
        assert_eq!(taylor_links.person(&taylor.id()), taylor.id());

        // Revocation: the desktop revokes the box's device.
        let old_device = sff.svc.device_id();
        mac.svc.revoke_device(cfg, ws, old_device).unwrap();
        assert!(mac.svc.own_devices(ws).unwrap().iter().any(|d| d.device_id == old_device && d.revoked));
        mac.sync().await;
        taylor.sync().await;
        sff.sync().await;
        assert_eq!(sff.svc.device_standing(ws, None).unwrap(), Standing::Revoked);
        // Whatever the box still sends is dropped — by the desktop and by a
        // teammate that pulled the revocation in an earlier batch.
        sff.svc.post_user_message(chat, ws, "after revocation").unwrap();
        sff.sync().await;
        mac.sync().await;
        taylor.sync().await;
        for dev in [&mac, &taylor] {
            assert!(!dev.bodies(chat).iter().any(|(b, _)| b == "after revocation"));
        }
        // A blocked service refuses writes outright.
        sff.svc.set_write_block(Some("revoked".into()));
        assert!(sff.svc.post_user_message(chat, ws, "nope").is_err());
        drop(sff);

        // Re-enroll: a fresh device key on the box, approved again.
        let ids = IdentityStore::new(tmp.path().join("sff"), FileKeyVault::new(tmp.path().join("sff")));
        let rotated = ids.rotate_device().unwrap();
        assert_ne!(rotated.device.id, old_device);
        let mut sff2 = Dev::open(&tmp.path().join("sff"), "hive-cli", &relay, ws);
        sff2.engine = SyncEngine::new(RelayClient::new(&relay), ws.to_string())
            .with_key(hive_core::derive_workspace_key("enroll flow passphrase"));
        sff2.sync().await;
        assert_eq!(sff2.svc.device_standing(ws, None).unwrap(), Standing::Own);
        let request = DeviceEnrollment::decode(&sff2.svc.enrollment_request(ws, "sff").encode()).unwrap();
        mac.svc.certify_device(&request).unwrap();
        mac.sync().await;
        sff2.sync().await;
        let Standing::OwnerDevice(owner) = sff2.svc.device_standing(ws, None).unwrap() else { panic!("not re-enrolled") };
        sff2.svc.act_as_owner_device(owner);
        sff2.svc.post_user_message(chat, ws, "re-enrolled").unwrap();
        sff2.sync().await;
        mac.sync().await;
        assert!(mac.bodies(chat).contains(&("re-enrolled".into(), alex.id.clone())));
    }
}
