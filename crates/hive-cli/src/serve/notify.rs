//! Which projection changes notify the user, and what the notification says
//! (ntfy [`Notice`] + end-to-end encrypted [`PushContent`], §14; the kinds and
//! fields are listed in `docs/push-notifications.md`).
//!
//! - an agent reply finished → `message.done` ("coder replied" / "coder hit
//!   an error"), or `mention` when the reply `@mentions` the user (one
//!   notification, never both);
//! - a message from another member that `@mentions` the user → `mention`;
//! - a proposal waiting for review → `proposal.created`, except a workflow
//!   gate's proposal, which is announced as `workflow.approval` instead;
//! - a workflow run's gate waiting for approval → `workflow.approval`.
//!
//! "The user" is this box's identity — the one the phone posts as — plus any
//! member sharing its account or its display name (the owner's desktop). Their
//! own messages never notify.

use std::collections::HashSet;

use hive_core::{
    ActionProposal, ActorIdentity, ChatMessage, ChatSession, MessageRole, ProposalKind, TranscriptEventKind, WorkflowRun,
    WorkspaceRole,
};

use super::ntfy::{self, Notice};
use super::tunnel::PushContent;

/// Who "you" are in a chat, for mentions and own-message checks.
#[derive(Debug, Default)]
pub struct Me {
    /// Member ids and actor ids.
    ids: HashSet<String>,
    roles: Vec<WorkspaceRole>,
    display_name: String,
}

impl Me {
    pub fn in_chat(author: &ActorIdentity, s: &ChatSession) -> Self {
        let mut me = Me { display_name: author.display_name.clone(), ..Default::default() };
        me.ids.insert(author.id.clone());
        let name = author.display_name.trim();
        for m in &s.members {
            let same = m.actor.id == author.id
                || (author.account_id.is_some() && m.actor.account_id == author.account_id)
                || (!name.is_empty() && m.actor.display_name.trim().eq_ignore_ascii_case(name));
            if same {
                me.ids.insert(m.id.clone());
                me.ids.insert(m.actor.id.clone());
                if !me.roles.contains(&m.role) {
                    me.roles.push(m.role);
                }
            }
        }
        me
    }

    /// Written by the user (here, on the phone, or on their desktop).
    pub fn wrote(&self, m: &ChatMessage) -> bool {
        if m.role != MessageRole::User {
            return false;
        }
        match &m.actor_identity {
            Some(a) => self.ids.contains(&a.id),
            None => !self.display_name.is_empty() && m.author == self.display_name,
        }
    }

    /// `body` mentions the user: by name/handle, `@you` / `@all`, or a role
    /// group the user is in (`@owners`, …).
    pub fn mentioned_in(&self, body: &str, s: &ChatSession) -> bool {
        if !body.contains('@') {
            return false;
        }
        let t = hive_runtime::parse_mentions(body, s);
        t.human_broadcast || t.humans.iter().any(|h| self.ids.contains(h)) || t.roles.iter().any(|r| self.roles.contains(r))
    }
}

fn is_reply(role: MessageRole) -> bool {
    matches!(role, MessageRole::Assistant | MessageRole::Agent)
}

fn base(kind: &str, s: &ChatSession, title: String) -> PushContent {
    PushContent { kind: kind.into(), chat_id: Some(s.id.to_string()), title, ..Default::default() }
}

/// A message just finished (agent reply) or appeared (anyone else's):
/// `message.done`, `mention`, or nothing.
pub fn for_message(s: &ChatSession, m: &ChatMessage, done: bool, me: &Me) -> Option<(Notice, PushContent)> {
    if m.is_streaming || m.role == MessageRole::System || m.kind != TranscriptEventKind::Message || me.wrote(m) {
        return None;
    }
    let reply = is_reply(m.role);
    // Agent replies count once finished (`done`); other messages when they
    // appear (they never stream).
    if reply != done {
        return None;
    }
    let failed = reply && m.body.starts_with("[error]");
    if !failed && me.mentioned_in(&m.body, s) {
        let notice = ntfy::mention_notice(&m.author, &s.title, s.id);
        let mut push = base("mention", s, notice.title.clone());
        push.sender = Some(m.author.clone());
        push.preview = Some(m.body.clone());
        return Some((notice, push));
    }
    if !reply {
        return None;
    }
    let notice = ntfy::reply_notice(&m.author, &s.title, s.id, failed);
    let mut push = base("message.done", s, notice.title.clone());
    push.sender = Some(m.author.clone());
    push.preview = Some(m.body.clone());
    Some((notice, push))
}

/// A workflow gate's backing proposal: announced as `workflow.approval` (from
/// the run), not `proposal.created`. An authorless decision is a gate whose
/// run hasn't synced in yet.
pub fn is_gate_proposal(s: &ChatSession, p: &ActionProposal) -> bool {
    s.workflow_runs.iter().any(|r| r.nodes.iter().any(|n| n.proposal_id == Some(p.id)))
        || (p.author_actor_id.is_empty() && p.kind == ProposalKind::Decision)
}

/// A new proposal waiting for review.
pub fn for_proposal(s: &ChatSession, p: &ActionProposal) -> Option<(Notice, PushContent)> {
    if is_gate_proposal(s, p) {
        return None;
    }
    let notice = ntfy::proposal_notice(&p.title, &s.title, s.id);
    let mut push = base("proposal.created", s, notice.title.clone());
    push.proposal_id = Some(p.id.to_string());
    push.preview = Some(p.title.clone());
    Some((notice, push))
}

/// A workflow run's gate `node` waiting for approval.
pub fn for_workflow_gate(s: &ChatSession, run: &WorkflowRun, node: &str) -> Option<(Notice, PushContent)> {
    let state = run.node_state(node)?;
    let stage = run.definition.nodes.iter().find(|n| n.id == node).map_or(node, |n| n.name.as_str());
    let notice = ntfy::workflow_notice(&run.definition.name, stage, &s.title, s.id);
    let mut push = base("workflow.approval", s, notice.title.clone());
    push.proposal_id = state.proposal_id.map(|p| p.to_string());
    push.workflow_run_id = Some(run.id.to_string());
    push.workflow_id = Some(run.definition_id.to_string());
    push.preview = Some(stage.to_string());
    Some((notice, push))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::tunnel::push_plaintext;
    use hive_core::{workflow, ActorKind, NodeRunStatus, WorkspaceAgent, WorkspaceMember};
    use uuid::Uuid;

    fn actor(id: &str, name: &str) -> ActorIdentity {
        let a: ActorIdentity = serde_json::from_value(serde_json::json!({ "id": id, "displayName": name, "kind": "human" })).unwrap();
        assert_eq!(a.kind, ActorKind::Human);
        a
    }

    fn member(a: &ActorIdentity, role: WorkspaceRole, index: u32) -> WorkspaceMember {
        WorkspaceMember { id: a.id.clone(), actor: a.clone(), role, title: String::new(), index, joined_at: Default::default() }
    }

    struct Fixture {
        s: ChatSession,
        me_actor: ActorIdentity,
        other: ActorIdentity,
    }

    fn fixture() -> Fixture {
        let me_actor = actor("box-1", "Zoe");
        let desktop = actor("desk-1", "zoe");
        let other = actor("ana-1", "Ana");
        let mut s = ChatSession::new("Fix the build", Uuid::new_v4(), "rt");
        s.members = vec![
            member(&desktop, WorkspaceRole::Owner, 1),
            member(&me_actor, WorkspaceRole::Contributor, 2),
            member(&other, WorkspaceRole::Admin, 3),
        ];
        s.workspace_agents = vec![WorkspaceAgent::new("coder", "rt")];
        Fixture { s, me_actor, other }
    }

    fn reply(body: &str) -> ChatMessage {
        ChatMessage::new(MessageRole::Assistant, "coder", body)
    }

    fn from(a: &ActorIdentity, body: &str) -> ChatMessage {
        let mut m = ChatMessage::new(MessageRole::User, &a.display_name, body);
        m.actor_identity = Some(a.clone());
        m
    }

    #[test]
    fn finished_agent_reply_is_message_done_with_preview_only_when_on() {
        let f = fixture();
        let me = Me::in_chat(&f.me_actor, &f.s);
        let (notice, p) = for_message(&f.s, &reply("Done: the build is green."), true, &me).unwrap();
        assert_eq!((p.kind.as_str(), p.title.as_str(), p.sender.as_deref()), ("message.done", "coder replied", Some("coder")));
        assert_eq!(p.chat_id, Some(f.s.id.to_string()));
        assert_eq!(notice.title, "coder replied");
        let on = push_plaintext(&p, true, 1, 0);
        assert_eq!((on["body"].as_str(), on["preview"].as_bool()), (Some("Done: the build is green."), Some(true)));
        let off = push_plaintext(&p, false, 2, 0);
        assert_eq!(off["body"], "");
        assert!(!off.to_string().contains("green"));
        // Errored turn.
        let (_, p) = for_message(&f.s, &reply("[error] model unreachable"), true, &me).unwrap();
        assert_eq!((p.kind.as_str(), p.title.as_str()), ("message.done", "coder hit an error"));
        // Not on creation (still streaming / not done), never for a streaming one.
        assert!(for_message(&f.s, &reply("x"), false, &me).is_none());
        let mut streaming = reply("x");
        streaming.is_streaming = true;
        assert!(for_message(&f.s, &streaming, true, &me).is_none());
        // User messages and system notes are never message.done.
        assert!(for_message(&f.s, &from(&f.other, "hello"), false, &me).is_none());
        assert!(for_message(&f.s, &ChatMessage::new(MessageRole::System, "hive", "note"), true, &me).is_none());
    }

    #[test]
    fn a_reply_that_mentions_you_is_one_mention_push() {
        let f = fixture();
        let me = Me::in_chat(&f.me_actor, &f.s);
        let (notice, p) = for_message(&f.s, &reply("@Zoe can you check the token?"), true, &me).unwrap();
        assert_eq!((p.kind.as_str(), p.title.as_str(), p.sender.as_deref()), ("mention", "coder mentioned you", Some("coder")));
        assert_eq!(notice.title, "coder mentioned you");
        assert!(!p.title.contains("token"), "titles never carry message text");
        assert_eq!(push_plaintext(&p, true, 1, 0)["body"], "@Zoe can you check the token?");
        assert_eq!(push_plaintext(&p, false, 1, 0)["body"], "");
    }

    #[test]
    fn mentions_from_other_members_by_name_broadcast_or_role() {
        let f = fixture();
        let me = Me::in_chat(&f.me_actor, &f.s);
        let (_, p) = for_message(&f.s, &from(&f.other, "@zoe take a look"), false, &me).unwrap();
        assert_eq!((p.kind.as_str(), p.title.as_str(), p.sender.as_deref()), ("mention", "Ana mentioned you", Some("Ana")));
        assert!(for_message(&f.s, &from(&f.other, "heads up @all"), false, &me).is_some());
        // The owner's desktop shares the display name: @owners includes "you".
        assert!(for_message(&f.s, &from(&f.other, "@owners please approve"), false, &me).is_some());
        assert!(for_message(&f.s, &from(&f.other, "@viewers fyi"), false, &me).is_none());
        // No mention of you → nothing for a user message.
        assert!(for_message(&f.s, &from(&f.other, "@coder fix it"), false, &me).is_none());
        assert!(for_message(&f.s, &from(&f.other, "mail zoe@example.com"), false, &me).is_none());
        // Mentioning someone else.
        assert!(for_message(&f.s, &from(&f.me_actor, "@Ana ping"), false, &Me::in_chat(&f.other, &f.s)).is_some());
    }

    #[test]
    fn your_own_messages_never_notify() {
        let f = fixture();
        let me = Me::in_chat(&f.me_actor, &f.s);
        assert!(for_message(&f.s, &from(&f.me_actor, "@all @Zoe note to self"), false, &me).is_none());
        // …nor from the owner's desktop identity (same display name).
        let desktop = f.s.members[0].actor.clone();
        assert!(for_message(&f.s, &from(&desktop, "@all hi"), false, &me).is_none());
    }

    #[test]
    fn proposal_push_carries_ids_and_title_preview() {
        let f = fixture();
        let p = ActionProposal::new("coder proposed changes to 2 files", ProposalKind::FileDiff, "agent-1");
        let (notice, push) = for_proposal(&f.s, &p).unwrap();
        assert_eq!((push.kind.as_str(), push.title.as_str()), ("proposal.created", "Change ready for review"));
        assert_eq!(notice.title, "Change ready for review");
        let pt = push_plaintext(&push, true, 3, 0);
        assert_eq!((pt["chatId"].as_str(), pt["proposalId"].as_str()), (Some(f.s.id.to_string().as_str()), Some(p.id.to_string().as_str())));
        assert_eq!(pt["body"], "coder proposed changes to 2 files");
        let off = push_plaintext(&push, false, 4, 0);
        assert_eq!((off["body"].as_str(), off["proposalId"].as_str()), (Some(""), Some(p.id.to_string().as_str())));
    }

    #[test]
    fn workflow_gate_is_workflow_approval_not_proposal_created() {
        let mut f = fixture();
        let def = workflow::preset_review_gate();
        let mut run = workflow::new_run(&def, "build it", "u1");
        let gate = ActionProposal::new("Approve the change?", ProposalKind::Decision, "");
        // Before the run syncs in, the authorless decision is already a gate.
        assert!(for_proposal(&f.s, &gate).is_none());
        let st = run.node_state_mut("approval").unwrap();
        st.status = NodeRunStatus::AwaitingApproval;
        st.proposal_id = Some(gate.id);
        f.s.workflow_runs.push(run.clone());
        f.s.proposals.push(gate.clone());
        assert!(for_proposal(&f.s, &gate).is_none());
        let (notice, p) = for_workflow_gate(&f.s, &run, "approval").unwrap();
        assert_eq!(p.kind, "workflow.approval");
        assert_eq!(p.title, format!("{} needs approval", def.name));
        assert_eq!(notice.title, p.title);
        let pt = push_plaintext(&p, true, 5, 0);
        assert_eq!(pt["chatId"], f.s.id.to_string());
        assert_eq!(pt["proposalId"], gate.id.to_string());
        assert_eq!(pt["workflowRunId"], run.id.to_string());
        assert_eq!(pt["workflowId"], def.id.to_string());
        assert_eq!(pt["body"], "Approval", "the stage name is the preview");
        assert_eq!(push_plaintext(&p, false, 6, 0)["body"], "");
        assert!(for_workflow_gate(&f.s, &run, "nope").is_none());
    }
}
