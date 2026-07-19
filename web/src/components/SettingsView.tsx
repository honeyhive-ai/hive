import type { ReactNode } from "react";
import { useEffect, useRef, useState } from "react";
import type { RuntimeSummaryDto } from "@/bindings/RuntimeSummaryDto";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  addRuntime,
  getAppSettings,
  getGitStatus,
  getClaudeCodeModel,
  setClaudeCodeModel,
  listClaudeCodeModels,
  setDefaultModel,
  installMcpServer,
  addRemoteMcpServer,
  authorizeMcpServer,
  setMcpOauthClient,
  listRuntimes,
  listMcpServers,
  openInEditor,
  removeMcpServer,
  removeRuntime,
  setDefaultRuntime,
  setDisplayName,
  setGitEmail,
  p2pMyCode,
  p2pListContacts,
  p2pAddContact,
  p2pRemoveContact,
  p2pShareCode,
  redeemShortCode,
  githubAccount,
  githubClientConfigured,
  setGithubClientId,
  githubLoginStart,
  openExternal,
  githubLoginPoll,
  githubLogout,
  listProviders,
  listProviderPresets,
  setProviderKey,
  setProviderBaseUrl,
  type ProviderDto,
  listAgentTemplates,
  addAgentTemplate,
  removeAgentTemplate,
  setMcpEnabled,
  setWorkspaceRoot,
  getContextCommands,
  setContextCommands,
  syncStatus,
  probeRelay,
  type RelayProbeDto,
  getConnectionSettings,
  updateConnectionSettings,
  resetLocalData,
  checkForUpdate,
  listSchedules,
  addSchedule,
  removeSchedule,
  setScheduleEnabled,
  listRelayUsers,
  createRelayUser,
  issueRelayToken,
  revokeRelayToken,
  setRelayUserDisabled,
  type ScheduleTrigger,
  type ClaudePermissionMode,
  type RelayUserDto,
} from "@/lib/ipc";
import { THEMES, type AppearanceMode, type ThemeName } from "@/lib/theme";
import {
  IconCheck,
  IconAlertTriangle,
  IconInfo,
  IconLock,
  IconHexagon,
  IconPlus,
  IconChevronDown,
  IconChevronRight,
} from "@/lib/icons";
import { Button } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";
import { promptDialog } from "@/components/Dialog";
import { loadTemplates, addTemplate, removeTemplate, type PromptTemplate } from "@/lib/templates";

/// Tabbed-style settings: identity, workspace, model, and appearance. Writes
/// flow back through the corresponding Tauri commands (theme is local-only).
export function SettingsView({
  palette,
  onPaletteChange,
  appearanceMode,
  onAppearanceModeChange,
  initialTab,
}: {
  palette: ThemeName;
  onPaletteChange: (t: ThemeName) => void;
  appearanceMode: AppearanceMode;
  onAppearanceModeChange: (m: AppearanceMode) => void;
  /// Which tab to open on (defaults to Account). The tray uses this to jump
  /// straight to e.g. "Team & Relay Sync".
  initialTab?: SettingsTab;
}) {
  const [tab, setTab] = useState<SettingsTab>(initialTab ?? "Account");
  return (
    <div className="mx-auto max-w-2xl overflow-y-auto p-8">
      <h1 className="text-2xl font-semibold">Settings</h1>

      {/* Group the sections so the page isn't one overwhelming scroll. Only the
          active group mounts, which also keeps hidden sections' queries idle. */}
      <nav
        className="mt-4 mb-6 flex flex-wrap gap-1 rounded-2xl border p-1"
        style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
      >
        {SETTINGS_TABS.map((t) => (
          <button
            key={t}
            onClick={() => setTab(t)}
            aria-pressed={tab === t}
            className="rounded-xl px-3 py-1.5 text-sm font-medium transition-all"
            style={
              tab === t
                ? { background: "var(--hive-panel)", color: "var(--hive-ink)", boxShadow: "0 1px 2px var(--hive-overlay)" }
                : { color: "var(--hive-ink)", opacity: 0.55 }
            }
          >
            {t}
          </button>
        ))}
      </nav>

      <div className="space-y-8">
        {tab === "Account" && (
          <>
            <IdentitySection />
            <GithubSection />
            <UpdatesSection />
            <DangerZoneSection />
          </>
        )}
        {tab === "Models" && (
          <>
            <ProvidersSection />
            <RuntimesSection />
            <AgentsSection />
            <TemplatesSection />
            <ContextCommandsSection />
          </>
        )}
        {tab === "Tools" && <McpSection />}
        {tab === "Schedules" && <SchedulesSection />}
        {tab === "Team" && (
          <>
            <SyncSection />
            <RelayUsersSection />
            <PeersSection />
          </>
        )}
        {tab === "Workspace" && <WorkspaceSection />}
        {tab === "Appearance" && (
          <AppearanceSection
            palette={palette}
            onPaletteChange={onPaletteChange}
            appearanceMode={appearanceMode}
            onAppearanceModeChange={onAppearanceModeChange}
          />
        )}
      </div>
    </div>
  );
}

const SETTINGS_TABS = ["Account", "Models", "Tools", "Schedules", "Team", "Workspace", "Appearance"] as const;
export type SettingsTab = (typeof SETTINGS_TABS)[number];

const inputStyle = { borderColor: "var(--hive-line)", background: "var(--hive-panel)" };
// Canonical styling for every Settings dropdown, so they're visually identical.
// Combine with a width class as needed: `${SELECT_CLASS} w-full`.
const SELECT_CLASS = "rounded-xl border px-3 py-2 text-sm";

// Identity, workspace, and the runtime form each own their input state so a
// keystroke only re-renders that one section — not the whole settings panel
// (which previously also re-rendered the query-bearing Sync/MCP sections every
// character, the source of the typing lag on the Windows webview).
function IdentitySection() {
  const qc = useQueryClient();
  const settings = useQuery({ queryKey: ["settings"], queryFn: getAppSettings });
  const account = useQuery({ queryKey: ["github-account"], queryFn: githubAccount });
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");

  useEffect(() => {
    if (settings.data) {
      setName(settings.data.displayName);
      setEmail(settings.data.gitEmail);
    }
  }, [settings.data]);

  // While signed in to GitHub, the git email is managed from the account (locked
  // here); sign out to edit it by hand. A signed-in account with no public email
  // still leaves it editable so the user isn't stuck.
  const emailLocked = Boolean(account.data?.email);

  async function saveIdentity() {
    try {
      await setDisplayName(name.trim() || "You");
      if (!emailLocked) await setGitEmail(email.trim());
      qc.invalidateQueries({ queryKey: ["settings"] });
      toast.success("Identity saved.");
    } catch (e) {
      toast.error(`Couldn't save identity: ${errMsg(e)}`);
    }
  }

  return (
    <Section title="Identity">
      <label className="block text-sm opacity-70">Display name</label>
      <input
        value={name}
        onChange={(e) => setName(e.target.value)}
        className="w-full rounded-xl border px-3 py-2 text-sm"
        style={inputStyle}
      />
      <label className="mt-2 block text-sm opacity-70">Git email</label>
      <input
        value={emailLocked ? (account.data?.email ?? email) : email}
        onChange={(e) => setEmail(e.target.value)}
        readOnly={emailLocked}
        placeholder="you@example.com"
        className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
        style={{ ...inputStyle, opacity: emailLocked ? 0.6 : 1 }}
      />
      <p className="text-xs opacity-50">
        {emailLocked
          ? "From your GitHub account — sign out (Account tab) to set it by hand."
          : "Used to credit you as the commit author when an agent does work you asked for — even if it runs on a teammate's machine."}
      </p>
      <div className="flex items-center gap-3">
        <SaveButton onClick={saveIdentity} />
        <span className="text-xs opacity-50">Device: {settings.data?.deviceName ?? "…"}</span>
      </div>
    </Section>
  );
}

function WorkspaceSection() {
  const qc = useQueryClient();
  const settings = useQuery({ queryKey: ["settings"], queryFn: getAppSettings });
  // Git status is its own (lazy) query — it shells out to git, so it's only
  // fetched here where the pill is shown, not on every getAppSettings.
  const git = useQuery({ queryKey: ["git-status"], queryFn: getGitStatus });
  const [root, setRoot] = useState("");

  useEffect(() => {
    if (settings.data) setRoot(settings.data.workspaceRoot);
  }, [settings.data]);

  async function saveRoot() {
    try {
      await setWorkspaceRoot(root.trim());
      qc.invalidateQueries({ queryKey: ["settings"] });
      qc.invalidateQueries({ queryKey: ["diffs"] });
      qc.invalidateQueries({ queryKey: ["runtimes"] });
      qc.invalidateQueries({ queryKey: ["mcp"] });
      toast.success("Workspace root updated.");
    } catch (e) {
      toast.error(`Couldn't set workspace root: ${errMsg(e)}`);
    }
  }

  return (
    <Section title="Workspace">
      <label className="block text-sm opacity-70">Root path (for the Diff canvas + git)</label>
      <div className="flex gap-2">
        <input
          value={root}
          onChange={(e) => setRoot(e.target.value)}
          className="flex-1 rounded-xl border px-3 py-2 font-mono text-sm"
          style={inputStyle}
        />
        <SaveButton onClick={saveRoot} />
      </div>
      <div className="flex items-center gap-3 text-xs opacity-60">
        <span>
          git: {git.data?.branch ?? "—"}
          {git.data ? ` · ${git.data.dirtyCount} changed` : ""}
        </span>
        <button className="underline" onClick={() => openInEditor()}>
          Open in editor
        </button>
      </div>
    </Section>
  );
}

/// Reusable agent definitions (personas) the user can attach to any chat.
function AgentsSection() {
  const qc = useQueryClient();
  const templates = useQuery({ queryKey: ["agent-templates"], queryFn: listAgentTemplates });
  const runtimes = useQuery({ queryKey: ["runtimes"], queryFn: listRuntimes });
  const [name, setName] = useState("");
  const [runtimeId, setRuntimeId] = useState("");
  const [role, setRole] = useState("contributor");
  const [instructions, setInstructions] = useState("");
  const rts = runtimes.data ?? [];
  const refresh = () => qc.invalidateQueries({ queryKey: ["agent-templates"] });

  const add = useMutation({
    mutationFn: () => addAgentTemplate(name.trim(), runtimeId || rts[0]?.id || "", role, instructions),
    onSuccess: () => {
      setName("");
      setInstructions("");
      refresh();
      toast.success("Agent saved.");
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const remove = useMutation({
    mutationFn: (id: string) => removeAgentTemplate(id),
    onSuccess: refresh,
    onError: (e) => toast.error(errMsg(e)),
  });

  return (
    <Section title="Agents">
      <p className="text-xs opacity-50">
        A reusable persona — name + model (runtime) + role + instructions. Define once here, then
        attach it to any chat from that chat's Tools pane.
      </p>
      <div className="space-y-2">
        {(templates.data ?? []).map((t) => {
          const rt = rts.find((r) => r.id === t.runtimeId);
          return (
            <div key={t.id} className="flex items-center justify-between gap-2 rounded-xl border px-3 py-2" style={inputStyle}>
              <div className="min-w-0">
                <div className="font-medium">
                  {t.name} <span className="text-xs opacity-50">· {t.role}</span>
                </div>
                <div className="truncate text-xs opacity-50">
                  {rt?.label ?? t.runtimeId ?? "no model"}
                  {t.instructions ? ` · ${t.instructions.slice(0, 48)}` : ""}
                </div>
              </div>
              <button
                className="shrink-0 text-xs text-[color:var(--hive-danger)] hover:opacity-80"
                onClick={() => confirmThen(`Remove agent "${t.name}"?`, () => remove.mutate(t.id))}
              >
                Remove
              </button>
            </div>
          );
        })}
        {(templates.data ?? []).length === 0 && <p className="text-sm opacity-50">No saved agents yet.</p>}
      </div>

      <div className="space-y-2 rounded-2xl border p-3" style={{ borderColor: "var(--hive-line)" }}>
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Agent name (e.g. Reviewer)"
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={inputStyle}
        />
        <select
          value={runtimeId || rts[0]?.id || ""}
          onChange={(e) => setRuntimeId(e.target.value)}
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={inputStyle}
        >
          {rts.length === 0 && <option value="">(no models configured)</option>}
          {rts.map((r) => (
            <option key={r.id} value={r.id}>
              {r.label}
            </option>
          ))}
        </select>
        <select
          value={role}
          onChange={(e) => setRole(e.target.value)}
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={inputStyle}
        >
          {["owner", "admin", "contributor", "viewer"].map((r) => (
            <option key={r} value={r}>
              {r}
            </option>
          ))}
        </select>
        <textarea
          value={instructions}
          onChange={(e) => setInstructions(e.target.value)}
          placeholder="Instructions (optional)"
          rows={2}
          className="w-full resize-none rounded-xl border px-3 py-2 text-sm"
          style={inputStyle}
        />
        <SaveButton onClick={() => add.mutate()} label="Save agent" />
      </div>
    </Section>
  );
}

/// A provider counts as "configured" once it has a key or a custom base URL —
/// i.e. the user set it up. Those show in the list; everything else is tucked
/// behind the "Add a provider…" selector so the section stays short.
function isProviderConfigured(p: ProviderDto): boolean {
  return p.hasKey || (p.supportsBaseUrl && p.baseUrl.trim() !== "");
}

/// Providers = LLM backends + their credentials. Runtimes (models) below pick a
/// provider; agents pick a runtime. Condensed: only configured providers are
/// listed; add more from the selector (most users only use one or two).
function ProvidersSection() {
  const qc = useQueryClient();
  const providers = useQuery({ queryKey: ["providers"], queryFn: listProviders });
  const refresh = () => qc.invalidateQueries({ queryKey: ["providers"] });
  // Providers the user opened via "Add" but hasn't saved a key for yet, and which
  // row is currently expanded.
  const [added, setAdded] = useState<string[]>([]);
  const [openKind, setOpenKind] = useState<string | null>(null);
  const [picker, setPicker] = useState("");

  const all = providers.data ?? [];
  const visible = all.filter((p) => isProviderConfigured(p) || added.includes(p.kind));
  const available = all.filter((p) => !visible.includes(p));

  function addProvider() {
    if (!picker) return;
    setAdded((a) => (a.includes(picker) ? a : [...a, picker]));
    setOpenKind(picker);
    setPicker("");
  }

  return (
    <Section title="LLM providers">
      <p className="text-xs opacity-50">
        A provider is a backend + how to reach it (key and/or base URL). Add the ones you use — the
        rest stay tucked away. Models (below) pick a provider; agents pick a model.
      </p>
      <div className="space-y-2">
        {visible.length === 0 && (
          <p className="rounded-xl border px-3 py-4 text-xs opacity-60" style={inputStyle}>
            No providers configured yet. Add one below to start.
          </p>
        )}
        {visible.map((p) => (
          <ProviderRow
            key={p.kind}
            provider={p}
            open={openKind === p.kind}
            configured={isProviderConfigured(p)}
            onToggle={() => setOpenKind(openKind === p.kind ? null : p.kind)}
            onChanged={refresh}
            onHide={() => {
              setAdded((a) => a.filter((k) => k !== p.kind));
              if (openKind === p.kind) setOpenKind(null);
            }}
          />
        ))}
      </div>
      {available.length > 0 && (
        <div className="mt-3 flex items-center gap-2">
          <select
            value={picker}
            onChange={(e) => setPicker(e.target.value)}
            className="flex-1 rounded-xl border px-3 py-2 text-sm"
            style={inputStyle}
          >
            <option value="">Add a provider…</option>
            {available.map((p) => (
              <option key={p.kind} value={p.kind}>
                {p.name}
              </option>
            ))}
          </select>
          <SaveButton onClick={addProvider} label="Add" />
        </div>
      )}
    </Section>
  );
}

function ProviderRow({
  provider,
  open,
  configured,
  onToggle,
  onChanged,
  onHide,
}: {
  provider: ProviderDto;
  open: boolean;
  configured: boolean;
  onToggle: () => void;
  onChanged: () => void;
  onHide: () => void;
}) {
  const [key, setKey] = useState("");
  const [baseUrl, setBaseUrl] = useState(provider.baseUrl);
  const saveKey = useMutation({
    mutationFn: () => setProviderKey(provider.kind, key.trim()),
    onSuccess: () => {
      setKey("");
      onChanged();
      toast.success(`${provider.name} key saved.`);
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const saveBase = useMutation({
    mutationFn: () => setProviderBaseUrl(provider.kind, baseUrl.trim()),
    onSuccess: () => {
      onChanged();
      toast.success(`${provider.name} base URL saved.`);
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  // Remove = clear whatever made it configured (key + base URL) and refresh; for
  // a not-yet-configured row the user just added, hide it again.
  const remove = useMutation({
    mutationFn: async () => {
      if (provider.hasKey) await setProviderKey(provider.kind, "");
      if (provider.supportsBaseUrl && provider.baseUrl.trim() !== "")
        await setProviderBaseUrl(provider.kind, "");
    },
    onSuccess: () => {
      onChanged();
      onHide();
      toast.success(`${provider.name} removed.`);
    },
    onError: (e) => toast.error(errMsg(e)),
  });

  const localOnly = !provider.needsKey && !provider.supportsBaseUrl;
  return (
    <div className="rounded-xl border" style={inputStyle}>
      <button
        type="button"
        onClick={onToggle}
        className="flex w-full items-center justify-between gap-2 px-3 py-2.5 text-left"
      >
        <span className="flex items-center gap-2">
          <span className="opacity-50">{open ? <IconChevronDown size={13} /> : <IconChevronRight size={13} />}</span>
          <span className="font-medium">{provider.name}</span>
        </span>
        <span className="flex items-center gap-2 text-xs">
          {provider.needsKey ? (
            <span style={{ color: provider.hasKey ? "var(--hive-success)" : "var(--hive-accent-warm)" }}>
              {provider.hasKey ? "● key set" : "○ no key"}
            </span>
          ) : configured && provider.baseUrl.trim() !== "" ? (
            <span className="opacity-50">custom URL</span>
          ) : (
            <span className="opacity-50">{localOnly ? "local · no key" : "—"}</span>
          )}
        </span>
      </button>
      {open && (
        <div className="border-t px-3 py-3" style={{ borderColor: "var(--hive-line)" }}>
          <div className="text-xs opacity-50">{provider.note}</div>
          {provider.needsKey && (
            <div className="mt-2 flex items-center gap-2">
              <input
                type="password"
                value={key}
                onChange={(e) => setKey(e.target.value)}
                placeholder={provider.hasKey ? "configured — enter to replace" : "API key"}
                className="flex-1 rounded-xl border px-3 py-2 font-mono text-sm"
                style={inputStyle}
              />
              <SaveButton onClick={() => saveKey.mutate()} label="Save" />
            </div>
          )}
          {provider.supportsBaseUrl && (
            <div className="mt-2 flex items-center gap-2">
              <input
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
                placeholder="Base URL (optional override)"
                className="flex-1 rounded-xl border px-3 py-2 font-mono text-xs"
                style={inputStyle}
              />
              <SaveButton onClick={() => saveBase.mutate()} label="Save" />
            </div>
          )}
          <div className="mt-2 text-right">
            <button
              className="text-xs text-[color:var(--hive-danger)] hover:opacity-80"
              onClick={() => (configured ? remove.mutate() : onHide())}
            >
              Remove
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

// The Primary Runtime's model (the default when no runtime is configured) is
// editable here. Common Anthropic models + whatever's currently set.
const DEFAULT_MODELS = [
  "claude-opus-4-8",
  "claude-sonnet-4-6",
  "claude-haiku-4-5-20251001",
  "claude-3-5-sonnet-latest",
];
function DefaultModelPicker() {
  const qc = useQueryClient();
  const settings = useQuery({ queryKey: ["settings"], queryFn: getAppSettings });
  const current = settings.data?.model ?? "";
  const options = current && !DEFAULT_MODELS.includes(current) ? [current, ...DEFAULT_MODELS] : DEFAULT_MODELS;
  const save = useMutation({
    mutationFn: setDefaultModel,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["settings"] });
      qc.invalidateQueries({ queryKey: ["runtimes"] });
      toast.success("Default model updated.");
    },
    onError: (e) => toast.error(`Couldn't set model: ${errMsg(e)}`),
  });
  return (
    <div className="mb-2 rounded-2xl border p-3" style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}>
      <label className="block text-xs opacity-60">Default model (Primary Runtime · Anthropic)</label>
      <select
        value={current}
        onChange={(e) => save.mutate(e.target.value)}
        className={`mt-1 w-full ${SELECT_CLASS}`}
        style={inputStyle}
      >
        {options.map((m) => (
          <option key={m} value={m}>
            {m}
          </option>
        ))}
      </select>
      <p className="mt-1 text-xs opacity-50">
        Used when a chat has no specific runtime selected. Separate from the local Claude Code model
        (Account → Agent file access).
      </p>
    </div>
  );
}

function RuntimesSection() {
  const qc = useQueryClient();
  const runtimes = useQuery({ queryKey: ["runtimes"], queryFn: listRuntimes });
  const [runtimeId, setRuntimeId] = useState("");
  const [runtimeName, setRuntimeName] = useState("");
  const [runtimeProvider, setRuntimeProvider] = useState("ollama");
  // `location` doesn't drive any behavior (dispatch routes by provider +
  // endpoint); we derive it from the endpoint just for the list label.
  const deriveLocation = () => {
    if (["claude-code", "pi", "aider"].includes(runtimeProvider)) return "local";
    const ep = `${runtimeEndpoint} ${runtimeBaseUrl}`.toLowerCase();
    return /localhost|127\.0\.0\.1|0\.0\.0\.0|::1/.test(ep) ? "local" : "remote";
  };
  const [runtimeEndpoint, setRuntimeEndpoint] = useState("");
  const [runtimeBaseUrl, setRuntimeBaseUrl] = useState("");
  const [runtimeModel, setRuntimeModel] = useState("");
  const [runtimeSupportsTools, setRuntimeSupportsTools] = useState(true);
  const [runtimeSupportsEmbeddings, setRuntimeSupportsEmbeddings] = useState(false);
  const [runtimeContextWindow, setRuntimeContextWindow] = useState("");
  // null = the form is adding; an id = editing that runtime in place (add_runtime
  // upserts by id, so the same form both adds and edits).
  const [editingId, setEditingId] = useState<string | null>(null);
  const formRef = useRef<HTMLDivElement>(null);
  const presets = useQuery({ queryKey: ["provider-presets"], queryFn: listProviderPresets });

  function resetRuntimeForm() {
    setEditingId(null);
    setRuntimeId("");
    setRuntimeName("");
    setRuntimeProvider("ollama");
    setRuntimeEndpoint("");
    setRuntimeBaseUrl("");
    setRuntimeModel("");
    setRuntimeContextWindow("");
    setRuntimeSupportsTools(true);
    setRuntimeSupportsEmbeddings(false);
  }

  function startEdit(runtime: RuntimeSummaryDto) {
    setEditingId(runtime.id);
    setRuntimeId(runtime.id);
    setRuntimeName(runtime.name);
    setRuntimeProvider(runtime.provider);
    setRuntimeEndpoint(runtime.endpoint);
    setRuntimeBaseUrl(runtime.modelBaseUrl ?? "");
    setRuntimeModel(runtime.model);
    setRuntimeContextWindow(runtime.contextWindow ? String(runtime.contextWindow) : "");
    setRuntimeSupportsTools(runtime.supportsTools);
    setRuntimeSupportsEmbeddings(runtime.supportsEmbeddings);
    formRef.current?.scrollIntoView({ behavior: "smooth", block: "nearest" });
  }

  const addRuntimeMutation = useMutation({
    mutationFn: () =>
      addRuntime(
        runtimeId.trim(),
        runtimeName.trim(),
        runtimeProvider,
        deriveLocation(),
        runtimeEndpoint.trim(),
        runtimeModel.trim(),
        runtimeSupportsTools,
        runtimeSupportsEmbeddings,
        runtimeBaseUrl.trim() || null,
        // pi targets an OpenAI-compatible provider; default that id to "ollama".
        runtimeBaseUrl.trim() && runtimeProvider === "pi" ? "ollama" : null,
        Number(runtimeContextWindow) > 0 ? Number(runtimeContextWindow) : null,
      ),
    onSuccess: () => {
      const wasEditing = editingId !== null;
      resetRuntimeForm();
      qc.invalidateQueries({ queryKey: ["runtimes"] });
      toast.success(wasEditing ? "Runtime updated." : "Runtime added.");
    },
    onError: (e) => toast.error(`Couldn't save runtime: ${errMsg(e)}`),
  });
  const removeRuntimeMutation = useMutation({
    mutationFn: (id: string) => removeRuntime(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["runtimes"] }),
    onError: (e) => toast.error(`Couldn't remove runtime: ${errMsg(e)}`),
  });
  const setDefaultMutation = useMutation({
    mutationFn: (id: string) => setDefaultRuntime(id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["runtimes"] });
      toast.success("Default runtime updated.");
    },
    onError: (e) => toast.error(`Couldn't set default: ${errMsg(e)}`),
  });

  return (
    <Section title="Models (runtimes)">
      <p className="text-xs opacity-50">
        A model on a provider (above) — what powers a chat or agent. Pick a provider kind + model;
        the key/base URL come from the provider. Mark one <strong>default</strong> for new chats.
      </p>
      {/* The "Default model" picker only configures the synthesized Anthropic
          Primary Runtime, which is surfaced only when no real runtime is
          configured. Once you have runtimes, the default is chosen per-row
          below, so this picker would point at a runtime that isn't listed. */}
      {(runtimes.data ?? []).some((r) => r.name === "Primary Runtime") && <DefaultModelPicker />}
      <div className="space-y-2">
        {(runtimes.data ?? []).map((runtime) => (
          <div
            key={runtime.id}
            className="rounded-2xl border px-3 py-3"
            style={inputStyle}
          >
            <div className="flex items-center justify-between gap-3">
              <div>
                <div className="font-medium">{runtime.label}</div>
                <div className="text-xs opacity-50">
                  {runtime.location} · {runtime.provider}
                  {runtime.endpoint ? ` · ${runtime.endpoint}` : ""}
                </div>
              </div>
              <div className="flex items-center gap-2">
                {runtime.isManaged && (
                  <button
                    onClick={() => startEdit(runtime)}
                    className="text-xs hover:opacity-80"
                  >
                    Edit
                  </button>
                )}
                {runtime.isManaged && (
                  <button
                    onClick={() => confirmThen(`Remove runtime "${runtime.label}"?`, () => removeRuntimeMutation.mutate(runtime.id))}
                    className="text-xs text-[color:var(--hive-danger)] hover:opacity-80"
                  >
                    Remove
                  </button>
                )}
                {runtime.isDefault ? (
                  <span className="rounded-full px-2 py-1 text-xs" style={{ background: "var(--hive-mist)" }}>
                    default
                  </span>
                ) : (
                  <button
                    onClick={() => setDefaultMutation.mutate(runtime.id)}
                    disabled={setDefaultMutation.isPending}
                    className="rounded-full border px-2 py-1 text-xs hover:opacity-80 disabled:opacity-50"
                    style={{ borderColor: "var(--hive-line)" }}
                  >
                    Set default
                  </button>
                )}
              </div>
            </div>
          </div>
        ))}
      </div>
      <div
        ref={formRef}
        className="space-y-2 rounded-2xl border p-4"
        style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}
      >
        <h3 className="text-sm font-semibold uppercase tracking-wide opacity-60">
          {editingId ? `Edit runtime · ${editingId}` : "Add runtime"}
        </h3>
        <div className="grid gap-2 md:grid-cols-2">
          <input
            value={runtimeName}
            onChange={(e) => setRuntimeName(e.target.value)}
            placeholder="Display name"
            className="rounded-xl border px-3 py-2 text-sm"
            style={inputStyle}
          />
          <input
            value={runtimeId}
            onChange={(e) => setRuntimeId(e.target.value)}
            placeholder="ID (optional)"
            readOnly={editingId !== null}
            title={editingId !== null ? "The id is fixed while editing" : undefined}
            className="rounded-xl border px-3 py-2 font-mono text-sm read-only:opacity-60"
            style={inputStyle}
          />
          <select
            defaultValue=""
            title="Quick-fill a known OpenAI-compatible backend"
            onChange={(e) => {
              const p = (presets.data ?? []).find((x) => x.label === e.target.value);
              e.target.value = "";
              if (!p) return;
              setRuntimeProvider(p.provider);
              setRuntimeEndpoint(p.endpoint);
              if (!runtimeName.trim()) setRuntimeName(p.label);
              if (!runtimeId.trim())
                setRuntimeId(p.label.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, ""));
            }}
            className="rounded-xl border px-3 py-2 text-sm"
            style={inputStyle}
          >
            <option value="">Preset…</option>
            {(presets.data ?? []).map((p) => (
              <option key={p.label} value={p.label}>
                {p.label}
              </option>
            ))}
          </select>
          <select
            value={runtimeProvider}
            onChange={(e) => setRuntimeProvider(e.target.value)}
            className="rounded-xl border px-3 py-2 text-sm"
            style={inputStyle}
          >
            <option value="anthropic">Anthropic</option>
            <option value="openAI">OpenAI</option>
            <option value="openRouter">OpenRouter</option>
            <option value="ollama">Ollama</option>
            <option value="azure">Azure OpenAI</option>
            <option value="custom">Custom</option>
            <option value="claude-code">Claude Code</option>
            <option value="pi">Pi</option>
            <option value="aider">Aider</option>
          </select>
        </div>
        <input
          value={runtimeEndpoint}
          onChange={(e) => setRuntimeEndpoint(e.target.value)}
          placeholder={
            runtimeProvider === "pi" || runtimeProvider === "aider"
              ? "Executable path (blank = found on PATH)"
              : "Endpoint URL"
          }
          className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
          style={inputStyle}
        />
        {runtimeProvider === "pi" && (
          <>
            <input
              value={runtimeBaseUrl}
              onChange={(e) => setRuntimeBaseUrl(e.target.value)}
              placeholder="Ollama base URL — e.g. http://localhost:11434"
              className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
              style={inputStyle}
            />
            <p className="text-xs opacity-50">
              Points <code>pi</code> at an OpenAI-compatible backend (a local Ollama). Leave blank
              to use your own <code>pi</code> provider config / <code>pi login</code>.
            </p>
          </>
        )}
        <input
          value={runtimeModel}
          onChange={(e) => setRuntimeModel(e.target.value)}
          placeholder={runtimeProvider === "pi" ? "Model — e.g. qwen2.5-coder" : "Preferred model"}
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={inputStyle}
        />
        <input
          value={runtimeContextWindow}
          onChange={(e) => setRuntimeContextWindow(e.target.value.replace(/[^0-9]/g, ""))}
          inputMode="numeric"
          placeholder="Context window in tokens (optional — e.g. 32768)"
          className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
          style={inputStyle}
        />
        <p className="text-xs opacity-50">
          Overrides the window the context planner budgets against. Useful for Ollama/custom
          models whose window can't be inferred from the model name.
        </p>
        <label className="flex items-center gap-2 text-sm opacity-75">
          <input
            type="checkbox"
            checked={runtimeSupportsTools}
            onChange={(e) => setRuntimeSupportsTools(e.target.checked)}
          />
          Supports tools
        </label>
        <label className="flex items-center gap-2 text-sm opacity-75">
          <input
            type="checkbox"
            checked={runtimeSupportsEmbeddings}
            onChange={(e) => setRuntimeSupportsEmbeddings(e.target.checked)}
          />
          Supports embeddings
        </label>
        <div className="flex items-center gap-2">
          <SaveButton
            onClick={() => addRuntimeMutation.mutate()}
            label={editingId ? "Update runtime" : "Add runtime"}
          />
          {editingId && (
            <button
              onClick={resetRuntimeForm}
              className="rounded-xl border px-3 py-2 text-sm hover:opacity-80"
              style={{ borderColor: "var(--hive-line)" }}
            >
              Cancel
            </button>
          )}
        </div>
      </div>
    </Section>
  );
}

/// Customize the instructions behind /summarize and /compact (and the
/// automatic overflow summarization, which reuses the /summarize one).
function ContextCommandsSection() {
  const qc = useQueryClient();
  const cmds = useQuery({ queryKey: ["context-commands"], queryFn: getContextCommands });
  const [summarize, setSummarize] = useState("");
  const [compact, setCompact] = useState("");

  useEffect(() => {
    if (cmds.data) {
      setSummarize(cmds.data.summarizePrompt);
      setCompact(cmds.data.compactPrompt);
    }
  }, [cmds.data]);

  const save = useMutation({
    mutationFn: () => setContextCommands(summarize, compact),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["context-commands"] });
      toast.success("Context commands saved.");
    },
    onError: (e) => toast.error(`Couldn't save context commands: ${errMsg(e)}`),
  });

  const dflt = cmds.data?.defaultPrompt ?? "";
  const fieldClass = "w-full resize-none rounded-xl border px-3 py-2 text-sm leading-5";

  return (
    <Section title="Context commands">
      <p className="text-xs opacity-50">
        The instructions the model follows for <code>/summarize</code> and <code>/compact</code>.
        Blank = the built-in default (shown as the placeholder). The <code>/summarize</code>{" "}
        instruction also guides the automatic summarization when a long chat overflows the
        model's context window.
      </p>
      <label className="block text-sm opacity-70">
        <code>/summarize</code> instruction
      </label>
      <textarea
        value={summarize}
        onChange={(e) => setSummarize(e.target.value)}
        placeholder={dflt}
        rows={3}
        className={fieldClass}
        style={inputStyle}
      />
      <label className="block text-sm opacity-70">
        <code>/compact</code> instruction
      </label>
      <textarea
        value={compact}
        onChange={(e) => setCompact(e.target.value)}
        placeholder={dflt}
        rows={3}
        className={fieldClass}
        style={inputStyle}
      />
      <div className="flex items-center gap-3">
        <SaveButton onClick={() => save.mutate()} label="Save" />
        {(summarize.trim() !== "" || compact.trim() !== "") && (
          <button
            onClick={() => {
              setSummarize("");
              setCompact("");
            }}
            className="text-xs opacity-60 underline underline-offset-2 hover:opacity-100"
          >
            Reset to defaults (then Save)
          </button>
        )}
      </div>
    </Section>
  );
}

function TemplatesSection() {
  const [items, setItems] = useState<PromptTemplate[]>(() => loadTemplates());
  const [name, setName] = useState("");
  const [body, setBody] = useState("");

  function add() {
    if (!name.trim() || !body.trim()) return;
    setItems(addTemplate(name, body));
    setName("");
    setBody("");
    toast.success("Template saved.");
  }

  return (
    <Section title="Prompt templates">
      <p className="text-xs opacity-50">
        Reusable prompts you can drop into the composer with <code>/</code>.
      </p>
      <div className="space-y-2">
        {items.map((t) => (
          <div
            key={t.id}
            className="flex items-start justify-between gap-3 rounded-xl border px-3 py-2"
            style={inputStyle}
          >
            <div className="min-w-0">
              <div className="font-medium">{t.name}</div>
              <div className="mt-0.5 truncate text-xs opacity-55">{t.body}</div>
            </div>
            <button
              className="shrink-0 text-xs text-[color:var(--hive-danger)] hover:opacity-80"
              onClick={() => confirmThen(`Delete template "${t.name}"?`, () => setItems(removeTemplate(t.id)))}
            >
              Delete
            </button>
          </div>
        ))}
        {items.length === 0 && <p className="text-sm opacity-50">No templates yet.</p>}
      </div>
      <div className="space-y-2 rounded-2xl border p-3" style={{ borderColor: "var(--hive-line)" }}>
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Template name (e.g. Bug report)"
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={inputStyle}
        />
        <textarea
          value={body}
          onChange={(e) => setBody(e.target.value)}
          placeholder="Prompt text…"
          rows={3}
          className="w-full resize-y rounded-xl border px-3 py-2 text-sm"
          style={inputStyle}
        />
        <SaveButton onClick={add} label="Add template" />
      </div>
    </Section>
  );
}

function AppearanceSection({
  palette,
  onPaletteChange,
  appearanceMode,
  onAppearanceModeChange,
}: {
  palette: ThemeName;
  onPaletteChange: (t: ThemeName) => void;
  appearanceMode: AppearanceMode;
  onAppearanceModeChange: (m: AppearanceMode) => void;
}) {
  return (
    <Section title="Appearance">
      <label className="block text-sm opacity-70">Mode</label>
      <div className="flex gap-2">
        {(["auto", "light", "dark"] as AppearanceMode[]).map((m) => (
          <button
            key={m}
            onClick={() => onAppearanceModeChange(m)}
            className="rounded-xl border px-3 py-2 capitalize"
            style={{
              borderColor: m === appearanceMode ? "var(--hive-accent-cool)" : "var(--hive-line)",
              background: m === appearanceMode ? "var(--hive-mist)" : "transparent",
            }}
          >
            {m === "auto" ? "Auto (system)" : m}
          </button>
        ))}
      </div>
      <p className="text-xs opacity-50">
        Auto follows your operating system's light/dark setting. Every theme has a light and a dark
        variant.
      </p>

      <label className="mt-3 block text-sm opacity-70">Theme</label>
      <div className="flex flex-wrap gap-2">
        {(Object.keys(THEMES) as ThemeName[]).map((t) => (
          <button
            key={t}
            onClick={() => onPaletteChange(t)}
            className="flex items-center gap-2 rounded-xl border px-3 py-2 capitalize"
            style={{
              borderColor: t === palette ? "var(--hive-accent-cool)" : "var(--hive-line)",
              background: t === palette ? "var(--hive-mist)" : "transparent",
            }}
          >
            <span className="h-3 w-3 rounded-full" style={{ background: THEMES[t].light.accentCool }} />
            {t}
          </button>
        ))}
      </div>
    </Section>
  );
}

const PROBE_COLOR: Record<RelayProbeDto["status"], string> = {
  ok: "var(--hive-success)",
  unauthorized: "var(--hive-warn)",
  httpError: "var(--hive-warn)",
  unreachable: "var(--hive-warn)",
  unconfigured: "var(--hive-ink)",
};
const PROBE_ICON: Record<RelayProbeDto["status"], ReactNode> = {
  ok: <IconCheck size={13} />,
  unauthorized: <IconAlertTriangle size={13} />,
  httpError: <IconAlertTriangle size={13} />,
  unreachable: <IconAlertTriangle size={13} />,
  unconfigured: <IconInfo size={13} />,
};

function SyncSection() {
  const qc = useQueryClient();
  const status = useQuery({ queryKey: ["sync-status"], queryFn: syncStatus });
  const conn = useQuery({ queryKey: ["connection-settings"], queryFn: getConnectionSettings });
  const s = status.data;
  const c = conn.data;

  const [relayUrl, setRelayUrl] = useState("");
  const [room, setRoom] = useState("");
  const [workspaceKey, setWorkspaceKey] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [relayAccessToken, setRelayAccessToken] = useState("");
  const [permissionMode, setPermissionMode] = useState<ClaudePermissionMode>("default");
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [probe, setProbe] = useState<RelayProbeDto | null>(null);

  // Live reachability + auth check (distinct from the config-only sync status).
  const testConnection = useMutation({
    mutationFn: probeRelay,
    onMutate: () => setProbe(null),
    onSuccess: (r) => setProbe(r),
    onError: (e) => setProbe({ status: "unreachable", detail: errMsg(e) }),
  });

  useEffect(() => {
    if (c) {
      setRelayUrl(c.relayUrl);
      setRoom(c.room);
      setPermissionMode(c.permissionMode);
      setWorkspaceKey("");
      setApiKey("");
      setRelayAccessToken("");
    }
  }, [c]);

  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["sync-status"] });
    qc.invalidateQueries({ queryKey: ["connection-settings"] });
  };

  const save = useMutation({
    mutationFn: () =>
      updateConnectionSettings({
        relayUrl,
        room,
        // Blank = keep existing secret; a value replaces it.
        workspaceKey: workspaceKey === "" ? null : workspaceKey,
        apiKey: apiKey === "" ? null : apiKey,
        relayAccessToken: relayAccessToken === "" ? null : relayAccessToken,
        permissionMode,
      }),
    onSuccess: () => {
      refresh();
      toast.success("Connection settings saved.");
    },
    onError: (e) => toast.error(`Couldn't save connection settings: ${errMsg(e)}`),
  });

  // Clear a stored secret without touching the rest.
  const clearSecret = useMutation({
    mutationFn: (field: "workspaceKey" | "apiKey" | "relayAccessToken") =>
      updateConnectionSettings({
        relayUrl,
        room,
        workspaceKey: field === "workspaceKey" ? "" : null,
        apiKey: field === "apiKey" ? "" : null,
        relayAccessToken: field === "relayAccessToken" ? "" : null,
        permissionMode,
      }),
    onSuccess: refresh,
    onError: (e) => toast.error(`Couldn't update secret: ${errMsg(e)}`),
  });

  const fieldStyle = "w-full rounded-xl border px-3 py-2 font-mono text-sm";

  return (
    <>
      <Section title="Team sync">
        {/* Live status */}
        <div
          className="rounded-xl border px-3 py-2.5 text-sm"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
        >
          {s?.relayConfigured ? (
            <div className="flex flex-wrap items-center gap-x-1.5 gap-y-1">
              <span className="flex items-center gap-1.5 opacity-80">
                <IconHexagon size={13} /> Relay configured
              </span>
              <span className="opacity-50">·</span>
              <code className="text-xs">{s.relayUrl}</code>
              <span className="opacity-50">· room</span>
              <code className="text-xs">{s.room}</code>
              <span className="opacity-50">·</span>
              {s.encrypted ? (
                <span
                  className="flex items-center gap-1"
                  title="Messages sealed with the workspace key"
                  style={{ color: "var(--hive-success)" }}
                >
                  <IconLock size={12} /> encrypted
                </span>
              ) : (
                <span className="flex items-center gap-1" style={{ color: "var(--hive-warn)" }}>
                  <IconAlertTriangle size={12} /> plaintext
                </span>
              )}
            </div>
          ) : (
            <span className="opacity-70">Local only — not syncing with anyone yet.</span>
          )}
        </div>

        {/* Live reachability + auth — the config pill above only means a URL is
            set, so a real probe is the source of truth for "is it working". */}
        <div className="flex flex-wrap items-center gap-2">
          <Button
            onClick={() => testConnection.mutate()}
            disabled={testConnection.isPending}
          >
            {testConnection.isPending ? "Testing…" : "Test connection"}
          </Button>
          {probe && (
            <span
              className="flex items-center gap-1.5 text-xs"
              style={{ color: PROBE_COLOR[probe.status] }}
            >
              {PROBE_ICON[probe.status]}
              {probe.detail}
            </span>
          )}
        </div>

        {/* How teams work now */}
        <p className="text-xs opacity-60">
          Teams live in the left rail — hit <strong>＋</strong> to create or join one. Hive
          generates the room, encryption key, and a shareable code for you. The relay below just
          brokers sync, short pairing codes, and member revocation; messages stay end-to-end
          encrypted and the relay only ever sees ciphertext.
        </p>

        <label className="block text-sm opacity-70">Relay URL</label>
        <input
          value={relayUrl}
          onChange={(e) => setRelayUrl(e.target.value)}
          placeholder="https://relay.example  ·  blank = local only"
          className={fieldStyle}
          style={inputStyle}
        />
        <p className="text-xs opacity-50">Use the same relay URL on every device that should sync.</p>

        <label className="block text-sm opacity-70">Relay access token</label>
        <div className="flex items-center gap-2">
          <input
            type="password"
            value={relayAccessToken}
            onChange={(e) => setRelayAccessToken(e.target.value)}
            placeholder={
              c?.hasRelayAccessToken ? "configured — blank to keep" : "only for a hosted/paid relay"
            }
            className={"flex-1 " + fieldStyle}
            style={inputStyle}
          />
          {c?.hasRelayAccessToken && (
            <button
              className="text-xs text-[color:var(--hive-danger)] hover:opacity-80"
              onClick={() => clearSecret.mutate("relayAccessToken")}
            >
              clear
            </button>
          )}
        </div>
        <p className="text-xs opacity-50">
          Needed only for a gated/paid hosted relay. Leave blank for a relay you host yourself.
          Paste only the token value — if yours looks like <code>name:abc123</code>, drop the
          <code> name:</code> prefix.
        </p>

        {/* Advanced: manual room + key (the rail normally fills these in) */}
        <button
          type="button"
          className="flex items-center gap-1 text-xs opacity-60 hover:opacity-100"
          onClick={() => setShowAdvanced((v) => !v)}
        >
          {showAdvanced ? <IconChevronDown size={13} /> : <IconChevronRight size={13} />} Advanced — set room &amp; key by hand
        </button>
        {showAdvanced && (
          <div className="space-y-2 rounded-2xl border p-3" style={{ borderColor: "var(--hive-line)" }}>
            <p className="text-xs opacity-50">
              Normally the rail’s ＋ fills these in. Set them by hand only to join a specific room
              or change the passphrase directly.
            </p>
            <label className="block text-sm opacity-70">Room</label>
            <input
              value={room}
              onChange={(e) => setRoom(e.target.value)}
              placeholder="default"
              className={fieldStyle}
              style={inputStyle}
            />
            <label className="block text-sm opacity-70">Workspace key (E2EE passphrase)</label>
            <div className="flex items-center gap-2">
              <input
                type="password"
                value={workspaceKey}
                onChange={(e) => setWorkspaceKey(e.target.value)}
                placeholder={c?.hasWorkspaceKey ? "configured — blank to keep" : "passphrase"}
                className={"flex-1 " + fieldStyle}
                style={inputStyle}
              />
              {c?.hasWorkspaceKey && (
                <button className="text-xs text-[color:var(--hive-danger)] hover:opacity-80" onClick={() => clearSecret.mutate("workspaceKey")}>
                  clear
                </button>
              )}
            </div>
          </div>
        )}

        <SaveButton onClick={() => save.mutate()} label={save.isPending ? "Saving…" : "Save sync settings"} />
      </Section>

      <Section title="Agent file access">
        <p className="text-xs opacity-50">
          The setting below controls the <code>claude</code> CLI's permission mode (it runs headless,
          so it can't show an approval prompt). Other agents gate file access their own way: aider/pi
          via their flags, and API/MCP-backed agents via built-in-tool consent + per-tool MCP trust.
        </p>
        <label className="block text-xs opacity-60">Claude Code permission mode</label>
        <select
          value={permissionMode}
          onChange={(e) => setPermissionMode(e.target.value as ClaudePermissionMode)}
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={inputStyle}
        >
          <option value="default">Read-only — blocks file edits (no approval prompt)</option>
          <option value="acceptEdits">Accept file edits automatically (needed to write files)</option>
          <option value="bypassPermissions">Bypass all permissions (edits + shell commands)</option>
        </select>
        {permissionMode === "default" ? (
          <p className="text-xs opacity-60">
            In read-only mode the <code>claude</code> agent's file writes are blocked. Choose{" "}
            <strong>Accept file edits</strong> to let it create/modify files in your workspace.
          </p>
        ) : (
          <p className="text-xs" style={{ color: "var(--hive-accent-warm)" }}>
            ⚠ The <code>claude</code> agent can modify files
            {permissionMode === "bypassPermissions" ? " and run shell commands" : ""} in your
            workspace without asking.
          </p>
        )}
        <ClaudeModelPicker />
        <SaveButton onClick={() => save.mutate()} label="Save" />
      </Section>
    </>
  );
}

// Lets the user pick which model their local Claude Code CLI uses (`--model`).
// Auto-saves on change; applies to the next turn (no restart).
const CLAUDE_MODELS = [
  { value: "", label: "Default (CLI's own setting)" },
  { value: "sonnet", label: "Sonnet" },
  { value: "opus", label: "Opus" },
  { value: "haiku", label: "Haiku" },
];
function ClaudeModelPicker() {
  const qc = useQueryClient();
  const model = useQuery({ queryKey: ["claude-model"], queryFn: getClaudeCodeModel });
  const models = useQuery({ queryKey: ["claude-model-options"], queryFn: listClaudeCodeModels });
  const [custom, setCustom] = useState(false);
  const save = useMutation({
    mutationFn: setClaudeCodeModel,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["claude-model"] });
      toast.success("Claude Code model updated.");
    },
    onError: (e) => toast.error(`Couldn't set model: ${errMsg(e)}`),
  });
  const options = models.data?.length ? models.data : CLAUDE_MODELS;
  const current = model.data ?? "";
  const known = options.some((m) => m.value === current);
  const showCustom = custom || (!known && current !== "");
  return (
    <div className="mt-3">
      <label className="block text-xs opacity-60">Claude Code model</label>
      <select
        value={showCustom ? "__custom__" : current}
        onChange={(e) => {
          if (e.target.value === "__custom__") {
            setCustom(true);
          } else {
            setCustom(false);
            save.mutate(e.target.value);
          }
        }}
        className="w-full rounded-xl border px-3 py-2 text-sm"
        style={inputStyle}
      >
        {options.map((m) => (
          <option key={m.value || "default"} value={m.value}>
            {m.label}
            {"description" in m && m.description ? ` — ${m.description}` : ""}
          </option>
        ))}
        <option value="__custom__">Custom…</option>
      </select>
      {showCustom && (
        <input
          defaultValue={known ? "" : current}
          onBlur={(e) => save.mutate(e.target.value.trim())}
          placeholder="Model alias or id (e.g. fable, claude-fable-5)"
          className="mt-2 w-full rounded-xl border px-3 py-2 text-sm"
          style={inputStyle}
        />
      )}
      <p className="text-xs opacity-50">
        Which model your local <code>claude</code> uses — the list mirrors your <code>claude</code>{" "}
        <code>/model</code> options. “Default” lets the CLI decide (usually Sonnet); type your own if
        it isn’t listed. Models require your Claude subscription to include them.
      </p>
    </div>
  );
}

function GithubSection() {
  const qc = useQueryClient();
  const account = useQuery({ queryKey: ["github-account"], queryFn: githubAccount });
  const configured = useQuery({ queryKey: ["github-configured"], queryFn: githubClientConfigured });
  const [clientId, setClientId] = useState("");
  const [flow, setFlow] = useState<import("@/lib/ipc").DeviceStartDto | null>(null);
  const [status, setStatus] = useState("");

  const saveClient = useMutation({
    mutationFn: () => setGithubClientId(clientId.trim()),
    onSuccess: () => {
      setClientId("");
      qc.invalidateQueries({ queryKey: ["github-configured"] });
      toast.success("GitHub client id saved.");
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const logout = useMutation({
    mutationFn: githubLogout,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["github-account"] });
      toast.success("Signed out of GitHub.");
    },
    onError: (e) => toast.error(errMsg(e)),
  });

  async function startLogin() {
    try {
      const s = await githubLoginStart();
      setFlow(s);
      setStatus("Waiting for you to authorize on GitHub…");
      // Route through the backend opener — window.open doesn't reach the OS
      // browser from the Tauri webview.
      void openExternal(s.verificationUri).catch(() => {
        /* user can open it manually via the link below */
      });
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  // Poll for the token while a device-flow login is in progress.
  useEffect(() => {
    if (!flow) return;
    let alive = true;
    let interval = Math.max(flow.interval, 1) * 1000;
    let timer: ReturnType<typeof setTimeout>;
    const tick = async () => {
      if (!alive) return;
      try {
        const r = await githubLoginPoll(flow.deviceCode);
        if (!alive) return;
        if (r.status === "success") {
          setFlow(null);
          setStatus("");
          qc.invalidateQueries({ queryKey: ["github-account"] });
          toast.success(`Signed in as @${r.account?.login ?? "github"}.`);
          return;
        }
        if (r.status === "denied" || r.status === "expired") {
          setFlow(null);
          setStatus("");
          toast.error(r.status === "denied" ? "Authorization denied." : "Code expired — try again.");
          return;
        }
        if (r.status === "slowDown") interval += 5000;
      } catch {
        /* transient — keep polling */
      }
      if (alive) timer = setTimeout(tick, interval);
    };
    timer = setTimeout(tick, interval);
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [flow, qc]);

  const acct = account.data;

  return (
    <Section title="Account">
      {acct ? (
        <div
          className="flex items-center justify-between gap-3 rounded-xl border px-3 py-2.5"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
        >
          <div className="flex min-w-0 items-center gap-3">
            {acct.avatarUrl && <img src={acct.avatarUrl} alt="" className="h-9 w-9 rounded-full" />}
            <div className="min-w-0">
              <div className="truncate font-medium">{acct.name || acct.login}</div>
              <div className="truncate text-xs opacity-60">
                @{acct.login}
                {acct.email ? ` · ${acct.email}` : ""}
              </div>
            </div>
          </div>
          <button className="shrink-0 text-xs text-[color:var(--hive-danger)] hover:opacity-80" onClick={() => logout.mutate()}>
            Sign out
          </button>
        </div>
      ) : configured.data === false ? (
        <div className="space-y-2">
          <p className="text-xs opacity-60">
            Sign in with GitHub for one identity across all your devices. First paste a GitHub OAuth
            App <strong>client ID</strong> (GitHub → Settings → Developer settings → OAuth Apps, with
            Device Flow enabled). No client secret needed.
          </p>
          <div className="flex gap-2">
            <input
              value={clientId}
              onChange={(e) => setClientId(e.target.value)}
              placeholder="OAuth App client id (e.g. Iv1.…)"
              className="flex-1 rounded-xl border px-3 py-2 font-mono text-sm"
              style={inputStyle}
            />
            <SaveButton onClick={() => saveClient.mutate()} label="Save" />
          </div>
        </div>
      ) : flow ? (
        <div className="space-y-2 rounded-xl border px-3 py-3" style={{ borderColor: "var(--hive-accent-cool)" }}>
          <p className="text-xs opacity-60">
            Go to <code>{flow.verificationUri}</code> and enter this code:
          </p>
          <div className="flex items-center gap-3">
            <code className="text-xl font-bold tracking-[0.25em]">{flow.userCode}</code>
            <button
              className="text-xs underline opacity-70 hover:opacity-100"
              onClick={() => {
                void navigator.clipboard.writeText(flow.userCode);
                toast.success("Code copied.");
              }}
            >
              copy
            </button>
            <button
              className="text-xs underline opacity-70 hover:opacity-100"
              onClick={() => void openExternal(flow.verificationUri).catch((e) => toast.error(errMsg(e)))}
            >
              open
            </button>
          </div>
          <p className="text-xs opacity-50">{status}</p>
          <button className="text-xs opacity-60 hover:opacity-100" onClick={() => { setFlow(null); setStatus(""); }}>
            cancel
          </button>
        </div>
      ) : (
        <div className="space-y-2">
          <p className="text-xs opacity-60">
            One identity across all your devices (this Mac, your Windows box, …). Sign in to invite
            teammates by GitHub handle and attribute commits automatically.
          </p>
          <Button variant="primary" size="md" onClick={startLogin}>
            Sign in with GitHub
          </Button>
        </div>
      )}
    </Section>
  );
}

function PeersSection() {
  const qc = useQueryClient();
  const myCode = useQuery({ queryKey: ["p2p-code"], queryFn: p2pMyCode, retry: false });
  const contacts = useQuery({ queryKey: ["p2p-contacts"], queryFn: p2pListContacts });
  const [code, setCode] = useState("");
  const [label, setLabel] = useState("");
  const [shortCode, setShortCode] = useState<{ code: string; expiresIn: number } | null>(null);
  const [redeemInput, setRedeemInput] = useState("");

  const share = useMutation({
    mutationFn: p2pShareCode,
    onSuccess: (r) => setShortCode(r),
    onError: (e) => toast.error(`Couldn't make a short code: ${errMsg(e)}`),
  });
  const redeem = useMutation({
    mutationFn: () => redeemShortCode(redeemInput.trim()),
    onSuccess: (r) => {
      setRedeemInput("");
      toast.success(r.kind === "workspace" ? `Joined "${r.label}".` : "Peer added.");
      qc.invalidateQueries({ queryKey: ["p2p-contacts"] });
      qc.invalidateQueries({ queryKey: ["workspaces"] });
    },
    onError: (e) => toast.error(`Couldn't use that code: ${errMsg(e)}`),
  });

  const add = useMutation({
    mutationFn: () => p2pAddContact(code.trim(), label.trim()),
    onSuccess: () => {
      setCode("");
      setLabel("");
      toast.success("Peer added.");
      qc.invalidateQueries({ queryKey: ["p2p-contacts"] });
    },
    onError: (e) => toast.error(`Couldn't add peer: ${errMsg(e)}`),
  });
  const remove = useMutation({
    mutationFn: (id: string) => p2pRemoveContact(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["p2p-contacts"] }),
    onError: (e) => toast.error(`Couldn't remove peer: ${errMsg(e)}`),
  });

  return (
    <Section title="Direct peers (P2P)">
      <p className="text-xs opacity-50">
        Connect to a friend device-to-device (no shared room). Share your code; add theirs to sync
        directly. Falls back to a relay when a direct connection can't be made.
      </p>
      <label className="block text-sm opacity-70">Your peer code</label>
      <div className="flex gap-2">
        <input
          readOnly
          value={myCode.data ?? (myCode.isError ? "P2P unavailable in this build" : "…")}
          className="flex-1 rounded-xl border px-3 py-2 font-mono text-xs"
          style={inputStyle}
          onFocus={(e) => e.currentTarget.select()}
        />
        <Button
          variant="primary"
          size="md"
          disabled={!myCode.data}
          onClick={() => {
            if (myCode.data) {
              void navigator.clipboard.writeText(myCode.data);
              toast.success("Peer code copied.");
            }
          }}
        >
          Copy
        </Button>
        <Button
          size="md"
          disabled={!myCode.data || share.isPending}
          onClick={() => share.mutate()}
          title="Create a short, speakable code you can read out over a call"
        >
          {share.isPending ? "…" : "Short code"}
        </Button>
      </div>

      {shortCode && (
        <div
          className="flex items-center justify-between gap-3 rounded-xl border px-3 py-2"
          style={{ borderColor: "var(--hive-accent-cool)" }}
        >
          <div>
            <div className="text-[10px] font-semibold uppercase tracking-wider opacity-50">
              Share this code (expires in {Math.round(shortCode.expiresIn / 60)} min)
            </div>
            <div className="font-mono text-2xl font-bold tracking-[0.2em]">{shortCode.code}</div>
          </div>
          <Button
            variant="primary"
            size="md"
            className="shrink-0"
            onClick={() => {
              void navigator.clipboard.writeText(shortCode.code);
              toast.success("Short code copied.");
            }}
          >
            Copy
          </Button>
        </div>
      )}

      <div className="flex gap-2 rounded-2xl border p-3" style={{ borderColor: "var(--hive-line)" }}>
        <input
          value={redeemInput}
          onChange={(e) => setRedeemInput(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && redeemInput.trim() && redeem.mutate()}
          placeholder="Got a short code? e.g. K7P2QX"
          className="flex-1 rounded-xl border px-3 py-2 font-mono text-sm uppercase"
          style={inputStyle}
        />
        <Button
          variant="primary"
          size="md"
          className="shrink-0"
          disabled={!redeemInput.trim() || redeem.isPending}
          onClick={() => redeem.mutate()}
        >
          {redeem.isPending ? "…" : "Use code"}
        </Button>
      </div>

      <div className="space-y-2">
        {(contacts.data ?? []).map((c) => (
          <div
            key={c.peerId}
            className="flex items-center justify-between gap-3 rounded-xl border px-3 py-2"
            style={inputStyle}
          >
            <div className="min-w-0">
              <div className="font-medium">{c.label || "Unnamed peer"}</div>
              <div className="truncate font-mono text-xs opacity-50">{c.peerId}</div>
            </div>
            <button
              className="shrink-0 text-xs text-[color:var(--hive-danger)] hover:opacity-80"
              onClick={() => confirmThen(`Remove peer "${c.label || c.peerId}"?`, () => remove.mutate(c.peerId))}
            >
              Remove
            </button>
          </div>
        ))}
        {(contacts.data ?? []).length === 0 && <p className="text-sm opacity-50">No peers added yet.</p>}
      </div>

      <div className="space-y-2 rounded-2xl border p-3" style={{ borderColor: "var(--hive-line)" }}>
        <input
          value={code}
          onChange={(e) => setCode(e.target.value)}
          placeholder="Friend's full peer code (hive_…)"
          className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
          style={inputStyle}
        />
        <input
          value={label}
          onChange={(e) => setLabel(e.target.value)}
          placeholder="Label (e.g. Sam's laptop)"
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={inputStyle}
        />
        <SaveButton onClick={() => add.mutate()} label="Add peer" />
      </div>
    </Section>
  );
}

function McpSection() {
  const qc = useQueryClient();
  const servers = useQuery({ queryKey: ["mcp"], queryFn: listMcpServers });
  const [source, setSource] = useState("");
  const toggle = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) =>
      setMcpEnabled(id, enabled),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["mcp"] }),
  });
  const install = useMutation({
    mutationFn: () => installMcpServer(source.trim()),
    onSuccess: () => {
      setSource("");
      qc.invalidateQueries({ queryKey: ["mcp"] });
    },
  });
  const remove = useMutation({
    mutationFn: (serverId: string) => removeMcpServer(serverId),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["mcp"] }),
  });
  const addLinear = useMutation({
    mutationFn: () => addRemoteMcpServer("linear", "https://mcp.linear.app/sse"),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["mcp"] });
      toast.success("Linear added — enable it, then Connect to authorize.");
    },
    onError: (e) => toast.error(`Couldn't add Linear: ${errMsg(e)}`),
  });
  const connect = useMutation({
    mutationFn: async (id: string) => {
      // First connect: capture the OAuth app's Client ID (+ secret for
      // confidential providers like Linear). Leave blank to reuse stored creds.
      const clientId = await promptDialog(`OAuth Client ID for "${id}"`, {
        placeholder: "blank = already configured",
      });
      if (clientId && clientId.trim()) {
        const secret =
          (await promptDialog("Client secret", {
            placeholder: "blank for public/PKCE-only clients",
            password: true,
          })) ?? "";
        await setMcpOauthClient(id, clientId.trim(), secret.trim() || undefined);
      }
      await authorizeMcpServer(id, id === "linear" ? "read,write,issues:create" : undefined);
    },
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["mcp"] });
      toast.success("Authorized — your browser completed the sign-in.");
    },
    onError: (e) => toast.error(`Authorization failed: ${errMsg(e)}`),
  });
  const hasLinear = (servers.data ?? []).some((s) => s.id === "linear");
  return (
    <Section title="MCP servers">
      <p className="text-xs opacity-50">
        Servers stay inert until enabled. Add a hosted server (e.g. Linear) and Connect to authorize
        it, or install a workspace-scoped entry from a manifest URL or GitHub reference.
      </p>
      {!hasLinear && (
        <Button
          onClick={() => addLinear.mutate()}
          disabled={addLinear.isPending}
          className="self-start"
        >
          <IconPlus size={15} /> Add Linear (issues)
        </Button>
      )}
      <div className="flex gap-2">
        <input
          value={source}
          onChange={(e) => setSource(e.target.value)}
          placeholder="owner/repo/mcp.json or https://…"
          className="flex-1 rounded-xl border px-3 py-2 font-mono text-sm"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
        />
        <SaveButton onClick={() => install.mutate()} label="Install" />
      </div>
      {servers.data?.length === 0 && (
        <p className="text-sm opacity-50">None configured.</p>
      )}
      {(servers.data ?? []).map((s) => (
        <label
          key={s.id}
          className="flex items-center justify-between rounded-xl border px-3 py-2"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
        >
          <span>
            <span className="font-medium">{s.id}</span>{" "}
            <span className="text-xs opacity-50">
              [{s.transport}] {s.detail}
            </span>
          </span>
          <span className="flex items-center gap-3">
            {s.transport === "http" && (
              <button
                className="text-xs underline opacity-70 hover:opacity-100 disabled:opacity-40"
                disabled={connect.isPending}
                onClick={() => connect.mutate(s.id)}
                title="Authorize this server in your browser (OAuth)"
              >
                Connect
              </button>
            )}
            <input
              type="checkbox"
              checked={s.enabled}
              onChange={(e) => toggle.mutate({ id: s.id, enabled: e.target.checked })}
            />
            {s.isManaged && (
              <button className="text-xs text-[color:var(--hive-danger)] hover:opacity-80" onClick={() => remove.mutate(s.id)}>
                Remove
              </button>
            )}
          </span>
        </label>
      ))}
    </Section>
  );
}

// A true "factory reset" — wipes the local DB, identity, keys, settings, and
// workspaces, then relaunches fresh. This is the supported way to start over
// (uninstalling leaves the data dir behind on every OS), so testers never have
// to hand-delete a hidden app-data folder.
// Auto-updater (#144). The button works once the updater is configured (signing
// keys + a published latest.json); until then it reports that gracefully.
function UpdatesSection() {
  const check = useMutation({
    mutationFn: checkForUpdate,
    onSuccess: (version) => {
      if (version) toast.success(`Update available: v${version}. Download from the latest release.`);
      else toast.success("You're on the latest version.");
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  return (
    <Section title="Updates">
      <div className="flex items-center justify-between gap-3">
        <p className="text-sm opacity-60">Check whether a newer signed build is available.</p>
        <Button
          onClick={() => check.mutate()}
          disabled={check.isPending}
          className="shrink-0"
        >
          {check.isPending ? "Checking…" : "Check for updates"}
        </Button>
      </div>
    </Section>
  );
}

function DangerZoneSection() {
  async function reset() {
    try {
      // Clear the webview's own UI state too (theme prefs, etc.) so the relaunch
      // is genuinely a clean slate. The backend wipes its files on next launch.
      try {
        localStorage.clear();
      } catch {
        /* ignore */
      }
      await resetLocalData(); // backend writes the sentinel and restarts the app
    } catch (e) {
      toast.error(`Couldn't reset: ${errMsg(e)}`);
    }
  }
  return (
    <Section title="Danger zone">
      <div
        className="rounded-2xl border p-4"
        style={{ borderColor: "rgba(200,70,70,0.35)", background: "rgba(200,70,70,0.08)" }}
      >
        <div className="font-medium">Reset local data</div>
        <p className="mt-1 text-sm opacity-60">
          Permanently deletes this device's chats, identity, keys, settings, and workspaces, then
          restarts Hive fresh. Anything synced to teammates or a relay is unaffected. This can't be
          undone.
        </p>
        <Button
          variant="danger"
          size="md"
          className="mt-3"
          onClick={() =>
            confirmThen(
              "Reset all local data? This deletes your chats, identity, and settings on this device and restarts Hive. This cannot be undone.",
              () => void reset(),
            )
          }
        >
          Reset local data…
        </Button>
      </div>
    </Section>
  );
}

function triggerSummary(t: ScheduleTrigger): string {
  if (t.kind === "interval") {
    const s = t.every_secs;
    if (s % 3600 === 0) return `every ${s / 3600} h`;
    if (s % 60 === 0) return `every ${s / 60} min`;
    return `every ${s}s`;
  }
  return `daily at ${String(t.hour).padStart(2, "0")}:${String(t.minute).padStart(2, "0")} UTC`;
}

// Scheduled / triggered agents: each fires a prompt on a recurring schedule
// into a fresh chat. Times are UTC (interval triggers are tz-independent).
function SchedulesSection() {
  const qc = useQueryClient();
  const schedules = useQuery({ queryKey: ["schedules"], queryFn: listSchedules });
  const runtimes = useQuery({ queryKey: ["runtimes"], queryFn: listRuntimes });

  const [label, setLabel] = useState("");
  const [prompt, setPrompt] = useState("");
  const [runtimeId, setRuntimeId] = useState("");
  const [mode, setMode] = useState<"interval" | "daily_at">("interval");
  const [everyMin, setEveryMin] = useState("60");
  const [atTime, setAtTime] = useState("09:00");

  const refresh = () => qc.invalidateQueries({ queryKey: ["schedules"] });
  const remove = useMutation({ mutationFn: removeSchedule, onSuccess: refresh });
  const toggle = useMutation({
    mutationFn: (v: { id: string; enabled: boolean }) => setScheduleEnabled(v.id, v.enabled),
    onSuccess: refresh,
  });
  const add = useMutation({
    mutationFn: () => {
      let trigger: ScheduleTrigger;
      if (mode === "interval") {
        const mins = Math.max(1, Math.round(Number(everyMin) || 0));
        trigger = { kind: "interval", every_secs: mins * 60 };
      } else {
        const [h, m] = atTime.split(":").map((n) => parseInt(n, 10));
        trigger = { kind: "daily_at", hour: h || 0, minute: m || 0 };
      }
      return addSchedule({ label, prompt, runtimeId: runtimeId || undefined, trigger });
    },
    onSuccess: () => {
      setLabel("");
      setPrompt("");
      refresh();
      toast.success("Schedule added.");
    },
    onError: (e) => toast.error(`Couldn't add schedule: ${errMsg(e)}`),
  });

  return (
    <Section title="Schedules">
      <p className="text-sm opacity-60">
        Run an agent on a recurring schedule — each fire opens a new chat, posts the prompt, and the
        agent answers. Times are UTC.
      </p>

      {(schedules.data ?? []).map((s) => (
        <div
          key={s.id}
          className="flex items-start justify-between gap-3 rounded-lg border p-3"
          style={{ borderColor: "var(--hive-line)" }}
        >
          <div className="min-w-0">
            <div className="font-medium">{s.label || "Scheduled run"}</div>
            <div className="text-xs opacity-60">{triggerSummary(s.trigger)}</div>
            <div className="mt-1 truncate text-sm opacity-75">{s.prompt}</div>
          </div>
          <div className="flex shrink-0 items-center gap-3">
            <label className="flex items-center gap-1 text-xs opacity-70">
              <input
                type="checkbox"
                checked={s.enabled}
                onChange={(e) => toggle.mutate({ id: s.id, enabled: e.target.checked })}
              />
              on
            </label>
            <button
              className="text-xs text-[color:var(--hive-danger)] hover:opacity-80"
              onClick={() => confirmThen(`Remove schedule "${s.label || "Scheduled run"}"?`, () => remove.mutate(s.id))}
            >
              Remove
            </button>
          </div>
        </div>
      ))}
      {(schedules.data ?? []).length === 0 && (
        <p className="text-sm opacity-50">No schedules yet.</p>
      )}

      {/* Add form */}
      <div className="mt-2 space-y-2 rounded-lg border p-3" style={{ borderColor: "var(--hive-line)" }}>
        <div className="text-sm font-medium">New schedule</div>
        <input
          value={label}
          onChange={(e) => setLabel(e.target.value)}
          placeholder="Label (e.g. Daily standup digest)"
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={inputStyle}
        />
        <textarea
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          placeholder="Prompt to send each run…"
          rows={2}
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={inputStyle}
        />
        <div className="flex flex-wrap items-center gap-2">
          <select
            value={mode}
            onChange={(e) => setMode(e.target.value as "interval" | "daily_at")}
            className="rounded-xl border px-3 py-2 text-sm"
            style={inputStyle}
          >
            <option value="interval">Every</option>
            <option value="daily_at">Daily at</option>
          </select>
          {mode === "interval" ? (
            <>
              <input
                type="number"
                min={1}
                value={everyMin}
                onChange={(e) => setEveryMin(e.target.value)}
                className="w-20 rounded-xl border px-2 py-1.5 text-sm"
                style={inputStyle}
              />
              <span className="text-sm opacity-70">minutes</span>
            </>
          ) : (
            <input
              type="time"
              value={atTime}
              onChange={(e) => setAtTime(e.target.value)}
              className="rounded-xl border px-3 py-2 text-sm"
              style={inputStyle}
            />
          )}
          <select
            value={runtimeId}
            onChange={(e) => setRuntimeId(e.target.value)}
            className="rounded-xl border px-3 py-2 text-sm"
            style={inputStyle}
          >
            <option value="">Default runtime</option>
            {(runtimes.data ?? []).map((r) => (
              <option key={r.id} value={r.id}>
                {r.label}
              </option>
            ))}
          </select>
          <Button
            variant="primary"
            disabled={!prompt.trim() || add.isPending}
            onClick={() => add.mutate()}
          >
            Add schedule
          </Button>
        </div>
      </div>
    </Section>
  );
}

/// Relay access-user management, driven by the enterprise relay's admin API.
/// Only rendered meaningfully when the signed-in user is a relay admin; for
/// everyone else the list query fails and we show a quiet hint instead.
function RelayUsersSection() {
  const qc = useQueryClient();
  const users = useQuery({
    queryKey: ["relay-users"],
    queryFn: listRelayUsers,
    retry: false,
  });

  const [name, setName] = useState("");
  const [login, setLogin] = useState("");
  const [creating, setCreating] = useState(false);
  // The one-time raw token to surface after create/issue.
  const [issued, setIssued] = useState<{ who: string; raw: string } | null>(null);

  const refresh = () => qc.invalidateQueries({ queryKey: ["relay-users"] });

  async function create() {
    const n = name.trim();
    if (!n || creating) return;
    setCreating(true);
    try {
      const res = await createRelayUser(n, login.trim());
      setIssued({ who: res.userName, raw: res.raw });
      setName("");
      setLogin("");
      refresh();
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setCreating(false);
    }
  }

  async function addToken(u: RelayUserDto) {
    try {
      const res = await issueRelayToken(u.id, "");
      setIssued({ who: res.userName, raw: res.raw });
      refresh();
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  async function act(fn: () => Promise<void>) {
    try {
      await fn();
      refresh();
    } catch (e) {
      toast.error(errMsg(e));
    }
  }

  // Not an admin / relay has no admin API: keep the panel quiet, not alarming.
  if (users.isError) {
    return (
      <Section title="Team members">
        <p className="text-xs opacity-55">
          Managing relay access here needs an admin account on a relay that supports user
          management. Ask your relay operator to add your GitHub login to its admin list.
        </p>
      </Section>
    );
  }

  return (
    <Section title="Team members">
      <p className="text-xs opacity-60">
        Give a teammate their own relay access token — no redeploy, and revoking is instant.
        The token is shown once; paste it into their Hive under Settings → Team.
      </p>

      {/* One-time token reveal */}
      {issued && (
        <div
          className="rounded-2xl border p-3"
          style={{ borderColor: "var(--hive-accent-cool)", background: "var(--hive-mist)" }}
        >
          <div className="text-xs font-semibold">
            New token for {issued.who} — copy it now, it won’t be shown again
          </div>
          <div className="mt-2 flex items-center gap-2">
            <code className="min-w-0 flex-1 truncate rounded-lg border px-2 py-1 text-xs" style={inputStyle}>
              {issued.raw}
            </code>
            <Button
              size="sm"
              onClick={() => {
                navigator.clipboard.writeText(issued.raw);
                toast.success("Token copied.");
              }}
            >
              Copy
            </Button>
            <Button size="sm" onClick={() => setIssued(null)}>
              Done
            </Button>
          </div>
        </div>
      )}

      {/* Create */}
      <div className="flex flex-wrap items-end gap-2">
        <div className="min-w-0 flex-1">
          <label className="block text-xs opacity-70">Name</label>
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && create()}
            placeholder="Teammate name"
            className="w-full rounded-xl border px-3 py-2 text-sm"
            style={inputStyle}
          />
        </div>
        <div className="min-w-0 flex-1">
          <label className="block text-xs opacity-70">GitHub login (optional)</label>
          <input
            value={login}
            onChange={(e) => setLogin(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && create()}
            placeholder="octocat"
            className="w-full rounded-xl border px-3 py-2 text-sm"
            style={inputStyle}
          />
        </div>
        <Button variant="primary" disabled={!name.trim() || creating} onClick={create}>
          {creating ? "Adding…" : "Add member"}
        </Button>
      </div>

      {/* List */}
      <div className="space-y-1.5">
        {(users.data ?? []).map((u) => (
          <div key={u.id} className="rounded-2xl border px-3 py-2.5" style={inputStyle}>
            <div className="flex items-center justify-between gap-2">
              <div className="min-w-0">
                <span className="text-sm font-medium">{u.name}</span>
                {u.login && <span className="ml-1.5 text-xs opacity-55">@{u.login}</span>}
                {u.disabled && (
                  <span className="ml-1.5 text-[10px] uppercase tracking-wider" style={{ color: "var(--hive-danger)" }}>
                    disabled
                  </span>
                )}
                <div className="text-xs opacity-50">
                  {u.tokens.length} active token{u.tokens.length === 1 ? "" : "s"}
                </div>
              </div>
              <div className="flex shrink-0 gap-1 text-xs">
                <Button size="sm" onClick={() => addToken(u)}>
                  Add token
                </Button>
                <Button
                  size="sm"
                  onClick={() => act(() => setRelayUserDisabled(u.id, !u.disabled))}
                >
                  {u.disabled ? "Enable" : "Disable"}
                </Button>
              </div>
            </div>
            {u.tokens.length > 0 && (
              <div className="mt-2 space-y-1 border-t pt-2" style={{ borderColor: "var(--hive-line)" }}>
                {u.tokens.map((t) => (
                  <div key={t.id} className="flex items-center justify-between gap-2 text-xs">
                    <span className="min-w-0 truncate opacity-70">
                      {t.label || "token"}
                      {t.lastUsed ? ` · last used ${new Date(t.lastUsed).toLocaleDateString()}` : " · never used"}
                    </span>
                    <button
                      className="shrink-0 underline opacity-70 hover:opacity-100"
                      style={{ color: "var(--hive-danger)" }}
                      onClick={() =>
                        confirmThen("Revoke this token? The device using it stops syncing.", () =>
                          act(() => revokeRelayToken(t.id)),
                        )
                      }
                    >
                      Revoke
                    </button>
                  </div>
                ))}
              </div>
            )}
          </div>
        ))}
        {!users.isLoading && (users.data ?? []).length === 0 && (
          <p className="text-xs opacity-50">No team members yet — add one above.</p>
        )}
      </div>
    </Section>
  );
}

function Section({ title, action, children }: { title: string; action?: ReactNode; children: ReactNode }) {
  return (
    <section className="space-y-2.5">
      <div className="flex items-center justify-between gap-2">
        <h2 className="text-xs font-semibold uppercase tracking-[0.16em] opacity-60">{title}</h2>
        {action}
      </div>
      {children}
    </section>
  );
}

function SaveButton({ onClick, label = "Save" }: { onClick: () => void; label?: string }) {
  return (
    <Button variant="primary" size="md" onClick={onClick}>
      {label}
    </Button>
  );
}
