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
use crate::channel::Channel;
use crate::chat::{ChatMessage, MessageReaction};
use crate::workspace_host::WorkspaceHost;
use crate::workspace_runtime::WorkspaceRuntime;
use crate::crypto::DeviceCertificate;
use crate::identity::{ActorStamp, WorkspaceMember, WorkspaceRole};
use crate::proposals::{ActionProposal, ProposalApproval};
use crate::session::ChatSession;
use crate::skills::SkillProfile;
use crate::time_util::Timestamp;
use crate::vault::MountedVault;
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
    /// A device claims the right to answer `trigger_id` (a user message). Because
    /// ownership is account-scoped, all of a person's online devices otherwise
    /// self-elect and double-answer. The fold applies events in canonical
    /// `(lamport, event_id)` order, so the FIRST claim seen per trigger wins
    /// deterministically on every device (see `ChatSession::turn_claims`); a
    /// device that has synced another device's winning claim backs off. Benign
    /// coordination — not projection-gated.
    TurnClaimed { trigger_id: Uuid, device_id: Uuid },
    MemberAdded { member: WorkspaceMember },
    MemberRemoved { member_id: String },
    MemberRoleChanged { change: MemberRoleChange },
    AgentRosterUpdated { agents: Vec<WorkspaceAgent> },
    /// Upsert a single workspace agent (by id) — the per-item delta that
    /// supersedes the whole-list `AgentRosterUpdated`. Two admins each adding a
    /// *different* agent concurrently from their own base both survive; the
    /// list-replace form dropped whichever event sorted earlier in canonical
    /// order. Rides the config log; workspace-scoped.
    AgentUpserted { agent: WorkspaceAgent },
    /// Remove a single workspace agent by id (delta). Idempotent — removing an
    /// unknown id is a no-op. Supersedes list-replace `AgentRosterUpdated`.
    AgentRemoved { agent_id: Uuid },
    SessionRuntimeChanged { runtime_id: String },
    /// Rename the session (manual, or auto-generated from the first exchange).
    SessionTitleChanged { title: String },
    /// Soft delete / restore — flips the session's `archived` flag.
    SessionArchivedChanged { archived: bool },
    /// Replace the session's loaded skill set.
    SkillsUpdated { skills: Vec<SkillProfile> },
    /// Create or update a proposal (upsert by id) — carries the full snapshot.
    /// Used for creation/metadata only; votes ride `ProposalVoteCast` so they
    /// don't clobber each other.
    ProposalUpserted { proposal: ActionProposal },
    /// A single vote on a proposal (a delta, not a whole-object snapshot). It
    /// merges into the proposal's approvals set — per-actor last-writer-wins by
    /// canonical order, with quorum recomputed deterministically — so concurrent
    /// votes from different actors are all preserved. Replaces the former
    /// read-modify-write-full-snapshot vote, which silently dropped one of two
    /// concurrent votes.
    ProposalVoteCast {
        proposal_id: Uuid,
        approval: ProposalApproval,
    },
    /// Hide (or restore) a proposal in the Review inbox. A delta, like a vote,
    /// rather than a full-proposal upsert — so clearing the inbox on one device
    /// can't clobber a concurrent vote from another. Deliberately *not* a row
    /// delete: the session is an event-sourced log, so a delete wouldn't survive
    /// a re-fold and wouldn't replicate.
    ProposalDismissed { proposal_id: Uuid, dismissed: bool },
    /// Replace the session's vault source set.
    VaultSourcesUpdated { sources: Vec<MountedVault> },
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
    /// Publishes an account's signing public key (GitHub-anchored: `account_id`
    /// is derived from the GitHub user id). This is the root a `DeviceCertificate`
    /// chains to. Trust metadata — inert in the session projection; consumed by
    /// the device-roster builder for signature verification. See
    /// docs/security-hardening-plan.md S1.
    AccountKeyRegistered {
        account_id: Uuid,
        signing_public_key: Vec<u8>,
    },
    /// Rotate an account's signing key. `succession_signature` is the NEW key,
    /// signed by the account's CURRENT (pre-rotation) key — proof that the same
    /// owner performed the rotation, not an attacker. The roster accepts the new
    /// key only when that signature verifies against the currently-pinned key; an
    /// absent/forged signature is a contested binding (disputed, fail-closed). This
    /// is what lets a legitimate owner rotate a key without tripping the
    /// first-wins/dispute protection (P2-2). It does NOT re-open the initial
    /// land-grab (a forged FIRST registration) — that still needs an external
    /// anchor (Option C).
    AccountKeyRotated {
        account_id: Uuid,
        new_signing_public_key: Vec<u8>,
        succession_signature: Vec<u8>,
    },
    /// Publishes a device certificate (`device_id → device signing key`, signed
    /// by the account key), extending trust from an account to one of its
    /// devices. Trust metadata — inert in projection; consumed by the roster.
    DeviceCertificateAdded {
        certificate: DeviceCertificate,
    },
    /// Revoke a single device (e.g. a lost/compromised laptop) without removing its
    /// whole account. Folded into the roster's revoked set, so the device can no
    /// longer sign events — even though its account is still a member. Pair with a
    /// `WorkspaceKeyRotation` that excludes the device to also cut its read access.
    DeviceRevoked {
        device_id: Uuid,
    },
    /// Create (or upsert by id) a channel. Rides the workspace-config log
    /// (spec §11); folded into `ChatSession.channels`. Workspace-scoped.
    ChannelCreated { channel: Channel },
    /// Rename a channel and/or set its one-line purpose (by id). `purpose` is a
    /// field-level delta, not a co-overwrite of the name: `None` means **leave the
    /// existing purpose unchanged** (a plain rename must not wipe a concurrently or
    /// previously set purpose), while `Some(s)` overwrites it — including `Some("")`
    /// as an explicit *clear*. This decouples the two fields so a rename and a
    /// purpose-set on the same channel no longer clobber each other all-or-nothing.
    ChannelRenamed {
        channel_id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        purpose: Option<String>,
    },
    /// Reorder channels; `channel_ids` is the new order (ascending position).
    ChannelReordered { channel_ids: Vec<String> },
    /// Soft delete / restore a channel and its chats (by id).
    ChannelArchived { channel_id: String, archived: bool },
    /// Assign THIS chat to a channel (per-chat, session-scoped — not on the
    /// config log). Empty clears the assignment.
    ChatChannelChanged { channel_id: String },
    /// Replace the workspace-owned runtime set (spec §12.5) — detached/headless
    /// agents run on these. Rides the config log; workspace-scoped.
    WorkspaceRuntimesUpdated { runtimes: Vec<WorkspaceRuntime> },
    /// Upsert a single workspace runtime (by id) — the per-item delta that
    /// supersedes whole-list `WorkspaceRuntimesUpdated`, so two concurrent adds
    /// of different runtimes both survive. Rides the config log; workspace-scoped.
    WorkspaceRuntimeUpserted { runtime: WorkspaceRuntime },
    /// Remove a single workspace runtime by id (delta). Idempotent.
    WorkspaceRuntimeRemoved { runtime_id: String },
    /// Replace the workspace host roster (spec §12.4) — devices + workers.
    /// Rides the config log; workspace-scoped.
    WorkspaceHostsUpdated { hosts: Vec<WorkspaceHost> },
    /// Upsert a single workspace host (by id) — the per-item delta that
    /// supersedes whole-list `WorkspaceHostsUpdated`, so two concurrent host
    /// registrations/heartbeats don't clobber each other. Rides the config log;
    /// workspace-scoped.
    WorkspaceHostUpserted { host: WorkspaceHost },
    /// Remove a single workspace host by id (delta). Idempotent.
    WorkspaceHostRemoved { host_id: String },
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
            SessionEvent::TurnClaimed { .. } => "turnClaimed",
            SessionEvent::MemberAdded { .. } => "memberAdded",
            SessionEvent::MemberRemoved { .. } => "memberRemoved",
            SessionEvent::MemberRoleChanged { .. } => "memberRoleChanged",
            SessionEvent::AgentRosterUpdated { .. } => "agentRosterUpdated",
            SessionEvent::AgentUpserted { .. } => "agentUpserted",
            SessionEvent::AgentRemoved { .. } => "agentRemoved",
            SessionEvent::SessionRuntimeChanged { .. } => "sessionRuntimeChanged",
            SessionEvent::SessionTitleChanged { .. } => "sessionTitleChanged",
            SessionEvent::SessionArchivedChanged { .. } => "sessionArchivedChanged",
            SessionEvent::SkillsUpdated { .. } => "skillsUpdated",
            SessionEvent::ProposalUpserted { .. } => "proposalUpserted",
            SessionEvent::ProposalVoteCast { .. } => "proposalVoteCast",
            SessionEvent::ProposalDismissed { .. } => "proposalDismissed",
            SessionEvent::VaultSourcesUpdated { .. } => "vaultSourcesUpdated",
            SessionEvent::WorkflowDefinitionsUpdated { .. } => "workflowDefinitionsUpdated",
            SessionEvent::WorkflowRunUpserted { .. } => "workflowRunUpserted",
            SessionEvent::MessageReactionAdded { .. } => "messageReactionAdded",
            SessionEvent::MessageReactionRemoved { .. } => "messageReactionRemoved",
            SessionEvent::AccountKeyRegistered { .. } => "accountKeyRegistered",
            SessionEvent::AccountKeyRotated { .. } => "accountKeyRotated",
            SessionEvent::DeviceCertificateAdded { .. } => "deviceCertificateAdded",
            SessionEvent::DeviceRevoked { .. } => "deviceRevoked",
            SessionEvent::ChannelCreated { .. } => "channelCreated",
            SessionEvent::ChannelRenamed { .. } => "channelRenamed",
            SessionEvent::ChannelReordered { .. } => "channelReordered",
            SessionEvent::ChannelArchived { .. } => "channelArchived",
            SessionEvent::ChatChannelChanged { .. } => "chatChannelChanged",
            SessionEvent::WorkspaceRuntimesUpdated { .. } => "workspaceRuntimesUpdated",
            SessionEvent::WorkspaceRuntimeUpserted { .. } => "workspaceRuntimeUpserted",
            SessionEvent::WorkspaceRuntimeRemoved { .. } => "workspaceRuntimeRemoved",
            SessionEvent::WorkspaceHostsUpdated { .. } => "workspaceHostsUpdated",
            SessionEvent::WorkspaceHostUpserted { .. } => "workspaceHostUpserted",
            SessionEvent::WorkspaceHostRemoved { .. } => "workspaceHostRemoved",
            SessionEvent::Unknown => "unknown",
        }
    }

    /// Event scope — mirrors `scope(for:)` in `SignedEnvelopeBuilder`.
    pub fn scope(&self) -> EventScope {
        match self {
            SessionEvent::MemberAdded { .. }
            | SessionEvent::MemberRemoved { .. }
            | SessionEvent::MemberRoleChanged { .. }
            | SessionEvent::AgentRosterUpdated { .. }
            | SessionEvent::AgentUpserted { .. }
            | SessionEvent::AgentRemoved { .. }
            | SessionEvent::AccountKeyRegistered { .. }
            | SessionEvent::DeviceCertificateAdded { .. }
            | SessionEvent::ChannelCreated { .. }
            | SessionEvent::ChannelRenamed { .. }
            | SessionEvent::ChannelReordered { .. }
            | SessionEvent::ChannelArchived { .. }
            | SessionEvent::WorkspaceRuntimesUpdated { .. }
            | SessionEvent::WorkspaceRuntimeUpserted { .. }
            | SessionEvent::WorkspaceRuntimeRemoved { .. }
            | SessionEvent::WorkspaceHostsUpdated { .. }
            | SessionEvent::WorkspaceHostUpserted { .. }
            | SessionEvent::WorkspaceHostRemoved { .. } => EventScope::Workspace,
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
            SessionEvent::TurnClaimed { trigger_id, device_id } => {
                // First claim in canonical fold order wins; later claims for the
                // same trigger are ignored. Deterministic across devices.
                self.turn_claims.entry(*trigger_id).or_insert(*device_id);
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
            // Whole-list replace — kept for backward-compat (old stored events).
            // New writes use the per-item `AgentUpserted`/`AgentRemoved` deltas
            // below; mixing the two is safe because the canonical fold order
            // resolves them deterministically on every device.
            SessionEvent::AgentRosterUpdated { agents } => {
                self.workspace_agents = agents.clone();
            }
            // Per-item agent delta — upsert by id (mirrors `ChannelCreated`).
            SessionEvent::AgentUpserted { agent } => {
                if let Some(slot) = self.workspace_agents.iter_mut().find(|a| a.id == agent.id) {
                    *slot = agent.clone();
                } else {
                    self.workspace_agents.push(agent.clone());
                }
            }
            SessionEvent::AgentRemoved { agent_id } => {
                self.workspace_agents.retain(|a| &a.id != agent_id);
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
            SessionEvent::ProposalVoteCast { proposal_id, approval } => {
                if let Some(p) = self.proposals.iter_mut().find(|p| p.id == *proposal_id) {
                    // Per-actor LWW + quorum recompute; folded in canonical order
                    // so the latest vote per actor wins on every device.
                    p.cast_vote(approval.clone());
                }
            }
            SessionEvent::ProposalDismissed { proposal_id, dismissed } => {
                if let Some(p) = self.proposals.iter_mut().find(|p| p.id == *proposal_id) {
                    p.dismissed = *dismissed;
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
            // Channels — folded on the workspace-config log (upsert/patch by id).
            SessionEvent::ChannelCreated { channel } => {
                if let Some(slot) = self.channels.iter_mut().find(|c| c.id == channel.id) {
                    *slot = channel.clone();
                } else {
                    self.channels.push(channel.clone());
                }
            }
            SessionEvent::ChannelRenamed { channel_id, name, purpose } => {
                if let Some(c) = self.channels.iter_mut().find(|c| &c.id == channel_id) {
                    c.name = name.clone();
                    // Field-level delta: `None` leaves the existing purpose intact
                    // (a rename never wipes a concurrently/previously set purpose);
                    // `Some(s)` overwrites, with an empty string being a clear.
                    if let Some(p) = purpose {
                        c.purpose = if p.is_empty() { None } else { Some(p.clone()) };
                    }
                }
            }
            SessionEvent::ChannelReordered { channel_ids } => {
                for (pos, id) in channel_ids.iter().enumerate() {
                    if let Some(c) = self.channels.iter_mut().find(|c| &c.id == id) {
                        c.position = pos as i32;
                    }
                }
            }
            SessionEvent::ChannelArchived { channel_id, archived } => {
                if let Some(c) = self.channels.iter_mut().find(|c| &c.id == channel_id) {
                    c.archived = *archived;
                }
            }
            // Per-chat channel assignment (session-scoped).
            SessionEvent::ChatChannelChanged { channel_id } => {
                self.channel_id = channel_id.clone();
            }
            // Whole-list replace — kept for backward-compat (old stored events).
            SessionEvent::WorkspaceRuntimesUpdated { runtimes } => {
                self.workspace_runtimes = runtimes.clone();
            }
            // Per-item runtime delta — upsert / remove by id.
            SessionEvent::WorkspaceRuntimeUpserted { runtime } => {
                if let Some(slot) = self.workspace_runtimes.iter_mut().find(|r| r.id == runtime.id) {
                    *slot = runtime.clone();
                } else {
                    self.workspace_runtimes.push(runtime.clone());
                }
            }
            SessionEvent::WorkspaceRuntimeRemoved { runtime_id } => {
                self.workspace_runtimes.retain(|r| &r.id != runtime_id);
            }
            // Whole-list replace — kept for backward-compat (old stored events).
            SessionEvent::WorkspaceHostsUpdated { hosts } => {
                self.workspace_hosts = hosts.clone();
            }
            // Per-item host delta — upsert / remove by id.
            SessionEvent::WorkspaceHostUpserted { host } => {
                if let Some(slot) = self.workspace_hosts.iter_mut().find(|h| h.id == host.id) {
                    *slot = host.clone();
                } else {
                    self.workspace_hosts.push(host.clone());
                }
            }
            SessionEvent::WorkspaceHostRemoved { host_id } => {
                self.workspace_hosts.retain(|h| &h.id != host_id);
            }
            // Trust metadata — inert in the session projection; consumed only by
            // the device-roster builder (see hive-runtime::envelope_verifier).
            SessionEvent::AccountKeyRegistered { .. }
            | SessionEvent::AccountKeyRotated { .. }
            | SessionEvent::DeviceCertificateAdded { .. }
            | SessionEvent::DeviceRevoked { .. } => {}
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
///
/// The state is seeded by the base snapshot (see base selection below), then
/// every later event is folded on top (later snapshots are inert — see
/// [`ChatSession::apply`]). Seeding from the *latest* snapshot instead would drop
/// any concurrent delta that a peer authored before that snapshot but which the
/// snapshot's creator hadn't yet seen — a real divergence/loss under multi-device
/// compaction. Seeding from the earliest (valid) snapshot replays every delta, so
/// nothing is dropped and all devices converge on the identical set. (A device
/// that joined mid-stream seeds from the recent snapshot it was handed — that
/// snapshot is simply its earliest.) Returns `None` if the set contains no
/// snapshot.
///
/// ## Seed trust root (security residual B2)
///
/// `lamport` is an attacker-settable envelope field and `SessionSnapshot` is a
/// full-state overwrite, so a malicious keyholder could forge a `lamport: 0`
/// snapshot that sorts *before* the genuine creation seed and rewrite the roster
/// for every peer (workspace takeover). The base is therefore chosen not as the
/// bare earliest snapshot but as the earliest **self-consistent creation seed**
/// (see [`is_self_consistent_seed`]): one authored by the very actor it names as
/// `creator_actor_id`, who is the *sole* Owner it grants. A forged snapshot that
/// keeps the victim's roster while elevating the attacker, or that names a creator
/// it was not signed by, fails this test and is skipped as a base candidate — the
/// real creation seed (always self-consistent, per `ChatService::create_chat` /
/// `ensure_workspace_config`) wins, and the forgery, sorting before it, is dropped
/// entirely.
///
/// If the set contains no self-consistent creation seed — a joiner holding only a
/// compaction tail (authored by a non-creator Owner, multi-owner roster), or a
/// legacy unsigned/unstamped seed — the base falls back to the earliest snapshot
/// of any kind, so legitimate compaction, joins, and legacy logs still project.
/// The predicate is a deterministic function of each envelope, so every device
/// reaches the identical base choice ⇒ convergence is preserved.
///
/// ## B2 residual closed: the out-of-band founder pin
///
/// A fully self-consistent seed a keyholder mints *as themselves* (`creator =
/// self`, self as the lone Owner, `lamport: 0`) is indistinguishable *in-band*
/// from a genuine founding: it satisfies [`is_self_consistent_seed`] and, sorting
/// first, would displace the real seed as base once it syncs (a fork/eviction
/// takeover). Closing it requires an out-of-band anchor — **who legitimately
/// founded this workspace** — carried by the `hivews1:` invite (see
/// `WorkspaceConn::founder_actor_id` in `app`) and threaded into projection via
/// [`project_with_founder`].
///
/// When a `trusted_founder` is pinned, the base MUST be a self-consistent seed
/// *authored by and naming that founder* as `creator_actor_id`. A forged
/// self-consistent seed from anyone else is rejected as base **even if its lamport
/// is lower**, so the real founder's seed wins regardless of a competing lamport-0
/// forgery — and the forgery, sorting before the chosen base, is dropped entirely.
/// The pin is a fixed string identical on every device (it rode the same invite),
/// so base selection stays a deterministic function of the event set ⇒ convergence
/// holds. [`project`] (no pin) keeps the pre-existing earliest-self-consistent-seed
/// behavior for workspaces created locally, joined without a founder-bearing
/// invite, or on legacy logs.
///
/// Residual: a peer that holds a forged seed but has **not yet synced** the real
/// founder's seed has no founder-matching base to prefer, so it falls back to the
/// earliest self-consistent seed (possibly the forgery) until the genuine seed
/// arrives — at which point the pin makes the founder's seed win. Likewise a
/// joiner who never received an invite carrying `founder_actor_id` (`None` pin)
/// gets only the in-band self-consistency guarantee.
///
/// Cost note: this replays from the base snapshot rather than the newest, trading
/// a bounded-load optimization for correctness. Causal-frontier snapshots (a
/// per-source watermark that lets a newer snapshot safely subsume deltas) are the
/// future optimization; correctness comes first.
pub fn project(envelopes: &[SessionEventEnvelope]) -> Option<ChatSession> {
    project_with_founder(envelopes, None)
}

/// [`project`], but pinned to a `trusted_founder` (an out-of-band account id from
/// the workspace invite) — the B2 residual fix. See [`project`] for the base
/// selection rule and convergence argument. `None` is exactly [`project`].
pub fn project_with_founder(
    envelopes: &[SessionEventEnvelope],
    trusted_founder: Option<&str>,
) -> Option<ChatSession> {
    let mut ordered: Vec<&SessionEventEnvelope> = envelopes.iter().collect();
    ordered.sort_by_key(|e| e.canonical_key());

    // Base selection, in priority order — each a deterministic function of the
    // event set (+ the fixed founder string), so every device agrees on the base:
    //   1. With a pin: the earliest self-consistent seed authored by & naming the
    //      trusted founder. A lower-lamport forgery from anyone else loses here.
    //   2. The earliest self-consistent creation seed (the no-pin B2 behavior; and
    //      the pre-sync fallback when the founder's own seed hasn't arrived yet).
    //   3. The earliest snapshot of any kind (compaction tail / legacy).
    let base = trusted_founder
        .and_then(|founder| ordered.iter().position(|e| is_founder_seed(e, founder)))
        .or_else(|| ordered.iter().position(|e| is_self_consistent_seed(e)))
        .or_else(|| {
            ordered
                .iter()
                .position(|e| matches!(e.payload, SessionEvent::SessionSnapshot { .. }))
        })?;
    let mut session = match &ordered[base].payload {
        SessionEvent::SessionSnapshot { session } => (**session).clone(),
        _ => unreachable!("position matched a snapshot"),
    };
    session.updated_at = ordered[base].timestamp;
    for env in &ordered[base + 1..] {
        if let SessionEvent::SessionSnapshot { session: snap } = &env.payload {
            // A non-seed snapshot is a full-state overwrite folded mid-stream (a
            // compaction checkpoint). Gate it like governance: its author must be
            // an Owner and it must not re-found the workspace or elevate anyone
            // (see authorization::nonseed_snapshot_authorized). `apply` folds a
            // non-seed snapshot as a no-op today, so this is defense-in-depth for
            // if compaction ever gains subsuming semantics — but it is a pure,
            // deterministic function of the fold-so-far, so it stays convergent.
            let author = env.actor_stamp.as_ref().map(|s| s.actor.id.as_str()).unwrap_or("");
            if !crate::authorization::nonseed_snapshot_authorized(snap, author, &session) {
                continue;
            }
        } else if crate::authorization::requires_projection_authz(&env.payload) {
            // Ingest-time authorization (spec: defense against a malicious member).
            // A governance/config event is dropped unless its author held the role
            // in the roster projected *so far*. Deterministic over the canonical
            // fold ⇒ every device drops the same events ⇒ convergence preserved.
            let author = env.actor_stamp.as_ref().map(|s| s.actor.id.as_str()).unwrap_or("");
            let role = session.role_of(author);
            if !crate::authorization::evaluate(&env.payload, author, role, &session).allowed {
                continue;
            }
        }
        session.apply(&env.payload);
        session.updated_at = env.timestamp;
    }
    Some(session)
}

/// Whether `env` is a **self-consistent creation seed** — the only shape trusted
/// as a projection base without an out-of-band founder pin (security residual B2;
/// see [`project`]). True iff the envelope is a `SessionSnapshot` that:
///   * carries an `actor_stamp` (unstamped/legacy seeds take the fallback path —
///     they cannot be wire-injected, as the envelope verifier rejects unsigned
///     events), AND
///   * is authored by the very actor it names as `creator_actor_id` (non-empty) —
///     you may only found a workspace *as yourself*; a snapshot claiming
///     `creator = X` but signed by `Y` is rejected, AND
///   * grants Owner to that creator and to **no other** member — a seed may not
///     silently hand Owner to a different actor (a forged takeover that preserves
///     the victim's roster while elevating the attacker, or vice-versa).
///
/// A genuine creation seed (`create_chat` / `ensure_workspace_config`) always
/// satisfies this: it is stamped by the creator, names them as `creator_actor_id`,
/// and seeds them as the lone Owner. A legitimate *compaction* snapshot does not
/// (author ≠ creator, multi-owner roster) — which is why it is never chosen over
/// the real seed on a full-history device, and only used as base via the fallback
/// when it is all a joiner holds.
fn is_self_consistent_seed(env: &SessionEventEnvelope) -> bool {
    let SessionEvent::SessionSnapshot { session } = &env.payload else {
        return false;
    };
    let Some(stamp) = env.actor_stamp.as_ref() else {
        return false;
    };
    let author = stamp.actor.id.as_str();
    let creator = session.creator_actor_id.as_str();
    if author.is_empty() || creator.is_empty() || author != creator {
        return false;
    }
    let creator_is_owner = session
        .members
        .iter()
        .any(|m| m.id == creator && m.role == WorkspaceRole::Owner);
    let has_other_owner = session
        .members
        .iter()
        .any(|m| m.id != creator && m.role == WorkspaceRole::Owner);
    creator_is_owner && !has_other_owner
}

/// Whether `env` is a self-consistent creation seed founded by `founder` — the
/// base a projection accepts when the true founder is pinned out of band (B2
/// residual; see [`project_with_founder`]). It is exactly [`is_self_consistent_seed`]
/// plus `creator_actor_id == founder`; because self-consistency already requires
/// `author == creator`, this also means the seed was *authored by* the founder, so
/// no one but the founder can supply a base once the pin is set.
fn is_founder_seed(env: &SessionEventEnvelope, founder: &str) -> bool {
    if founder.is_empty() || !is_self_consistent_seed(env) {
        return false;
    }
    match &env.payload {
        SessionEvent::SessionSnapshot { session } => session.creator_actor_id == founder,
        _ => false,
    }
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

    fn owner_and_viewer_base() -> ChatSession {
        let mut b = base_session();
        b.members.push(member("owner", "owner", WorkspaceRole::Owner));
        b.members.push(member("mallory", "mallory", WorkspaceRole::Viewer));
        b
    }

    #[test]
    fn ingest_authz_rejects_forged_governance_from_a_viewer() {
        // The B1 takeover attack: a malicious member (Viewer) signs governance
        // events AS THEMSELVES (so they pass the envelope verifier) and syncs
        // them. Projection must drop them — the author lacked the role.
        let base = owner_and_viewer_base();
        let (sid, wid) = (base.id, base.workspace_id);
        let forged = vec![
            snapshot_env(base, 1),
            // Self-promotion Viewer → Owner (validly signed by mallory).
            by("mallory", env(sid, wid, 2, SessionEvent::MemberRoleChanged {
                change: MemberRoleChange {
                    member_id: "mallory".into(),
                    old_role: WorkspaceRole::Viewer,
                    new_role: WorkspaceRole::Owner,
                },
            })),
            // Eviction of the real owner (forged MemberRemoved → also a trust DoS).
            by("mallory", env(sid, wid, 3, SessionEvent::MemberRemoved { member_id: "owner".into() })),
        ];
        let s = project(&forged).unwrap();
        assert_eq!(s.role_of("mallory"), WorkspaceRole::Viewer, "forged self-promotion must be dropped");
        assert!(s.members.iter().any(|m| m.id == "owner"), "forged owner-removal must be dropped");

        // Control: the SAME role change, authored by the owner, IS applied.
        let legit_base = owner_and_viewer_base();
        let (sid2, wid2) = (legit_base.id, legit_base.workspace_id);
        let legit = vec![
            snapshot_env(legit_base, 1),
            by("owner", env(sid2, wid2, 2, SessionEvent::MemberRoleChanged {
                change: MemberRoleChange {
                    member_id: "mallory".into(),
                    old_role: WorkspaceRole::Viewer,
                    new_role: WorkspaceRole::Contributor,
                },
            })),
        ];
        assert_eq!(
            project(&legit).unwrap().role_of("mallory"),
            WorkspaceRole::Contributor,
            "an owner-authored role change is still applied"
        );
    }

    #[test]
    fn member_add_remove_and_role_change() {
        let mut base = base_session();
        base.members.push(member("owner", "owner", WorkspaceRole::Owner));
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
        // Governance events must be authored by a privileged member (the owner)
        // to pass the ingest-time authorization gate in `project`.
        let envs = vec![
            snapshot_env(base, 1),
            by(
                "owner",
                SessionEventEnvelope::new(
                    sid,
                    wid,
                    2,
                    SessionEvent::MemberAdded {
                        member: member.clone(),
                    },
                ),
            ),
            by(
                "owner",
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
            ),
        ];
        let s = project(&envs).unwrap();
        // owner (seeded) + m1; m1 was promoted to Admin.
        assert_eq!(s.members.len(), 2);
        assert_eq!(s.role_of("m1"), WorkspaceRole::Admin);

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

    /// Attribute an envelope to `author` (as `append_signed` does in production),
    /// so ingest-time authorization can evaluate the author's role. Governance/
    /// config events need this to pass the projection gate.
    fn by(author: &str, mut e: SessionEventEnvelope) -> SessionEventEnvelope {
        e.actor_stamp = Some(ActorStamp {
            actor: ActorIdentity::new(author, author, ActorKind::Human),
            recorded_at: Timestamp::epoch(),
        });
        e
    }

    /// A base session that already has an Owner (`owner`) — the realistic starting
    /// point for a workspace, so governance events authored by the owner are
    /// authorized during projection.
    fn owned_base() -> ChatSession {
        let mut s = base_session();
        s.members.push(member("owner", "owner", WorkspaceRole::Owner));
        s
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
        let mut base = base_session();
        base.members.push(member("owner", "owner", WorkspaceRole::Owner));
        let (sid, wid) = (base.id, base.workspace_id);
        let msg = ChatMessage::new(MessageRole::Assistant, "Hive", "draft");
        let mid = msg.id;

        // Governance events are authored by the owner (stamped), as in production;
        // content events don't need attribution.
        let events = vec![
            env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) }),
            env(sid, wid, 2, SessionEvent::MessageAppended { message: msg }),
            by("owner", env(sid, wid, 3, SessionEvent::MemberAdded { member: member("m1", "u1", WorkspaceRole::Contributor) })),
            by("owner", env(sid, wid, 4, SessionEvent::MemberAdded { member: member("m2", "u2", WorkspaceRole::Contributor) })),
            env(sid, wid, 5, SessionEvent::SessionTitleChanged { title: "Foo".into() }),
            env(sid, wid, 6, SessionEvent::MessageReactionAdded { message_id: mid, reaction: reaction("u1", "👍") }),
            by("owner", env(sid, wid, 7, SessionEvent::MemberRoleChanged { change: MemberRoleChange { member_id: "m1".into(), old_role: WorkspaceRole::Contributor, new_role: WorkspaceRole::Admin } })),
            by("owner", env(sid, wid, 8, SessionEvent::MemberRemoved { member_id: "m2".into() })),
            env(sid, wid, 9, SessionEvent::SessionTitleChanged { title: "Bar".into() }),
            env(sid, wid, 10, SessionEvent::MessageReactionRemoved { message_id: mid, actor_id: "u1".into(), emoji: "👍".into() }),
            env(sid, wid, 11, SessionEvent::MessageReactionAdded { message_id: mid, reaction: reaction("u2", "🎉") }),
            env(sid, wid, 12, SessionEvent::MessageCompleted { message_id: mid, body: "final".into() }),
        ];

        let expected = project(&events).expect("session");
        // Correct deterministic resolution (not just self-consistency):
        assert_eq!(expected.title, "Bar", "latest title wins by canonical order");
        // owner (seeded) + m1; m2 was added then removed.
        assert_eq!(expected.members.len(), 2, "m2 removed, owner + m1 remain");
        assert!(expected.members.iter().any(|m| m.id == "owner"));
        assert!(!expected.members.iter().any(|m| m.id == "m2"), "m2 removed");
        assert_eq!(expected.role_of("m1"), WorkspaceRole::Admin, "role change applied");
        assert_eq!(expected.messages[0].body, "final");
        assert_eq!(expected.messages[0].reactions.len(), 1, "no resurrection: 👍 stays removed");
        assert_eq!(expected.messages[0].reactions[0].actor_id, "u2");

        // Property: every permutation projects to identical state.
        for seed in 0..500u64 {
            let permuted = shuffled(events.clone(), seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            assert_eq!(project(&permuted).expect("session"), expected, "divergence at seed {seed}");
        }
    }

    /// Compaction safety: a later snapshot that didn't incorporate a peer's
    /// concurrent delta must not cause that delta to be dropped. Seeding from the
    /// earliest snapshot replays every delta, so nothing is lost.
    #[test]
    fn compaction_never_drops_a_concurrent_delta() {
        let mut base = base_session();
        base.members.push(member("owner", "owner", WorkspaceRole::Owner));
        let (sid, wid) = (base.id, base.workspace_id);

        // A device compacted after seeing m1 but BEFORE a concurrent m2 from a peer.
        let mut compacted = base_session();
        compacted.members.push(member("owner", "owner", WorkspaceRole::Owner));
        compacted.members.push(member("m1", "u1", WorkspaceRole::Contributor));

        let events = vec![
            env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) }),
            by("owner", env(sid, wid, 2, SessionEvent::MemberAdded { member: member("m1", "u1", WorkspaceRole::Contributor) })),
            // Concurrent — authored by a peer, canonically before the snapshot below.
            by("owner", env(sid, wid, 3, SessionEvent::MemberAdded { member: member("m2", "u2", WorkspaceRole::Contributor) })),
            // A later snapshot whose captured state is missing m2.
            env(sid, wid, 5, SessionEvent::SessionSnapshot { session: Box::new(compacted) }),
        ];

        let s = project(&events).expect("session");
        let ids: Vec<_> = s.members.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"m1"), "m1 present");
        assert!(ids.contains(&"m2"), "concurrent m2 must NOT be dropped by the later snapshot");

        // And it still converges under every delivery order.
        for seed in 0..200u64 {
            assert_eq!(project(&shuffled(events.clone(), seed | 1)).expect("session"), s, "divergence at seed {seed}");
        }
    }

    /// The fold applies envelopes in canonical `(lamport, event_id)` order, so
    /// the first `TurnClaimed` per trigger wins on every device (later claims are
    /// ignored). This is what stops an account's two devices double-answering.
    #[test]
    fn turn_claim_first_in_fold_order_wins_and_is_idempotent() {
        let mut s = base_session();
        let trigger = Uuid::new_v4();
        let device_a = Uuid::new_v4();
        let device_b = Uuid::new_v4();
        s.apply(&SessionEvent::TurnClaimed { trigger_id: trigger, device_id: device_a });
        s.apply(&SessionEvent::TurnClaimed { trigger_id: trigger, device_id: device_b });
        assert_eq!(s.turn_claims.get(&trigger), Some(&device_a), "first claim wins");
        // A different trigger claims independently.
        let t2 = Uuid::new_v4();
        s.apply(&SessionEvent::TurnClaimed { trigger_id: t2, device_id: device_b });
        assert_eq!(s.turn_claims.get(&t2), Some(&device_b));
    }

    /// A plain rename (`purpose: None`) must leave an existing purpose intact;
    /// `Some(text)` overwrites and `Some("")` is an explicit clear.
    #[test]
    fn channel_rename_preserves_existing_purpose() {
        let mut s = base_session();
        let mut ch = Channel::new("c1", "auth");
        ch.purpose = Some("login flows".into());
        s.channels.push(ch);

        // Rename only — purpose must survive.
        s.apply(&SessionEvent::ChannelRenamed {
            channel_id: "c1".into(),
            name: "authentication".into(),
            purpose: None,
        });
        assert_eq!(s.channels[0].name, "authentication");
        assert_eq!(
            s.channels[0].purpose.as_deref(),
            Some("login flows"),
            "purpose preserved across a plain rename"
        );

        // Some(text) overwrites.
        s.apply(&SessionEvent::ChannelRenamed {
            channel_id: "c1".into(),
            name: "authn".into(),
            purpose: Some("SSO + OAuth".into()),
        });
        assert_eq!(s.channels[0].purpose.as_deref(), Some("SSO + OAuth"));

        // Some("") is an explicit clear.
        s.apply(&SessionEvent::ChannelRenamed {
            channel_id: "c1".into(),
            name: "authn".into(),
            purpose: Some(String::new()),
        });
        assert_eq!(s.channels[0].purpose, None, "empty string clears the purpose");
    }

    /// A concurrent purpose-set and rename on the same channel must not clobber
    /// each other: the rename (purpose `None`) preserves the set purpose, and the
    /// result converges under every delivery order.
    #[test]
    fn concurrent_channel_rename_and_purpose_set_converge() {
        let base = owned_base();
        let (sid, wid) = (base.id, base.workspace_id);
        let events = vec![
            env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) }),
            by("owner", env(sid, wid, 2, SessionEvent::ChannelCreated { channel: Channel::new("c1", "auth") })),
            // Set the purpose (carries the current name).
            by("owner", env(sid, wid, 3, SessionEvent::ChannelRenamed {
                channel_id: "c1".into(),
                name: "auth".into(),
                purpose: Some("login flows".into()),
            })),
            // A concurrent rename that only changes the name (purpose None).
            by("owner", env(sid, wid, 4, SessionEvent::ChannelRenamed {
                channel_id: "c1".into(),
                name: "authentication".into(),
                purpose: None,
            })),
        ];

        let expected = project(&events).expect("session");
        let ch = expected.channels.iter().find(|c| c.id == "c1").unwrap();
        assert_eq!(ch.name, "authentication", "latest name wins by canonical order");
        assert_eq!(
            ch.purpose.as_deref(),
            Some("login flows"),
            "rename did not clobber the concurrently-set purpose"
        );

        for seed in 0..300u64 {
            let permuted = shuffled(events.clone(), seed | 1);
            assert_eq!(project(&permuted).expect("session"), expected, "divergence at seed {seed}");
        }
    }

    /// Concurrent votes from different actors must all survive (the old
    /// whole-proposal upsert dropped one of two), and the outcome must converge.
    #[test]
    fn concurrent_proposal_votes_all_survive_and_converge() {
        use crate::proposals::{ActionProposal, ProposalApproval, ProposalKind, ProposalStatus};

        let base = base_session();
        let (sid, wid) = (base.id, base.workspace_id);
        let mut proposal = ActionProposal::new("Ship it", ProposalKind::Decision, "");
        proposal.required_approvals = 2;
        let pid = proposal.id;
        let vote = |actor: &str, approved: bool| ProposalApproval {
            actor_id: actor.into(),
            role: WorkspaceRole::Contributor,
            approved,
            created_at: Timestamp::epoch(),
        };

        let events = vec![
            env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) }),
            env(sid, wid, 2, SessionEvent::ProposalUpserted { proposal }),
            env(sid, wid, 3, SessionEvent::ProposalVoteCast { proposal_id: pid, approval: vote("u1", true) }),
            env(sid, wid, 4, SessionEvent::ProposalVoteCast { proposal_id: pid, approval: vote("u2", true) }),
            env(sid, wid, 5, SessionEvent::ProposalVoteCast { proposal_id: pid, approval: vote("u1", false) }),
        ];

        let expected = project(&events).expect("session");
        let p = &expected.proposals[0];
        assert_eq!(p.approvals.len(), 2, "both actors' votes preserved (no clobber)");
        assert_eq!(p.status, ProposalStatus::Rejected, "u1's later reject wins per-actor");

        for seed in 0..300u64 {
            let permuted = shuffled(events.clone(), seed | 1);
            assert_eq!(project(&permuted).expect("session"), expected, "vote divergence at seed {seed}");
        }
    }

    /// Dismiss hides a proposal without destroying it, and — being a delta
    /// rather than a whole-proposal upsert — must not clobber a vote that landed
    /// concurrently on another device, in either arrival order.
    #[test]
    fn dismiss_hides_the_proposal_without_losing_it_or_clobbering_votes() {
        use crate::proposals::{ActionProposal, ProposalApproval, ProposalKind, ProposalStatus};

        let base = base_session();
        let (sid, wid) = (base.id, base.workspace_id);
        let mut proposal = ActionProposal::new("Ship it", ProposalKind::Decision, "");
        proposal.required_approvals = 1;
        let pid = proposal.id;

        let events = vec![
            env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) }),
            env(sid, wid, 2, SessionEvent::ProposalUpserted { proposal }),
            env(
                sid,
                wid,
                3,
                SessionEvent::ProposalVoteCast {
                    proposal_id: pid,
                    approval: ProposalApproval {
                        actor_id: "u1".into(),
                        role: WorkspaceRole::Contributor,
                        approved: true,
                        created_at: Timestamp::epoch(),
                    },
                },
            ),
            env(sid, wid, 4, SessionEvent::ProposalDismissed { proposal_id: pid, dismissed: true }),
        ];

        let expected = project(&events).expect("session");
        let p = &expected.proposals[0];
        assert!(p.dismissed, "dismissed");
        assert_eq!(expected.proposals.len(), 1, "hidden, not deleted — the record survives a re-fold");
        assert_eq!(p.status, ProposalStatus::Approved, "the concurrent vote still counts");

        for seed in 0..300u64 {
            let permuted = shuffled(events.clone(), seed | 1);
            assert_eq!(project(&permuted).expect("session"), expected, "divergence at seed {seed}");
        }
    }

    /// Restoring is the same event with `dismissed: false`, so a mistaken
    /// "Clear settled" is recoverable rather than a one-way door.
    #[test]
    fn dismiss_is_reversible() {
        use crate::proposals::{ActionProposal, ProposalKind};

        let base = base_session();
        let (sid, wid) = (base.id, base.workspace_id);
        let proposal = ActionProposal::new("Ship it", ProposalKind::Decision, "");
        let pid = proposal.id;

        let session = project(&[
            env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) }),
            env(sid, wid, 2, SessionEvent::ProposalUpserted { proposal }),
            env(sid, wid, 3, SessionEvent::ProposalDismissed { proposal_id: pid, dismissed: true }),
            env(sid, wid, 4, SessionEvent::ProposalDismissed { proposal_id: pid, dismissed: false }),
        ])
        .expect("session");

        assert!(!session.proposals[0].dismissed, "restored to the inbox");
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

    // ── Per-item config deltas: concurrent-add convergence ─────────────────
    //
    // The data-loss bug these close: with the whole-list `*Updated` events, two
    // admins each editing from their own base emitted the FULL roster. The
    // canonically-later event's list REPLACED the other's, so one admin's added
    // agent/host/runtime silently vanished (convergent but lossy). The per-item
    // `*Upserted` deltas below are additive by id, so both survive in every
    // permutation.

    fn admin_base() -> ChatSession {
        let mut b = base_session();
        b.members.push(member("owner", "owner", WorkspaceRole::Owner));
        b.members.push(member("admin1", "admin1", WorkspaceRole::Admin));
        b.members.push(member("admin2", "admin2", WorkspaceRole::Admin));
        b
    }

    #[test]
    fn concurrent_agent_upserts_both_survive_and_converge() {
        let base = admin_base();
        let (sid, wid) = (base.id, base.workspace_id);
        let a1 = WorkspaceAgent::new("reviewer", "ws-claude");
        let a2 = WorkspaceAgent::new("planner", "ws-claude");
        let (id1, id2) = (a1.id, a2.id);

        // Same lamport (2) → genuinely concurrent, tie-broken by event_id. Two
        // different authors, each adding a different agent from their own base.
        let events = vec![
            env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) }),
            by("admin1", env(sid, wid, 2, SessionEvent::AgentUpserted { agent: a1 })),
            by("admin2", env(sid, wid, 2, SessionEvent::AgentUpserted { agent: a2 })),
        ];
        let expected = project(&events).expect("session");
        assert_eq!(expected.workspace_agents.len(), 2, "both concurrent agent adds survive");
        assert!(expected.workspace_agents.iter().any(|a| a.id == id1));
        assert!(expected.workspace_agents.iter().any(|a| a.id == id2));

        for seed in 0..300u64 {
            assert_eq!(
                project(&shuffled(events.clone(), seed | 1)).expect("s"),
                expected,
                "agent divergence at seed {seed}"
            );
        }
    }

    #[test]
    fn concurrent_host_upserts_both_survive_and_converge() {
        use crate::workspace_host::{HostKind, WorkspaceHost};
        let base = admin_base();
        let (sid, wid) = (base.id, base.workspace_id);
        let mut h1 = WorkspaceHost::new("box1", HostKind::Worker, "one");
        h1.owner_actor_id = "admin1".into();
        let mut h2 = WorkspaceHost::new("box2", HostKind::Worker, "two");
        h2.owner_actor_id = "admin2".into();

        let events = vec![
            env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) }),
            by("admin1", env(sid, wid, 2, SessionEvent::WorkspaceHostUpserted { host: h1 })),
            by("admin2", env(sid, wid, 2, SessionEvent::WorkspaceHostUpserted { host: h2 })),
        ];
        let expected = project(&events).expect("session");
        assert_eq!(expected.workspace_hosts.len(), 2, "both concurrent host adds survive");

        for seed in 0..300u64 {
            assert_eq!(
                project(&shuffled(events.clone(), seed | 1)).expect("s"),
                expected,
                "host divergence at seed {seed}"
            );
        }
    }

    #[test]
    fn concurrent_runtime_upserts_both_survive_and_converge() {
        use crate::runtime::ModelProviderKind;
        use crate::workspace_runtime::WorkspaceRuntime;
        let base = admin_base();
        let (sid, wid) = (base.id, base.workspace_id);
        let r1 = WorkspaceRuntime::new("rt1", "One", ModelProviderKind::Anthropic, "m");
        let r2 = WorkspaceRuntime::new("rt2", "Two", ModelProviderKind::Anthropic, "m");

        let events = vec![
            env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) }),
            by("admin1", env(sid, wid, 2, SessionEvent::WorkspaceRuntimeUpserted { runtime: r1 })),
            by("admin2", env(sid, wid, 2, SessionEvent::WorkspaceRuntimeUpserted { runtime: r2 })),
        ];
        let expected = project(&events).expect("session");
        assert_eq!(expected.workspace_runtimes.len(), 2, "both concurrent runtime adds survive");

        for seed in 0..300u64 {
            assert_eq!(
                project(&shuffled(events.clone(), seed | 1)).expect("s"),
                expected,
                "runtime divergence at seed {seed}"
            );
        }
    }

    #[test]
    fn config_delta_upsert_in_place_and_remove_by_id() {
        let base = admin_base();
        let (sid, wid) = (base.id, base.workspace_id);
        let mut a1 = WorkspaceAgent::new("reviewer", "ws-claude");
        let id1 = a1.id;
        let a2 = WorkspaceAgent::new("planner", "ws-claude");
        let id2 = a2.id;
        // A second upsert of the SAME id (renamed) updates in place — no dup.
        let mut a1_renamed = a1.clone();
        a1_renamed.name = "critic".into();
        a1.name = "reviewer".into();

        let events = vec![
            env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) }),
            by("admin1", env(sid, wid, 2, SessionEvent::AgentUpserted { agent: a1 })),
            by("admin1", env(sid, wid, 3, SessionEvent::AgentUpserted { agent: a2 })),
            by("admin1", env(sid, wid, 4, SessionEvent::AgentUpserted { agent: a1_renamed })),
            by("admin1", env(sid, wid, 5, SessionEvent::AgentRemoved { agent_id: id2 })),
        ];
        let s = project(&events).expect("session");
        assert_eq!(s.workspace_agents.len(), 1, "renamed upsert is in place; id2 removed");
        assert_eq!(s.workspace_agents[0].id, id1);
        assert_eq!(s.workspace_agents[0].name, "critic", "later upsert wins by canonical order");
    }

    #[test]
    fn legacy_whole_list_config_events_still_project() {
        // Backward-compat: events written by an OLD client used the whole-list
        // `*Updated` variants. They must still fold (canonical order handles
        // mixing old whole-list and new per-item events on every device).
        use crate::workspace_host::{HostKind, WorkspaceHost};
        use crate::runtime::ModelProviderKind;
        use crate::workspace_runtime::WorkspaceRuntime;
        let base = admin_base();
        let (sid, wid) = (base.id, base.workspace_id);
        let agents = vec![
            WorkspaceAgent::new("a", "ws-claude"),
            WorkspaceAgent::new("b", "ws-claude"),
        ];
        let hosts = vec![WorkspaceHost::new("box1", HostKind::Worker, "one")];
        let runtimes = vec![WorkspaceRuntime::new("rt1", "One", ModelProviderKind::Anthropic, "m")];

        let events = vec![
            env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(base) }),
            by("admin1", env(sid, wid, 2, SessionEvent::AgentRosterUpdated { agents })),
            by("admin1", env(sid, wid, 3, SessionEvent::WorkspaceHostsUpdated { hosts })),
            by("admin1", env(sid, wid, 4, SessionEvent::WorkspaceRuntimesUpdated { runtimes })),
        ];
        let s = project(&events).expect("session");
        assert_eq!(s.workspace_agents.len(), 2, "legacy agent roster projects");
        assert_eq!(s.workspace_hosts.len(), 1, "legacy host list projects");
        assert_eq!(s.workspace_runtimes.len(), 1, "legacy runtime list projects");
    }

    #[test]
    fn forged_agent_upsert_from_a_viewer_is_dropped_owners_is_applied() {
        // The projection gate: `AgentUpserted` is governance/config (Contributor
        // floor). A Viewer's forged one is dropped during the fold; an owner's is
        // applied — mirroring the whole-list `AgentRosterUpdated` behavior.
        let base = owner_and_viewer_base();
        let (sid, wid) = (base.id, base.workspace_id);
        let forged = WorkspaceAgent::new("evil", "ws-claude");
        let events = vec![
            snapshot_env(base, 1),
            by("mallory", env(sid, wid, 2, SessionEvent::AgentUpserted { agent: forged })),
        ];
        assert!(project(&events).unwrap().workspace_agents.is_empty(), "Viewer's forged upsert dropped");

        let legit_base = owner_and_viewer_base();
        let (sid2, wid2) = (legit_base.id, legit_base.workspace_id);
        let good = WorkspaceAgent::new("reviewer", "ws-claude");
        let legit = vec![
            snapshot_env(legit_base, 1),
            by("owner", env(sid2, wid2, 2, SessionEvent::AgentUpserted { agent: good })),
        ];
        assert_eq!(project(&legit).unwrap().workspace_agents.len(), 1, "owner's upsert applied");
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

    // ── B2: forged-seed takeover ────────────────────────────────────────────
    //
    // A `SessionSnapshot` is a full-state overwrite, and `lamport` is an
    // attacker-settable envelope field. A malicious keyholder can craft a
    // `lamport: 0` snapshot that sorts before the genuine creation seed and
    // rewrites the roster for every peer. The base is now the earliest
    // *self-consistent creation seed*, so these forgeries lose to the real seed.

    /// A self-consistent creation seed: `creator` is the sole Owner, on the given
    /// session/workspace ids (so several seeds can be placed in one log).
    fn seed_session(creator: &str, sid: Uuid, wid: Uuid) -> ChatSession {
        let mut s = base_session();
        s.id = sid;
        s.workspace_id = wid;
        s.creator_actor_id = creator.into();
        s.members.push(member(creator, creator, WorkspaceRole::Owner));
        s
    }

    #[test]
    fn forged_low_lamport_seed_cannot_take_over() {
        let sid = Uuid::new_v4();
        let wid = Uuid::nil();
        // Legit creation seed: Alice founds as herself (sole Owner), lamport 5.
        let alice = seed_session("alice", sid, wid);
        // Forged takeover: Mallory keeps Alice's Owner roster but adds herself as a
        // SECOND Owner and names herself creator, at lamport 0 so it sorts FIRST.
        let mut forged = base_session();
        forged.id = sid;
        forged.workspace_id = wid;
        forged.creator_actor_id = "mallory".into();
        forged.members.push(member("alice", "alice", WorkspaceRole::Owner));
        forged.members.push(member("mallory", "mallory", WorkspaceRole::Owner));

        let events = vec![
            by("mallory", env(sid, wid, 0, SessionEvent::SessionSnapshot { session: Box::new(forged) })),
            by("alice", env(sid, wid, 5, SessionEvent::SessionSnapshot { session: Box::new(alice) })),
        ];
        let s = project(&events).expect("session");
        assert_eq!(s.creator_actor_id, "alice", "forged seed must not rewrite the creator");
        assert_eq!(s.role_of("alice"), WorkspaceRole::Owner, "legit creator remains Owner");
        assert_ne!(s.role_of("mallory"), WorkspaceRole::Owner, "forged seed did not make Mallory Owner");
        assert!(!s.members.iter().any(|m| m.id == "mallory"), "Mallory not injected into the roster");
    }

    #[test]
    fn forged_seed_claiming_a_foreign_creator_is_rejected_as_base() {
        let sid = Uuid::new_v4();
        let wid = Uuid::nil();
        let alice = seed_session("alice", sid, wid);
        // Mallory forges a seed that NAMES Alice as creator (to mimic the real
        // seed) but signs it herself, at lamport 0.
        let mut forged = base_session();
        forged.id = sid;
        forged.workspace_id = wid;
        forged.creator_actor_id = "alice".into();
        forged.members.push(member("mallory", "mallory", WorkspaceRole::Owner));
        let events = vec![
            by("mallory", env(sid, wid, 0, SessionEvent::SessionSnapshot { session: Box::new(forged) })),
            by("alice", env(sid, wid, 5, SessionEvent::SessionSnapshot { session: Box::new(alice) })),
        ];
        let s = project(&events).expect("session");
        assert_eq!(s.role_of("alice"), WorkspaceRole::Owner, "author≠creator seed rejected → Alice wins");
        assert_ne!(s.role_of("mallory"), WorkspaceRole::Owner);
    }

    #[test]
    fn legit_single_creator_seed_projects() {
        // The self-consistency invariant holds for a genuine `create_chat` seed.
        let sid = Uuid::new_v4();
        let wid = Uuid::nil();
        let alice = seed_session("alice", sid, wid);
        let events =
            vec![by("alice", env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(alice) }))];
        let s = project(&events).expect("session");
        assert_eq!(s.role_of("alice"), WorkspaceRole::Owner);
        assert_eq!(s.creator_actor_id, "alice");
    }

    #[test]
    fn legit_compaction_snapshot_still_projects_as_a_joiner_tail() {
        // A compaction checkpoint authored by an Admin (not the creator), with a
        // multi-member roster where the original creator is still Owner — what a
        // joiner is handed as its earliest event. Not a self-consistent creation
        // seed, so it becomes base via the fallback and MUST still project.
        let sid = Uuid::new_v4();
        let wid = Uuid::nil();
        let mut compacted = base_session();
        compacted.id = sid;
        compacted.workspace_id = wid;
        compacted.creator_actor_id = "alice".into();
        compacted.members.push(member("alice", "alice", WorkspaceRole::Owner));
        compacted.members.push(member("bob", "bob", WorkspaceRole::Admin));
        let events =
            vec![by("bob", env(sid, wid, 9, SessionEvent::SessionSnapshot { session: Box::new(compacted) }))];
        let s = project(&events).expect("compaction snapshot must still project");
        assert_eq!(s.role_of("alice"), WorkspaceRole::Owner);
        assert_eq!(s.role_of("bob"), WorkspaceRole::Admin);
    }

    #[test]
    fn full_history_prefers_creation_seed_over_later_compaction() {
        // Creation seed + a later compaction snapshot: the self-consistent seed is
        // the base; the compaction snapshot folds inert; the roster is preserved.
        let sid = Uuid::new_v4();
        let wid = Uuid::nil();
        let alice = seed_session("alice", sid, wid);
        let bob = member("bob", "bob", WorkspaceRole::Admin);
        let mut compacted = seed_session("alice", sid, wid);
        compacted.members.push(bob.clone());
        let events = vec![
            by("alice", env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(alice) })),
            by("alice", env(sid, wid, 2, SessionEvent::MemberAdded { member: bob })),
            by("bob", env(sid, wid, 3, SessionEvent::SessionSnapshot { session: Box::new(compacted) })),
        ];
        let s = project(&events).expect("session");
        assert_eq!(s.creator_actor_id, "alice");
        assert_eq!(s.role_of("alice"), WorkspaceRole::Owner);
        assert_eq!(s.role_of("bob"), WorkspaceRole::Admin, "bob added by alice survives");
    }

    #[test]
    fn seed_selection_converges_across_permutations() {
        // The seed-trust check is a deterministic function of each envelope, so the
        // forged-seed defense is order-independent — every delivery order agrees.
        let sid = Uuid::new_v4();
        let wid = Uuid::nil();
        let alice = seed_session("alice", sid, wid);
        let mut forged = base_session();
        forged.id = sid;
        forged.workspace_id = wid;
        forged.creator_actor_id = "mallory".into();
        forged.members.push(member("alice", "alice", WorkspaceRole::Owner));
        forged.members.push(member("mallory", "mallory", WorkspaceRole::Owner));
        let msg = ChatMessage::new(MessageRole::User, "alice", "hi");
        let events = vec![
            by("mallory", env(sid, wid, 0, SessionEvent::SessionSnapshot { session: Box::new(forged) })),
            by("alice", env(sid, wid, 5, SessionEvent::SessionSnapshot { session: Box::new(alice) })),
            env(sid, wid, 6, SessionEvent::MessageAppended { message: msg }),
        ];
        let expected = project(&events).expect("session");
        assert_eq!(expected.role_of("alice"), WorkspaceRole::Owner);
        assert_ne!(expected.role_of("mallory"), WorkspaceRole::Owner);
        for seed in 0..500u64 {
            let permuted = shuffled(events.clone(), seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            assert_eq!(project(&permuted).expect("session"), expected, "divergence at seed {seed}");
        }
    }

    // ── B2 residual closed: the out-of-band founder pin ─────────────────────
    //
    // A keyholder can mint a *fully self-consistent* seed AS THEMSELVES (sole
    // Owner, creator = self, lamport 0), which passes `is_self_consistent_seed`
    // and would otherwise displace the real founding seed as base once synced.
    // `project_with_founder` closes it by pinning the true founder out of band
    // (the invite's identity) and accepting only that founder's seed as base.

    #[test]
    fn founder_pin_defeats_self_consistent_forged_seed() {
        let sid = Uuid::new_v4();
        let wid = Uuid::nil();
        // Alice's genuine founding seed, lamport 5.
        let alice = seed_session("alice", sid, wid);
        // Mallory mints a FULLY self-consistent seed as herself (sole Owner,
        // creator = mallory) at lamport 0 so it sorts first — the residual the
        // in-band self-consistency check alone cannot catch.
        let mallory = seed_session("mallory", sid, wid);
        let events = vec![
            by("mallory", env(sid, wid, 0, SessionEvent::SessionSnapshot { session: Box::new(mallory) })),
            by("alice", env(sid, wid, 5, SessionEvent::SessionSnapshot { session: Box::new(alice) })),
        ];

        // Pinned to Alice (from the invite): her seed is the base despite the
        // lower-lamport forgery; Alice stays Owner, Mallory is not injected.
        let s = project_with_founder(&events, Some("alice")).expect("session");
        assert_eq!(s.creator_actor_id, "alice", "founder's seed wins over lower-lamport forgery");
        assert_eq!(s.role_of("alice"), WorkspaceRole::Owner, "founder stays Owner");
        assert_ne!(s.role_of("mallory"), WorkspaceRole::Owner, "forger did not become Owner");
        assert!(!s.members.iter().any(|m| m.id == "mallory"), "Mallory not injected into the roster");

        // Documented residual: with NO pin, the earliest self-consistent seed still
        // wins — here Mallory's lamport-0 forgery — which is exactly why the pin
        // exists (the pre-existing B2 partial behavior).
        let unpinned = project(&events).expect("session");
        assert_eq!(unpinned.creator_actor_id, "mallory", "no pin ⇒ earliest self-consistent seed wins");
    }

    #[test]
    fn founder_pinned_creation_and_compaction_still_project() {
        // Legitimate creation (founder == creator) + a later compaction snapshot
        // must still project under the pin.
        let sid = Uuid::new_v4();
        let wid = Uuid::nil();
        let alice = seed_session("alice", sid, wid);
        let bob = member("bob", "bob", WorkspaceRole::Admin);
        let mut compacted = seed_session("alice", sid, wid);
        compacted.members.push(bob.clone());
        let events = vec![
            by("alice", env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(alice) })),
            by("alice", env(sid, wid, 2, SessionEvent::MemberAdded { member: bob })),
            by("bob", env(sid, wid, 3, SessionEvent::SessionSnapshot { session: Box::new(compacted) })),
        ];
        let s = project_with_founder(&events, Some("alice")).expect("session");
        assert_eq!(s.creator_actor_id, "alice");
        assert_eq!(s.role_of("alice"), WorkspaceRole::Owner);
        assert_eq!(s.role_of("bob"), WorkspaceRole::Admin, "bob added by the founder survives");
    }

    #[test]
    fn founder_pin_does_not_break_a_nonfounder_created_chat() {
        // A member (not the founder) legitimately creates a chat: its seed names
        // bob as sole-Owner creator. With the workspace founder pinned to alice,
        // there is no alice-seed for THIS chat, so projection deterministically
        // falls back to bob's own self-consistent seed — the pin must not blank out
        // a member-created chat.
        let sid = Uuid::new_v4();
        let wid = Uuid::nil();
        let bob = seed_session("bob", sid, wid);
        let events =
            vec![by("bob", env(sid, wid, 1, SessionEvent::SessionSnapshot { session: Box::new(bob) }))];
        let s = project_with_founder(&events, Some("alice")).expect("member-created chat must project");
        assert_eq!(s.creator_actor_id, "bob");
        assert_eq!(s.role_of("bob"), WorkspaceRole::Owner);
    }

    #[test]
    fn founder_pinned_base_selection_converges_across_permutations() {
        // The founder-pinned base choice is a deterministic function of the event
        // set + the fixed founder string, so it is order-independent.
        let sid = Uuid::new_v4();
        let wid = Uuid::nil();
        let alice = seed_session("alice", sid, wid);
        let mallory = seed_session("mallory", sid, wid);
        let msg = ChatMessage::new(MessageRole::User, "alice", "hi");
        let events = vec![
            by("mallory", env(sid, wid, 0, SessionEvent::SessionSnapshot { session: Box::new(mallory) })),
            by("alice", env(sid, wid, 5, SessionEvent::SessionSnapshot { session: Box::new(alice) })),
            env(sid, wid, 6, SessionEvent::MessageAppended { message: msg }),
        ];
        let expected = project_with_founder(&events, Some("alice")).expect("session");
        assert_eq!(expected.creator_actor_id, "alice");
        assert_ne!(expected.role_of("mallory"), WorkspaceRole::Owner);
        for seed in 0..500u64 {
            let permuted = shuffled(events.clone(), seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            assert_eq!(
                project_with_founder(&permuted, Some("alice")).expect("session"),
                expected,
                "divergence at seed {seed}"
            );
        }
    }
}
