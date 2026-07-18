//! Chat orchestration over the event store — the signed-event bookkeeping that
//! turns a user turn + a streamed provider reply into projected session state.
//! Ported from the streaming/persistence glue in `PrototypeStore.swift` +
//! `SessionPersistence.swift`, minus the multi-agent routing (Phase 5).
//!
//! Every mutation is recorded as a signed [`SessionEventEnvelope`] so the local
//! store, sync, and verify-on-read all see the same authenticated stream. The
//! network provider is decoupled: callers drive `append_chunk` /
//! `complete_assistant_message` from whatever produces the deltas (the
//! Anthropic client in production, synthetic chunks in tests).

use hive_core::crypto::sign_envelope;
use hive_core::events::MemberRoleChange;
use hive_core::{
    ActionProposal, ActorIdentity, ActorStamp, ChatMessage, ChatSession, MessageReaction,
    MessageRole, ProposalApproval, SessionEvent, SessionEventEnvelope, SigningKeypair,
    SkillProfile, Timestamp, VaultSource, WorkflowDefinition, WorkflowRun, WorkspaceAgent,
    WorkspaceMember, WorkspaceRole,
};
use uuid::Uuid;

use hive_core::authorization::{evaluate as authorize, requires_authz};

use crate::event_store::{EventStore, EventStoreError};
use crate::provider::ChatTurn;

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error(transparent)]
    Store(#[from] EventStoreError),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
}

pub type Result<T> = std::result::Result<T, ChatError>;

/// Drives a single workspace's chat sessions over an [`EventStore`], signing
/// every appended event with this device's key.
pub struct ChatService {
    store: EventStore,
    device_id: Uuid,
    keypair: SigningKeypair,
    author: ActorIdentity,
}

impl ChatService {
    pub fn new(
        store: EventStore,
        device_id: Uuid,
        keypair: SigningKeypair,
        author: ActorIdentity,
    ) -> Self {
        Self {
            store,
            device_id,
            keypair,
            author,
        }
    }

    pub fn store(&self) -> &EventStore {
        &self.store
    }

    /// One-time DB maintenance: drop chunk rows already superseded by a
    /// completed message. Deliberately *not* called on the launch path — the
    /// app defers it to a background task so it never blocks first paint.
    pub fn prune_superseded_chunks(&mut self) -> std::result::Result<usize, EventStoreError> {
        self.store.prune_superseded_chunks()
    }

    /// The local actor used to author + sign new messages. Stable account id;
    /// the display name can change at runtime (see [`set_author_display_name`]).
    pub fn author(&self) -> &ActorIdentity {
        &self.author
    }

    /// Update the display name used for new messages + actor stamps. Past events
    /// keep their original author (the log is immutable).
    pub fn set_author_display_name(&mut self, name: impl Into<String>) {
        self.author.display_name = name.into();
    }

    /// Set the git email carried on this user's identity (for commit
    /// attribution when a host runs an agent on their behalf). Empty clears it.
    pub fn set_author_git_email(&mut self, email: Option<String>) {
        self.author.git_email = email.map(|e| e.trim().to_string()).filter(|e| !e.is_empty());
    }

    /// Set this device's X25519 key-agreement public key on the local actor, so
    /// it rides in the roster and an owner can seal a rotated workspace key to
    /// this member's device when revoking access.
    pub fn set_author_key_agreement_public(&mut self, public: Option<Vec<u8>>) {
        self.author.key_agreement_public = public.filter(|p| !p.is_empty());
    }

    /// Bind the local actor to a signed-in account (e.g. GitHub). The actor id
    /// becomes the stable account id, so the same person on multiple devices is
    /// recognized as one member. Device keys (signing/key-agreement) are
    /// per-device and untouched.
    pub fn set_author_account(
        &mut self,
        account_id: impl Into<String>,
        display_name: impl Into<String>,
        git_email: Option<String>,
    ) {
        let id = account_id.into();
        self.author.account_id = uuid::Uuid::parse_str(&id).ok();
        self.author.id = id;
        self.author.display_name = display_name.into();
        if let Some(e) = git_email {
            let e = e.trim().to_string();
            self.author.git_email = (!e.is_empty()).then_some(e);
        }
    }

    /// The acting actor's role in a session — members carry roles; a non-member
    /// (e.g. the local workspace creator) is treated as owner.
    fn actor_role(&self, session: &ChatSession) -> WorkspaceRole {
        session
            .members
            .iter()
            .find(|m| m.actor.id == self.author.id)
            .map(|m| m.role)
            .unwrap_or(WorkspaceRole::Owner)
    }

    /// Append a signed envelope carrying `payload`, assigning the next sequence.
    /// Governance/content events are role-checked against the local actor first.
    fn append_signed(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        payload: SessionEvent,
    ) -> Result<SessionEventEnvelope> {
        if requires_authz(&payload) {
            if let Some(session) = self.store.load_session(session_id)? {
                let decision = authorize(&payload, self.actor_role(&session), &session);
                if !decision.allowed {
                    return Err(ChatError::Unauthorized(decision.summary));
                }
            }
        }
        let next_seq = self.store.max_sequence(session_id)?.unwrap_or(0) + 1;
        let mut env = SessionEventEnvelope::new(session_id, workspace_id, next_seq, payload);
        env.actor_stamp = Some(ActorStamp {
            actor: self.author.clone(),
            recorded_at: Timestamp::now(),
        });
        sign_envelope(&mut env, self.device_id, &self.keypair);
        self.store.append_envelope(&env)?;
        Ok(env)
    }

    /// Create a chat by seeding a `SessionSnapshot`.
    pub fn create_chat(
        &mut self,
        title: impl Into<String>,
        workspace_id: Uuid,
        runtime_id: impl Into<String>,
    ) -> Result<ChatSession> {
        let mut session = ChatSession::new(title, workspace_id, runtime_id);
        // The creator owns the primary runtime (cross-device dispatch) and is the
        // first workspace member.
        session.creator_actor_id = self.author.id.clone();
        session.members.push(WorkspaceMember {
            id: self.author.id.clone(),
            actor: self.author.clone(),
            role: WorkspaceRole::Owner,
            title: String::new(),
            index: 1,
            joined_at: Timestamp::now(),
        });
        let id = session.id;
        self.append_signed(
            id,
            workspace_id,
            SessionEvent::SessionSnapshot {
                session: Box::new(session),
            },
        )?;
        Ok(self.load(id)?.expect("session exists after snapshot"))
    }

    /// Record a user message. Returns the stored message.
    pub fn post_user_message(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        body: impl Into<String>,
    ) -> Result<ChatMessage> {
        let mut message = ChatMessage::new(MessageRole::User, &self.author.display_name, body);
        message.actor_identity = Some(self.author.clone());
        let stored = message.clone();
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::MessageAppended { message },
        )?;
        Ok(stored)
    }

    /// Record a system note — visible in the transcript, excluded from
    /// provider turns (used for app-level notices like "agent added a
    /// workflow").
    pub fn post_system_note(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        body: impl Into<String>,
    ) -> Result<()> {
        let message = ChatMessage::new(MessageRole::System, "Hive", body);
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::MessageAppended { message },
        )?;
        Ok(())
    }

    /// Append an empty streaming assistant placeholder and return its id. Chunk
    /// in deltas via [`append_chunk`], then [`complete_assistant_message`].
    pub fn begin_assistant_message(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        author: impl Into<String>,
        runtime_id: impl Into<String>,
    ) -> Result<Uuid> {
        let mut message = ChatMessage::new(MessageRole::Assistant, author, "");
        message.is_streaming = true;
        message.runtime_id = Some(runtime_id.into());
        let id = message.id;
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::MessageAppended { message },
        )?;
        Ok(id)
    }

    pub fn append_chunk(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        message_id: Uuid,
        chunk: impl Into<String>,
    ) -> Result<()> {
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::MessageChunkReceived {
                message_id,
                chunk: chunk.into(),
            },
        )?;
        Ok(())
    }

    pub fn complete_assistant_message(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        message_id: Uuid,
        body: impl Into<String>,
    ) -> Result<()> {
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::MessageCompleted {
                message_id,
                body: body.into(),
            },
        )?;
        // A completed reply is a natural checkpoint — snapshot periodically so
        // reopening a long chat replays only recent events (#3).
        self.maybe_snapshot(session_id, workspace_id)?;
        Ok(())
    }

    /// Remove a message from the transcript (e.g. the last assistant turn, before
    /// regenerating it). Idempotent.
    pub fn remove_message(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        message_id: Uuid,
    ) -> Result<()> {
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::MessageRemoved { message_id },
        )?;
        Ok(())
    }

    /// Number of events after the latest snapshot that triggers a re-snapshot.
    const SNAPSHOT_EVERY: i64 = 200;

    /// Write a fresh `SessionSnapshot` of the current projected state when the
    /// session has accumulated enough events since the last one. Bounds replay
    /// cost on long sessions; the snapshot is signed like any other event.
    pub fn maybe_snapshot(&mut self, session_id: Uuid, workspace_id: Uuid) -> Result<()> {
        if self.store.rows_since_last_snapshot(session_id)? < Self::SNAPSHOT_EVERY {
            return Ok(());
        }
        if let Some(session) = self.load(session_id)? {
            self.append_signed(
                session_id,
                workspace_id,
                SessionEvent::SessionSnapshot {
                    session: Box::new(session),
                },
            )?;
        }
        Ok(())
    }

    /// Add (or replace, by id) a workspace agent, emitting the full new roster.
    pub fn add_agent(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        agent: WorkspaceAgent,
    ) -> Result<()> {
        let mut agents = self
            .load(session_id)?
            .map(|s| s.workspace_agents)
            .unwrap_or_default();
        if let Some(slot) = agents.iter_mut().find(|a| a.id == agent.id) {
            *slot = agent;
        } else {
            agents.push(agent);
        }
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::AgentRosterUpdated { agents },
        )?;
        Ok(())
    }

    /// Remove a workspace agent by id, emitting the full new roster.
    pub fn remove_agent(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        agent_id: Uuid,
    ) -> Result<()> {
        let agents: Vec<WorkspaceAgent> = self
            .load(session_id)?
            .map(|s| s.workspace_agents)
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.id != agent_id)
            .collect();
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::AgentRosterUpdated { agents },
        )?;
        Ok(())
    }

    /// Change the primary runtime that answers non-agent turns for a session.
    pub fn set_session_runtime(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        runtime_id: impl Into<String>,
    ) -> Result<()> {
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::SessionRuntimeChanged {
                runtime_id: runtime_id.into(),
            },
        )?;
        Ok(())
    }

    /// Add the local actor to a session's roster if not already present (called
    /// when a device opens/joins a chat, so the People tab shows everyone who's
    /// here). Returns true if a member was added. No-op once present.
    pub fn ensure_self_member(&mut self, session_id: Uuid, workspace_id: Uuid) -> Result<bool> {
        let Some(session) = self.load(session_id)? else {
            return Ok(false);
        };
        // Already present? Match by actor id, or by the same GitHub account — so
        // signing in (which can change the actor id) doesn't add a second "self".
        let already = session.members.iter().any(|m| {
            m.actor.id == self.author.id
                || (self.author.account_id.is_some() && m.actor.account_id == self.author.account_id)
        });
        if already {
            return Ok(false);
        }
        let next_index = session.members.iter().map(|m| m.index).max().unwrap_or(0) + 1;
        let member = WorkspaceMember {
            id: self.author.id.clone(),
            actor: self.author.clone(),
            role: WorkspaceRole::Contributor,
            title: String::new(),
            index: next_index,
            joined_at: Timestamp::now(),
        };
        self.add_member(session_id, workspace_id, member)?;
        Ok(true)
    }

    /// Rename the session (manual, or auto-generated from the first exchange).
    pub fn set_title(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        title: impl Into<String>,
    ) -> Result<()> {
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::SessionTitleChanged {
                title: title.into(),
            },
        )?;
        Ok(())
    }

    /// Install (or replace, by name) a loaded skill, emitting the new set.
    pub fn add_skill(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        skill: SkillProfile,
    ) -> Result<()> {
        let mut skills = self
            .load(session_id)?
            .map(|s| s.loaded_skills)
            .unwrap_or_default();
        if let Some(slot) = skills.iter_mut().find(|s| s.name == skill.name) {
            *slot = skill;
        } else {
            skills.push(skill);
        }
        self.append_signed(session_id, workspace_id, SessionEvent::SkillsUpdated { skills })?;
        Ok(())
    }

    /// Remove a loaded skill by id.
    pub fn remove_skill(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        skill_id: Uuid,
    ) -> Result<()> {
        let skills: Vec<SkillProfile> = self
            .load(session_id)?
            .map(|s| s.loaded_skills)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.id != skill_id)
            .collect();
        self.append_signed(session_id, workspace_id, SessionEvent::SkillsUpdated { skills })?;
        Ok(())
    }

    /// Create or replace (by id) a workflow definition, emitting the new set.
    pub fn save_workflow_definition(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        definition: WorkflowDefinition,
    ) -> Result<()> {
        let mut definitions = self
            .load(session_id)?
            .map(|s| s.workflow_definitions)
            .unwrap_or_default();
        if let Some(slot) = definitions.iter_mut().find(|d| d.id == definition.id) {
            *slot = definition;
        } else {
            definitions.push(definition);
        }
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::WorkflowDefinitionsUpdated { definitions },
        )?;
        Ok(())
    }

    /// Remove a workflow definition by id.
    pub fn remove_workflow_definition(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        definition_id: Uuid,
    ) -> Result<()> {
        let definitions: Vec<WorkflowDefinition> = self
            .load(session_id)?
            .map(|s| s.workflow_definitions)
            .unwrap_or_default()
            .into_iter()
            .filter(|d| d.id != definition_id)
            .collect();
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::WorkflowDefinitionsUpdated { definitions },
        )?;
        Ok(())
    }

    /// Persist a workflow run snapshot (create or update by id).
    pub fn upsert_workflow_run(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        run: WorkflowRun,
    ) -> Result<()> {
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::WorkflowRunUpserted { run: Box::new(run) },
        )?;
        Ok(())
    }

    /// Add a workspace member (authz: admin+).
    pub fn add_member(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        member: WorkspaceMember,
    ) -> Result<()> {
        self.append_signed(session_id, workspace_id, SessionEvent::MemberAdded { member })?;
        Ok(())
    }

    /// Remove a workspace member (authz: admin+, last-owner protected).
    pub fn remove_member(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        member_id: impl Into<String>,
    ) -> Result<()> {
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::MemberRemoved {
                member_id: member_id.into(),
            },
        )?;
        Ok(())
    }

    /// Change a member's role (authz: admin+, last-owner protected).
    pub fn set_member_role(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        member_id: impl Into<String>,
        new_role: WorkspaceRole,
    ) -> Result<()> {
        let member_id = member_id.into();
        let old_role = self
            .load(session_id)?
            .and_then(|s| s.members.iter().find(|m| m.id == member_id).map(|m| m.role))
            .unwrap_or(WorkspaceRole::Contributor);
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::MemberRoleChanged {
                change: MemberRoleChange {
                    member_id,
                    old_role,
                    new_role,
                },
            },
        )?;
        Ok(())
    }

    /// Add (dedup by raw URL) a vault source.
    pub fn add_vault_source(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        source: VaultSource,
    ) -> Result<()> {
        let mut sources = self
            .load(session_id)?
            .map(|s| s.vault_sources)
            .unwrap_or_default();
        if !sources.iter().any(|s| s.raw_url() == source.raw_url()) {
            sources.push(source);
        }
        self.append_signed(session_id, workspace_id, SessionEvent::VaultSourcesUpdated { sources })?;
        Ok(())
    }

    /// Remove a vault source by its raw URL.
    pub fn remove_vault_source(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        raw_url: &str,
    ) -> Result<()> {
        let sources: Vec<VaultSource> = self
            .load(session_id)?
            .map(|s| s.vault_sources)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.raw_url() != raw_url)
            .collect();
        self.append_signed(session_id, workspace_id, SessionEvent::VaultSourcesUpdated { sources })?;
        Ok(())
    }

    /// Soft delete / restore — appends a `SessionArchivedChanged` event.
    pub fn set_archived(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        archived: bool,
    ) -> Result<()> {
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::SessionArchivedChanged { archived },
        )?;
        Ok(())
    }

    /// Hard delete — removes the session's events entirely.
    pub fn delete_chat(&mut self, session_id: Uuid) -> Result<()> {
        self.store.delete_session(session_id)?;
        Ok(())
    }

    /// Create or update a proposal (upsert by id).
    pub fn upsert_proposal(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        proposal: ActionProposal,
    ) -> Result<()> {
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::ProposalUpserted { proposal },
        )?;
        Ok(())
    }

    /// Cast a vote on a proposal (latest vote per actor wins; status recomputes
    /// against the quorum), then persist the updated proposal.
    pub fn vote_on_proposal(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        proposal_id: Uuid,
        actor_id: impl Into<String>,
        role: WorkspaceRole,
        approved: bool,
    ) -> Result<Option<ActionProposal>> {
        let Some(session) = self.load(session_id)? else {
            return Ok(None);
        };
        let Some(mut proposal) = session.proposals.into_iter().find(|p| p.id == proposal_id) else {
            return Ok(None);
        };
        proposal.cast_vote(ProposalApproval {
            actor_id: actor_id.into(),
            role,
            approved,
            created_at: Timestamp::now(),
        });
        let updated = proposal.clone();
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::ProposalUpserted { proposal },
        )?;
        Ok(Some(updated))
    }

    /// Toggle an emoji reaction on a message for an actor: removes it if the
    /// same actor+emoji vote already exists, otherwise adds it.
    pub fn toggle_reaction(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        message_id: Uuid,
        emoji: impl Into<String>,
        actor: &ActorIdentity,
    ) -> Result<()> {
        let emoji = emoji.into();
        let existing = self
            .load(session_id)?
            .and_then(|s| s.messages.into_iter().find(|m| m.id == message_id))
            .map(|m| {
                m.reactions
                    .iter()
                    .any(|r| r.actor_id == actor.id && r.emoji == emoji)
            })
            .unwrap_or(false);

        let event = if existing {
            SessionEvent::MessageReactionRemoved {
                message_id,
                actor_id: actor.id.clone(),
                emoji,
            }
        } else {
            SessionEvent::MessageReactionAdded {
                message_id,
                reaction: MessageReaction {
                    emoji,
                    actor_id: actor.id.clone(),
                    actor_display_name: actor.display_name.clone(),
                    actor_kind: actor.kind,
                    created_at: Timestamp::now(),
                },
            }
        };
        self.append_signed(session_id, workspace_id, event)?;
        Ok(())
    }

    /// Add an emoji reaction by a specific actor (used to seed agent
    /// `[[react:]]`/`[[vote:]]` directives). Idempotent via the projector.
    pub fn add_reaction(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        message_id: Uuid,
        emoji: impl Into<String>,
        actor: &ActorIdentity,
    ) -> Result<()> {
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::MessageReactionAdded {
                message_id,
                reaction: MessageReaction {
                    emoji: emoji.into(),
                    actor_id: actor.id.clone(),
                    actor_display_name: actor.display_name.clone(),
                    actor_kind: actor.kind,
                    created_at: Timestamp::now(),
                },
            },
        )?;
        Ok(())
    }

    pub fn load(&self, session_id: Uuid) -> Result<Option<ChatSession>> {
        Ok(self.store.load_session(session_id)?)
    }
}

/// Map a session's transcript into provider wire turns.
pub fn turns_for(session: &ChatSession) -> Vec<ChatTurn> {
    turns_from(&session.messages)
}

/// Map a message slice into provider wire turns: user→user,
/// assistant/agent→assistant. System and empty/streaming placeholders are
/// skipped (system is passed separately as the `system` param). Used with the
/// compacted (windowed) history.
pub fn turns_from(messages: &[ChatMessage]) -> Vec<ChatTurn> {
    messages
        .iter()
        .filter(|m| !m.body.is_empty() && !m.is_streaming)
        .filter_map(|m| match m.role {
            MessageRole::User => Some(ChatTurn::user(m.body.clone())),
            MessageRole::Assistant | MessageRole::Agent => Some(ChatTurn::assistant(m.body.clone())),
            MessageRole::System => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_core::crypto::verify_envelope;

    fn service() -> (ChatService, Vec<u8>) {
        let store = EventStore::open_in_memory().unwrap();
        let kp = SigningKeypair::generate().unwrap();
        let public = kp.public_key_bytes().to_vec();
        let device_id = Uuid::new_v4();
        let author = ActorIdentity::new("u1", "Mara", hive_core::ActorKind::Human);
        (ChatService::new(store, device_id, kp, author), public)
    }

    #[test]
    fn create_chat_seeds_creator_as_owner_member() {
        let (mut svc, _) = service();
        let chat = svc.create_chat("New chat", Uuid::new_v4(), "claude-code").unwrap();
        assert_eq!(chat.creator_actor_id, "u1");
        assert_eq!(chat.members.len(), 1);
        assert_eq!(chat.members[0].actor.id, "u1");
        assert_eq!(chat.members[0].role, WorkspaceRole::Owner);
    }

    #[test]
    fn ensure_self_member_is_idempotent_for_creator() {
        let (mut svc, _) = service();
        let chat = svc.create_chat("New chat", Uuid::new_v4(), "claude-code").unwrap();
        // Creator is already a member → no-op.
        let added = svc.ensure_self_member(chat.id, chat.workspace_id).unwrap();
        assert!(!added);
        assert_eq!(svc.load(chat.id).unwrap().unwrap().members.len(), 1);
    }

    #[test]
    fn full_turn_streams_and_projects() {
        let (mut svc, _pk) = service();
        let chat = svc.create_chat("Demo", Uuid::nil(), "anthropic").unwrap();
        let sid = chat.id;
        let wid = chat.workspace_id;

        svc.post_user_message(sid, wid, "Say hello").unwrap();
        let mid = svc
            .begin_assistant_message(sid, wid, "Hive", "anthropic")
            .unwrap();
        for piece in ["Hel", "lo, ", "world"] {
            svc.append_chunk(sid, wid, mid, piece).unwrap();
        }
        svc.complete_assistant_message(sid, wid, mid, "Hello, world")
            .unwrap();

        let session = svc.load(sid).unwrap().unwrap();
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].body, "Say hello");
        assert_eq!(session.messages[1].body, "Hello, world");
        assert!(!session.messages[1].is_streaming);

        // provider turns include both, in wire shape
        let turns = turns_for(&session);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[1].role, "assistant");
    }

    #[test]
    fn skills_install_replace_and_remove_round_trip() {
        // The event → store → projection loop behind Skills: installing emits
        // SkillsUpdated, re-installing the same name replaces (not duplicates),
        // and removal by id drops it. `prompt::tests` covers the last hop
        // (loaded skills → system prompt).
        let (mut svc, _) = service();
        let chat = svc.create_chat("Demo", Uuid::nil(), "anthropic").unwrap();
        let (sid, wid) = (chat.id, chat.workspace_id);

        let skill = hive_core::SkillProfile::new("review", "Always review diffs first.");
        svc.add_skill(sid, wid, skill).unwrap();
        let loaded = svc.load(sid).unwrap().unwrap().loaded_skills;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "review");
        assert_eq!(loaded[0].instructions, "Always review diffs first.");

        // Same name → replaced in place, not duplicated.
        let updated = hive_core::SkillProfile::new("review", "v2 instructions");
        svc.add_skill(sid, wid, updated).unwrap();
        let loaded = svc.load(sid).unwrap().unwrap().loaded_skills;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].instructions, "v2 instructions");

        svc.remove_skill(sid, wid, loaded[0].id).unwrap();
        assert!(svc.load(sid).unwrap().unwrap().loaded_skills.is_empty());
    }

    #[test]
    fn workflow_definition_save_replace_remove_round_trip() {
        let (mut svc, _) = service();
        let chat = svc.create_chat("Demo", Uuid::nil(), "anthropic").unwrap();
        let (sid, wid) = (chat.id, chat.workspace_id);

        let mut def = hive_core::workflow::preset_review_gate();
        svc.save_workflow_definition(sid, wid, def.clone()).unwrap();
        let defs = svc.load(sid).unwrap().unwrap().workflow_definitions;
        assert_eq!(defs.len(), 1);

        // Same id → replaced in place, not duplicated.
        def.name = "Review gate v2".into();
        svc.save_workflow_definition(sid, wid, def.clone()).unwrap();
        let defs = svc.load(sid).unwrap().unwrap().workflow_definitions;
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "Review gate v2");

        svc.remove_workflow_definition(sid, wid, def.id).unwrap();
        assert!(svc.load(sid).unwrap().unwrap().workflow_definitions.is_empty());
    }

    #[test]
    fn workflow_run_upserts_persist_status_transitions() {
        use hive_core::workflow::{self, NodeRunStatus, WorkflowRunStatus};

        let (mut svc, _) = service();
        let chat = svc.create_chat("Demo", Uuid::nil(), "anthropic").unwrap();
        let (sid, wid) = (chat.id, chat.workspace_id);

        let def = workflow::preset_fan_out_vote();
        let mut run = workflow::new_run(&def, "task", "u1");
        svc.upsert_workflow_run(sid, wid, run.clone()).unwrap();

        run.nodes[0].status = NodeRunStatus::Succeeded;
        run.status = WorkflowRunStatus::AwaitingGate;
        svc.upsert_workflow_run(sid, wid, run.clone()).unwrap();

        let runs = svc.load(sid).unwrap().unwrap().workflow_runs;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, WorkflowRunStatus::AwaitingGate);
        assert_eq!(runs[0].nodes[0].status, NodeRunStatus::Succeeded);
    }

    #[test]
    fn appended_events_are_signed_by_the_device() {
        let (mut svc, pk) = service();
        let chat = svc.create_chat("Demo", Uuid::nil(), "anthropic").unwrap();
        let sid = chat.id;
        svc.post_user_message(sid, chat.workspace_id, "hi").unwrap();

        for env in svc.store().list(sid).unwrap() {
            assert!(env.signature.is_some(), "every event must be signed");
            assert!(verify_envelope(&env, &pk).is_ok());
            assert!(env.actor_stamp.is_some());
        }
    }
}
