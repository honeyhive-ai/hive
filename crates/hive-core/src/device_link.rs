//! Members that are another member's **own device** (a `hive serve` home box
//! the owner runs for their phone), so dispatch treats them as one person.
//!
//! A `hive serve` box enrolls with its own identity and joins the roster as a
//! human member. Without this, a room with you and your box isn't "solo" (two
//! humans), and a chat the phone creates is owned by the box's actor, which can
//! never run the chat's primary (`@hive`) — so nobody answers it.
//!
//! **The signal is backward compatible**: no new field or event. The box
//! registers its host (`WorkspaceHost`, keyed by its device id) with
//! `owner_actor_id` set to the account it belongs to (`HIVE_SERVE_OWNER`).
//! Older clients see an ordinary host record and keep today's behavior.
//!
//! **Enrolled devices (the real fix).** Once the owner approves the box on a
//! desktop that holds their account key, the box's device carries a
//! certificate signed by the owner's account and the box authors events *as the
//! owner* (see `hive_runtime::device_enroll`). The box's old member is then
//! retired, and it becomes a **retired alias**: an actor that is no longer a
//! member, whose own device (one that signed events as it) the owner has since
//! certified. Aliases map to the owner too, so chats the box created before
//! enrollment keep being answered by the owner's desktop and its history reads
//! as theirs. See [`DeviceLinks::link_retired`].
//!
//! **Trust (host claim, pre-enrollment).** A member `M` is a device of `X` only
//! when all hold:
//!   * a host's id is a device certified (cert signed by `M`'s own account key)
//!     to `M`, and that host names `X` as its owner;
//!   * `M` and `X` are both human members, `M != X`;
//!   * `M` is an Admin or Owner. Admins already define the workspace runtimes
//!     (credentials) every device runs, so honoring an Admin's claim grants no
//!     capability it lacks; a Contributor's claim is ignored.

use std::collections::HashMap;

use uuid::Uuid;

use crate::identity::{ActorKind, WorkspaceRole};
use crate::session::ChatSession;

/// member actor id → the account actor id it is a device of.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceLinks(HashMap<String, String>);

impl DeviceLinks {
    /// Fold the links out of a workspace-config projection. `certified(device,
    /// account)` says whether `device` holds a valid certificate for `account`.
    pub fn from_config(config: &ChatSession, certified: impl Fn(Uuid, Uuid) -> bool) -> Self {
        let humans: HashMap<&str, &crate::identity::WorkspaceMember> = config
            .members
            .iter()
            .filter(|m| m.actor.kind == ActorKind::Human)
            .map(|m| (m.actor.id.as_str(), m))
            .collect();
        let mut out = HashMap::new();
        for host in &config.workspace_hosts {
            let owner = host.owner_actor_id.trim();
            if owner.is_empty() || !humans.contains_key(owner) {
                continue;
            }
            let Ok(device) = Uuid::parse_str(&host.id) else { continue };
            for m in humans.values() {
                if m.actor.id == owner || m.role.rank() < WorkspaceRole::Admin.rank() {
                    continue;
                }
                let Some(account) = m.actor.account_id else { continue };
                if certified(device, account) {
                    out.insert(m.actor.id.clone(), owner.to_string());
                }
            }
        }
        // One hop only: a device of a device is not followed (no chains).
        Self(out)
    }

    /// Link a **retired alias**: `alias` (an actor id that is no longer a member
    /// of `config`) acts for `owner` (a current human member's actor id). The
    /// caller has established the cryptographic facts: a device that signed
    /// events as `alias` is certified by `owner`'s account key, which only the
    /// owner can produce. Ignored when `alias` is still a member (a live member is
    /// never silently merged into another), when `owner` isn't a human member, or
    /// when they're the same actor. Returns whether it linked.
    pub fn link_retired(&mut self, config: &ChatSession, alias: &str, owner: &str) -> bool {
        if alias == owner || config.members.iter().any(|m| m.actor.id == alias || m.id == alias) {
            return false;
        }
        let owner_is_human =
            config.members.iter().any(|m| m.actor.id == owner && m.actor.kind == ActorKind::Human);
        if !owner_is_human {
            return false;
        }
        self.0.entry(alias.to_string()).or_insert_with(|| owner.to_string());
        true
    }

    /// Every actor id that is `person` or acts for them (their linked devices
    /// and retired aliases).
    pub fn ids_of<'a>(&'a self, person: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        std::iter::once(person).chain(self.0.iter().filter(move |(_, p)| p.as_str() == person).map(|(a, _)| a.as_str()))
    }

    /// The person `actor_id` acts for: its owner if it's a linked device, else itself.
    pub fn person<'a>(&'a self, actor_id: &'a str) -> &'a str {
        self.0.get(actor_id).map(String::as_str).unwrap_or(actor_id)
    }

    /// The account `actor_id` is a device of, if linked.
    pub fn owner_of(&self, actor_id: &str) -> Option<&str> {
        self.0.get(actor_id).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Distinct people among `session`'s human members: a member and its linked
    /// devices count once.
    pub fn human_count(&self, session: &ChatSession) -> usize {
        let mut seen: Vec<&str> = Vec::new();
        for m in session.members.iter().filter(|m| m.actor.kind == ActorKind::Human) {
            let p = self.person(&m.actor.id);
            if !seen.contains(&p) {
                seen.push(p);
            }
        }
        seen.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{ActorIdentity, WorkspaceMember};
    use crate::workspace_host::{HostKind, WorkspaceHost};

    fn member(id: Uuid, role: WorkspaceRole, kind: ActorKind) -> WorkspaceMember {
        WorkspaceMember {
            id: id.to_string(),
            actor: ActorIdentity {
                id: id.to_string(),
                display_name: id.to_string(),
                kind,
                account_id: Some(id),
                device_id: None,
                git_email: None,
                key_agreement_public: None,
                avatar_url: None,
            },
            role,
            title: String::new(),
            index: 0,
            joined_at: Default::default(),
        }
    }

    fn host(device: Uuid, owner: Uuid) -> WorkspaceHost {
        let mut h = WorkspaceHost::new(device.to_string(), HostKind::Worker, "sff");
        h.owner_actor_id = owner.to_string();
        h
    }

    struct World {
        config: ChatSession,
        owner: Uuid,
        boxed: Uuid,
        box_device: Uuid,
    }

    fn world(box_role: WorkspaceRole) -> World {
        let (owner, boxed, box_device) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut config = ChatSession::new("", Uuid::new_v4(), "");
        config.members = vec![
            member(owner, WorkspaceRole::Owner, ActorKind::Human),
            member(boxed, box_role, ActorKind::Human),
        ];
        config.workspace_hosts = vec![host(box_device, owner)];
        World { config, owner, boxed, box_device }
    }

    fn certs(w: &World) -> impl Fn(Uuid, Uuid) -> bool + '_ {
        move |d, a| d == w.box_device && a == w.boxed
    }

    #[test]
    fn admin_box_owned_by_its_owner_is_their_device() {
        let w = world(WorkspaceRole::Admin);
        let links = DeviceLinks::from_config(&w.config, certs(&w));
        assert_eq!(links.owner_of(&w.boxed.to_string()), Some(w.owner.to_string().as_str()));
        assert_eq!(links.person(&w.boxed.to_string()), w.owner.to_string());
        // The owner and the box are one person: a solo room.
        assert_eq!(links.human_count(&w.config), 1);
    }

    #[test]
    fn contributor_claim_is_ignored() {
        let w = world(WorkspaceRole::Contributor);
        let links = DeviceLinks::from_config(&w.config, certs(&w));
        assert!(links.is_empty());
        assert_eq!(links.human_count(&w.config), 2);
    }

    #[test]
    fn host_without_a_cert_for_the_member_links_nothing() {
        let w = world(WorkspaceRole::Admin);
        // A host naming the owner, but no cert ties its device to the box's account.
        let links = DeviceLinks::from_config(&w.config, |_, _| false);
        assert!(links.is_empty());
    }

    #[test]
    fn self_owned_host_links_nothing_and_unknown_owner_is_ignored() {
        let mut w = world(WorkspaceRole::Admin);
        // Today's box: its host is owned by itself.
        w.config.workspace_hosts = vec![host(w.box_device, w.boxed)];
        assert!(DeviceLinks::from_config(&w.config, certs(&w)).is_empty());
        // A host naming a non-member as owner.
        w.config.workspace_hosts = vec![host(w.box_device, Uuid::new_v4())];
        assert!(DeviceLinks::from_config(&w.config, certs(&w)).is_empty());
    }

    #[test]
    fn retired_alias_maps_to_its_owner_but_a_live_member_never_merges() {
        let w = world(WorkspaceRole::Contributor);
        let mut links = DeviceLinks::default();
        // The box is still a member: never merged by the alias rule.
        assert!(!links.link_retired(&w.config, &w.boxed.to_string(), &w.owner.to_string()));
        assert!(links.is_empty());
        // Retired (removed from the roster): its history and chats are the owner's.
        let mut config = w.config.clone();
        config.members.retain(|m| m.actor.id != w.boxed.to_string());
        assert!(links.link_retired(&config, &w.boxed.to_string(), &w.owner.to_string()));
        assert_eq!(links.person(&w.boxed.to_string()), w.owner.to_string());
        let owner = w.owner.to_string();
        let ids: Vec<&str> = links.ids_of(&owner).collect();
        assert!(ids.contains(&w.boxed.to_string().as_str()) && ids.contains(&w.owner.to_string().as_str()));
        // An owner that isn't a human member links nothing.
        let mut other = DeviceLinks::default();
        assert!(!other.link_retired(&config, &w.boxed.to_string(), &Uuid::new_v4().to_string()));
        // Self-alias is meaningless.
        assert!(!other.link_retired(&config, &w.owner.to_string(), &w.owner.to_string()));
    }

    #[test]
    fn a_second_person_still_counts() {
        let mut w = world(WorkspaceRole::Admin);
        w.config.members.push(member(Uuid::new_v4(), WorkspaceRole::Contributor, ActorKind::Human));
        // Agents never count.
        w.config.members.push(member(Uuid::new_v4(), WorkspaceRole::Contributor, ActorKind::Agent));
        let links = DeviceLinks::from_config(&w.config, certs(&w));
        assert_eq!(links.human_count(&w.config), 2);
    }
}
