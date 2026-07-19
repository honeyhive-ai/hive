// Typed wrappers around the Tauri IPC commands/events. Each corresponds to a
// `#[tauri::command]` (or emitted event) in `app`, typed with the ts-rs
// bindings generated from `hive-proto`. This is the single seam the frontend
// uses to reach the Rust backend.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AppInfo } from "@/bindings/AppInfo";
import type { ChatSummaryDto } from "@/bindings/ChatSummaryDto";
import type { ChatSessionDto } from "@/bindings/ChatSessionDto";
import type { ChatStreamEvent } from "@/bindings/ChatStreamEvent";
import type { GitFileDiffDto } from "@/bindings/GitFileDiffDto";
import type { AppSettingsDto } from "@/bindings/AppSettingsDto";
import type { WorkspaceAgentDto } from "@/bindings/WorkspaceAgentDto";
import type { SkillDto } from "@/bindings/SkillDto";
import type { McpServerDto } from "@/bindings/McpServerDto";
import type { ProposalDto } from "@/bindings/ProposalDto";
import type { WorkspaceMemberDto } from "@/bindings/WorkspaceMemberDto";
import type { VaultSourceDto } from "@/bindings/VaultSourceDto";
import type { RuntimeSummaryDto } from "@/bindings/RuntimeSummaryDto";
import type { ContextTelemetryDto } from "@/bindings/ContextTelemetryDto";
import type { WorkflowDefinitionDto } from "@/bindings/WorkflowDefinitionDto";
import type { WorkflowRunDto } from "@/bindings/WorkflowRunDto";
import type { WorkflowRunEvent } from "@/bindings/WorkflowRunEvent";
import type { RelayUserDto } from "@/bindings/RelayUserDto";
import type { IssuedRelayTokenDto } from "@/bindings/IssuedRelayTokenDto";

export type { ChatSummaryDto } from "@/bindings/ChatSummaryDto";
export type { ChatSessionDto } from "@/bindings/ChatSessionDto";
export type { ChatMessageDto } from "@/bindings/ChatMessageDto";
export type { GitFileDiffDto } from "@/bindings/GitFileDiffDto";
export type { AppSettingsDto } from "@/bindings/AppSettingsDto";
export type { WorkspaceAgentDto } from "@/bindings/WorkspaceAgentDto";
export type { SkillDto } from "@/bindings/SkillDto";
export type { McpServerDto } from "@/bindings/McpServerDto";
export type { ProposalDto } from "@/bindings/ProposalDto";
export type { ReactionDto } from "@/bindings/ReactionDto";
export type { WorkspaceMemberDto } from "@/bindings/WorkspaceMemberDto";
export type { VaultSourceDto } from "@/bindings/VaultSourceDto";
export type { RuntimeSummaryDto } from "@/bindings/RuntimeSummaryDto";
export type { ContextTelemetryDto } from "@/bindings/ContextTelemetryDto";

export const getAppInfo = () => invoke<AppInfo>("get_app_info");

export const listChats = () => invoke<ChatSummaryDto[]>("list_chats");

export const createChat = (title: string) =>
  invoke<ChatSessionDto>("create_chat", { title });

export const getChat = (sessionId: string) =>
  invoke<ChatSessionDto | null>("get_chat", { sessionId });

export const getContextTelemetry = (sessionId: string) =>
  invoke<ContextTelemetryDto | null>("get_context_telemetry", { sessionId });

export const sendMessage = (sessionId: string, body: string) =>
  invoke<void>("send_message", { sessionId, body });

/// Replace the last assistant/agent turn with a fresh generation.
export const regenerate = (sessionId: string) =>
  invoke<void>("regenerate", { sessionId });

export const archiveChat = (sessionId: string, archived: boolean) =>
  invoke<void>("archive_chat", { sessionId, archived });

/// Rename a chat (the pencil affordance in the chat header).
export const renameChat = (sessionId: string, title: string) =>
  invoke<void>("rename_chat", { sessionId, title });

export const deleteChat = (sessionId: string) =>
  invoke<void>("delete_chat", { sessionId });

export const getWorkspaceDiffs = () =>
  invoke<GitFileDiffDto[]>("get_workspace_diffs");

export const getAppSettings = () => invoke<AppSettingsDto>("get_app_settings");

/// Git branch + dirty count for the active workspace. Separate (lazy) command —
/// it shells out to git, so it's queried only where the pill is shown.
export interface GitStatus {
  branch: string | null;
  dirtyCount: number;
}
export const getGitStatus = () => invoke<GitStatus>("get_git_status");

export const listRuntimes = () => invoke<RuntimeSummaryDto[]>("list_runtimes");

export const addRuntime = (
  id: string,
  name: string,
  provider: string,
  location: string,
  endpoint: string,
  model: string,
  supportsTools: boolean,
  supportsEmbeddings: boolean,
  modelBaseUrl?: string | null,
  modelProviderId?: string | null,
  contextWindow?: number | null,
) =>
  invoke<void>("add_runtime", {
    id,
    name,
    provider,
    location,
    endpoint,
    model,
    supportsTools,
    supportsEmbeddings,
    modelBaseUrl: modelBaseUrl ?? null,
    modelProviderId: modelProviderId ?? null,
    contextWindow: contextWindow ?? null,
  });

/// Custom /summarize + /compact instructions ("" = using the built-in default,
/// which is returned so the UI can show it).
export interface ContextCommandsDto {
  summarizePrompt: string;
  compactPrompt: string;
  defaultPrompt: string;
}
export const getContextCommands = () =>
  invoke<ContextCommandsDto>("get_context_commands");
export const setContextCommands = (summarizePrompt: string, compactPrompt: string) =>
  invoke<void>("set_context_commands", { summarizePrompt, compactPrompt });

export const removeRuntime = (runtimeId: string) =>
  invoke<void>("remove_runtime", { runtimeId });

export const setChatRuntime = (sessionId: string, runtimeId: string) =>
  invoke<void>("set_chat_runtime", { sessionId, runtimeId });

/// Add the local user to a chat's roster on open (so People shows who's here).
export const ensureSelfMember = (sessionId: string) =>
  invoke<void>("ensure_self_member", { sessionId });

/// Cross-device dispatch: ask this device to answer a synced, unanswered user
/// message if it owns the responder (no-op otherwise). Safe to call on sync.
export const maybeRespond = (sessionId: string) =>
  invoke<void>("maybe_respond", { sessionId });

/// Raise a local notification if the newest synced message @-mentions you.
/// Deduped server-side; safe to call on every sync.
export const notifyMentions = (sessionId: string) =>
  invoke<void>("notify_mentions", { sessionId });

export interface PresenceDto {
  actorId: string;
  name: string;
  typing: boolean;
  sessionId: string;
}

/// Heartbeat the local user's presence (online + typing in `sessionId`) to the
/// relay. No-op when relay-less.
export const presencePing = (sessionId: string, typing: boolean) =>
  invoke<void>("presence_ping", { sessionId, typing });

/// Live presence for the workspace (excludes self + stale entries).
export const presenceList = () => invoke<PresenceDto[]>("presence_list");

export const setWorkspaceRoot = (path: string) =>
  invoke<void>("set_workspace_root", { path });

export const addWorkspaceToList = (path: string) =>
  invoke<string[]>("add_workspace_to_list", { path });

export const pickWorkspaceFolder = () =>
  invoke<string | null>("pick_workspace_folder");

/// Read a workspace file (relative to the workspace root) to reference into a
/// chat. Path-safe + size-capped on the backend.
export const readWorkspaceFile = (path: string) =>
  invoke<string>("read_workspace_file", { path });

/// Export a chat transcript to a Markdown file via a save dialog. Resolves to
/// the written path, or null if the user cancelled.
export const exportChat = (sessionId: string) =>
  invoke<string | null>("export_chat", { sessionId });

/// Persist a composer attachment (base64 bytes) to disk; resolves to its
/// absolute path, embedded in the message as a `[Attached: ...]` marker.
export const saveAttachment = (name: string, dataBase64: string) =>
  invoke<string>("save_attachment", { name, dataBase64 });

export const removeWorkspaceFromList = (path: string) =>
  invoke<string[]>("remove_workspace_from_list", { path });

export const setDisplayName = (name: string) =>
  invoke<void>("set_display_name", { name });

/// Git email used to attribute commits agents make on this user's behalf.
export const setGitEmail = (email: string) =>
  invoke<void>("set_git_email", { email });

/// Direct peer-to-peer (iroh): connect to friends by their peer code.
export interface ContactDto {
  peerId: string;
  label: string;
}
/// This device's shareable friend code (its P2P public key).
export const p2pMyCode = () => invoke<string>("p2p_my_code");
export const p2pListContacts = () => invoke<ContactDto[]>("p2p_list_contacts");
export const p2pAddContact = (code: string, label: string) =>
  invoke<void>("p2p_add_contact", { code, label });
export const p2pRemoveContact = (peerId: string) =>
  invoke<void>("p2p_remove_contact", { peerId });

/// Short, speakable pairing codes brokered through the relay.
export interface ShortCodeDto {
  code: string;
  expiresIn: number;
}
export interface RedeemResultDto {
  /** "peer" | "workspace" */
  kind: string;
  label: string;
}
/// Publish this device's friend code as a short code (e.g. "K7P2QX").
export const p2pShareCode = () => invoke<ShortCodeDto>("p2p_share_code");
/// Publish a workspace invite as a short code.
export const workspaceShareCode = (workspaceId: string) =>
  invoke<ShortCodeDto>("workspace_share_code", { workspaceId });
/// Resolve a short code and act on it (add peer / join workspace).
export const redeemShortCode = (code: string) =>
  invoke<RedeemResultDto>("redeem_short_code", { code });

export const openInEditor = () => invoke<void>("open_in_editor");

/// Both sides of one file's diff (HEAD blob vs working tree) for the
/// side-by-side editor. Binary files come back flagged and empty.
export interface FileDiffSidesDto {
  original: string;
  modified: string;
  isBinary: boolean;
}
export const getFileDiffSides = (path: string) =>
  invoke<FileDiffSidesDto>("get_file_diff_sides", { path });

/// Code editors installed on this machine (VS Code, Cursor, Zed, …).
export interface EditorDto {
  id: string;
  label: string;
}
export const detectEditors = () => invoke<EditorDto[]>("detect_editors");

/// Open a workspace file (or the root, when path is "") in a detected editor.
export const openPathInEditor = (editorId: string, path: string) =>
  invoke<void>("open_path_in_editor", { editorId, path });

/// Open an http(s) URL in the system browser (window.open doesn't reach the OS
/// browser from the Tauri webview).
export const openExternal = (url: string) =>
  invoke<void>("open_external", { url });

/// GitHub sign-in (device flow). The GitHub user is the Hive account; the same
/// account works across all your devices.
export interface GithubAccountDto {
  id: number;
  login: string;
  name?: string | null;
  email?: string | null;
  avatarUrl?: string | null;
}
export interface DeviceStartDto {
  deviceCode: string;
  userCode: string;
  verificationUri: string;
  interval: number;
  expiresIn: number;
}
export interface GithubPollDto {
  /** "pending" | "slowDown" | "success" | "denied" | "expired" */
  status: string;
  account: GithubAccountDto | null;
}
export const githubAccount = () => invoke<GithubAccountDto | null>("github_account");
export const githubClientConfigured = () => invoke<boolean>("github_client_configured");
export const setGithubClientId = (clientId: string) =>
  invoke<void>("set_github_client_id", { clientId });
export const githubLoginStart = () => invoke<DeviceStartDto>("github_login_start");
export const githubLoginPoll = (deviceCode: string) =>
  invoke<GithubPollDto>("github_login_poll", { deviceCode });
export const githubLogout = () => invoke<void>("github_logout");

/// What's installed/available on this machine, to drive first-run defaults.
export interface EnvDetectDto {
  claudeCode: boolean;
  ollama: boolean;
  anthropicEnv: boolean;
  openaiEnv: boolean;
  gitName?: string | null;
  gitEmail?: string | null;
}
export const detectEnvironment = () => invoke<EnvDetectDto>("detect_environment");

/// LLM providers (connections): a backend kind + optional API key + base URL.
/// Runtimes (models) reference a provider for credentials/endpoint.
export interface ProviderDto {
  kind: string;
  name: string;
  needsKey: boolean;
  hasKey: boolean;
  supportsBaseUrl: boolean;
  baseUrl: string;
  note: string;
}
export const listProviders = () => invoke<ProviderDto[]>("list_providers");

/// A known OpenAI-compatible backend preset (Gemini, LM Studio, Groq, Azure, …)
/// for one-click runtime setup.
export interface ProviderPresetDto {
  label: string;
  provider: string; // the value `addRuntime` expects
  endpoint: string; // prefills the chat-completions URL
  needsKey: boolean;
}
export const listProviderPresets = () =>
  invoke<ProviderPresetDto[]>("list_provider_presets");
export const setProviderKey = (kind: string, key: string) =>
  invoke<void>("set_provider_key", { kind, key });
export const setProviderBaseUrl = (kind: string, baseUrl: string) =>
  invoke<void>("set_provider_base_url", { kind, baseUrl });

/// Reusable agent definitions (persona = name + model/runtime + role +
/// instructions) you can attach to any chat.
export interface AgentTemplateDto {
  id: string;
  name: string;
  runtimeId: string;
  role: string;
  instructions: string;
}
export const listAgentTemplates = () => invoke<AgentTemplateDto[]>("list_agent_templates");
export const addAgentTemplate = (
  name: string,
  runtimeId: string,
  role: string,
  instructions: string,
) => invoke<void>("add_agent_template", { name, runtimeId, role, instructions });
export const removeAgentTemplate = (id: string) =>
  invoke<void>("remove_agent_template", { id });

/// Register this device under the signed-in GitHub account (directory).
export const directoryRegister = () => invoke<void>("directory_register");

export interface InviteResultDto {
  login: string;
  devices: number;
  sealed: boolean;
}
/// Invite a GitHub user by handle to this chat's workspace (seals the key to
/// all their devices via the directory).
export const inviteByHandle = (sessionId: string, handle: string) =>
  invoke<InviteResultDto>("invite_by_handle", { sessionId, handle });

/// A server-side workspace member (membership-enforcing / paid relays only).
export interface MemberEntry {
  account: string; // "github:<id>"
  login: string;
  role: "owner" | "admin" | "contributor" | "viewer";
  addedBy: string;
  addedAt: number;
}
/// Claim the active workspace's room on a membership-enforcing relay (caller →
/// Owner). Returns false on an open relay or if already claimed.
export const workspaceClaimMembership = () =>
  invoke<boolean>("workspace_claim_membership");
/// Server-side members of the active workspace (empty on open relays).
export const workspaceMembers = () => invoke<MemberEntry[]>("workspace_members");
/// Add a member by GitHub handle / set their role (caller must be Admin+).
export const workspaceAddMember = (handle: string, role: string) =>
  invoke<void>("workspace_add_member", { handle, role });
/// Remove a member by account id ("github:<id>"); caller must be Admin+.
export const workspaceRemoveMember = (account: string) =>
  invoke<void>("workspace_remove_member", { account });

// ── Social graph: friends + presence ───────────────────────────────────────

export type Presence = "online" | "away" | "offline";

export interface FriendDto {
  accountId: string;
  login: string;
  presence: Presence;
}
export interface IncomingRequestDto {
  requestId: string;
  fromAccount: string;
  fromLogin: string;
  createdAt: number;
}
export interface FriendsOverviewDto {
  /** True once signed in + a relay is configured. */
  enabled: boolean;
  friends: FriendDto[];
  incoming: IncomingRequestDto[];
}
export interface FriendRequestResultDto {
  /** "sent" | "alreadyFriends" | "capReached" | "userNotFound" | "invalid" */
  outcome: string;
  requestId?: string | null;
}

/// Heartbeat this device + fetch friends (with presence) and incoming requests.
export const friendsOverview = () => invoke<FriendsOverviewDto>("friends_overview");
/// Send a friend request to a GitHub @username.
export const friendSendRequest = (login: string) =>
  invoke<FriendRequestResultDto>("friend_send_request", { login });
/// Accept a pending friend request by id.
export const friendAccept = (requestId: string) =>
  invoke<void>("friend_accept", { requestId });
/// Reject (recipient) or cancel (sender) a pending request by id.
export const friendReject = (requestId: string) =>
  invoke<void>("friend_reject", { requestId });
/// Remove an accepted friend by account id ("github:<id>").
export const friendRemove = (accountId: string) =>
  invoke<void>("friend_remove", { accountId });
/// Toggle "appear offline" for this account.
export const friendSetVisibility = (appearOffline: boolean) =>
  invoke<void>("friend_set_visibility", { appearOffline });

export interface DmDto {
  workspaceId: string;
  account: string;
  login: string;
}
/// Open (provisioning on first use) the 1:1 DM workspace with a friend and
/// switch the chat list to it. Returns the DM workspace id.
export const friendOpenDm = (accountId: string, login: string) =>
  invoke<string>("friend_open_dm", { accountId, login });
/// The provisioned DM workspaces (Friends section).
export const listDms = () => invoke<DmDto[]>("list_dms");

export const listAgents = (sessionId: string) =>
  invoke<WorkspaceAgentDto[]>("list_agents", { sessionId });

export const addAgent = (
  sessionId: string,
  name: string,
  runtimeId: string,
  role: string,
) => invoke<void>("add_agent", { sessionId, name, runtimeId, role });

export const removeAgent = (sessionId: string, agentId: string) =>
  invoke<void>("remove_agent", { sessionId, agentId });

export const listSkills = (sessionId: string) =>
  invoke<SkillDto[]>("list_skills", { sessionId });

export const addSkillInline = (sessionId: string, name: string, instructions: string) =>
  invoke<void>("add_skill_inline", { sessionId, name, instructions });

export const installSkill = (sessionId: string, name: string, source: string) =>
  invoke<void>("install_skill", { sessionId, name, source });

export const removeSkill = (sessionId: string, skillId: string) =>
  invoke<void>("remove_skill", { sessionId, skillId });

export const listMcpServers = () => invoke<McpServerDto[]>("list_mcp_servers");

export const installMcpServer = (source: string) =>
  invoke<void>("install_mcp_server", { source });

export const removeMcpServer = (serverId: string) =>
  invoke<void>("remove_mcp_server", { serverId });

export const setMcpEnabled = (serverId: string, enabled: boolean) =>
  invoke<void>("set_mcp_enabled", { serverId, enabled });

export const listProposals = (sessionId: string) =>
  invoke<ProposalDto[]>("list_proposals", { sessionId });

export const createProposal = (
  sessionId: string,
  title: string,
  body: string,
  kind: string,
  requiredApprovals: number,
) => invoke<void>("create_proposal", { sessionId, title, body, kind, requiredApprovals });

export const voteProposal = (sessionId: string, proposalId: string, approved: boolean) =>
  invoke<ProposalDto | null>("vote_proposal", { sessionId, proposalId, approved });

export const toggleReaction = (sessionId: string, messageId: string, emoji: string) =>
  invoke<void>("toggle_reaction", { sessionId, messageId, emoji });

export const listMembers = (sessionId: string) =>
  invoke<WorkspaceMemberDto[]>("list_members", { sessionId });

export const addMember = (sessionId: string, displayName: string, role: string, title: string) =>
  invoke<void>("add_member", { sessionId, displayName, role, title });

export const removeMember = (sessionId: string, memberId: string) =>
  invoke<void>("remove_member", { sessionId, memberId });

export interface RevokeResultDto {
  /** Whether the workspace key was rotated (false if no relay / no recipients). */
  rotated: boolean;
  recipients: number;
}
/// Remove a member AND rotate the workspace key so they lose read access to new
/// messages (owner/admin only). Rotation is sealed to the remaining members.
export const removeAndRevoke = (sessionId: string, memberId: string) =>
  invoke<RevokeResultDto>("remove_and_revoke", { sessionId, memberId });

export const setMemberRole = (sessionId: string, memberId: string, role: string) =>
  invoke<void>("set_member_role", { sessionId, memberId, role });

export const listVaults = (sessionId: string) =>
  invoke<VaultSourceDto[]>("list_vaults", { sessionId });

export const addVault = (sessionId: string, kind: string, reference: string) =>
  invoke<void>("add_vault", { sessionId, kind, reference });

export const removeVault = (sessionId: string, url: string) =>
  invoke<void>("remove_vault", { sessionId, url });

export const previewVault = (url: string) => invoke<string>("preview_vault", { url });

/// Subscribe to streaming chat updates. Returns an unlisten function.
export const onChatStream = (
  cb: (e: ChatStreamEvent) => void,
): Promise<UnlistenFn> =>
  listen<ChatStreamEvent>("chat://stream", (evt) => cb(evt.payload));

export interface SyncStatusDto {
  relayConfigured: boolean;
  relayUrl: string;
  room: string;
  encrypted: boolean;
}

export const syncStatus = () => invoke<SyncStatusDto>("sync_status");

export type RelayProbeStatus =
  | "ok"
  | "unauthorized"
  | "httpError"
  | "unreachable"
  | "unconfigured";

export interface RelayProbeDto {
  status: RelayProbeStatus;
  detail: string;
}

/// Actually hit the configured relay and report reachability + auth, as opposed
/// to `sync_status` which only reflects whether a URL is set.
export const probeRelay = () => invoke<RelayProbeDto>("probe_relay");

/// Probe a URL + token being entered (onboarding/settings), before saving it.
export const probeRelayAt = (url: string, accessToken: string | null) =>
  invoke<RelayProbeDto>("probe_relay_at", { url, accessToken });

export interface WorkspaceInfoDto {
  id: string;
  name: string;
  /** "local" | "room" */
  kind: string;
  active: boolean;
  /** Optional workspace icon as a `data:` URL; rail renders it over initials. */
  iconUrl?: string | null;
}

/// The selectable workspaces: "My workspace" (local) + any joined relay room.
export const listWorkspaces = () => invoke<WorkspaceInfoDto[]>("list_workspaces");

/// Scope the chat list to a workspace (local id or a joined room id).
export const setActiveWorkspace = (workspaceId: string) =>
  invoke<void>("set_active_workspace", { workspaceId });

/// Set a workspace icon (a `data:image/…` URL), or clear it with `null`.
export const setWorkspaceIcon = (workspaceId: string, icon: string | null) =>
  invoke<void>("set_workspace_icon", { workspaceId, icon });

/// Create a new team workspace (generates a room + E2EE key), switch to it.
export const createWorkspace = (name: string) =>
  invoke<WorkspaceInfoDto>("create_workspace", { name });

/// Join a team workspace from an invite code (`hivews1:…`).
export const joinWorkspace = (invite: string) =>
  invoke<WorkspaceInfoDto>("join_workspace", { invite });

/// Shareable invite code for a workspace (bundles relay + room + key).
export const workspaceInvite = (workspaceId: string) =>
  invoke<string>("workspace_invite", { workspaceId });

/// Leave a team workspace (removes it from the rail).
export const removeWorkspace = (workspaceId: string) =>
  invoke<void>("remove_workspace", { workspaceId });

export type ClaudePermissionMode =
  | "default"
  | "acceptEdits"
  | "bypassPermissions";

export interface ConnectionSettingsDto {
  relayUrl: string;
  room: string;
  hasWorkspaceKey: boolean;
  hasApiKey: boolean;
  /// Whether a relay access token (for a gated/paid hosted relay) is set.
  hasRelayAccessToken: boolean;
  permissionMode: ClaudePermissionMode;
}

export const getConnectionSettings = () =>
  invoke<ConnectionSettingsDto>("get_connection_settings");

/// Update connection settings. For `workspaceKey`/`apiKey`/`relayAccessToken`:
/// pass `null` to leave unchanged, "" to clear, or a value to set (secrets
/// aren't echoed back).
export const updateConnectionSettings = (args: {
  relayUrl: string;
  room: string;
  workspaceKey: string | null;
  apiKey: string | null;
  relayAccessToken: string | null;
  permissionMode: ClaudePermissionMode;
}) => invoke<ConnectionSettingsDto>("update_connection_settings", args);

/// Fires when the background relay sync applies remote events.
export const onWorkspaceSynced = (cb: () => void): Promise<UnlistenFn> =>
  listen("workspace://synced", () => cb());

/// Fires when a system-tray menu item asks the UI to navigate. The payload is a
/// route string: `"friends"`, `"settings"`, or `"settings:<Tab>"`.
export const onTrayNavigate = (cb: (route: string) => void): Promise<UnlistenFn> =>
  listen<string>("tray://navigate", (evt) => cb(evt.payload));

/// Tint the native title bar to match the app background (Windows 11; no-op
/// elsewhere). Called by `applyTheme` on launch and every theme change.
export const setTitlebarColor = (r: number, g: number, b: number, dark: boolean) =>
  invoke<void>("set_titlebar_color", { r, g, b, dark });

/// Factory reset: wipe all local data (chats, identity, keys, settings,
/// workspaces) and relaunch the app. The backend defers the actual deletion to
/// the next launch (the DB file is open/locked) and restarts immediately.
export const resetLocalData = () => invoke<void>("reset_local_data");

/// Scheduled / triggered agents (Settings → Schedules).
export type ScheduleTrigger =
  | { kind: "interval"; every_secs: number }
  | { kind: "daily_at"; hour: number; minute: number };

export interface ScheduledAgent {
  id: string;
  enabled: boolean;
  label: string;
  workspaceId: string | null;
  runtimeId: string;
  prompt: string;
  trigger: ScheduleTrigger;
  lastRun: string | null;
}

export const listSchedules = () => invoke<ScheduledAgent[]>("list_schedules");
export const addSchedule = (args: {
  label: string;
  prompt: string;
  runtimeId?: string;
  workspaceId?: string;
  trigger: ScheduleTrigger;
}) => invoke<ScheduledAgent>("add_schedule", args);
export const removeSchedule = (id: string) => invoke<void>("remove_schedule", { id });
export const setScheduleEnabled = (id: string, enabled: boolean) =>
  invoke<void>("set_schedule_enabled", { id, enabled });

/// `/summarize` — post a model summary of the chat (transcript kept intact).
export const summarizeChat = (sessionId: string) => invoke<void>("summarize_chat", { sessionId });
/// `/compact` — collapse the conversation into a single summary checkpoint.
export const compactChat = (sessionId: string) => invoke<void>("compact_chat", { sessionId });

/// Add a remote HTTP MCP server by URL (e.g. a hosted server like Linear's).
export const addRemoteMcpServer = (id: string, url: string) =>
  invoke<void>("add_remote_mcp_server", { id, url });
/// Connect a remote MCP server via OAuth 2.1 + PKCE (opens the browser).
export const authorizeMcpServer = (serverId: string, scope?: string) =>
  invoke<void>("authorize_mcp_server", { serverId, scope });

/// Carry out an approved proposal (agreement-gated; agent executes it).
export const implementProposal = (sessionId: string, proposalId: string) =>
  invoke<void>("implement_proposal", { sessionId, proposalId });

/// Check for an app update. Resolves to the new version if one is available,
/// null if up to date; rejects with a friendly message if the updater isn't
/// configured yet (#144).
export const checkForUpdate = () => invoke<string | null>("check_for_update");

/// Enterprise (#143): import a GitHub org's Teams as workspace members + roles.
/// Returns the count added. Needs a signed-in GitHub token with read:org.
export const importGithubTeams = (sessionId: string, org: string) =>
  invoke<number>("import_github_teams", { sessionId, org });


/// Linear context source (#145 L3): pull issues from the connected Linear MCP
/// server as a block to insert into the composer.
export const linearIssuesContext = () => invoke<string>("linear_issues_context");

/// Store a remote MCP server's OAuth client credentials before connecting
/// (e.g. a Linear OAuth app's Client ID + secret). Secret stays local.
export const setMcpOauthClient = (serverId: string, clientId: string, clientSecret?: string) =>
  invoke<void>("set_mcp_oauth_client", { serverId, clientId, clientSecret });

/// Lightweight "is a newer published version available?" check (independent of
/// the auto-updater). Returns the update info, or null when current / dev / offline.
export interface AppUpdateInfo { tag: string; name: string; url: string; notes: string }
export const checkForAppUpdate = () => invoke<AppUpdateInfo | null>("check_for_app_update");

/// Selected Claude Code CLI model alias (`--model`); empty = the CLI default.
export const getClaudeCodeModel = () => invoke<string>("get_claude_code_model");
export const setClaudeCodeModel = (model: string) =>
  invoke<void>("set_claude_code_model", { model });

/// A model to offer for the local Claude Code CLI. `value` is passed to
/// `--model` verbatim (empty = the CLI's own default).
export interface ClaudeModelOption {
  value: string;
  label: string;
  description: string | null;
}
/// Base aliases merged with whatever Claude Code has cached as available for
/// this account (Fable, …), so the picker mirrors the CLI's own `/model` list.
/// Falls back to the base aliases if the CLI cache isn't present.
export const listClaudeCodeModels = () =>
  invoke<ClaudeModelOption[]>("list_claude_code_models");

/// Set the Primary Runtime's model (the default Anthropic model shown in
/// Settings → Models). Persisted; applies immediately.
export const setDefaultModel = (model: string) =>
  invoke<void>("set_default_model", { model });

/// Set which runtime new chats default to (the per-row "Set default" in
/// Settings → Models). Persisted; drives `isDefault` in listRuntimes.
export const setDefaultRuntime = (runtimeId: string) =>
  invoke<void>("set_default_runtime", { runtimeId });

// --- Agentic workflows -------------------------------------------------------

export type { WorkflowDefinitionDto } from "@/bindings/WorkflowDefinitionDto";
export type { WorkflowNodeDto } from "@/bindings/WorkflowNodeDto";
export type { WorkflowRunDto } from "@/bindings/WorkflowRunDto";
export type { WorkflowNodeRunDto } from "@/bindings/WorkflowNodeRunDto";

export const listWorkflows = (sessionId: string) =>
  invoke<WorkflowDefinitionDto[]>("list_workflows", { sessionId });

/// Create or update a definition (empty id ⇒ backend assigns one). The backend
/// re-validates the DAG authoritatively and returns the saved definition.
export const saveWorkflow = (sessionId: string, definition: WorkflowDefinitionDto) =>
  invoke<WorkflowDefinitionDto>("save_workflow", { sessionId, definition });

export const removeWorkflow = (sessionId: string, workflowId: string) =>
  invoke<void>("remove_workflow", { sessionId, workflowId });

/// Instantiate a built-in preset: "reviewGate" | "fanOutVote".
export const addWorkflowPreset = (sessionId: string, preset: string) =>
  invoke<WorkflowDefinitionDto>("add_workflow_preset", { sessionId, preset });

export const listWorkflowRuns = (sessionId: string) =>
  invoke<WorkflowRunDto[]>("list_workflow_runs", { sessionId });

/// Start a run; resolves to the run id. Stage turns stream into the chat.
export const startWorkflowRun = (sessionId: string, workflowId: string, input: string) =>
  invoke<string>("start_workflow_run", { sessionId, workflowId, input });

export const cancelWorkflowRun = (sessionId: string, runId: string) =>
  invoke<void>("cancel_workflow_run", { sessionId, runId });

/// Restart an interrupted run (e.g. after an app restart mid-run).
export const resumeWorkflowRun = (sessionId: string, runId: string) =>
  invoke<void>("resume_workflow_run", { sessionId, runId });

/// Fires whenever a workflow run's persisted state changes.
export const onWorkflowRun = (
  cb: (e: WorkflowRunEvent) => void,
): Promise<UnlistenFn> =>
  listen<WorkflowRunEvent>("workflow://run", (evt) => cb(evt.payload));

// --- Relay access user management (Settings → Team; enterprise relay) --------

export type { RelayUserDto } from "@/bindings/RelayUserDto";
export type { RelayTokenDto } from "@/bindings/RelayTokenDto";
export type { IssuedRelayTokenDto } from "@/bindings/IssuedRelayTokenDto";

/// List relay access users + their live tokens. Rejects if the relay has no
/// admin API or the signed-in user isn't a relay admin.
export const listRelayUsers = () => invoke<RelayUserDto[]>("list_relay_users");

/// Create a user + issue their first token. The returned `raw` is shown once.
export const createRelayUser = (name: string, login: string) =>
  invoke<IssuedRelayTokenDto>("create_relay_user", { name, login });

/// Issue an additional token for an existing user (raw shown once).
export const issueRelayToken = (userId: string, label: string) =>
  invoke<IssuedRelayTokenDto>("issue_relay_token", { userId, label });

export const revokeRelayToken = (tokenId: string) =>
  invoke<void>("revoke_relay_token", { tokenId });

export const setRelayUserDisabled = (userId: string, disabled: boolean) =>
  invoke<void>("set_relay_user_disabled", { userId, disabled });
