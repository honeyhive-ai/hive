//! System-prompt assembly — ported from `PrototypeStore.swift`
//! (`agentIdentitySystemPrompt` / `primaryRuntimeSystemPrompt` /
//! `workspaceParticipantsRoster`). Builds the context block that makes each
//! participant aware of who else is in the workspace, their roles, and that
//! they can reach them with `@mentions`.

use hive_core::{ChatSession, WorkspaceAgent};
use uuid::Uuid;

/// A human-readable roster of everyone in the workspace and how to address them.
pub fn workspace_roster(session: &ChatSession) -> String {
    let mut lines = vec!["Workspace participants:".to_string()];

    lines.push("- @primary — the primary runtime (coordinator).".to_string());

    for member in &session.members {
        let title = if member.title.is_empty() {
            String::new()
        } else {
            format!(", {}", member.title)
        };
        lines.push(format!(
            "- @{} — human, role: {:?}{} (reach all humans with @you).",
            member.actor.display_name, member.role, title
        ));
    }

    for agent in &session.workspace_agents {
        let role = if agent.role.is_empty() {
            "agent".to_string()
        } else {
            agent.role.clone()
        };
        lines.push(format!("- @{} — agent, {}.", agent.name, role));
    }

    lines.join("\n")
}

/// Instruction block for the skills that apply to a given responder, or empty.
///
/// `responder` is `None` for the primary runtime (only global skills apply) or
/// `Some(agent_id)` for a specific agent (global skills plus any that target it).
pub fn skills_section(session: &ChatSession, responder: Option<Uuid>) -> String {
    let applicable: Vec<_> = session
        .loaded_skills
        .iter()
        .filter(|s| s.applies_to(responder))
        .collect();
    if applicable.is_empty() {
        return String::new();
    }
    let mut lines = vec!["Loaded skills (follow these):".to_string()];
    for skill in applicable {
        lines.push(format!("## {}\n{}", skill.name, skill.instructions));
    }
    lines.join("\n\n")
}

/// Append the skills section to a base prompt when any apply to the responder.
fn with_skills(base: String, session: &ChatSession, responder: Option<Uuid>) -> String {
    let skills = skills_section(session, responder);
    if skills.is_empty() {
        base
    } else {
        format!("{base}\n\n{skills}")
    }
}

fn mention_guidance() -> &'static str {
    "You can address other participants with @mentions: @primary for the \
coordinator, @<agent name> for a specific agent, @<name> for a specific human, \
@owners/@admins for a role group, and @you to notify the humans present. Only \
mention someone when you need their input."
}

fn workflow_guidance() -> &'static str {
    r#"When asked to set up a multi-stage pipeline, you can author a workflow by ending a reply with a [[workflow: {…}]] directive. Authoring saves it — a human then launches the run from the Workflows pane (workflows do execute: their stages run as the DAG's ready-set clears). Format:
[[workflow: {"name": "…", "description": "…", "inputLabel": "…", "stages": [
  {"id": "slug", "name": "…", "kind": "agent", "agent": "<roster agent name, omit for primary>", "prompt": "… {{input}} … {{nodes.<id>.output}} …", "after": ["<upstream ids>"]},
  {"id": "check", "kind": "gate", "title": "…", "body": "…", "approvals": 1, "onReject": "halt", "after": ["slug"]}
]}]]
Rules: stage ids are slugs; "after" edges must form a DAG (stages whose deps are all done run in parallel); {{nodes.<id>.output}} may only reference upstream stages; gates pause for human approval — "onReject" is "halt" or {"retryFrom": "<upstream id>"}."#
}

/// System prompt for the primary runtime: it coordinates and may act directly.
pub fn primary_system_prompt(session: &ChatSession) -> String {
    let base = format!(
        "You are the primary runtime for the Hive workspace \"{title}\". You \
coordinate the conversation and may take actions or answer directly.\n\n{roster}\n\n{guide}\n\n{wf}",
        title = session.title,
        roster = workspace_roster(session),
        guide = mention_guidance(),
        wf = workflow_guidance(),
    );
    with_skills(base, session, None)
}

/// System prompt for a specific agent: its identity + the shared roster.
pub fn agent_system_prompt(session: &ChatSession, agent: &WorkspaceAgent) -> String {
    let role = if agent.role.is_empty() {
        "a workspace agent".to_string()
    } else {
        format!("the workspace's {}", agent.role)
    };
    let base = format!(
        "You are {name}, {role}, collaborating in the Hive workspace \"{title}\". \
Stay in character as {name}; you are distinct from the primary runtime and the \
other agents.\n\n{roster}\n\n{guide}\n\n{wf}",
        name = agent.name,
        role = role,
        title = session.title,
        roster = workspace_roster(session),
        guide = mention_guidance(),
        wf = workflow_guidance(),
    );
    with_skills(base, session, Some(agent.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::{ActorIdentity, ActorKind, WorkspaceMember, WorkspaceRole};
    use uuid::Uuid;

    fn session() -> ChatSession {
        let mut s = ChatSession::new("Launch", Uuid::nil(), "anthropic");
        s.members.push(WorkspaceMember {
            id: "m1".into(),
            actor: ActorIdentity::new("u1", "Mara", ActorKind::Human),
            role: WorkspaceRole::Owner,
            title: "PM".into(),
            index: 1,
            joined_at: Default::default(),
        });
        let mut scout = WorkspaceAgent::new("Scout", "r1");
        scout.role = "researcher".into();
        s.workspace_agents.push(scout);
        s
    }

    #[test]
    fn roster_lists_primary_humans_and_agents() {
        let r = workspace_roster(&session());
        assert!(r.contains("@primary"));
        assert!(r.contains("@Mara"));
        assert!(r.contains("Owner"));
        assert!(r.contains("PM"));
        assert!(r.contains("@Scout"));
        assert!(r.contains("researcher"));
    }

    #[test]
    fn agent_prompt_sets_identity_and_distinguishes_from_primary() {
        let s = session();
        let agent = s.workspace_agents[0].clone();
        let p = agent_system_prompt(&s, &agent);
        assert!(p.contains("You are Scout"));
        assert!(p.contains("distinct from the primary runtime"));
        assert!(p.contains("Launch"));
    }

    #[test]
    fn primary_prompt_mentions_coordination_and_roster() {
        let p = primary_system_prompt(&session());
        assert!(p.contains("primary runtime"));
        assert!(p.contains("@Scout"));
    }

    #[test]
    fn loaded_skills_are_injected_into_prompts() {
        let mut s = session();
        assert_eq!(skills_section(&s, None), "");
        s.loaded_skills.push(hive_core::SkillProfile::new(
            "Concise",
            "Always answer in under 100 words.",
        ));
        let p = primary_system_prompt(&s);
        assert!(p.contains("Loaded skills"));
        assert!(p.contains("Concise"));
        assert!(p.contains("under 100 words"));
    }

    #[test]
    fn global_skill_injects_into_primary_and_every_agent() {
        let mut s = session();
        s.loaded_skills
            .push(hive_core::SkillProfile::new("Global", "applies everywhere"));
        let agent = s.workspace_agents[0].clone();
        assert!(primary_system_prompt(&s).contains("Global"));
        assert!(agent_system_prompt(&s, &agent).contains("Global"));
    }

    #[test]
    fn agent_targeted_skill_injects_only_into_that_agent() {
        let mut s = session();
        // Two agents: Scout (already present) and a second one.
        let mut other = WorkspaceAgent::new("Nova", "r1");
        other.role = "writer".into();
        s.workspace_agents.push(other);
        let scout = s.workspace_agents[0].clone();
        let nova = s.workspace_agents[1].clone();

        let mut skill = hive_core::SkillProfile::new("ScoutOnly", "only Scout sees this");
        skill.agent_ids = vec![scout.id];
        s.loaded_skills.push(skill);

        assert!(agent_system_prompt(&s, &scout).contains("ScoutOnly"));
        assert!(!agent_system_prompt(&s, &nova).contains("ScoutOnly"));
        assert!(!primary_system_prompt(&s).contains("ScoutOnly"));
    }
}
