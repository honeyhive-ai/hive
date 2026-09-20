//! System-prompt assembly — ported from `PrototypeStore.swift`
//! (`agentIdentitySystemPrompt` / `primaryRuntimeSystemPrompt` /
//! `workspaceParticipantsRoster`). Builds the context block that makes each
//! participant aware of who else is in the workspace, their roles, and that
//! they can reach them with `@mentions`.

use hive_core::{ChatSession, ModelProviderKind, WorkspaceAgent};
use uuid::Uuid;

use crate::provider::dispatch::ResolvedRuntime;
use crate::provider::endpoint_host;

/// Human-readable provider name for the identity block.
fn provider_display(provider: ModelProviderKind) -> &'static str {
    match provider {
        ModelProviderKind::Anthropic => "Anthropic API",
        ModelProviderKind::OpenAI => "OpenAI API",
        ModelProviderKind::OpenRouter => "OpenRouter",
        ModelProviderKind::Ollama => "Ollama",
        ModelProviderKind::Azure => "Azure OpenAI",
        ModelProviderKind::Custom => "custom OpenAI-compatible endpoint",
        ModelProviderKind::HiveDaemon => "Hive daemon (OpenAI-compatible endpoint)",
        ModelProviderKind::ClaudeCode => "Claude Code CLI",
        ModelProviderKind::Codex => "OpenAI Codex CLI",
        ModelProviderKind::Aider => "aider CLI",
        ModelProviderKind::Pi => "pi CLI",
        ModelProviderKind::Hermes => "hermes CLI",
    }
}

/// A short, authoritative statement of what the responder actually runs on.
///
/// Models — small local ones especially — have no visibility into their own
/// runtime and will confabulate ("shared compute", made-up product names) when
/// asked. This block gives them the true answer, and, for plain HTTP turns
/// that carry no tool definitions, tells them they cannot act on files or run
/// commands so they don't pretend to. `host` is the device label the request
/// is made from.
pub fn runtime_identity_block(rt: &ResolvedRuntime, host: &str) -> String {
    let mut lines = vec![
        "Your runtime (authoritative — when asked what model, provider, or \
infrastructure you run on, answer from these facts; never guess or invent names):"
            .to_string(),
        format!("- provider: {}", provider_display(rt.provider)),
    ];
    let model = rt.model.trim();
    if model.is_empty() {
        lines.push("- model: chosen by the CLI (its default)".to_string());
    } else {
        lines.push(format!("- model: {model}"));
    }
    if rt.is_subprocess() || matches!(rt.provider, ModelProviderKind::Codex | ModelProviderKind::Hermes) {
        let program = if rt.endpoint.trim().is_empty() { "(default)" } else { rt.endpoint.trim() };
        lines.push(format!("- program: {program}"));
    } else if rt.provider == ModelProviderKind::Anthropic {
        lines.push("- endpoint: api.anthropic.com".to_string());
    } else if !rt.endpoint.trim().is_empty() {
        lines.push(format!("- endpoint: {}", endpoint_host(&rt.endpoint)));
    }
    if !host.trim().is_empty() {
        lines.push(format!("- runs from: {}", host.trim()));
    }
    if rt.is_openai_wire() {
        lines.push(
            "This request carries no tool definitions: you cannot run commands, read \
or edit files, browse, or call APIs — you can only reply with text. If asked to do \
those, say so plainly rather than pretending you did."
                .to_string(),
        );
    }
    lines.join("\n")
}

/// A human-readable roster of everyone in the workspace and how to address them.
pub fn workspace_roster(session: &ChatSession) -> String {
    let mut lines = vec!["Workspace participants:".to_string()];

    lines.push("- @hive (alias @primary) — the primary runtime (coordinator).".to_string());

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
    r#"Author a workflow ONLY when the user EXPLICITLY asks to set up a multi-stage pipeline or a loop (e.g. "build a workflow", "set up a pipeline", "loop until…"). An ordinary question, a code review, an explanation, or a single edit is NOT a workflow request — do NOT emit a [[workflow:]] directive for those; when in doubt, don't. If you do emit one, it MUST be a single, strictly-valid JSON object (double-quoted keys/strings, NO trailing commas, NO comments), or it will be rejected.
When the user does ask for one, author it by ending a reply with a [[workflow: {…}]] directive. Authoring saves it; a human launches the run from the Workflows pane, and stages execute as the DAG's ready-set clears. Format:
[[workflow: {"name": "…", "description": "…", "inputLabel": "…", "stages": [
  {"id": "slug", "name": "…", "kind": "agent", "agent": "<roster agent name, omit for primary>", "prompt": "… {{input}} … {{nodes.<id>.output}} …", "after": ["<upstream ids>"]},
  {"id": "check", "kind": "gate", "title": "…", "body": "…", "approvals": 1, "onReject": "halt", "after": ["slug"]}
]}]]
Rules: stage ids are slugs; "after" edges must form a DAG (stages whose deps are all done run in parallel); a stage's prompt may reference {{input}} and {{nodes.<id>.output}} of UPSTREAM stages only; every agent stage names the agent to run it ("agent"; omit for the primary @hive).

LOOPS (iterate until good). The loop primitive is a gate that routes back: give a review gate "onReject": {"retryFrom": "<id of the stage to redo>"}. A reject re-runs that stage and everything after it, then pauses at the gate again — so a reviewer can cycle "fix → recheck" as many times as needed; approving moves on. Because a human (or quorum) decides each gate, the loop can't run away on its own — that human decision IS the exit condition, so there is no separate "max iterations" to set. Author loops well:
- Keep the loop body small (the stage to redo + at most a couple after it).
- Make the redo stage's prompt read {{nodes.<review-or-gate>.output}} so it sees WHY it was sent back and can act on the feedback, not just retry blind.
- Always leave a clean exit (approve) and prefer one loop per workflow; keep everything outside the loop linear.
Example — draft, then loop until a reviewer is satisfied:
[[workflow: {"name": "Draft & revise", "inputLabel": "Topic", "stages": [
  {"id": "draft", "name": "Draft", "kind": "agent", "prompt": "Write a first draft about {{input}}. If revising, address: {{nodes.review.output}}"},
  {"id": "review", "name": "Review", "kind": "agent", "agent": "reviewer", "prompt": "Review this draft and list concrete fixes, or reply LGTM:\n{{nodes.draft.output}}", "after": ["draft"]},
  {"id": "ok", "name": "Approve?", "kind": "gate", "title": "Ship the draft?", "body": "Approve to finish, reject to send back for another pass.", "approvals": 1, "onReject": {"retryFrom": "draft"}, "after": ["review"]}
]}]]"#
}

fn transcript_guidance() -> &'static str {
    "In the conversation, each turn from another participant is prefixed with \
their name (\"Name: message\") so you can tell who said what; your own earlier \
turns appear without a prefix. Write your reply as yourself — do not add a name \
prefix to it."
}

fn path_guidance() -> &'static str {
    "Your replies are shared with teammates whose machines have different file \
layouts, so never write absolute local filesystem paths (such as your working \
directory) in a reply — refer to files by their repository-relative path."
}

fn proposal_guidance() -> &'static str {
    r#"To put a concrete action up for the team to approve, end a reply with a [[propose: {…}]] directive:
[[propose: {"title": "…", "kind": "fileDiff|command|decision", "body": "…", "requiredApprovals": 1}]]
This is saved for human review — it is NOT auto-executed. It appears in the Review pane and is approved by quorum; your own authorship does not count toward it. On approval a human dispatches it back to an agent to carry out. Use "fileDiff" for a proposed diff, "command" for a command to run, "decision" for a call to make. title is required; kind defaults to decision, requiredApprovals to 1."#
}

/// System prompt for the primary runtime: it coordinates and may act directly.
pub fn primary_system_prompt(session: &ChatSession) -> String {
    let base = format!(
        "You are the primary runtime for the Hive workspace \"{title}\". You \
coordinate the conversation and may take actions or answer directly.\n\n{roster}\n\n{guide}\n\n{transcript}\n\n{paths}\n\n{wf}\n\n{prop}",
        title = session.title,
        roster = workspace_roster(session),
        guide = mention_guidance(),
        transcript = transcript_guidance(),
        paths = path_guidance(),
        wf = workflow_guidance(),
        prop = proposal_guidance(),
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
other agents.\n\n{roster}\n\n{guide}\n\n{transcript}\n\n{paths}\n\n{wf}\n\n{prop}",
        name = agent.name,
        role = role,
        title = session.title,
        roster = workspace_roster(session),
        guide = mention_guidance(),
        transcript = transcript_guidance(),
        paths = path_guidance(),
        wf = workflow_guidance(),
        prop = proposal_guidance(),
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
    fn path_guidance_warns_against_absolute_paths_in_both() {
        let s = session();
        let agent = s.workspace_agents[0].clone();
        for p in [primary_system_prompt(&s), agent_system_prompt(&s, &agent)] {
            assert!(p.contains("repository-relative"));
            assert!(p.contains("never write absolute local filesystem paths"));
            // Transcript attribution guidance rides alongside (multi-agent).
            assert!(p.contains("prefixed with their name"));
            assert!(p.contains("do not add a name prefix"));
        }
    }

    #[test]
    fn proposal_guidance_injected_into_primary_and_agents() {
        let s = session();
        let agent = s.workspace_agents[0].clone();
        for p in [primary_system_prompt(&s), agent_system_prompt(&s, &agent)] {
            assert!(p.contains("[[propose:"));
            assert!(p.contains("Review pane"));
            assert!(p.contains("NOT auto-executed"));
        }
    }

    fn ollama_rt() -> ResolvedRuntime {
        ResolvedRuntime {
            provider: ModelProviderKind::Ollama,
            model: "qwen3.5".into(),
            endpoint: "http://100.64.0.5:11434/v1/chat/completions".into(),
            api_key: None,
            args: vec![],
            model_provider_id: None,
            model_base_url: None,
            context_window_tokens: None,
            keep_alive: None,
            think: None,
        }
    }

    #[test]
    fn identity_block_names_provider_model_host_and_no_tools_for_http() {
        let b = runtime_identity_block(&ollama_rt(), "Michael's MacBook (this device)");
        assert!(b.contains("provider: Ollama"), "{b}");
        assert!(b.contains("model: qwen3.5"), "{b}");
        assert!(b.contains("endpoint: 100.64.0.5:11434"), "{b}");
        assert!(!b.contains("/v1/chat/completions"), "path must not leak: {b}");
        assert!(b.contains("runs from: Michael's MacBook"), "{b}");
        assert!(b.contains("no tool definitions"), "{b}");
        assert!(b.contains("never guess"), "{b}");
    }

    #[test]
    fn identity_block_for_claude_code_names_the_cli_and_omits_no_tools_line() {
        let mut rt = ollama_rt();
        rt.provider = ModelProviderKind::ClaudeCode;
        rt.model = String::new();
        rt.endpoint = "claude".into();
        let b = runtime_identity_block(&rt, "");
        assert!(b.contains("Claude Code CLI"), "{b}");
        assert!(b.contains("chosen by the CLI"), "{b}");
        assert!(b.contains("program: claude"), "{b}");
        assert!(!b.contains("runs from"), "{b}");
        assert!(!b.contains("no tool definitions"), "{b}");
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
