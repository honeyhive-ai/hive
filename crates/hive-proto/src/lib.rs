//! `hive-proto` — the IPC contract shared between the Rust backend
//! (`hive-runtime` / `app`) and the TypeScript frontend (`web/`).
//!
//! Every type derives `serde` (for Tauri command (de)serialization) and
//! `ts-rs::TS`. The `export_bindings` test writes the matching TypeScript into
//! `web/src/bindings/`; CI fails if those files drift from these definitions,
//! so the frontend can never get out of sync with the backend contract.
//!
//! These are presentation DTOs — flat, string-keyed, camelCase — deliberately
//! decoupled from the richer `hive-core` domain types. The backend converts
//! `hive-core` values into these at the IPC boundary.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Backend build/version info — the Phase 0 smoke-test round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub core_version: String,
    pub build_profile: String,
}

/// One emoji reaction on a message.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ReactionDto {
    pub emoji: String,
    pub actor_id: String,
    pub actor_display_name: String,
}

/// One transcript message, flattened for rendering.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ChatMessageDto {
    pub id: String,
    /// "system" | "user" | "assistant" | "agent"
    pub role: String,
    pub author: String,
    pub body: String,
    pub is_streaming: bool,
    /// RFC 3339 timestamp.
    pub created_at: String,
    #[serde(default)]
    pub reactions: Vec<ReactionDto>,
    /// Tool calls this assistant turn made (for inline tool-call cards).
    #[serde(default)]
    pub tool_calls: Vec<ToolCallDto>,
    /// Tool results carried by this (user-role) turn, keyed back to a call id.
    #[serde(default)]
    pub tool_results: Vec<ToolResultDto>,
}

/// A tool invocation an assistant turn made (MCP or built-in).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ToolCallDto {
    pub id: String,
    pub name: String,
    /// JSON-encoded arguments.
    pub input_json: String,
    #[serde(default)]
    pub server_id: Option<String>,
}

/// The result of a tool call, keyed to its `call_id`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ToolResultDto {
    pub call_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

/// One vote on a proposal.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ApprovalDto {
    pub actor_id: String,
    pub role: String,
    pub approved: bool,
}

/// A proposal in the review queue, with computed quorum state.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ProposalDto {
    pub id: String,
    pub title: String,
    pub body: String,
    /// "fileDiff" | "command" | "decision"
    pub kind: String,
    /// "open" | "approved" | "rejected" | "applied"
    pub status: String,
    pub required_approvals: u32,
    pub qualifying_approvals: u32,
    pub quorum_met: bool,
    pub approvals: Vec<ApprovalDto>,
}

/// A chat as listed in the sidebar.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ChatSummaryDto {
    pub id: String,
    pub title: String,
    pub last_activity_at: String,
    pub message_count: u32,
    pub archived: bool,
}

/// One file's uncommitted change, for the Diff canvas.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct GitFileDiffDto {
    pub path: String,
    /// "modified" | "added" | "deleted" | "renamed" | "untracked" | "conflicted"
    pub kind: String,
    /// Unified-diff text (synthesized for untracked files).
    pub patch: String,
    pub added_lines: u32,
    pub removed_lines: u32,
}

/// A workspace agent in the roster (a named participant on a runtime).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkspaceAgentDto {
    pub id: String,
    pub name: String,
    pub runtime_id: String,
    pub role: String,
}

/// A configured runtime surfaced to the desktop shell.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RuntimeSummaryDto {
    pub id: String,
    pub name: String,
    pub label: String,
    pub provider: String,
    pub location: String,
    pub model: String,
    pub endpoint: String,
    pub supports_tools: bool,
    pub supports_embeddings: bool,
    pub is_default: bool,
    pub is_managed: bool,
    /// OpenAI-compatible base URL (pi → local backends). None for providers that
    /// don't use one. Carried so an edit form can round-trip it.
    pub model_base_url: Option<String>,
    /// Sub-provider id for pi-style bridges (e.g. "ollama"). None otherwise.
    pub model_provider_id: Option<String>,
    /// Explicit context-window override (Ollama/custom endpoints). None = inferred.
    pub context_window: Option<u32>,
}

/// Actual context-budget telemetry computed by the backend planner.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ContextTelemetryDto {
    pub session_id: String,
    pub runtime_id: String,
    pub model: String,
    pub context_window_tokens: u32,
    pub reserved_output_tokens: u32,
    pub summary_reserve_tokens: u32,
    pub system_prompt_tokens: u32,
    pub history_budget_tokens: u32,
    pub history_tokens: u32,
    pub message_count: u32,
    pub kept_message_count: u32,
    pub kept_tokens: u32,
    pub overflow_message_count: u32,
    pub overflow_tokens: u32,
    pub skill_count: u32,
    pub skill_tokens: u32,
    pub vault_count: u32,
    /// "none" | "cached" | "incremental" | "fresh"
    pub summary_strategy: String,
}

/// A workspace member with a governance role.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkspaceMemberDto {
    pub id: String,
    /// The member's actor id — used to match against the presence feed (online).
    pub actor_id: String,
    pub display_name: String,
    /// "owner" | "admin" | "contributor" | "viewer"
    pub role: String,
    pub title: String,
    /// Per-workspace join index (1-based). Disambiguates duplicate display
    /// names — surfaced as `Name #N`, matchable as `@Name#N`.
    pub index: u32,
    /// True when this member is the local user. The People list hides "you"
    /// (you're shown in the identity card), so it shows only collaborators.
    pub is_self: bool,
}

/// One selectable workspace in the switcher: the local "My workspace", or a
/// joined relay room. The chat list is scoped to whichever is `active`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkspaceInfoDto {
    pub id: String,
    pub name: String,
    /// "local" | "room"
    pub kind: String,
    pub active: bool,
    /// Optional workspace icon as a `data:` URL (set by an owner/admin); when
    /// present the rail renders it in place of the initials/brand mark.
    pub icon_url: Option<String>,
}

/// A loaded skill (instruction bundle injected into prompts).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct SkillDto {
    pub id: String,
    pub name: String,
    pub instructions: String,
    pub source_url: Option<String>,
}

/// A configured MCP server and whether it's enabled (the inert-until-enabled
/// gate: a disabled server is never launched/connected).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct McpServerDto {
    pub id: String,
    /// "stdio" | "http"
    pub transport: String,
    pub detail: String,
    pub enabled: bool,
    pub is_managed: bool,
}

/// A reference-material vault source mounted in the workspace.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct VaultSourceDto {
    /// "github" | "gitlab" | "https"
    pub kind: String,
    pub label: String,
    pub url: String,
}

/// App + workspace settings surfaced to the Settings pane.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct AppSettingsDto {
    pub display_name: String,
    /// Git email used to attribute commits agents make on this user's behalf.
    pub git_email: String,
    pub device_name: String,
    pub workspace_root: String,
    pub known_workspaces: Vec<String>,
    pub model: String,
    pub git_branch: Option<String>,
    pub git_dirty_count: u32,
}

/// A chat opened in the transcript pane.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ChatSessionDto {
    pub id: String,
    pub title: String,
    pub runtime_id: String,
    pub messages: Vec<ChatMessageDto>,
}

/// A streaming update pushed as a Tauri event while an assistant reply is
/// generated. `phase` is "delta" (append `text`), "completed" (final body in
/// `text`), or "error" (`text` is the message).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ChatStreamEvent {
    pub session_id: String,
    pub message_id: String,
    pub phase: String,
    pub text: String,
}

impl ChatStreamEvent {
    /// The Tauri event name the frontend listens on.
    pub const EVENT: &'static str = "chat://stream";
}

/// One stage of a workflow definition, flattened for the wire: `kind` is
/// "agent" | "gate" and only that kind's optionals are populated.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkflowNodeDto {
    pub id: String,
    pub name: String,
    pub depends_on: Vec<String>,
    /// "agent" | "gate"
    pub kind: String,
    /// Agent stages: workspace agent id, empty ⇒ session primary runtime.
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub prompt_template: Option<String>,
    /// Gate stages.
    #[serde(default)]
    pub gate_title: Option<String>,
    #[serde(default)]
    pub gate_body: Option<String>,
    #[serde(default)]
    pub required_approvals: Option<u32>,
    /// "halt" | "routeTo"
    #[serde(default)]
    pub on_reject: Option<String>,
    #[serde(default)]
    pub reject_target: Option<String>,
    /// Builder-canvas position (px); null ⇒ auto-layout.
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
}

/// A workflow definition (DAG of stages) available in a chat.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkflowDefinitionDto {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub input_label: Option<String>,
    pub nodes: Vec<WorkflowNodeDto>,
}

/// Live state of one stage within a run.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkflowNodeRunDto {
    pub node_id: String,
    pub name: String,
    /// "agent" | "gate"
    pub kind: String,
    /// "pending" | "running" | "awaitingApproval" | "succeeded" | "failed" |
    /// "rejected" | "skipped"
    pub status: String,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub proposal_id: Option<String>,
    pub output_excerpt: String,
    pub attempts: u32,
    pub error: String,
}

/// A workflow run card: frozen definition name + per-stage state.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkflowRunDto {
    pub id: String,
    pub definition_id: String,
    pub definition_name: String,
    pub input: String,
    /// "running" | "awaitingGate" | "completed" | "failed" | "halted" | "canceled"
    pub status: String,
    pub nodes: Vec<WorkflowNodeRunDto>,
    pub started_at: String,
}

/// Pushed as a Tauri event whenever a run's persisted state changes.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkflowRunEvent {
    pub session_id: String,
    pub run_id: String,
    pub status: String,
}

impl WorkflowRunEvent {
    /// The Tauri event name the frontend listens on.
    pub const EVENT: &'static str = "workflow://run";
}

/// One issued relay access token's metadata (never the raw value/hash).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RelayTokenDto {
    pub id: String,
    pub label: String,
    /// RFC-3339 or empty; "" when never used.
    pub last_used: String,
    pub revoked: bool,
}

/// A relay access user + their tokens, for the Team management panel.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RelayUserDto {
    pub id: String,
    pub name: String,
    pub login: String,
    pub disabled: bool,
    pub tokens: Vec<RelayTokenDto>,
}

/// A freshly issued token: the `raw` value is shown ONCE, never retrievable again.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct IssuedRelayTokenDto {
    pub user_id: String,
    pub user_name: String,
    pub raw: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regenerate all TypeScript bindings into `web/src/bindings/`. CI runs this
    /// (filter: `export_bindings`) and fails if the committed files differ.
    #[test]
    fn export_bindings() {
        const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/src/bindings");
        AppInfo::export_all_to(DIR).unwrap();
        ChatMessageDto::export_all_to(DIR).unwrap();
        ToolCallDto::export_all_to(DIR).unwrap();
        ToolResultDto::export_all_to(DIR).unwrap();
        ChatSummaryDto::export_all_to(DIR).unwrap();
        ChatSessionDto::export_all_to(DIR).unwrap();
        ChatStreamEvent::export_all_to(DIR).unwrap();
        GitFileDiffDto::export_all_to(DIR).unwrap();
        AppSettingsDto::export_all_to(DIR).unwrap();
        RuntimeSummaryDto::export_all_to(DIR).unwrap();
        ContextTelemetryDto::export_all_to(DIR).unwrap();
        WorkspaceAgentDto::export_all_to(DIR).unwrap();
        SkillDto::export_all_to(DIR).unwrap();
        McpServerDto::export_all_to(DIR).unwrap();
        ReactionDto::export_all_to(DIR).unwrap();
        ProposalDto::export_all_to(DIR).unwrap();
        ApprovalDto::export_all_to(DIR).unwrap();
        WorkspaceMemberDto::export_all_to(DIR).unwrap();
        WorkspaceInfoDto::export_all_to(DIR).unwrap();
        VaultSourceDto::export_all_to(DIR).unwrap();
        WorkflowNodeDto::export_all_to(DIR).unwrap();
        WorkflowDefinitionDto::export_all_to(DIR).unwrap();
        WorkflowNodeRunDto::export_all_to(DIR).unwrap();
        WorkflowRunDto::export_all_to(DIR).unwrap();
        WorkflowRunEvent::export_all_to(DIR).unwrap();
        RelayTokenDto::export_all_to(DIR).unwrap();
        RelayUserDto::export_all_to(DIR).unwrap();
        IssuedRelayTokenDto::export_all_to(DIR).unwrap();
    }
}
