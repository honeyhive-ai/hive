//! `@`-mention routing — ported from the mention matrix in `PrototypeStore.swift`
//! (`humanMentionTargets` / `mentionsPrimaryRuntime` / `mentionsHumanBroadcast`
//! / agent mentions). Pure text analysis over a session's roster.
//!
//! Recognized targets:
//! - `@primary` — the workspace's primary runtime
//! - `@you` / `@all` — broadcast to humans (drives a notification)
//! - `@<agent name>` — a specific workspace agent (case-insensitive)
//! - `@<role>` — a role group: `@owners` / `@admins` / `@contributors` / `@viewers`
//! - `@<handle>` — a specific human member by handle (actor id)

use hive_core::{ChatSession, WorkspaceRole};
use uuid::Uuid;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MentionTargets {
    /// `@primary` was mentioned.
    pub primary: bool,
    /// `@you` / `@all` — broadcast to humans (notification trigger).
    pub human_broadcast: bool,
    /// Specific workspace agents mentioned, by id.
    pub agents: Vec<Uuid>,
    /// Specific human members mentioned, by member id.
    pub humans: Vec<String>,
    /// Role groups mentioned.
    pub roles: Vec<WorkspaceRole>,
}

impl MentionTargets {
    pub fn is_empty(&self) -> bool {
        !self.primary
            && !self.human_broadcast
            && self.agents.is_empty()
            && self.humans.is_empty()
            && self.roles.is_empty()
    }
}

fn role_from_group(token: &str) -> Option<WorkspaceRole> {
    match token {
        "owners" | "owner" => Some(WorkspaceRole::Owner),
        "admins" | "admin" => Some(WorkspaceRole::Admin),
        "contributors" | "contributor" => Some(WorkspaceRole::Contributor),
        "viewers" | "viewer" => Some(WorkspaceRole::Viewer),
        _ => None,
    }
}

/// True if `body` contains `@needle` as a mention (case-insensitive, not part of
/// a longer word). `needle` may contain spaces (agent display names).
fn mentions_phrase(body_lower: &str, needle: &str) -> bool {
    mentions_phrase_opts(body_lower, needle, true)
}

/// `@needle` mention check. When `allow_hash_after` is false, a following `#`
/// also disqualifies the match — so a bare `@Sam` won't swallow `@Sam#2`.
fn mentions_phrase_opts(body_lower: &str, needle: &str, allow_hash_after: bool) -> bool {
    let needle = needle.to_lowercase();
    if needle.is_empty() {
        return false;
    }
    let pat = format!("@{needle}");
    let mut from = 0;
    while let Some(idx) = body_lower[from..].find(&pat) {
        let start = from + idx;
        let after = start + pat.len();
        // the char following the match must not continue an identifier
        let ok_after = body_lower[after..]
            .chars()
            .next()
            .map(|c| {
                !c.is_alphanumeric() && c != '_' && c != '-' && (allow_hash_after || c != '#')
            })
            .unwrap_or(true);
        if ok_after {
            return true;
        }
        from = after;
    }
    false
}

/// Parse mention targets from a message body against a session's roster.
pub fn parse_mentions(body: &str, session: &ChatSession) -> MentionTargets {
    let lower = body.to_lowercase();
    let mut targets = MentionTargets::default();

    if mentions_phrase(&lower, "primary") {
        targets.primary = true;
    }
    if mentions_phrase(&lower, "you") || mentions_phrase(&lower, "all") {
        targets.human_broadcast = true;
    }

    // Agents by display name (longest names first so "@code reviewer" wins
    // over "@code").
    let mut agents: Vec<_> = session.workspace_agents.iter().collect();
    agents.sort_by_key(|a| std::cmp::Reverse(a.name.len()));
    for agent in agents {
        if !agent.name.is_empty() && mentions_phrase(&lower, &agent.name) {
            if !targets.agents.contains(&agent.id) {
                targets.agents.push(agent.id);
            }
        }
    }

    // Role groups + specific human handles.
    for role in [
        WorkspaceRole::Owner,
        WorkspaceRole::Admin,
        WorkspaceRole::Contributor,
        WorkspaceRole::Viewer,
    ] {
        let group = match role {
            WorkspaceRole::Owner => "owners",
            WorkspaceRole::Admin => "admins",
            WorkspaceRole::Contributor => "contributors",
            WorkspaceRole::Viewer => "viewers",
        };
        if mentions_phrase(&lower, group) {
            targets.roles.push(role);
        }
    }
    let _ = role_from_group; // singular forms accepted via the helper if needed later

    for member in &session.members {
        let name = &member.actor.display_name;
        if name.is_empty() {
            continue;
        }
        // Precise `@Name#index` (disambiguates duplicate display names), or a
        // bare `@Name` not followed by `#` (which would target a specific one).
        let indexed =
            member.index > 0 && mentions_phrase(&lower, &format!("{name}#{}", member.index));
        let bare = mentions_phrase_opts(&lower, name, false);
        if (indexed || bare) && !targets.humans.contains(&member.id) {
            targets.humans.push(member.id.clone());
        }
    }

    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::{ActorIdentity, ActorKind, WorkspaceAgent, WorkspaceMember};

    fn session_with(agents: Vec<WorkspaceAgent>, members: Vec<WorkspaceMember>) -> ChatSession {
        let mut s = ChatSession::new("t", Uuid::nil(), "anthropic");
        s.workspace_agents = agents;
        s.members = members;
        s
    }

    #[test]
    fn detects_primary_and_broadcast() {
        let s = session_with(vec![], vec![]);
        let t = parse_mentions("hey @primary can you and @you look?", &s);
        assert!(t.primary);
        assert!(t.human_broadcast);
    }

    #[test]
    fn matches_agent_by_name_not_substring() {
        let scout = WorkspaceAgent::new("Scout", "r1");
        let scout_id = scout.id;
        let s = session_with(vec![scout], vec![]);
        assert_eq!(parse_mentions("ask @Scout please", &s).agents, vec![scout_id]);
        // substring should not match
        assert!(parse_mentions("email scout@example.com", &s).agents.is_empty());
        assert!(parse_mentions("@Scouts go", &s).agents.is_empty());
    }

    #[test]
    fn matches_role_group_and_human() {
        let mara = WorkspaceMember {
            id: "m1".into(),
            actor: ActorIdentity::new("u1", "Mara", ActorKind::Human),
            role: WorkspaceRole::Owner,
            title: String::new(),
            index: 1,
            joined_at: Default::default(),
        };
        let s = session_with(vec![], vec![mara]);
        let t = parse_mentions("@admins please review, cc @Mara", &s);
        assert_eq!(t.roles, vec![WorkspaceRole::Admin]);
        assert_eq!(t.humans, vec!["m1".to_string()]);
    }

    #[test]
    fn empty_when_no_mentions() {
        let s = session_with(vec![], vec![]);
        assert!(parse_mentions("just a normal message", &s).is_empty());
    }

    #[test]
    fn name_collision_pings_all_matching_humans() {
        // Two members share the display name "Sam" — `@Sam` must not silently
        // fail; it notifies every Sam (graceful disambiguation).
        let make = |id: &str, actor: &str, index: u32| WorkspaceMember {
            id: id.into(),
            actor: ActorIdentity::new(actor, "Sam", ActorKind::Human),
            role: WorkspaceRole::Contributor,
            title: String::new(),
            index,
            joined_at: Default::default(),
        };
        let s = session_with(vec![], vec![make("m1", "u1", 1), make("m2", "u2", 2)]);
        // Bare `@Sam` is ambiguous → pings both.
        let all = parse_mentions("@Sam can you take this?", &s);
        assert_eq!(all.humans.len(), 2);
        assert!(all.humans.contains(&"m1".to_string()));
        assert!(all.humans.contains(&"m2".to_string()));
        // `@Sam#2` targets exactly the second Sam.
        let precise = parse_mentions("@Sam#2 specifically", &s);
        assert_eq!(precise.humans, vec!["m2".to_string()]);
    }
}
