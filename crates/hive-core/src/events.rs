//! Event-sourced session state — ported from `HiveModels.swift`
//! (`SessionEventKind`, `SessionEvent`, `SessionEventEnvelope`, `EventScope`,
//! `MemberRoleChange`) and the `SessionProjector` in `SessionPersistence.swift`.
//!
//! Swift modeled `SessionEvent` as one struct with a `kind` plus a bag of
//! optional payload fields. Per the clean-replacement plan we use an idiomatic
//! Rust enum — payload data lives on the variant — and the projector is an
//! exhaustive `match`. The internally-tagged `"kind"` discriminant keeps the
//! JSON tags equal to Swift's `SessionEventKind` raw values.
//!
//! Only the Phase-1 spine variants (snapshot, message lifecycle, membership,
//! agent roster, reactions) are implemented; later phases add proposals, runs,
//! actions, review queue, trust, artifacts, vaults, keys, devices, MCP.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::WorkspaceAgent;
use crate::chat::{ChatMessage, MessageReaction};
use crate::identity::{ActorStamp, WorkspaceMember, WorkspaceRole};
use crate::proposals::ActionProposal;
use crate::session::ChatSession;
use crate::skills::SkillProfile;
use crate::time_util::Timestamp;
use crate::vault::VaultSource;
use crate::workflow::{WorkflowDefinition, WorkflowRun};

/// Which log a signed event belongs to. Workspace-scoped events fold into the
/// canonical roster; run-scoped events are run-internal; the rest are session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventScope {
    Workspace,
    Session,
    Run,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberRoleChange {
    pub member_id: String,
    pub old_role: WorkspaceRole,
    pub new_role: WorkspaceRole,
}

/// A session event. The `"kind"` tag matches Swift's `SessionEventKind` raw
/// values (camelCase).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SessionEvent {
    /// Full state seed — the base a delta stream is folded onto.
    SessionSnapshot { session: Box<ChatSession> },
    MessageAppended { message: ChatMessage },
    MessageChunkReceived { message_id: Uuid, chunk: String },
    MessageCompleted { message_id: Uuid, body: String },
    /// Drop a message from the transcript (used by "regenerate" to replace the
    /// last assistant turn). Idempotent — removing an unknown id is a no-op.
    MessageRemoved { message_id: Uuid },
    MemberAdded { member: WorkspaceMember },
    MemberRemoved { member_id: String },
    MemberRoleChanged { change: MemberRoleChange },
    AgentRosterUpdated { agents: Vec<WorkspaceAgent> },
    SessionRuntimeChanged { runtime_id: String },
    /// Rename the session (manual, or auto-generated from the first exchange).
    SessionTitleChanged { title: String },
    /// Soft delete / restore — flips the session's `archived` flag.
    SessionArchivedChanged { archived: bool },
    /// Replace the session's loaded skill set.
    SkillsUpdated { skills: Vec<SkillProfile> },
    /// Create or update a proposal (upsert by id) — carries the full snapshot.
    ProposalUpserted { proposal: ActionProposal },
    /// Replace the session's vault source set.
    VaultSourcesUpdated { sources: Vec<VaultSource> },
    /// Replace the session's workflow definition set.
    WorkflowDefinitionsUpdated { definitions: Vec<WorkflowDefinition> },
    /// Create or update a workflow run (upsert by id) — full snapshot.
    WorkflowRunUpserted { run: Box<WorkflowRun> },
    MessageReactionAdded {
        message_id: Uuid,
        reaction: MessageReaction,
    },
    MessageReactionRemoved {
        message_id: Uuid,
        actor_id: String,
        emoji: String,
    },
    /// Forward-compat catch-all: an event `kind` this build does not recognize
    /// (produced by a newer client). Serde deserializes an unknown tag here
    /// instead of failing the whole stream; it projects as a no-op. The raw
    /// envelope JSON is preserved verbatim in the event store, so a newer peer
    /// still receives the original event intact — only *this* build treats it
    /// as inert. Adding new variants above is therefore backward-compatible.
    #[serde(other)]
    Unknown,
}

impl SessionEvent {
    /// Stable kind string (also the SQLite `kind` column / index key).
    pub fn kind_str(&self) -> &'static str {
        match self {
            SessionEvent::SessionSnapshot { .. } => "sessionSnapshot",
            SessionEvent::MessageAppended { .. } => "messageAppended",
            SessionEvent::MessageChunkReceived { .. } => "messageChunkReceived",
            SessionEvent::MessageCompleted { .. } => "messageCompleted",
            SessionEvent::MessageRemoved { .. } => "messageRemoved",
            SessionEvent::MemberAdded { .. } => "memberAdded",
            SessionEvent::MemberRemoved { .. } => "memberRemoved",
            SessionEvent::MemberRoleChanged { .. } => "memberRoleChanged",
            SessionEvent::AgentRosterUpdated { .. } => "agentRosterUpdated",
            SessionEvent::SessionRuntimeChanged { .. } => "sessionRuntimeChanged",
            SessionEvent::SessionTitleChanged { .. } => "sessionTitleChanged",
            SessionEvent::SessionArchivedChanged { .. } => "sessionArchivedChanged",
            SessionEvent::SkillsUpdated { .. } => "skillsUpdated",
            SessionEvent::ProposalUpserted { .. } => "proposalUpserted",
            SessionEvent::VaultSourcesUpdated { .. } => "vaultSourcesUpdated",
            SessionEvent::WorkflowDefinitionsUpdated { .. } => "workflowDefinitionsUpdated",
            SessionEvent::WorkflowRunUpserted { .. } => "workflowRunUpserted",
            SessionEvent::MessageReactionAdded { .. } => "messageReactionAdded",
            SessionEvent::MessageReactionRemoved { .. } => "messageReactionRemoved",
            SessionEvent::Unknown => "unknown",
        }
    }

    /// Event scope — mirrors `scope(for:)` in `SignedEnvelopeBuilder`.
    pub fn scope(&self) -> EventScope {
        match self {
            SessionEvent::MemberAdded { .. }
            | SessionEvent::MemberRemoved { .. }
            | SessionEvent::MemberRoleChanged { .. }
            | SessionEvent::AgentRosterUpdated { .. } => EventScope::Workspace,
            SessionEvent::WorkflowRunUpserted { .. } => EventScope::Run,
            _ => EventScope::Session,
        }
    }
}

/// A sequenced, optionally-signed event on a workspace log. Signing
/// (`signer_device_id` / `signature`) is populated in Phase 2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventEnvelope {
    pub id: Uuid,
    pub event_id: Uuid,
    pub session_id: Uuid,
    pub workspace_id: Uuid,
    pub sequence: i64,
    /// Lamport timestamp assigned at authoring; it travels with the event and is
    /// never reassigned on ingest. The canonical fold order is
    /// `(lamport, event_id)` — a deterministic total order every device shares,
    /// so projection converges regardless of arrival/insertion order. Absent on
    /// legacy events (`#[serde(default)]` → 0, so they sort first, tie-broken by
    /// `event_id`).
    #[serde(default)]
    pub lamport: u64,
    #[serde(default)]
    pub timestamp: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_stamp: Option<ActorStamp>,
    pub payload: SessionEvent,
    #[serde(default = "default_scope")]
    pub scope: EventScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_device_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<Vec<u8>>,
}

fn default_scope() -> EventScope {
    EventScope::Session
}

impl SessionEventEnvelope {
    /// Build an unsigned envelope, deriving scope from the payload.
    pub fn new(
        session_id: Uuid,
        workspace_id: Uuid,
        sequence: i64,
        payload: SessionEvent,
    ) -> Self {
        let scope = payload.scope();
        Self {
            id: Uuid::new_v4(),
            event_id: Uuid::new_v4(),
            session_id,
            workspace_id,
            sequence,
            // Default Lamport = the authoring sequence (monotonic on the author
            // device). Cross-device correctness comes from the runtime stamping a
            // true Lamport clock (bumped on receive) before signing.
            lamport: sequence.max(0) as u64,
            timestamp: Timestamp::now(),
            actor_stamp: None,
            payload,
            scope,
            signer_device_id: None,
            signature: None,
        }
    }

    /// The canonical, device-independent ordering key. Every device folds events
    /// by this key, so projection is a pure function of the event *set*, not of
    /// arrival order. The `event_id` is the deterministic tie-break for events
    /// that share a Lamport value (concurrent or legacy-zero).
    pub fn canonical_key(&self) -> (u64, Uuid) {
        (self.lamport, self.event_id)
    }
}

impl ChatSession {
    /// Apply a single delta event in place. `SessionSnapshot` is a base seed,
    /// not a delta, so it is ignored here (handled by [`project`]).
    pub fn apply(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::SessionSnapshot { .. } => {}
            SessionEvent::MessageAppended { message } => {
                if let Some(idx) = self.message_index(message.id) {
                    self.messages[idx] = message.clone();
                } else {
                    self.messages.push(message.clone());
                }
            }
            SessionEvent::MessageChunkReceived { message_id, chunk } => {
                if let Some(idx) = self.message_index(*message_id) {
                    self.messages[idx].body.push_str(chunk);
                    self.messages[idx].is_streaming = true;
                }
            }
            SessionEvent::MessageCompleted { message_id, body } => {
                if let Some(idx) = self.message_index(*message_id) {
                    self.messages[idx].body = body.clone();
                    self.messages[idx].is_streaming = false;
                }
            }
            SessionEvent::MessageRemoved { message_id } => {
                self.messages.retain(|m| &m.id != message_id);
            }
            SessionEvent::MemberAdded { member } => {
                if let Some(idx) = self.members.iter().position(|m| m.id == member.id) {
                    self.members[idx] = member.clone();
                } else {
                    self.members.push(member.clone());
                }
            }
            SessionEvent::MemberRemoved { member_id } => {
                self.members.retain(|m| &m.id != member_id);
            }
            SessionEvent::MemberRoleChanged { change } => {
                if let Some(m) = self.members.iter_mut().find(|m| m.id == change.member_id) {
                    m.role = change.new_role;
                }
            }
            SessionEvent::AgentRosterUpdated { agents } => {
                self.workspace_agents = agents.clone();
            }
            SessionEvent::SessionRuntimeChanged { runtime_id } => {
                self.runtime_id = runtime_id.clone();
            }
            SessionEvent::SessionTitleChanged { title } => {
                self.title = title.clone();
            }
            SessionEvent::SessionArchivedChanged { archived } => {
                self.archived = *archived;
            }
            SessionEvent::SkillsUpdated { skills } => {
                self.loaded_skills = skills.clone();
            }
            SessionEvent::ProposalUpserted { proposal } => {
                if let Some(slot) = self.proposals.iter_mut().find(|p| p.id == proposal.id) {
                    *slot = proposal.clone();
                } else {
                    self.proposals.push(proposal.clone());
                }
            }
            SessionEvent::VaultSourcesUpdated { sources } => {
                self.vault_sources = sources.clone();
            }
            SessionEvent::WorkflowDefinitionsUpdated { definitions } => {
                self.workflow_definitions = definitions.clone();
            }
            SessionEvent::WorkflowRunUpserted { run } => {
                if let Some(slot) = self.workflow_runs.iter_mut().find(|r| r.id == run.id) {
                    *slot = (**run).clone();
                } else {
                    self.workflow_runs.push((**run).clone());
                }
            }
            SessionEvent::MessageReactionAdded {
                message_id,
                reaction,
            } => {
                if let Some(idx) = self.message_index(*message_id) {
                    let msg = &mut self.messages[idx];
                    if !msg.reactions.iter().any(|r| r.is_same_vote(reaction)) {
                        msg.reactions.push(reaction.clone());
                    }
                }
            }
            SessionEvent::MessageReactionRemoved {
                message_id,
                actor_id,
                emoji,
            } => {
                if let Some(idx) = self.message_index(*message_id) {
                    self.messages[idx]
                        .reactions
                        .retain(|r| !(&r.actor_id == actor_id && &r.emoji == emoji));
                }
            }
            // Unrecognized (newer-client) event — inert in this build.
            SessionEvent::Unknown => {}
        }
    }
}

/// Fold an envelope set into the current session state.
///
/// Convergence contract: projection is a pure function of the event **set**, not
/// of arrival or insertion order. Events are folded in the canonical total order
/// `(lamport, event_id)` (see [`SessionEventEnvelope::canonical_key`]), so two
/// devices that hold the same events — in any order — project identical state.
/// The state is seeded by the *latest* `SessionSnapshot` in canonical order;
/// deltas ordered before that snapshot are considered folded into it and are
/// dropped. Returns `None` if the set contains no snapshot to seed from.
pub fn project(envelopes: &[SessionEventEnvelope]) -> Option<ChatSession> {
    let mut ordered: Vec<&SessionEventEnvelope> = envelopes.iter().collect();
    ordered.sort_by_key(|e| e.canonical_key());

    let base = ordered
        .iter()
        .rposition(|e| matches!(e.payload, SessionEvent::SessionSnapshot { .. }))?;
    let mut session = match &ordered[base].payload {
        SessionEvent::SessionSnapshot { session } => (**session).clone(),
        _ => unreachable!("rposition matched a snapshot"),
    };
    session.updated_at = ordered[base].timestamp;
    for env in &ordered[base + 1..] {
        session.apply(&env.payload);
        session.updated_at = env.timestamp;
    }
    Some(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{ChatMessage, MessageRole};
    use crate::identity::{ActorIdentity, ActorKind};

    fn snapshot_env(session: ChatSession, seq: i64) -> SessionEventEnvelope {
        let wid = session.workspace_id;
        let sid = session.id;
        SessionEventEnvelope::new(
            sid,
            wid,
            seq,
            SessionEvent::SessionSnapshot {
                session: Box::new(session),
            },
        )
    }

    fn base_session() -> ChatSession {
        ChatSession::new("Demo", Uuid::nil(), "anthropic")
    }

    #[test]
    fn projects_snapshot_then_appends_message() {
        let base = base_session();
        let sid = base.id;
        let wid = base.workspace_id;
        let msg = ChatMessage::new(MessageRole::User, "Mara", "hi");
        let msg_id = msg.id;

        let envs = vec![
            snapshot_env(base, 1),
            SessionEventEnvelope::new(sid, wid, 2, SessionEvent::MessageAppended { message: msg }),
        ];
        let projected = project(&envs).expect("session");
        assert_eq!(projected.messages.len(), 1);
        assert_eq!(projected.messages[0].id, msg_id);
        assert_eq!(projected.messages[0].body, "hi");
    }

    #[test]
    fn streaming_chunks_then_complete() {
        let base = base_session();
        let sid = base.id;
        let wid = base.workspace_id;
        let mut placeholder = ChatMessage::new(MessageRole::Assistant, "Hive", "");
        placeholder.is_streaming = true;
        let mid = placeholder.id;

        let envs = vec![
            snapshot_env(base, 1),
            SessionEventEnvelope::new(
                sid,
                wid,
                2,
                SessionEvent::MessageAppended {
                    message: placeholder,
                },
            ),
            SessionEventEnvelope::new(
                sid,
                wid,
                3,
                SessionEvent::MessageChunkReceived {
                    message_id: mid,
                    chunk: "Hel".into(),
                },
            ),
            SessionEventEnvelope::new(
                sid,
                wid,
                4,
                SessionEvent::MessageChunkReceived {
                    message_id: mid,
                    chunk: "lo".into(),
                },
            ),
            SessionEventEnvelope::new(
                sid,
                wid,
                5,
                SessionEvent::MessageCompleted {
                    message_id: mid,
                    body: "Hello".into(),
                },
            ),
        ];
        let s = project(&envs).unwrap();
        assert_eq!(s.messages[0].body, "Hello");
        assert!(!s.messages[0].is_streaming);
    }

    #[test]
    fn member_add_remove_and_role_change() {
        let base = base_session();
        let sid = base.id;
        let wid = base.workspace_id;
        let member = WorkspaceMember {
            id: "m1".into(),
            actor: ActorIdentity::new("u1", "Mara", ActorKind::Human),
            role: WorkspaceRole::Contributor,
            title: String::new(),
            index: 0,
            joined_at: Timestamp::epoch(),
        };
        let envs = vec![
            snapshot_env(base, 1),
            SessionEventEnvelope::new(
                sid,
                wid,
                2,
                SessionEvent::MemberAdded {
                    member: member.clone(),
                },
            ),
            SessionEventEnvelope::new(
                sid,
                wid,
                3,
                SessionEvent::MemberRoleChanged {
                    change: MemberRoleChange {
                        member_id: "m1".into(),
                        old_role: WorkspaceRole::Contributor,
                        new_role: WorkspaceRole::Admin,
                    },
                },
            ),
        ];
        let s = project(&envs).unwrap();
        assert_eq!(s.members.len(), 1);
        assert_eq!(s.members[0].role, WorkspaceRole::Admin);

        // membership events are workspace-scoped
        assert_eq!(
            SessionEvent::MemberRemoved {
                member_id: "m1".into()
            }
            .scope(),
            EventScope::Workspace
        );
    }

    #[test]
    fn reactions_are_idempotent_per_actor_emoji() {
        let base = base_session();
        let sid = base.id;
        let wid = base.workspace_id;
        let msg = ChatMessage::new(MessageRole::User, "Mara", "vote?");
        let mid = msg.id;
        let reaction = MessageReaction {
            emoji: "👍".into(),
            actor_id: "u1".into(),
            actor_display_name: "Mara".into(),
            actor_kind: ActorKind::Human,
            created_at: Timestamp::epoch(),
        };
        let envs = vec![
            snapshot_env(base, 1),
            SessionEventEnvelope::new(sid, wid, 2, SessionEvent::MessageAppended { message: msg }),
            SessionEventEnvelope::new(
                sid,
                wid,
                3,
                SessionEvent::MessageReactionAdded {
                    message_id: mid,
                    reaction: reaction.clone(),
                },
            ),
            SessionEventEnvelope::new(
                sid,
                wid,
                4,
                SessionEvent::MessageReactionAdded {
                    message_id: mid,
                    reaction,
                },
            ),
        ];
        let s = project(&envs).unwrap();
        assert_eq!(s.messages[0].reactions.len(), 1);

        // removing it clears the vote
        let mut s2 = s.clone();
        s2.apply(&SessionEvent::MessageReactionRemoved {
            message_id: mid,
            actor_id: "u1".into(),
            emoji: "👍".into(),
        });
        assert!(s2.messages[0].reactions.is_empty());
    }

    fn env(sid: Uuid, wid: Uuid, lamport: i64, payload: SessionEvent) -> SessionEventEnvelope {
        // `new` sets lamport = the sequence argument, so passing distinct values
        // gives each event a distinct canonical key.
        SessionEventEnvelope::new(sid, wid, lamport, payload)
    }

    fn member(id: &str, actor: &str, role: WorkspaceRole) -> WorkspaceMember {
        WorkspaceMember {
            id: id.into(),
            actor: ActorIdentity::new(actor, actor, ActorKind::Human),
            role,
            title: String::new(),
            index: 0,
            joined_at: Timestamp::epoch(),
        }
    }

    fn reaction(actor: &str, emoji: &str) -> MessageReaction {
        MessageReaction {
            emoji: emoji.into(),
            actor_id: actor.into(),
            actor_display_name: actor.into(),
            actor_kind: ActorKind::Human,
            created_at: Timestamp::epoch(),
        }
    }

    /// Deterministic in-place Fisher–Yates shuffle (seeded xorshift64) — lets us
    /// exercise many permutations without a proptest dependency.
    fn shuffled(mut v: Vec<SessionEventEnvelope>, seed: u64) -> Vec<SessionEventEnvelope> {
        let mut s = seed | 1;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for i in (1..v.len()).rev() {
            let j = (next() % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
        v
    }

    /// The north-star invariant: given the same valid event set, projection is
    /// identical under EVERY delivery order — and resolves conflicts correctly.
    /// The set deliberately contains each formerly-divergent case: two title
    /// writes (register LWW), a role change, an add/remove pair (member set),
    /// and a reaction add→remove→add (the resurrection bug).
    #[test]
    fn projection_converges_across_all_permutations() {
        let base = base_session();
        let (sid, wid) = (base.id, base.workspace_id);
        let msg = ChatMessage::new(MessageRole::Assistant, "Hive", "draft");
        let mid = msg.id;

        let events = vec![
            env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) }),
            env(sid, wid, 2, SessionEvent::MessageAppended { message: msg }),
            env(sid, wid, 3, SessionEvent::MemberAdded { member: member("m1", "u1", WorkspaceRole::Contributor) }),
            env(sid, wid, 4, SessionEvent::MemberAdded { member: member("m2", "u2", WorkspaceRole::Contributor) }),
            env(sid, wid, 5, SessionEvent::SessionTitleChanged { title: "Foo".into() }),
            env(sid, wid, 6, SessionEvent::MessageReactionAdded { message_id: mid, reaction: reaction("u1", "👍") }),
            env(sid, wid, 7, SessionEvent::MemberRoleChanged { change: MemberRoleChange { member_id: "m1".into(), old_role: WorkspaceRole::Contributor, new_role: WorkspaceRole::Admin } }),
            env(sid, wid, 8, SessionEvent::MemberRemoved { member_id: "m2".into() }),
            env(sid, wid, 9, SessionEvent::SessionTitleChanged { title: "Bar".into() }),
            env(sid, wid, 10, SessionEvent::MessageReactionRemoved { message_id: mid, actor_id: "u1".into(), emoji: "👍".into() }),
            env(sid, wid, 11, SessionEvent::MessageReactionAdded { message_id: mid, reaction: reaction("u2", "🎉") }),
            env(sid, wid, 12, SessionEvent::MessageCompleted { message_id: mid, body: "final".into() }),
        ];

        let expected = project(&events).expect("session");
        // Correct deterministic resolution (not just self-consistency):
        assert_eq!(expected.title, "Bar", "latest title wins by canonical order");
        assert_eq!(expected.members.len(), 1, "m2 removed");
        assert_eq!(expected.members[0].id, "m1");
        assert_eq!(expected.members[0].role, WorkspaceRole::Admin, "role change applied");
        assert_eq!(expected.messages[0].body, "final");
        assert_eq!(expected.messages[0].reactions.len(), 1, "no resurrection: 👍 stays removed");
        assert_eq!(expected.messages[0].reactions[0].actor_id, "u2");

        // Property: every permutation projects to identical state.
        for seed in 0..500u64 {
            let permuted = shuffled(events.clone(), seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            assert_eq!(project(&permuted).expect("session"), expected, "divergence at seed {seed}");
        }
    }

    #[test]
    fn unknown_event_kind_is_inert_not_fatal() {
        // A newer client emits an event kind this build doesn't recognize. It
        // must deserialize to `Unknown` (not error) and project as a no-op,
        // leaving surrounding known events intact.
        let base = base_session();
        let (sid, wid) = (base.id, base.workspace_id);
        let msg = ChatMessage::new(MessageRole::User, "Mara", "hi");
        let known =
            SessionEventEnvelope::new(sid, wid, 3, SessionEvent::MessageAppended { message: msg });

        // Hand-craft an envelope carrying a future "kind".
        let mut future = serde_json::to_value(&known).unwrap();
        future["sequence"] = serde_json::json!(2);
        future["event_id"] = serde_json::json!(Uuid::new_v4());
        future["payload"] = serde_json::json!({ "kind": "somethingFromTheFuture", "extra": 42 });
        let future: SessionEventEnvelope = serde_json::from_value(future).unwrap();
        assert!(matches!(future.payload, SessionEvent::Unknown));

        let s = project(&[snapshot_env(base, 1), future, known]).unwrap();
        assert_eq!(s.messages.len(), 1, "known append survived; unknown was inert");
    }

    #[test]
    fn envelope_json_uses_kind_tag() {
        let env = SessionEventEnvelope::new(
            Uuid::nil(),
            Uuid::nil(),
            1,
            SessionEvent::MessageAppended {
                message: ChatMessage::new(MessageRole::User, "u", "hi"),
            },
        );
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["payload"]["kind"], "messageAppended");
        assert_eq!(json["scope"], "session");
    }

    #[test]
    fn projects_workflow_definitions_and_run_upserts() {
        use crate::workflow::{self, WorkflowRunStatus};

        let base = base_session();
        let (sid, wid) = (base.id, base.workspace_id);
        let def = workflow::preset_review_gate();
        let mut run = workflow::new_run(&def, "build it", "u1");
        let run_id = run.id;

        let mut envs = vec![
            snapshot_env(base, 1),
            SessionEventEnvelope::new(
                sid,
                wid,
                2,
                SessionEvent::WorkflowDefinitionsUpdated { definitions: vec![def.clone()] },
            ),
            SessionEventEnvelope::new(
                sid,
                wid,
                3,
                SessionEvent::WorkflowRunUpserted { run: Box::new(run.clone()) },
            ),
        ];
        // Same run id again with a new status → updated in place, not duplicated.
        run.status = WorkflowRunStatus::Completed;
        envs.push(SessionEventEnvelope::new(
            sid,
            wid,
            4,
            SessionEvent::WorkflowRunUpserted { run: Box::new(run) },
        ));

        let s = project(&envs).unwrap();
        assert_eq!(s.workflow_definitions.len(), 1);
        assert_eq!(s.workflow_definitions[0].name, "Review gate");
        assert_eq!(s.workflow_runs.len(), 1);
        assert_eq!(s.workflow_runs[0].id, run_id);
        assert_eq!(s.workflow_runs[0].status, WorkflowRunStatus::Completed);

        // Run events ride the reserved Run scope; definitions stay session-scoped.
        assert_eq!(envs[3].scope, EventScope::Run);
        assert_eq!(envs[1].scope, EventScope::Session);
    }
}
