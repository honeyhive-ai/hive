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

use hive_core::crypto::{sign_envelope, DeviceCertificate};
use hive_core::events::MemberRoleChange;
use hive_core::{
    workspace_config_session_id, ActionProposal, ActorIdentity, ActorStamp, Channel, ChatMessage,
    ChatSession, MessageReaction, MessageRole, MountedVault, ProposalApproval, SessionEvent,
    SessionEventEnvelope, SigningKeypair, SkillProfile, Timestamp, WorkflowDefinition, WorkflowRun,
    WorkspaceAgent, WorkspaceCredential, WorkspaceHost, WorkspaceMember, WorkspaceRole,
    WorkspaceRuntime,
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
    #[error("not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, ChatError>;

/// Drives a single workspace's chat sessions over an [`EventStore`], signing
/// every appended event with this device's key.
pub struct ChatService {
    store: EventStore,
    device_id: Uuid,
    keypair: SigningKeypair,
    /// The account signing keypair — signs this device's certificate and is
    /// published (as a public key) so peers can verify events from this device.
    account_keypair: SigningKeypair,
    author: ActorIdentity,
    /// The out-of-band-pinned founder of the *active* workspace: the account id
    /// carried by the `hivews1:` invite (`WorkspaceConn::founder_actor_id`). When
    /// set, projection accepts only a base seed authored by this founder, so a
    /// forged low-lamport `SessionSnapshot` from another keyholder cannot displace
    /// the real founding seed (B2 residual fix). `None` = no invite / local /
    /// legacy workspace ⇒ the earliest-self-consistent-seed fallback. Set via
    /// [`set_trusted_founder`] whenever the active workspace changes.
    trusted_founder: Option<String>,
}

impl ChatService {
    pub fn new(
        store: EventStore,
        device_id: Uuid,
        keypair: SigningKeypair,
        account_keypair: SigningKeypair,
        author: ActorIdentity,
    ) -> Self {
        Self {
            store,
            device_id,
            keypair,
            account_keypair,
            author,
            trusted_founder: None,
        }
    }

    /// Pin (or clear, with `None`/empty) the trusted founder for the active
    /// workspace — the account id the `hivews1:` invite names as its founder. The
    /// caller sets this every time the active workspace changes (creation pins the
    /// creator; joining pins the invite's founder). Read internally by [`load`] and
    /// threaded into projection; it deliberately does not widen `load`'s signature.
    pub fn set_trusted_founder(&mut self, founder: Option<String>) {
        self.trusted_founder = founder
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty());
    }

    /// The currently-pinned trusted founder, if any.
    pub fn trusted_founder(&self) -> Option<&str> {
        self.trusted_founder.as_deref()
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

    /// Set (or clear) the avatar carried on this user's identity, so new
    /// messages stamp it and it rides the synced roster to every member.
    pub fn set_author_avatar(&mut self, avatar_url: Option<String>) {
        self.author.avatar_url = avatar_url.filter(|u| !u.is_empty());
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

    /// Adopt a signed-in account identity for `workspace_id`, **migrating** the
    /// provisional pre-sign-in self instead of leaving a second "self" behind.
    ///
    /// Root cause of the churn class: a fresh install creates the workspace +
    /// self-member under a provisional device identity (no `account_id`) *before*
    /// sign-in. Plain [`set_author_account`] then switches the actor id to the
    /// account, and `ensure_self_member` — unable to match the provisional member
    /// (whose `account_id` is `None`) — **adds a second member**. With membership
    /// hoisted onto the config log (#61), that inflates every chat's roster to two
    /// humans and breaks solo detection (auto-reply, ownership).
    ///
    /// Fix: while still authorized as the provisional member, add the account at
    /// the **same role** (an Owner adds an Owner — `min_role_for(MemberAdded)` is
    /// Admin and there's no grant-above-self rule), switch identity, publish the
    /// account key + device certificate, then drop the provisional self (now not
    /// the last owner, so last-owner protection doesn't fire). Authz-clean, no
    /// bypass. Idempotent and a no-op when not churning (already an account, or the
    /// provisional isn't a member). Returns whether a migration happened.
    pub fn adopt_account_identity(
        &mut self,
        workspace_id: Uuid,
        account_id: impl Into<String>,
        display_name: impl Into<String>,
        git_email: Option<String>,
    ) -> Result<bool> {
        let account_id = account_id.into();
        let display_name = display_name.into();
        let old = self.author.clone();
        let config_id = self.ensure_workspace_config(workspace_id)?;
        let churning =
            old.account_id.is_none() && !old.id.is_empty() && old.id != account_id;

        if churning {
            let config = self.load(config_id)?.expect("config log exists after ensure");
            let old_role = config.members.iter().find(|m| m.id == old.id).map(|m| m.role);
            if let Some(role) = old_role {
                // The account actor: same device keys, account id/name/flag swapped in.
                let mut acct_actor = self.author.clone();
                acct_actor.id = account_id.clone();
                acct_actor.account_id = Uuid::parse_str(&account_id).ok();
                acct_actor.display_name = display_name.clone();
                match config.members.iter().find(|m| m.id == account_id).map(|m| m.role) {
                    // Account not yet present → add it at the provisional self's role
                    // (authorized as the provisional member at that same role).
                    None => {
                        let next_index =
                            config.members.iter().map(|m| m.index).max().unwrap_or(0) + 1;
                        self.add_member(
                            config_id,
                            workspace_id,
                            WorkspaceMember {
                                id: account_id.clone(),
                                actor: acct_actor,
                                role,
                                title: String::new(),
                                index: next_index,
                                joined_at: Timestamp::now(),
                            },
                        )?;
                    }
                    // Account already a member but below the provisional self's role
                    // (e.g. ensure_self_member added it as Contributor): promote it so
                    // removing the provisional owner can't strip the last owner.
                    Some(existing) if existing.rank() < role.rank() => {
                        self.set_member_role(config_id, workspace_id, &account_id, role)?;
                    }
                    Some(_) => {}
                }
            }
        }

        // Switch to the account identity + publish its key/device certificate so the
        // account's own events verify.
        self.set_author_account(account_id, display_name, git_email);
        self.publish_identity(config_id, workspace_id)?;

        // Drop the stale provisional self now that the account carries its role.
        if churning {
            let still_present = self
                .load(config_id)?
                .map(|s| s.members.iter().any(|m| m.id == old.id))
                .unwrap_or(false);
            if still_present {
                self.remove_member(config_id, workspace_id, &old.id)?;
            }
        }
        Ok(churning)
    }

    /// The acting actor's role in the projected roster. A non-member gets the
    /// **safe floor** (`Viewer`), never Owner — so being absent from the roster
    /// can't grant governance powers (PR #61 seam). The one bootstrap that needs
    /// to act without a roster role — a device adding *itself* — is handled by the
    /// self-enroll allowance in `authorization::evaluate`, not by this floor.
    fn actor_role(&self, session: &ChatSession) -> WorkspaceRole {
        session
            .members
            .iter()
            .find(|m| m.actor.id == self.author.id)
            .map(|m| m.role)
            .unwrap_or(WorkspaceRole::Viewer)
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
            // Authorize against the *projected* session — i.e. the workspace-wide
            // roster after the member hoist (§11) unions the config-log members on
            // (config-wins). For a config-log write this loads raw, so it gates on
            // the config roster itself. Closes the self-member staleness gap where
            // a role differed per-chat.
            if let Some(session) = self.load(session_id)? {
                let decision =
                    authorize(&payload, &self.author.id, self.actor_role(&session), &session);
                if !decision.allowed {
                    return Err(ChatError::Unauthorized(decision.summary));
                }
            }
        }
        let next_seq = self.store.max_sequence(session_id)?.unwrap_or(0) + 1;
        let mut env = SessionEventEnvelope::new(session_id, workspace_id, next_seq, payload);
        // Causal Lamport clock: strictly greater than every event this device has
        // seen for the session (locally authored or ingested from a peer), so the
        // canonical fold order (lamport, event_id) respects causality — a reply
        // never sorts before the message it answers. `new` seeded lamport from the
        // local sequence; override it with the causal value before signing.
        env.lamport = self.store.max_lamport(session_id)?.saturating_add(1);
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
        // Publish this device's identity so peers can verify events it authors.
        self.publish_identity(id, workspace_id)?;
        Ok(self.load(id)?.expect("session exists after snapshot"))
    }

    /// Publish this device's trust events into a workspace — an
    /// `AccountKeyRegistered` (the account's signing public key) and a
    /// `DeviceCertificateAdded` (this device's certificate, freshly issued under
    /// the account's *current* id so it stays consistent across a GitHub sign-in
    /// that changes the id). Peers fold these into their device roster
    /// (`envelope_verifier::build_roster`) to verify signatures. Idempotent per
    /// (workspace, device, account); a no-op when the actor has no account id
    /// (a local-only session with no verifiable identity). Returns whether it
    /// published.
    pub fn publish_identity(&mut self, session_id: Uuid, workspace_id: Uuid) -> Result<bool> {
        let Some(account_id) = self.author.account_id else {
            return Ok(false);
        };
        if self
            .store
            .has_device_certificate(workspace_id, self.device_id, account_id)?
        {
            return Ok(false);
        }
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::AccountKeyRegistered {
                account_id,
                signing_public_key: self.account_keypair.public_key_bytes().to_vec(),
            },
        )?;
        let certificate = DeviceCertificate::issue(
            &self.account_keypair,
            account_id,
            self.device_id,
            &self.keypair.public_key_bytes(),
            Timestamp::now(),
        );
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::DeviceCertificateAdded { certificate },
        )?;
        Ok(true)
    }

    /// Rotate this account's signing key (P2-2). Signs the new public key with the
    /// CURRENT account key (the succession proof the roster verifies), emits
    /// `AccountKeyRotated`, adopts the new keypair, and re-certifies this device
    /// under it — the device's old certificate no longer verifies against the newly
    /// pinned key, so without the fresh cert the device would drop out of the
    /// roster. A no-op (Ok(false)) if the local actor has no account id.
    pub fn rotate_account_key(
        &mut self,
        new_keypair: SigningKeypair,
        session_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<bool> {
        let Some(account_id) = self.author.account_id else {
            return Ok(false);
        };
        let new_pub = new_keypair.public_key_bytes().to_vec();
        let succession_signature = self.account_keypair.sign(&new_pub).to_vec();
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::AccountKeyRotated {
                account_id,
                new_signing_public_key: new_pub,
                succession_signature,
            },
        )?;
        // Adopt the new account key, then re-certify this device under it.
        self.account_keypair = new_keypair;
        let certificate = DeviceCertificate::issue(
            &self.account_keypair,
            account_id,
            self.device_id,
            &self.keypair.public_key_bytes(),
            Timestamp::now(),
        );
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::DeviceCertificateAdded { certificate },
        )?;
        Ok(true)
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
        responder_id: Option<Uuid>,
        reply_to_message_id: Option<Uuid>,
    ) -> Result<Uuid> {
        let mut message = ChatMessage::new(MessageRole::Assistant, author, "");
        message.is_streaming = true;
        message.runtime_id = Some(runtime_id.into());
        // Stamp the responder's stable id (for own-turn attribution) and this
        // device (so the stale-stream sweep never truncates a peer's stream).
        message.responder_id = responder_id;
        // The request this turn answers — the per-agent correlation key that lets
        // two concurrent tasks to the same agent be told apart (see pending_mentions).
        message.reply_to_message_id = reply_to_message_id;
        message.origin_device_id = Some(self.device_id);
        // Stamp the generating user's identity so a reader can tell whose "hive"
        // answered (every user's default agent is named "hive"). Rides synced like
        // a human's identity; message_dto surfaces it only for the primary agent
        // (named agents keep their roster avatar/name).
        message.actor_identity = Some(self.author.clone());
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

    /// Startup sweep: clear stale `is_streaming` flags left by a hard quit
    /// mid-stream.
    ///
    /// [`begin_assistant_message`](Self::begin_assistant_message) marks a
    /// placeholder `is_streaming: true`; only
    /// [`complete_assistant_message`](Self::complete_assistant_message) clears it.
    /// If the process dies mid-stream the flag is persisted forever and the message
    /// renders as a perpetual "typing" ghost on next launch. This sweep — meant to
    /// run once at startup, *before* any new stream begins — finds every message
    /// still `is_streaming` and emits a `MessageCompleted` carrying the body
    /// accumulated so far, flipping the flag off and persisting the fix. Returns the
    /// number of messages swept.
    ///
    /// Convergence-safe: `MessageCompleted` is an ordinary event folded in the
    /// canonical `(lamport, event_id)` order like any other, so every device reaches
    /// the same state. It cannot fight a genuinely in-flight stream — this runs at
    /// startup when no stream is live in *this* process, and were another device
    /// still streaming the same message its later chunks/completion carry a higher
    /// lamport and win the fold.
    pub fn clear_stale_streaming(&mut self) -> Result<usize> {
        let mut cleared = 0usize;
        for session_id in self.store.list_session_ids()? {
            let Some(session) = self.load(session_id)? else { continue };
            let workspace_id = session.workspace_id;
            let stale: Vec<(Uuid, String)> = session
                .messages
                .iter()
                // Only complete OUR own orphaned streams (or legacy ones with no
                // device stamp). A message still streaming on a peer's device
                // must not be swept here — that would truncate their live reply.
                .filter(|m| {
                    m.is_streaming
                        && m.origin_device_id.map(|d| d == self.device_id).unwrap_or(true)
                })
                .map(|m| (m.id, m.body.clone()))
                .collect();
            for (message_id, body) in stale {
                self.append_signed(
                    session_id,
                    workspace_id,
                    SessionEvent::MessageCompleted { message_id, body },
                )?;
                cleared += 1;
            }
        }
        Ok(cleared)
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

    /// Add (or replace, by id) a workspace agent, emitting a per-item delta so
    /// two admins adding different agents concurrently don't clobber each other.
    pub fn add_agent(
        &mut self,
        _session_id: Uuid,
        workspace_id: Uuid,
        agent: WorkspaceAgent,
    ) -> Result<()> {
        // Config hoist (§11): these records are workspace-level — target the
        // workspace-config log, not the calling chat's session.
        let session_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::AgentUpserted { agent },
        )?;
        Ok(())
    }

    /// Set an agent's avatar (image `data:` URL and/or accent color), emitting a
    /// per-item upsert delta. Only the agent's owner may edit it; a mismatch is a
    /// no-op error. Passing `None` for a field clears it.
    pub fn set_agent_avatar(
        &mut self,
        _session_id: Uuid,
        workspace_id: Uuid,
        agent_id: Uuid,
        avatar_url: Option<String>,
        avatar_color_hex: Option<String>,
    ) -> Result<()> {
        // Config hoist (§11): these records are workspace-level — target the
        // workspace-config log, not the calling chat's session.
        let session_id = self.ensure_workspace_config(workspace_id)?;
        let mut agents = self
            .load(session_id)?
            .map(|s| s.workspace_agents)
            .unwrap_or_default();
        let Some(slot) = agents.iter_mut().find(|a| a.id == agent_id) else {
            return Err(ChatError::NotFound("unknown agent".into()));
        };
        // Owner-only: an unowned agent (empty owner) is editable by anyone who
        // can reach it; an owned one only by its owner.
        if !slot.owner_actor_id.is_empty() && slot.owner_actor_id != self.author.id {
            return Err(ChatError::Unauthorized(
                "only the agent's owner can change its avatar".into(),
            ));
        }
        slot.avatar_url = avatar_url.filter(|u| !u.is_empty());
        slot.avatar_color_hex = avatar_color_hex.filter(|c| !c.is_empty());
        let agent = slot.clone();
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::AgentUpserted { agent },
        )?;
        Ok(())
    }

    /// Remove a workspace agent by id, emitting a per-item delta.
    pub fn remove_agent(
        &mut self,
        _session_id: Uuid,
        workspace_id: Uuid,
        agent_id: Uuid,
    ) -> Result<()> {
        // Config hoist (§11): these records are workspace-level — target the
        // workspace-config log, not the calling chat's session.
        let session_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::AgentRemoved { agent_id },
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
        // The workspace-config log is materialized on demand by
        // `ensure_workspace_config` below, so a fresh joiner — no chats yet, the
        // owner's config log not yet synced — can register self by passing the
        // config session id directly (from `join_workspace`/`set_active_workspace`).
        // Any other session_id must already exist locally; don't invent phantom
        // membership in a workspace this device has no presence in.
        if session_id != workspace_config_session_id(workspace_id) && self.load(session_id)?.is_none()
        {
            return Ok(false);
        }
        // Member hoist (§11): membership is workspace-wide, held on the config log —
        // register self there (not on this chat) so we're visible in every chat.
        // `ensure_workspace_config` seeds the very first toucher as Owner, so a
        // fresh workspace creator is found by the check below (no self-demote).
        let config_id = self.ensure_workspace_config(workspace_id)?;
        let config = self.load(config_id)?.expect("config log exists after ensure");
        // Already present? Match by actor id, or by the same GitHub account — so
        // signing in (which can change the actor id) doesn't add a second "self".
        let already = config.members.iter().any(|m| {
            m.actor.id == self.author.id
                || (self.author.account_id.is_some() && m.actor.account_id == self.author.account_id)
        });
        let added = if already {
            false
        } else {
            let next_index = config.members.iter().map(|m| m.index).max().unwrap_or(0) + 1;
            let member = WorkspaceMember {
                id: self.author.id.clone(),
                actor: self.author.clone(),
                role: WorkspaceRole::Contributor,
                title: String::new(),
                index: next_index,
                joined_at: Timestamp::now(),
            };
            self.add_member(session_id, workspace_id, member)?;
            true
        };
        // Publish this device's identity so peers can verify our events. Runs
        // even when already a member (idempotent) — and re-publishes once after a
        // GitHub sign-in changes the account id.
        self.publish_identity(session_id, workspace_id)?;
        Ok(added)
    }

    /// Claim the right to answer `trigger_id` (a user message), returning whether
    /// THIS device won. Ownership is account-scoped, so without this every online
    /// device of the responder's owner self-elects and double-answers. The first
    /// `TurnClaimed` in canonical fold order wins deterministically, so a device
    /// that has already synced a competing claim backs off (`Ok(false)`). It only
    /// resolves once claims sync, so it *reduces* rather than fully eliminates the
    /// concurrent-within-one-round-trip race — the streaming placeholder stays the
    /// second-line guard. A vanished/legacy session (no claim state) → proceed.
    pub fn claim_turn(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        trigger_id: Uuid,
    ) -> Result<bool> {
        if let Some(session) = self.load(session_id)? {
            if let Some(winner) = session.turn_claims.get(&trigger_id) {
                return Ok(*winner == self.device_id);
            }
        }
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::TurnClaimed { trigger_id, device_id: self.device_id },
        )?;
        Ok(self
            .load(session_id)?
            .and_then(|s| s.turn_claims.get(&trigger_id).copied())
            .map(|w| w == self.device_id)
            .unwrap_or(true))
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

    // ── Channels (spec §11) ────────────────────────────────────────────────
    // Channels live on the workspace-config log — a reserved session whose id is
    // derived from the workspace id. It syncs like any other session. Its members
    // roster (seeded here) is what authorizes config-log edits.

    /// Ensure the workspace-config log exists, seeded with the creator as the
    /// first Owner member so its authz has a roster. Idempotent. Returns the
    /// config-log session id.
    pub fn ensure_workspace_config(&mut self, workspace_id: Uuid) -> Result<Uuid> {
        let config_id = workspace_config_session_id(workspace_id);
        if self.load(config_id)?.is_none() {
            let mut session = ChatSession::new("", workspace_id, "");
            session.id = config_id;
            session.creator_actor_id = self.author.id.clone();
            session.members.push(WorkspaceMember {
                id: self.author.id.clone(),
                actor: self.author.clone(),
                role: WorkspaceRole::Owner,
                title: String::new(),
                index: 1,
                joined_at: Timestamp::now(),
            });
            self.append_signed(
                config_id,
                workspace_id,
                SessionEvent::SessionSnapshot { session: Box::new(session) },
            )?;
            self.publish_identity(config_id, workspace_id)?;
        }
        Ok(config_id)
    }

    /// Ensure a workspace has at least one channel — create `#general` (+ its
    /// drop-in chat) if it has none. Idempotent; a no-op once any channel exists.
    /// Returns whether it created the default.
    pub fn ensure_default_channel(
        &mut self,
        workspace_id: Uuid,
        runtime_id: impl Into<String>,
    ) -> Result<bool> {
        if !self.list_channels(workspace_id)?.is_empty() {
            return Ok(false);
        }
        self.create_channel(workspace_id, "general", None, runtime_id)?;
        Ok(true)
    }

    /// The workspace's channels, ordered by position.
    pub fn list_channels(&self, workspace_id: Uuid) -> Result<Vec<Channel>> {
        let config_id = workspace_config_session_id(workspace_id);
        let mut channels = self
            .load(config_id)?
            .map(|s| s.channels)
            .unwrap_or_default();
        channels.sort_by(|a, b| a.position.cmp(&b.position).then(a.id.cmp(&b.id)));
        Ok(channels)
    }

    /// Create a channel and its drop-in default chat (titled after the channel).
    /// Returns (channel, default_chat).
    pub fn create_channel(
        &mut self,
        workspace_id: Uuid,
        name: impl Into<String>,
        purpose: Option<String>,
        runtime_id: impl Into<String>,
    ) -> Result<(Channel, ChatSession)> {
        self.ensure_workspace_config(workspace_id)?;
        let config_id = workspace_config_session_id(workspace_id);
        let name = name.into();
        let channel_id = Uuid::new_v4().to_string();
        // The default chat lives in the channel from birth.
        let default_chat = self.create_chat(name.clone(), workspace_id, runtime_id)?;
        self.set_chat_channel(default_chat.id, workspace_id, &channel_id)?;
        let next_pos = self
            .load(config_id)?
            .map(|s| s.channels.iter().map(|c| c.position).max().unwrap_or(-1) + 1)
            .unwrap_or(0);
        let mut channel = Channel::new(channel_id, name);
        channel.purpose = purpose.filter(|p| !p.trim().is_empty());
        channel.position = next_pos;
        channel.default_chat_id = default_chat.id.to_string();
        self.append_signed(
            config_id,
            workspace_id,
            SessionEvent::ChannelCreated { channel: channel.clone() },
        )?;
        self.maybe_snapshot(config_id, workspace_id)?;
        Ok((channel, default_chat))
    }

    /// Rename a channel and/or set its purpose.
    ///
    /// `purpose` semantics are a field-level delta (see [`SessionEvent::ChannelRenamed`]):
    /// `None` = rename only, leaving any existing purpose untouched; `Some(text)` =
    /// set the purpose; `Some("")` = explicitly clear it. The caller's intent is
    /// passed through verbatim, so a plain rename can no longer wipe a purpose set
    /// concurrently (or previously) on the same channel.
    pub fn rename_channel(
        &mut self,
        workspace_id: Uuid,
        channel_id: impl Into<String>,
        name: impl Into<String>,
        purpose: Option<String>,
    ) -> Result<()> {
        let config_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(
            config_id,
            workspace_id,
            SessionEvent::ChannelRenamed {
                channel_id: channel_id.into(),
                name: name.into(),
                purpose,
            },
        )?;
        Ok(())
    }

    /// Set the channel order (ascending position).
    pub fn reorder_channels(&mut self, workspace_id: Uuid, channel_ids: Vec<String>) -> Result<()> {
        let config_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(
            config_id,
            workspace_id,
            SessionEvent::ChannelReordered { channel_ids },
        )?;
        Ok(())
    }

    /// Archive (or restore) a channel.
    pub fn archive_channel(
        &mut self,
        workspace_id: Uuid,
        channel_id: impl Into<String>,
        archived: bool,
    ) -> Result<()> {
        let config_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(
            config_id,
            workspace_id,
            SessionEvent::ChannelArchived { channel_id: channel_id.into(), archived },
        )?;
        Ok(())
    }

    // ── Workspace-owned runtimes (spec §12.5) ──────────────────────────────
    // Detached/headless agents run on these — a workspace credential, never a
    // member's personal key. They ride the config log (synced) like other config.

    /// The workspace's owned runtimes.
    pub fn list_workspace_runtimes(&self, workspace_id: Uuid) -> Result<Vec<WorkspaceRuntime>> {
        Ok(self
            .load(workspace_config_session_id(workspace_id))?
            .map(|s| s.workspace_runtimes)
            .unwrap_or_default())
    }

    /// Add (or replace by id) a workspace-owned runtime.
    pub fn add_workspace_runtime(
        &mut self,
        workspace_id: Uuid,
        runtime: WorkspaceRuntime,
    ) -> Result<()> {
        let config_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(
            config_id,
            workspace_id,
            SessionEvent::WorkspaceRuntimeUpserted { runtime },
        )?;
        Ok(())
    }

    /// Remove a workspace-owned runtime by id.
    pub fn remove_workspace_runtime(&mut self, workspace_id: Uuid, id: &str) -> Result<()> {
        let config_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(
            config_id,
            workspace_id,
            SessionEvent::WorkspaceRuntimeRemoved { runtime_id: id.to_string() },
        )?;
        Ok(())
    }

    // ── Hosts (spec §12.4) ─────────────────────────────────────────────────
    // Devices + workers that execute agents' turns. Ride the config log.

    /// This device's id — used as this machine's host id.
    pub fn device_id(&self) -> Uuid {
        self.device_id
    }

    /// The workspace's registered hosts.
    pub fn list_hosts(&self, workspace_id: Uuid) -> Result<Vec<WorkspaceHost>> {
        Ok(self
            .load(workspace_config_session_id(workspace_id))?
            .map(|s| s.workspace_hosts)
            .unwrap_or_default())
    }

    /// Register (or replace by id) a host. The id is a machine's device id.
    pub fn register_host(&mut self, workspace_id: Uuid, mut host: WorkspaceHost) -> Result<()> {
        if host.owner_actor_id.is_empty() {
            host.owner_actor_id = self.author.id.clone();
        }
        let config_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(
            config_id,
            workspace_id,
            SessionEvent::WorkspaceHostUpserted { host },
        )?;
        Ok(())
    }

    /// Remove a host by id.
    pub fn remove_host(&mut self, workspace_id: Uuid, id: &str) -> Result<()> {
        let config_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(
            config_id,
            workspace_id,
            SessionEvent::WorkspaceHostRemoved { host_id: id.to_string() },
        )?;
        Ok(())
    }

    /// Heartbeat this box's host presence (spec §12.5). Throttled: re-emits the
    /// host with a fresh `last_seen` only if the prior heartbeat is older than
    /// `throttle_secs`, so it doesn't bloat the log. Returns whether it emitted.
    pub fn touch_host(&mut self, workspace_id: Uuid, host_id: &str, throttle_secs: i64) -> Result<bool> {
        let config_id = workspace_config_session_id(workspace_id);
        let mut hosts = self
            .load(config_id)?
            .map(|s| s.workspace_hosts)
            .unwrap_or_default();
        let Some(host) = hosts.iter_mut().find(|h| h.id == host_id) else {
            return Ok(false);
        };
        let now = Timestamp::now();
        if now.inner().unix_timestamp() - host.last_seen.inner().unix_timestamp() < throttle_secs {
            return Ok(false);
        }
        host.last_seen = now;
        let host = host.clone();
        self.append_signed(
            config_id,
            workspace_id,
            SessionEvent::WorkspaceHostUpserted { host },
        )?;
        Ok(true)
    }

    /// Bind an agent to a host (empty = the owner's device). A worker host makes
    /// the agent detached (spec §12.4).
    pub fn set_agent_host(&mut self, workspace_id: Uuid, agent_id: Uuid, host_id: &str) -> Result<()> {
        let config_id = self.ensure_workspace_config(workspace_id)?;
        let mut agents = self
            .load(config_id)?
            .map(|s| s.workspace_agents)
            .unwrap_or_default();
        let Some(slot) = agents.iter_mut().find(|a| a.id == agent_id) else {
            return Err(ChatError::NotFound("unknown agent".into()));
        };
        slot.host_id = host_id.to_string();
        let agent = slot.clone();
        self.append_signed(
            config_id,
            workspace_id,
            SessionEvent::AgentUpserted { agent },
        )?;
        Ok(())
    }

    /// File a chat into a channel (per-chat, session-scoped). Empty id unfiles.
    pub fn set_chat_channel(
        &mut self,
        session_id: Uuid,
        workspace_id: Uuid,
        channel_id: impl Into<String>,
    ) -> Result<()> {
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::ChatChannelChanged { channel_id: channel_id.into() },
        )?;
        Ok(())
    }

    /// Install (or replace, by id) a loaded skill, emitting the new set.
    ///
    /// Identity is the skill's stable `id`, not its display `name` — add, remove,
    /// and the config-hoist union all key on `id`, so two skills that happen to
    /// share a name coexist and remove-by-id always targets the record add wrote.
    pub fn add_skill(
        &mut self,
        _session_id: Uuid,
        workspace_id: Uuid,
        skill: SkillProfile,
    ) -> Result<()> {
        // Config hoist (§11): these records are workspace-level — target the
        // workspace-config log, not the calling chat's session.
        let session_id = self.ensure_workspace_config(workspace_id)?;
        let mut skills = self
            .load(session_id)?
            .map(|s| s.loaded_skills)
            .unwrap_or_default();
        if let Some(slot) = skills.iter_mut().find(|s| s.id == skill.id) {
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
        _session_id: Uuid,
        workspace_id: Uuid,
        skill_id: Uuid,
    ) -> Result<()> {
        // Config hoist (§11): these records are workspace-level — target the
        // workspace-config log, not the calling chat's session.
        let session_id = self.ensure_workspace_config(workspace_id)?;
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
        _session_id: Uuid,
        workspace_id: Uuid,
        member: WorkspaceMember,
    ) -> Result<()> {
        // Member hoist (§11): the roster is workspace-level, not per-chat — target
        // the workspace-config log so a member added in one chat is seen in every
        // chat (closes the self-member staleness gap). The passed-in session is
        // shadowed by the config-log id.
        let session_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(session_id, workspace_id, SessionEvent::MemberAdded { member })?;
        Ok(())
    }

    /// Remove a workspace member (authz: admin+, last-owner protected).
    pub fn remove_member(
        &mut self,
        _session_id: Uuid,
        workspace_id: Uuid,
        member_id: impl Into<String>,
    ) -> Result<()> {
        // Member hoist (§11): route to the workspace-config log so the removal
        // propagates workspace-wide.
        let session_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::MemberRemoved {
                member_id: member_id.into(),
            },
        )?;
        Ok(())
    }

    /// Revoke a single device (lost/compromised) without removing its account
    /// (authz: admin+). Folds into the roster's revoked set so the device can no
    /// longer sign events. Pair with a `WorkspaceKeyRotation` excluding the device
    /// to also cut its read access. Rides the workspace-config log (workspace-wide).
    pub fn revoke_device(
        &mut self,
        _session_id: Uuid,
        workspace_id: Uuid,
        device_id: Uuid,
    ) -> Result<()> {
        let session_id = self.ensure_workspace_config(workspace_id)?;
        self.append_signed(session_id, workspace_id, SessionEvent::DeviceRevoked { device_id })?;
        Ok(())
    }

    /// Change a member's role (authz: admin+, last-owner protected).
    pub fn set_member_role(
        &mut self,
        _session_id: Uuid,
        workspace_id: Uuid,
        member_id: impl Into<String>,
        new_role: WorkspaceRole,
    ) -> Result<()> {
        // Member hoist (§11): route to the workspace-config log; read the prior
        // role from that same (config) roster so the delta is correct.
        let session_id = self.ensure_workspace_config(workspace_id)?;
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

    /// Add (dedup by raw URL) a vault source. If a matching URL already exists,
    /// its agent targeting is replaced with the incoming mount's.
    pub fn add_vault_source(
        &mut self,
        _session_id: Uuid,
        workspace_id: Uuid,
        vault: MountedVault,
    ) -> Result<()> {
        // Config hoist (§11): these records are workspace-level — target the
        // workspace-config log, not the calling chat's session.
        let session_id = self.ensure_workspace_config(workspace_id)?;
        let mut sources = self
            .load(session_id)?
            .map(|s| s.vault_sources)
            .unwrap_or_default();
        if let Some(slot) = sources.iter_mut().find(|s| s.raw_url() == vault.raw_url()) {
            *slot = vault;
        } else {
            sources.push(vault);
        }
        self.append_signed(session_id, workspace_id, SessionEvent::VaultSourcesUpdated { sources })?;
        Ok(())
    }

    /// Remove a vault source by its raw URL.
    pub fn remove_vault_source(
        &mut self,
        _session_id: Uuid,
        workspace_id: Uuid,
        raw_url: &str,
    ) -> Result<()> {
        // Config hoist (§11): these records are workspace-level — target the
        // workspace-config log, not the calling chat's session.
        let session_id = self.ensure_workspace_config(workspace_id)?;
        let sources: Vec<MountedVault> = self
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
        let approval = ProposalApproval {
            actor_id: actor_id.into(),
            role,
            approved,
            created_at: Timestamp::now(),
        };
        // Emit the vote as a delta, not a full-proposal snapshot: concurrent
        // votes from other devices then merge (per-actor LWW + quorum recompute)
        // instead of the last write clobbering the others.
        self.append_signed(
            session_id,
            workspace_id,
            SessionEvent::ProposalVoteCast {
                proposal_id,
                approval: approval.clone(),
            },
        )?;
        // Locally-projected result for immediate UI feedback.
        proposal.cast_vote(approval);
        Ok(Some(proposal))
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

    /// Project a session. For a chat, the effective agents/skills/vaults are the
    /// workspace's — held on the config log (§11 config hoist) — so overlay them
    /// (union with any pre-hoist per-chat config, so nothing vanishes before the
    /// migration). The config log itself loads raw: it IS the source.
    pub fn load(&self, session_id: Uuid) -> Result<Option<ChatSession>> {
        // Pin the base seed to the out-of-band founder (B2 residual fix). This is
        // load-bearing for the workspace-config log — where the roster/ownership
        // lives — so a forged low-lamport config seed can't displace the founder's.
        // A regular chat created by a non-founder member has no founder-matching
        // seed, so projection deterministically falls back to that chat's own
        // self-consistent seed (see `events::project_with_founder`); the pin never
        // breaks a legitimately member-created chat.
        let founder = self.trusted_founder.as_deref();
        let Some(mut session) = self.store.load_session_with_founder(session_id, founder)? else {
            return Ok(None);
        };
        let config_id = workspace_config_session_id(session.workspace_id);
        if session_id != config_id {
            if let Some(cfg) = self.store.load_session_with_founder(config_id, founder)? {
                // Members hoist onto the config log so the roster is workspace-wide.
                // Dedup by member id; the config-log entry is the source of truth,
                // so it *replaces* any same-id per-session member (role/title/index
                // are governed workspace-wide). A per-session-only member (e.g. a
                // pre-hoist `MemberAdded` or the `create_chat` creator seed) has no
                // config counterpart, so it's preserved (union, don't drop).
                for m in cfg.members {
                    if let Some(slot) = session.members.iter_mut().find(|x| x.id == m.id) {
                        *slot = m;
                    } else {
                        session.members.push(m);
                    }
                }
                for a in cfg.workspace_agents {
                    if !session.workspace_agents.iter().any(|x| x.id == a.id) {
                        session.workspace_agents.push(a);
                    }
                }
                for s in cfg.loaded_skills {
                    // Dedup by stable id (names are display-only) — consistent with
                    // add_skill/remove_skill's identity key.
                    if !session.loaded_skills.iter().any(|x| x.id == s.id) {
                        session.loaded_skills.push(s);
                    }
                }
                for v in cfg.vault_sources {
                    if !session.vault_sources.iter().any(|x| x.raw_url() == v.raw_url()) {
                        session.vault_sources.push(v);
                    }
                }
                for r in cfg.workspace_runtimes {
                    if !session.workspace_runtimes.iter().any(|x| x.id == r.id) {
                        session.workspace_runtimes.push(r);
                    }
                }
                for h in cfg.workspace_hosts {
                    if !session.workspace_hosts.iter().any(|x| x.id == h.id) {
                        session.workspace_hosts.push(h);
                    }
                }
            }
        }
        Ok(Some(session))
    }
}

/// One unanswered agent-directed mention — the unit of "queued" work (spec
/// §12.5): a message waiting for its agent's host.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingMention {
    pub session_id: Uuid,
    pub message_id: Uuid,
    pub agent_id: Uuid,
    pub agent_name: String,
}

/// Unanswered agent mentions in a session: a user message that `@mentions` an
/// agent with **no later assistant reply authored by that agent**. This is the
/// "queue" a worker drains and `hive queue` surfaces — keeping only the latest
/// unanswered mention per agent (the state a responder should act on).
pub fn pending_mentions(session: &ChatSession) -> Vec<PendingMention> {
    let mut out: Vec<PendingMention> = Vec::new();
    for (i, m) in session.messages.iter().enumerate() {
        if m.role != MessageRole::User {
            continue;
        }
        let targets = crate::mentions::parse_mentions(&m.body, session);
        for aid in &targets.agents {
            let Some(agent) = session.workspace_agents.iter().find(|a| &a.id == aid) else {
                continue;
            };
            let answered = session.messages[i + 1..].iter().any(|later| {
                if later.role != MessageRole::Assistant {
                    return false;
                }
                match later.reply_to_message_id {
                    // Precise per-request match: this reply answers exactly this
                    // mention. Two concurrent tasks to the same agent no longer
                    // collapse — a reply to the *other* task won't clear this one.
                    Some(answers) => answers == m.id,
                    // Legacy reply with no request id (pre-dates the field): fall
                    // back to the old author-name heuristic so existing
                    // transcripts still resolve. This is the only remaining path
                    // that can collapse concurrent same-agent mentions.
                    None => later.author == agent.name,
                }
            });
            if !answered {
                out.retain(|p| p.agent_id != *aid);
                out.push(PendingMention {
                    session_id: session.id,
                    message_id: m.id,
                    agent_id: *aid,
                    agent_name: agent.name.clone(),
                });
            }
        }
    }
    // Cross-device cascade: the trailing message is an agent reply that @mentions
    // another agent (not itself). It's actionable work for that agent's worker,
    // which — unlike the desktop's inline loop — otherwise never sees it. Bound by
    // cascade depth so an A↔B chain across devices/workers can't loop forever.
    if let Some(last) = session.messages.iter().rev().find(|m| m.role != MessageRole::System) {
        if matches!(last.role, MessageRole::Assistant | MessageRole::Agent)
            && session.cascade_depth() < hive_core::MAX_CASCADE_DEPTH
        {
            let targets = crate::mentions::parse_mentions(&last.body, session);
            for aid in &targets.agents {
                let Some(agent) = session.workspace_agents.iter().find(|a| &a.id == aid) else {
                    continue;
                };
                if agent.name == last.author {
                    continue; // an agent mentioning itself never re-triggers
                }
                out.retain(|p| p.agent_id != *aid);
                out.push(PendingMention {
                    session_id: session.id,
                    message_id: last.id,
                    agent_id: *aid,
                    agent_name: agent.name.clone(),
                });
            }
        }
    }
    out
}

/// Resolve a workspace runtime's credential to a usable secret — where a
/// headless worker turns a **workspace-owned** runtime into an actual key (spec
/// §12.5), never a personal one. `SecretRef` reads the worker's out-of-band env
/// (`HIVE_WS_SECRET_<ref>`); `Inline` returns the secret carried E2EE on the
/// config log; `None` is keyless.
pub fn resolve_workspace_credential(cred: &WorkspaceCredential) -> Option<String> {
    match cred {
        WorkspaceCredential::SecretRef { secret_ref } => {
            std::env::var(WorkspaceCredential::env_var(secret_ref))
                .ok()
                .filter(|s| !s.is_empty())
        }
        WorkspaceCredential::Inline { secret } => (!secret.is_empty()).then(|| secret.clone()),
        WorkspaceCredential::None => None,
    }
}

/// Map a session's transcript into provider wire turns, attributed for the
/// responder named `own_author` (see [`turns_from`]).
pub fn turns_for(session: &ChatSession, own_id: Option<Uuid>, own_author: &str) -> Vec<ChatTurn> {
    turns_from(&session.messages, own_id, own_author)
}

/// Map a message slice into provider wire turns: user→user,
/// assistant/agent→assistant. System and empty/streaming placeholders are
/// skipped (system is passed separately as the `system` param). Used with the
/// compacted (windowed) history.
///
/// Each turn is prefixed with its speaker (`"Name: …"`) so a responder can tell
/// participants apart in a multi-agent thread — the roster names everyone, but
/// without this the transcript collapses every agent + human into anonymous
/// assistant/user turns. The responder's OWN prior turns (author == `own_author`)
/// stay unprefixed: they are genuinely "your" earlier outputs, so the model has
/// a clean template and never learns to prefix its own name. Pass an empty
/// `own_author` to attribute every turn (no self to exclude).
pub fn turns_from(messages: &[ChatMessage], own_id: Option<Uuid>, own_author: &str) -> Vec<ChatTurn> {
    let turns: Vec<ChatTurn> = messages
        .iter()
        .filter(|m| !m.body.is_empty() && !m.is_streaming)
        .filter_map(|m| {
            let attributed = || {
                let who = m.author.trim();
                if who.is_empty() {
                    m.body.clone()
                } else {
                    format!("{who}: {}", m.body)
                }
            };
            match m.role {
                MessageRole::User => Some(ChatTurn::user(attributed())),
                MessageRole::Assistant | MessageRole::Agent => {
                    // "Own" turns stay unprefixed. Key on the responder's stable
                    // id when responding as an agent (robust to rename/duplicate
                    // name); for the primary (no agent id) fall back to a
                    // primary-authored turn matched by name.
                    let is_own = match own_id {
                        Some(oid) => m.responder_id == Some(oid),
                        None => {
                            m.responder_id.is_none()
                                && !own_author.is_empty()
                                && m.author == own_author
                        }
                    };
                    if is_own {
                        Some(ChatTurn::assistant(m.body.clone()))
                    } else {
                        Some(ChatTurn::assistant(attributed()))
                    }
                }
                MessageRole::System => None,
            }
        })
        .collect();
    repair_alternation(turns)
}

/// Make a turn list provider-safe: coalesce consecutive same-role turns and
/// guarantee the first turn is `user`. Anthropic rejects a leading assistant
/// message and (per its docs) consecutive same-role messages with a 400 — which
/// a multi-agent thread produces routinely (two agents reply in a row) and which
/// `/compact` produces every time (it posts a lone assistant "Summary" as the
/// new head). OpenAI-style providers are lenient, so this only ever normalizes.
/// Coalescing joins bodies with a blank line, preserving the `Name:` attribution
/// prefixes; a leading assistant turn gets a minimal synthetic user turn ahead of
/// it rather than being dropped (which would lose that context).
fn repair_alternation(turns: Vec<ChatTurn>) -> Vec<ChatTurn> {
    let mut merged: Vec<ChatTurn> = Vec::with_capacity(turns.len());
    for t in turns {
        match merged.last_mut() {
            Some(last) if last.role == t.role => {
                last.content.push_str("\n\n");
                last.content.push_str(&t.content);
            }
            _ => merged.push(t),
        }
    }
    if merged.first().map(|t| t.role == "assistant").unwrap_or(false) {
        merged.insert(0, ChatTurn::user("(Continuing the earlier conversation.)".to_string()));
    }
    merged
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
        let account_kp = SigningKeypair::generate().unwrap();
        // No account id → publish_identity is a no-op (local-only, unverifiable).
        let author = ActorIdentity::new("u1", "Mara", hive_core::ActorKind::Human);
        (ChatService::new(store, device_id, kp, account_kp, author), public)
    }

    #[test]
    fn pending_mentions_track_unanswered_work() {
        let (mut svc, _pk) = service();
        let wid = Uuid::new_v4();
        let chat = svc.create_chat("A", wid, "anthropic").unwrap();
        let agent = WorkspaceAgent::new("reviewer", "ws-claude");
        let agent_id = agent.id;
        svc.add_agent(chat.id, wid, agent).unwrap();

        // A user @mentions the agent → queued (pending) work.
        svc.post_user_message(chat.id, wid, "@reviewer take a look").unwrap();
        let s = svc.load(chat.id).unwrap().unwrap();
        let pend = pending_mentions(&s);
        assert_eq!(pend.len(), 1);
        assert_eq!(pend[0].agent_id, agent_id);
        assert_eq!(pend[0].agent_name, "reviewer");

        // The agent replies → no longer pending.
        let mid = svc.begin_assistant_message(chat.id, wid, "reviewer", "ws-claude", None, None).unwrap();
        svc.complete_assistant_message(chat.id, wid, mid, "done").unwrap();
        let s = svc.load(chat.id).unwrap().unwrap();
        assert!(pending_mentions(&s).is_empty(), "answered mention should not be pending");

        // A fresh mention after the reply is pending again (catch-up on the latest).
        svc.post_user_message(chat.id, wid, "@reviewer another").unwrap();
        let s = svc.load(chat.id).unwrap().unwrap();
        assert_eq!(pending_mentions(&s).len(), 1, "a new unanswered mention re-queues");
    }

    #[test]
    fn concurrent_same_agent_mentions_do_not_collapse() {
        // Two near-simultaneous tasks to the SAME agent (e.g. two channels / two
        // people). A reply to one must not mark the other answered — the bug the
        // per-request reply_to_message_id correlation fixes.
        let (mut svc, _pk) = service();
        let wid = Uuid::new_v4();
        let chat = svc.create_chat("A", wid, "anthropic").unwrap();
        let agent = WorkspaceAgent::new("reviewer", "ws-claude");
        svc.add_agent(chat.id, wid, agent).unwrap();

        let m1 = svc.post_user_message(chat.id, wid, "@reviewer task one").unwrap().id;
        let m2 = svc.post_user_message(chat.id, wid, "@reviewer task two").unwrap().id;

        // The agent answers task ONE — reply stamped with m1 as its request id.
        let mid = svc
            .begin_assistant_message(chat.id, wid, "reviewer", "ws-claude", None, Some(m1))
            .unwrap();
        svc.complete_assistant_message(chat.id, wid, mid, "done with one").unwrap();

        // Task two must STILL be pending. Under the old author-name match this
        // reply cleared BOTH tasks; with reply_to it only clears task one.
        let s = svc.load(chat.id).unwrap().unwrap();
        let pend = pending_mentions(&s);
        assert_eq!(pend.len(), 1, "reply to task one must not answer task two");
        assert_eq!(pend[0].message_id, m2, "the still-pending mention is task two");
    }

    #[test]
    fn pending_mentions_surfaces_trailing_agent_cascade_not_self() {
        let (mut svc, _pk) = service();
        let wid = Uuid::new_v4();
        let sid = svc.create_chat("c", wid, "anthropic").unwrap().id;
        let scout = WorkspaceAgent::new("Scout", "r");
        let nova = WorkspaceAgent::new("Nova", "r");
        let nova_id = nova.id;
        svc.add_agent(sid, wid, scout).unwrap();
        svc.add_agent(sid, wid, nova).unwrap();
        svc.post_user_message(sid, wid, "kick off").unwrap();
        // Scout replies, @mentioning Nova AND itself — Nova becomes cross-device
        // cascade work (a worker can pick it up); Scout's self-mention must not.
        let m = svc.begin_assistant_message(sid, wid, "Scout", "r", None, None).unwrap();
        svc.complete_assistant_message(sid, wid, m, "@Nova your turn — I (@Scout) am done").unwrap();
        let s = svc.load(sid).unwrap().unwrap();
        let pend = pending_mentions(&s);
        assert!(pend.iter().any(|p| p.agent_id == nova_id), "Nova's cascade mention is queued");
        assert!(
            !pend.iter().any(|p| p.agent_name == "Scout"),
            "an agent's self-mention never re-triggers itself"
        );
    }

    #[test]
    fn host_heartbeat_and_presence() {
        use hive_core::{HostKind, WorkspaceHost};
        let (mut svc, _pk) = service();
        let wid = Uuid::new_v4();
        let hid = svc.device_id().to_string();
        svc.register_host(wid, WorkspaceHost::new(hid.clone(), HostKind::Worker, "box")).unwrap();

        let now = Timestamp::now();
        let hosts = svc.list_hosts(wid).unwrap();
        assert!(hosts[0].is_online(&now, 90), "just-registered host is online");
        // A heartbeat right after registration is throttled (no new event).
        assert!(!svc.touch_host(wid, &hid, 60).unwrap(), "recent heartbeat throttled");

        // A host last seen at the epoch is offline.
        let stale = WorkspaceHost { last_seen: Timestamp::from_unix_seconds(0), ..hosts[0].clone() };
        assert!(!stale.is_online(&now, 90), "stale host is offline");
    }

    #[test]
    fn hosts_and_agent_binding_sync() {
        use hive_core::{HostKind, WorkspaceHost};
        let (mut svc, _pk) = service();
        let wid = Uuid::new_v4();
        let chat = svc.create_chat("A", wid, "anthropic").unwrap();
        let agent = WorkspaceAgent::new("reviewer", "ws-claude");
        let agent_id = agent.id;
        svc.add_agent(chat.id, wid, agent).unwrap();

        // Register this box as a worker host and bind the agent to it.
        let host_id = svc.device_id().to_string();
        svc.register_host(wid, WorkspaceHost::new(host_id.clone(), HostKind::Worker, "box-1")).unwrap();
        svc.set_agent_host(wid, agent_id, &host_id).unwrap();

        // Host + binding are visible from any chat (config hoist).
        let hosts = svc.list_hosts(wid).unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].kind, HostKind::Worker);
        let s = svc.load(chat.id).unwrap().unwrap();
        let bound = s.workspace_agents.iter().find(|a| a.id == agent_id).unwrap();
        assert_eq!(bound.host_id, host_id, "agent should be bound to the worker host");
        assert!(s.workspace_hosts.iter().any(|h| h.id == host_id));

        svc.remove_host(wid, &host_id).unwrap();
        assert!(svc.list_hosts(wid).unwrap().is_empty());
    }

    #[test]
    fn workspace_runtimes_sync_and_resolve_credentials() {
        use hive_core::{ModelProviderKind, WorkspaceCredential, WorkspaceRuntime};
        let (mut svc, _pk) = service();
        let wid = Uuid::new_v4();
        let chat = svc.create_chat("A", wid, "anthropic").unwrap();

        // Define a workspace-owned runtime with an out-of-log secret reference.
        let mut rt = WorkspaceRuntime::new("ws-claude", "Team Claude", ModelProviderKind::Anthropic, "claude-sonnet-4-5");
        rt.credential = WorkspaceCredential::SecretRef { secret_ref: "acme".into() };
        svc.add_workspace_runtime(wid, rt.clone()).unwrap();

        // It's listed on the workspace and visible from any chat (config hoist).
        assert_eq!(svc.list_workspace_runtimes(wid).unwrap().len(), 1);
        let loaded = svc.load(chat.id).unwrap().unwrap();
        assert_eq!(loaded.workspace_runtimes.len(), 1);
        assert_eq!(loaded.workspace_runtimes[0].id, "ws-claude");

        // SecretRef resolves from the worker's env; Inline returns the carried secret.
        assert_eq!(resolve_workspace_credential(&rt.credential), None);
        std::env::set_var("HIVE_WS_SECRET_acme", "sk-workspace-123");
        assert_eq!(resolve_workspace_credential(&rt.credential).as_deref(), Some("sk-workspace-123"));
        std::env::remove_var("HIVE_WS_SECRET_acme");
        assert_eq!(
            resolve_workspace_credential(&WorkspaceCredential::Inline { secret: "sk-inline".into() }).as_deref(),
            Some("sk-inline")
        );

        // Removal clears it everywhere.
        svc.remove_workspace_runtime(wid, "ws-claude").unwrap();
        assert!(svc.load(chat.id).unwrap().unwrap().workspace_runtimes.is_empty());
    }

    #[test]
    fn config_hoists_to_the_workspace_and_is_shared_across_chats() {
        let (mut svc, _pk) = service();
        let wid = Uuid::new_v4();
        let chat_a = svc.create_chat("A", wid, "anthropic").unwrap();
        let chat_b = svc.create_chat("B", wid, "anthropic").unwrap();

        // An agent added "in" chat A is written to the workspace-config log…
        let agent = WorkspaceAgent::new("Scout", "anthropic");
        let agent_id = agent.id;
        svc.add_agent(chat_a.id, wid, agent).unwrap();

        // …and is therefore visible from chat B too (hoist), and from the config log.
        let b = svc.load(chat_b.id).unwrap().unwrap();
        assert!(b.workspace_agents.iter().any(|a| a.id == agent_id), "agent not shared to chat B");
        let cfg = svc.load(workspace_config_session_id(wid)).unwrap().unwrap();
        assert!(cfg.workspace_agents.iter().any(|a| a.id == agent_id));

        // Removing it clears it everywhere.
        svc.remove_agent(chat_a.id, wid, agent_id).unwrap();
        let b = svc.load(chat_b.id).unwrap().unwrap();
        assert!(!b.workspace_agents.iter().any(|a| a.id == agent_id));
    }

    #[test]
    fn channels_ride_the_workspace_config_log() {
        let (mut svc, _pk) = service();
        let wid = Uuid::new_v4();

        // Creating a channel seeds the config log, the channel, and its default chat.
        let (channel, default_chat) = svc
            .create_channel(wid, "auth", Some("auth work".into()), "anthropic")
            .unwrap();
        assert_eq!(channel.name, "auth");
        assert_eq!(channel.default_chat_id, default_chat.id.to_string());
        assert_eq!(channel.position, 0);

        // The channel is listed off the config log (id = workspace_config_session_id).
        let channels = svc.list_channels(wid).unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].id, channel.id);

        // The config log is a distinct session from the default chat.
        let config_id = hive_core::workspace_config_session_id(wid);
        assert_ne!(config_id, default_chat.id);

        // The default chat carries its channel assignment (per-chat, session-scoped).
        let loaded = svc.load(default_chat.id).unwrap().unwrap();
        assert_eq!(loaded.channel_id, channel.id);
        // ...and the config log itself has no channel assignment but holds the roster.
        let cfg = svc.load(config_id).unwrap().unwrap();
        assert!(cfg.channel_id.is_empty());
        assert_eq!(cfg.channels.len(), 1);
        assert_eq!(cfg.members.len(), 1, "config log seeds the creator as member for authz");

        // A second channel gets the next position; rename + archive fold correctly.
        let (c2, _) = svc.create_channel(wid, "ui", None, "anthropic").unwrap();
        assert_eq!(c2.position, 1);
        svc.rename_channel(wid, &channel.id, "authentication", None).unwrap();
        svc.archive_channel(wid, &c2.id, true).unwrap();
        let after = svc.list_channels(wid).unwrap();
        assert_eq!(after.iter().find(|c| c.id == channel.id).unwrap().name, "authentication");
        assert!(after.iter().find(|c| c.id == c2.id).unwrap().archived);
    }

    #[test]
    fn authoring_uses_a_causal_lamport_clock() {
        let (mut svc, pk) = service();
        let chat = svc.create_chat("Demo", Uuid::nil(), "anthropic").unwrap();
        let (sid, wid) = (chat.id, chat.workspace_id);

        // A peer's event arrives with a far-ahead Lamport clock.
        let mut foreign = SessionEventEnvelope::new(
            sid,
            wid,
            1,
            SessionEvent::SessionTitleChanged { title: "from peer".into() },
        );
        foreign.lamport = 500;
        assert!(svc.store.ingest(&foreign).unwrap());

        // The next locally-authored event is causally after everything seen (>500),
        // not just the local authoring count — and is signed with the v2 preimage.
        let env = svc
            .append_signed(sid, wid, SessionEvent::SessionTitleChanged { title: "local".into() })
            .unwrap();
        assert!(env.lamport > 500, "authored lamport {} not causal", env.lamport);
        assert!(verify_envelope(&env, &pk).is_ok());
    }

    #[test]
    fn create_chat_publishes_verifiable_identity() {
        use crate::envelope_verifier::{build_roster, verdict_for, Verdict};

        let store = EventStore::open_in_memory().unwrap();
        let device_kp = SigningKeypair::generate().unwrap();
        let device_id = Uuid::new_v4();
        let account_kp = SigningKeypair::generate().unwrap();
        let account_id = Uuid::new_v4();
        let author = ActorIdentity {
            id: account_id.to_string(),
            display_name: "Alice".into(),
            kind: hive_core::ActorKind::Human,
            account_id: Some(account_id),
            device_id: Some(device_id),
            git_email: None,
            key_agreement_public: None,
            avatar_url: None,
        };
        let mut svc = ChatService::new(store, device_id, device_kp, account_kp, author);

        let chat = svc.create_chat("Demo", Uuid::new_v4(), "anthropic").unwrap();
        svc.post_user_message(chat.id, chat.workspace_id, "hello").unwrap();

        // A peer folds the synced trust events into a roster and verifies our
        // events: our device is trusted and our signed message is Valid.
        let roster = build_roster(&svc.store().roster_envelopes().unwrap());
        let envs = svc.store().list(chat.id).unwrap();
        let msg = envs
            .iter()
            .find(|e| matches!(e.payload, SessionEvent::MessageAppended { .. }))
            .expect("message event");
        assert_eq!(
            verdict_for(&roster, msg),
            Verdict::Valid,
            "our own signed message verifies against the published identity"
        );

        // Idempotent: publishing again adds nothing.
        let before = svc.store().roster_envelopes().unwrap().len();
        assert!(!svc.publish_identity(chat.id, chat.workspace_id).unwrap());
        assert_eq!(svc.store().roster_envelopes().unwrap().len(), before);
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

    // ── Member config-hoist (§11) ──────────────────────────────────────────

    fn test_member(id: &str, name: &str, role: WorkspaceRole) -> WorkspaceMember {
        WorkspaceMember {
            id: id.into(),
            actor: ActorIdentity::new(id, name, hive_core::ActorKind::Human),
            role,
            title: String::new(),
            index: 0,
            joined_at: Timestamp::now(),
        }
    }

    /// A fresh service for `author`, backed by a shared (file) store — so two
    /// actors can act against the same workspace, as separate devices would.
    fn svc_on(path: &std::path::Path, id: &str, name: &str) -> ChatService {
        let store = EventStore::open(path).unwrap();
        let kp = SigningKeypair::generate().unwrap();
        let account_kp = SigningKeypair::generate().unwrap();
        let author = ActorIdentity::new(id, name, hive_core::ActorKind::Human);
        ChatService::new(store, Uuid::new_v4(), kp, account_kp, author)
    }

    #[test]
    fn adopt_account_identity_migrates_provisional_self_without_duplicate() {
        // The churn root cure: a provisional pre-sign-in self (Owner, no account_id)
        // must become the ONE account self — not a second member.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hive.db");
        let wid = Uuid::new_v4();
        let account = Uuid::new_v4().to_string();
        let cfg = workspace_config_session_id(wid);

        let mut svc = svc_on(&path, "prov-device", "You"); // provisional, account_id None
        svc.ensure_workspace_config(wid).unwrap(); // seeds prov as Owner
        let before = svc.load(cfg).unwrap().unwrap();
        assert_eq!(before.members.len(), 1);
        assert_eq!(before.members[0].id, "prov-device");

        let migrated = svc
            .adopt_account_identity(wid, account.clone(), "Michael", Some("m@x.com".into()))
            .unwrap();
        assert!(migrated, "should report a migration");

        let after = svc.load(cfg).unwrap().unwrap();
        let humans: Vec<_> = after
            .members
            .iter()
            .filter(|m| m.actor.kind == hive_core::ActorKind::Human)
            .collect();
        assert_eq!(humans.len(), 1, "exactly one self after migration");
        assert_eq!(humans[0].id, account);
        assert_eq!(humans[0].role, WorkspaceRole::Owner);
        assert!(
            after.members.iter().all(|m| m.id != "prov-device"),
            "provisional self removed"
        );
        assert_eq!(svc.author().id, account, "author switched to the account");
    }

    #[test]
    fn adopt_account_identity_promotes_then_dedups_a_preexisting_contributor_self() {
        // The already-churned shape: sign-in already added the account as a
        // Contributor (ensure_self_member) while the provisional stayed Owner.
        // Adopt must promote the account to Owner, then drop the provisional —
        // never stripping the last owner.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hive.db");
        let wid = Uuid::new_v4();
        let account = Uuid::new_v4().to_string();
        let cfg = workspace_config_session_id(wid);

        let mut svc = svc_on(&path, "prov-device", "You");
        svc.ensure_workspace_config(wid).unwrap(); // prov = Owner
        // Simulate ensure_self_member having added the account as a Contributor.
        svc.add_member(cfg, wid, test_member(&account, "Michael", WorkspaceRole::Contributor))
            .unwrap();

        let migrated = svc
            .adopt_account_identity(wid, account.clone(), "Michael", None)
            .unwrap();
        assert!(migrated);

        let after = svc.load(cfg).unwrap().unwrap();
        let humans: Vec<_> = after
            .members
            .iter()
            .filter(|m| m.actor.kind == hive_core::ActorKind::Human)
            .collect();
        assert_eq!(humans.len(), 1);
        assert_eq!(humans[0].id, account);
        assert_eq!(humans[0].role, WorkspaceRole::Owner, "account promoted to owner");
    }

    #[test]
    fn adopt_account_identity_is_a_noop_when_already_an_account() {
        // Signing in again (already an account identity) must not churn or migrate.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hive.db");
        let wid = Uuid::new_v4();
        let account = Uuid::new_v4().to_string();
        let mut svc = svc_on(&path, &account, "Michael");
        svc.set_author_account(account.clone(), "Michael", None); // author already an account
        svc.ensure_workspace_config(wid).unwrap();
        // Re-adopting the SAME account (e.g. a second sign-in) is a no-op migration.
        let migrated = svc.adopt_account_identity(wid, account.clone(), "Michael", None).unwrap();
        assert!(!migrated, "no migration when the prior identity was already an account");
        let cfg = workspace_config_session_id(wid);
        let humans = svc
            .load(cfg)
            .unwrap()
            .unwrap()
            .members
            .iter()
            .filter(|m| m.actor.kind == hive_core::ActorKind::Human)
            .count();
        assert_eq!(humans, 1, "still exactly one self");
    }

    #[test]
    fn non_member_worker_manages_its_own_host_but_not_governance() {
        // The self-host carve-out: a worker box (not a workspace member, so
        // Viewer-floored) may register + heartbeat the host it owns, yet still
        // can't perform governance. Proves the #61-hardening worker regression
        // is closed without enrolling the worker as a member.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hive.db");
        let wid = Uuid::new_v4();
        {
            let mut owner = svc_on(&path, "owner", "Owner");
            owner.create_chat("A", wid, "anthropic").unwrap();
            owner.ensure_workspace_config(wid).unwrap();
        }
        let mut worker = svc_on(&path, "worker", "Worker");
        // register_host stamps owner_actor_id = self → allowed via the carve-out.
        worker
            .register_host(wid, hive_core::WorkspaceHost::new("box1", hive_core::HostKind::Worker, "prod"))
            .unwrap();
        // Heartbeat its own host (throttle 0 forces an emit) → allowed.
        assert!(worker.touch_host(wid, "box1", 0).unwrap());
        // But governance stays denied — the Viewer floor holds for member adds.
        let victim = test_member("victim", "V", WorkspaceRole::Contributor);
        assert!(worker.add_member(wid, wid, victim).is_err());
    }

    #[test]
    fn member_added_in_one_chat_is_visible_in_another_chat() {
        // (a) The staleness gap: a member added "in" chat A must appear in the
        // roster of a *different* chat B in the same workspace after load().
        let (mut svc, _) = service();
        let wid = Uuid::new_v4();
        let chat_a = svc.create_chat("A", wid, "anthropic").unwrap();
        let chat_b = svc.create_chat("B", wid, "anthropic").unwrap();

        svc.add_member(chat_a.id, wid, test_member("u2", "Bo", WorkspaceRole::Contributor))
            .unwrap();

        let b = svc.load(chat_b.id).unwrap().unwrap();
        assert!(
            b.members.iter().any(|m| m.id == "u2"),
            "member added in chat A must be visible in chat B (hoist)"
        );
        // …and it lives on the config log, the workspace-wide source of truth.
        let cfg = svc.load(workspace_config_session_id(wid)).unwrap().unwrap();
        assert!(cfg.members.iter().any(|m| m.id == "u2"));
    }

    #[test]
    fn removing_a_member_propagates_workspace_wide() {
        // (d) Removal in one chat clears the member from every chat.
        let (mut svc, _) = service();
        let wid = Uuid::new_v4();
        let chat_a = svc.create_chat("A", wid, "anthropic").unwrap();
        let chat_b = svc.create_chat("B", wid, "anthropic").unwrap();
        svc.add_member(chat_a.id, wid, test_member("u2", "Bo", WorkspaceRole::Contributor))
            .unwrap();
        assert!(svc.load(chat_b.id).unwrap().unwrap().members.iter().any(|m| m.id == "u2"));

        // Remove via chat B's service call — propagates to chat A too.
        svc.remove_member(chat_b.id, wid, "u2").unwrap();
        assert!(
            !svc.load(chat_a.id).unwrap().unwrap().members.iter().any(|m| m.id == "u2"),
            "removal must propagate to every chat"
        );
        let cfg = svc.load(workspace_config_session_id(wid)).unwrap().unwrap();
        assert!(!cfg.members.iter().any(|m| m.id == "u2"));
    }

    #[test]
    fn role_change_propagates_workspace_wide() {
        let (mut svc, _) = service();
        let wid = Uuid::new_v4();
        let chat_a = svc.create_chat("A", wid, "anthropic").unwrap();
        let chat_b = svc.create_chat("B", wid, "anthropic").unwrap();
        svc.add_member(chat_a.id, wid, test_member("u2", "Bo", WorkspaceRole::Contributor))
            .unwrap();
        svc.set_member_role(chat_a.id, wid, "u2", WorkspaceRole::Admin).unwrap();

        let b = svc.load(chat_b.id).unwrap().unwrap();
        let u2 = b.members.iter().find(|m| m.id == "u2").unwrap();
        assert_eq!(u2.role, WorkspaceRole::Admin, "role change must propagate workspace-wide");
    }

    #[test]
    fn ensure_self_member_registers_on_config_log_workspace_wide() {
        // (b) A joining actor's ensure_self_member writes to the config log and is
        // therefore visible from every chat, on every device.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hive.db");
        let wid = Uuid::new_v4();

        let (chat_a, chat_b) = {
            let mut a = svc_on(&path, "u1", "Ana");
            let ca = a.create_chat("A", wid, "anthropic").unwrap();
            let cb = a.create_chat("B", wid, "anthropic").unwrap();
            // Seed the config log with u1 as Owner (first toucher).
            a.ensure_workspace_config(wid).unwrap();
            (ca.id, cb.id)
        };

        // A different device/actor u2 opens chat A and registers itself.
        {
            let mut b = svc_on(&path, "u2", "Bo");
            assert!(b.ensure_self_member(chat_a, wid).unwrap(), "u2 should be added");
            // Idempotent second call.
            assert!(!b.ensure_self_member(chat_a, wid).unwrap(), "second call is a no-op");
        }

        // From u1's device, u2 now shows up on the config log and in chat B.
        {
            let a = svc_on(&path, "u1", "Ana");
            let cfg = a.load(workspace_config_session_id(wid)).unwrap().unwrap();
            assert!(cfg.members.iter().any(|m| m.id == "u2"), "self-member missing from config log");
            let b = a.load(chat_b).unwrap().unwrap();
            assert!(
                b.members.iter().any(|m| m.id == "u2"),
                "self-member must be visible workspace-wide"
            );
        }
    }

    #[test]
    fn ensure_self_member_via_config_id_seeds_a_fresh_joiner() {
        // Fix #3: `join_workspace`/`set_active_workspace` call `ensure_self_member`
        // with the workspace-config session id the moment a workspace goes active —
        // before any chat exists and before the owner's config log has synced. The
        // old existence guard short-circuited that to a no-op, so a joiner stayed
        // invisible in the roster. The config log is materialized on demand, so the
        // joiner must now land on it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hive.db");
        let wid = Uuid::new_v4();
        let cfg_id = workspace_config_session_id(wid);

        let mut joiner = svc_on(&path, "u2", "Bo");
        // No chats, no synced config log — the pre-fix guard would `return Ok(false)`
        // here without ever creating the config log.
        joiner.ensure_self_member(cfg_id, wid).unwrap();

        let cfg = joiner.load(cfg_id).unwrap().unwrap();
        assert!(
            cfg.members.iter().any(|m| m.id == "u2"),
            "a fresh joiner must appear on the workspace-config roster"
        );
    }

    #[test]
    fn authorization_evaluates_against_the_hoisted_roster() {
        // (c) min-role enforcement now reads the unioned/hoisted roster: a Viewer
        // per the workspace roster cannot add members from any chat; an Owner can.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hive.db");
        let wid = Uuid::new_v4();

        let chat_id = {
            let mut a = svc_on(&path, "u1", "Ana");
            let chat = a.create_chat("A", wid, "anthropic").unwrap();
            // Enroll u2 as a Viewer on the workspace-wide roster.
            a.add_member(chat.id, wid, test_member("u2", "Bo", WorkspaceRole::Viewer)).unwrap();
            chat.id
        };

        // u2 is a Viewer in the hoisted roster → cannot add members (needs Admin).
        {
            let mut b = svc_on(&path, "u2", "Bo");
            let err = b.add_member(chat_id, wid, test_member("u3", "Cy", WorkspaceRole::Contributor));
            assert!(
                matches!(err, Err(ChatError::Unauthorized(_))),
                "a Viewer in the hoisted roster must not add members, got {err:?}"
            );
        }

        // The Owner u1 still can, and it lands on the workspace roster.
        {
            let mut a = svc_on(&path, "u1", "Ana");
            a.add_member(chat_id, wid, test_member("u3", "Cy", WorkspaceRole::Contributor)).unwrap();
            let cfg = a.load(workspace_config_session_id(wid)).unwrap().unwrap();
            assert!(cfg.members.iter().any(|m| m.id == "u3"));
        }
    }

    // ── Authz hardening: bootstrap preserved, Owner fallback closed (#61 seams) ─

    #[test]
    fn bootstrap_creator_seeds_owner_and_can_add_a_second_member() {
        // Fresh workspace: the creator is Owner and governance works for them.
        let (mut svc, _) = service();
        let wid = Uuid::new_v4();
        let chat = svc.create_chat("A", wid, "anthropic").unwrap();
        let cfg = svc.load(workspace_config_session_id(wid));
        // (config log is seeded lazily; first governance call seeds it)
        assert!(cfg.unwrap().is_none());
        svc.add_member(chat.id, wid, test_member("u2", "Bo", WorkspaceRole::Contributor))
            .unwrap();
        let cfg = svc.load(workspace_config_session_id(wid)).unwrap().unwrap();
        assert_eq!(cfg.members.iter().find(|m| m.id == "u1").unwrap().role, WorkspaceRole::Owner);
        assert!(cfg.members.iter().any(|m| m.id == "u2"));
    }

    #[test]
    fn non_member_cannot_remove_or_demote_via_owner_fallback() {
        // Previously a non-member fell back to Owner and could govern. Now the
        // floor is Viewer: a stranger's device is denied removal *and* demotion.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hive.db");
        let wid = Uuid::new_v4();
        let chat_id = {
            let mut a = svc_on(&path, "u1", "Ana");
            let chat = a.create_chat("A", wid, "anthropic").unwrap();
            a.add_member(chat.id, wid, test_member("u2", "Bo", WorkspaceRole::Contributor)).unwrap();
            chat.id
        };
        let mut x = svc_on(&path, "outsider", "Eve");
        assert!(
            matches!(x.remove_member(chat_id, wid, "u2"), Err(ChatError::Unauthorized(_))),
            "a non-member must not remove an existing member"
        );
        assert!(
            matches!(
                x.set_member_role(chat_id, wid, "u2", WorkspaceRole::Viewer),
                Err(ChatError::Unauthorized(_))
            ),
            "a non-member must not demote an existing member"
        );
        // The roster is untouched.
        let cfg = svc_on(&path, "u1", "Ana").load(workspace_config_session_id(wid)).unwrap().unwrap();
        assert_eq!(cfg.members.iter().find(|m| m.id == "u2").unwrap().role, WorkspaceRole::Contributor);
    }

    #[test]
    fn contributor_denied_governance_admin_allowed() {
        // Role gating on the hoisted roster: a Contributor member can't govern; an
        // Admin member can.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hive.db");
        let wid = Uuid::new_v4();
        let chat_id = {
            let mut a = svc_on(&path, "u1", "Ana");
            let chat = a.create_chat("A", wid, "anthropic").unwrap();
            a.add_member(chat.id, wid, test_member("con", "Con", WorkspaceRole::Contributor)).unwrap();
            a.add_member(chat.id, wid, test_member("adm", "Adm", WorkspaceRole::Admin)).unwrap();
            chat.id
        };
        let mut con = svc_on(&path, "con", "Con");
        assert!(
            matches!(
                con.add_member(chat_id, wid, test_member("x", "X", WorkspaceRole::Viewer)),
                Err(ChatError::Unauthorized(_))
            ),
            "a Contributor must not add members"
        );
        let mut adm = svc_on(&path, "adm", "Adm");
        adm.add_member(chat_id, wid, test_member("x", "X", WorkspaceRole::Viewer)).unwrap();
        let cfg = svc_on(&path, "u1", "Ana").load(workspace_config_session_id(wid)).unwrap().unwrap();
        assert!(cfg.members.iter().any(|m| m.id == "x"));
    }

    #[test]
    fn non_member_cannot_self_enroll_as_owner() {
        // No self-escalation: the self-`MemberAdded` bootstrap path can't grant
        // governance. A device may only self-enroll at a non-governance role.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hive.db");
        let wid = Uuid::new_v4();
        let chat_id = {
            let mut a = svc_on(&path, "u1", "Ana");
            let chat = a.create_chat("A", wid, "anthropic").unwrap();
            a.ensure_workspace_config(wid).unwrap();
            chat.id
        };
        let mut eve = svc_on(&path, "eve", "Eve");
        // Directly attempt the self-escalation the allowance must reject.
        let cfg_id = workspace_config_session_id(wid);
        let escalate = eve.add_member(cfg_id, wid, test_member("eve", "Eve", WorkspaceRole::Owner));
        assert!(
            matches!(escalate, Err(ChatError::Unauthorized(_))),
            "self-enroll as Owner must be denied, got {escalate:?}"
        );
        // But an honest self-enroll (Contributor, via ensure_self_member) succeeds.
        assert!(eve.ensure_self_member(chat_id, wid).unwrap());
        let cfg = svc_on(&path, "u1", "Ana").load(cfg_id).unwrap().unwrap();
        assert_eq!(cfg.members.iter().find(|m| m.id == "eve").unwrap().role, WorkspaceRole::Contributor);
    }

    #[test]
    fn config_member_wins_over_stale_per_session_member() {
        // Dedup rule: when a chat carries a per-session member with the same id as
        // a config-log member, the config entry (source of truth) wins on load().
        let (mut svc, _) = service();
        let wid = Uuid::new_v4();
        let chat = svc.create_chat("A", wid, "anthropic").unwrap();

        // Write a stale per-session member directly onto the chat (pre-hoist shape).
        svc.append_signed(
            chat.id,
            wid,
            SessionEvent::MemberAdded { member: test_member("u2", "Bo", WorkspaceRole::Viewer) },
        )
        .unwrap();
        // The authoritative config-log entry gives u2 a higher role.
        svc.add_member(chat.id, wid, test_member("u2", "Bo", WorkspaceRole::Admin)).unwrap();

        let loaded = svc.load(chat.id).unwrap().unwrap();
        let u2: Vec<_> = loaded.members.iter().filter(|m| m.id == "u2").collect();
        assert_eq!(u2.len(), 1, "no duplicate member after union");
        assert_eq!(u2[0].role, WorkspaceRole::Admin, "config-log entry wins over per-session");
    }

    #[test]
    fn full_turn_streams_and_projects() {
        let (mut svc, _pk) = service();
        let chat = svc.create_chat("Demo", Uuid::nil(), "anthropic").unwrap();
        let sid = chat.id;
        let wid = chat.workspace_id;

        svc.post_user_message(sid, wid, "Say hello").unwrap();
        let mid = svc
            .begin_assistant_message(sid, wid, "Hive", "anthropic", None, None)
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

        // provider turns include both, in wire shape, attributed for "Hive":
        // the human turn is name-prefixed; Hive's own turn stays unprefixed.
        let turns = turns_for(&session, None, "Hive");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].content, "Mara: Say hello");
        assert_eq!(turns[1].role, "assistant");
        assert_eq!(turns[1].content, "Hello, world");
    }

    #[test]
    fn turns_attribute_other_agents_but_not_the_responder() {
        let (mut svc, _pk) = service();
        let wid = Uuid::new_v4();
        let chat = svc.create_chat("Multi", wid, "anthropic").unwrap();
        let sid = chat.id;
        svc.post_user_message(sid, wid, "kick off").unwrap();
        // Two agents reply into the same thread.
        let m1 = svc.begin_assistant_message(sid, wid, "Scout", "ws-claude", None, None).unwrap();
        svc.complete_assistant_message(sid, wid, m1, "found the bug").unwrap();
        let m2 = svc.begin_assistant_message(sid, wid, "Nova", "ws-claude", None, None).unwrap();
        svc.complete_assistant_message(sid, wid, m2, "drafting the fix").unwrap();

        // From Scout's perspective: its own turn is clean; Nova + the human are
        // attributed so Scout can tell the contributions apart. Scout's and Nova's
        // consecutive assistant turns coalesce (alternation repair), preserving
        // the attribution prefixes.
        let session = svc.load(sid).unwrap().unwrap();
        let turns = turns_for(&session, None, "Scout");
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].content, "Mara: kick off");
        assert_eq!(turns[1].role, "assistant");
        assert_eq!(turns[1].content, "found the bug\n\nNova: drafting the fix");
    }

    #[test]
    fn attribution_keys_on_responder_id_not_name() {
        // Two agents share the display name "Nova" but have distinct ids. When
        // Nova-A responds (own_id = its id), only its own id-matched turn is
        // unprefixed; Nova-B's same-named turn is attributed, not mistaken as A's.
        let (mut svc, _pk) = service();
        let wid = Uuid::new_v4();
        let sid = svc.create_chat("dup", wid, "anthropic").unwrap().id;
        svc.post_user_message(sid, wid, "go").unwrap();
        let nova_a = Uuid::new_v4();
        let nova_b = Uuid::new_v4();
        let a = svc.begin_assistant_message(sid, wid, "Nova", "rt", Some(nova_a), None).unwrap();
        svc.complete_assistant_message(sid, wid, a, "A analysis").unwrap();
        let b = svc.begin_assistant_message(sid, wid, "Nova", "rt", Some(nova_b), None).unwrap();
        svc.complete_assistant_message(sid, wid, b, "B take").unwrap();

        let session = svc.load(sid).unwrap().unwrap();
        let turns = turns_for(&session, Some(nova_a), "Nova");
        // A's turn (id-matched) unprefixed; B's (same name, different id) named.
        // The two consecutive assistant turns coalesce.
        assert_eq!(turns.last().unwrap().content, "A analysis\n\nNova: B take");
    }

    #[test]
    fn alternation_repair_prepends_user_before_a_leading_assistant() {
        // A window boundary / `/compact` can leave an assistant turn at the head;
        // Anthropic requires the first message to be `user`.
        let turns = repair_alternation(vec![
            ChatTurn::assistant("Summary of earlier work".to_string()),
            ChatTurn::user("now what?".to_string()),
        ]);
        assert_eq!(turns[0].role, "user"); // synthetic user prepended
        assert_eq!(turns[1].role, "assistant");
        assert_eq!(turns[1].content, "Summary of earlier work"); // not dropped
        assert_eq!(turns[2].content, "now what?");
    }

    #[test]
    fn clear_stale_streaming_sweeps_a_ghost_message() {
        // Simulate a hard quit mid-stream: a placeholder is begun and chunked but
        // never completed, so it persists `is_streaming: true` — a perpetual
        // "typing" ghost. The startup sweep must clear it while preserving the body.
        let (mut svc, _) = service();
        let chat = svc.create_chat("Demo", Uuid::nil(), "anthropic").unwrap();
        let (sid, wid) = (chat.id, chat.workspace_id);

        svc.post_user_message(sid, wid, "hi").unwrap();
        let mid = svc.begin_assistant_message(sid, wid, "Hive", "anthropic", None, None).unwrap();
        svc.append_chunk(sid, wid, mid, "partial reply").unwrap();
        // No complete_assistant_message — the process "died" here.

        let before = svc.load(sid).unwrap().unwrap();
        assert!(before.messages.iter().any(|m| m.id == mid && m.is_streaming));

        let swept = svc.clear_stale_streaming().unwrap();
        assert_eq!(swept, 1, "one ghost message swept");

        let after = svc.load(sid).unwrap().unwrap();
        let msg = after.messages.iter().find(|m| m.id == mid).unwrap();
        assert!(!msg.is_streaming, "stale is_streaming cleared on sweep");
        assert_eq!(msg.body, "partial reply", "accumulated body preserved");

        // Idempotent: a second sweep finds nothing to clear.
        assert_eq!(svc.clear_stale_streaming().unwrap(), 0);
    }

    #[test]
    fn skills_install_replace_and_remove_round_trip() {
        // The event → store → projection loop behind Skills: installing emits
        // SkillsUpdated, re-installing the SAME id replaces (not duplicates), and
        // removal by id drops it. Identity is the stable id, not the display name.
        // `prompt::tests` covers the last hop (loaded skills → system prompt).
        let (mut svc, _) = service();
        let chat = svc.create_chat("Demo", Uuid::nil(), "anthropic").unwrap();
        let (sid, wid) = (chat.id, chat.workspace_id);

        let skill = hive_core::SkillProfile::new("review", "Always review diffs first.");
        let skill_id = skill.id;
        svc.add_skill(sid, wid, skill).unwrap();
        let loaded = svc.load(sid).unwrap().unwrap().loaded_skills;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "review");
        assert_eq!(loaded[0].instructions, "Always review diffs first.");

        // Same id → replaced in place, not duplicated.
        let mut updated = hive_core::SkillProfile::new("review", "v2 instructions");
        updated.id = skill_id;
        svc.add_skill(sid, wid, updated).unwrap();
        let loaded = svc.load(sid).unwrap().unwrap().loaded_skills;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].instructions, "v2 instructions");

        svc.remove_skill(sid, wid, loaded[0].id).unwrap();
        assert!(svc.load(sid).unwrap().unwrap().loaded_skills.is_empty());
    }

    #[test]
    fn skills_identity_is_id_not_name() {
        // Two skills sharing a display name but with distinct ids must coexist
        // (add dedups by id), and remove-by-id must target exactly one — proving
        // add/remove/union all key on the stable id, not the name.
        let (mut svc, _) = service();
        let chat = svc.create_chat("Demo", Uuid::nil(), "anthropic").unwrap();
        let (sid, wid) = (chat.id, chat.workspace_id);

        let a = hive_core::SkillProfile::new("review", "variant A");
        let b = hive_core::SkillProfile::new("review", "variant B");
        let (a_id, b_id) = (a.id, b.id);
        svc.add_skill(sid, wid, a).unwrap();
        svc.add_skill(sid, wid, b).unwrap();

        let loaded = svc.load(sid).unwrap().unwrap().loaded_skills;
        assert_eq!(loaded.len(), 2, "same-name distinct-id skills coexist");

        // Remove-by-id drops only the targeted one (the other same-name survives).
        svc.remove_skill(sid, wid, a_id).unwrap();
        let loaded = svc.load(sid).unwrap().unwrap().loaded_skills;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, b_id);
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
