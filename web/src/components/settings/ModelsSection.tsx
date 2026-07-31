import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { RuntimeSummaryDto } from "@/bindings/RuntimeSummaryDto";
import {
  addRuntime,
  getAppSettings,
  getClaudeCodeModel,
  setClaudeCodeModel,
  listClaudeCodeModels,
  setDefaultModel,
  listRuntimes,
  removeRuntime,
  testRuntime,
  type RuntimeTestResult,
  setDefaultRuntime,
  listProviders,
  listProviderPresets,
  setProviderKey,
  setProviderBaseUrl,
  type ProviderDto,
  listAgentTemplates,
  addAgentTemplate,
  removeAgentTemplate,
  getContextCommands,
  setContextCommands,
} from "@/lib/ipc";
import { IconChevronDown, IconChevronRight } from "@/lib/icons";
import { Button, Section, Switch, fieldStyle } from "@/components/ui";
import { SELECT_CLASS } from "@/components/settings/shared";
import { toast, errMsg } from "@/components/Toast";
import { confirmThen } from "@/lib/confirm";
import { loadTemplates, addTemplate, removeTemplate, type PromptTemplate } from "@/lib/templates";

/// Models & runtimes: providers + their credentials, the runtimes (models)
/// built on them, reusable agent personas, prompt templates, and the context
/// commands. Settings owns installing/configuring; the Tools pane attaches.
export function ModelsSection() {
  return (
    <>
      <ProvidersSection />
      <RuntimesSection />
      <AgentsSection />
      <TemplatesSection />
      <ContextCommandsSection />
    </>
  );
}

/// A provider counts as "configured" once it has a key or a custom base URL —
/// i.e. the user set it up. Those show in the list; everything else is tucked
/// behind the "Add a provider…" selector so the section stays short.
function isProviderConfigured(p: ProviderDto): boolean {
  return p.hasKey || (p.supportsBaseUrl && p.baseUrl.trim() !== "");
}

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
          <p className="rounded-xl border px-3 py-4 text-xs opacity-60" style={fieldStyle}>
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
            style={fieldStyle}
          >
            <option value="">Add a provider…</option>
            {available.map((p) => (
              <option key={p.kind} value={p.kind}>
                {p.name}
              </option>
            ))}
          </select>
          <Button variant="primary" size="md" onClick={addProvider}>
            Add
          </Button>
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
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const saveBase = useMutation({
    mutationFn: () => setProviderBaseUrl(provider.kind, baseUrl.trim()),
    onSuccess: () => onChanged(),
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
    },
    onError: (e) => toast.error(errMsg(e)),
  });

  const localOnly = !provider.needsKey && !provider.supportsBaseUrl;
  return (
    <div className="rounded-xl border" style={fieldStyle}>
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
                style={fieldStyle}
              />
              <Button variant="primary" size="md" onClick={() => saveKey.mutate()}>
                Save
              </Button>
            </div>
          )}
          {provider.supportsBaseUrl && (
            <div className="mt-2 flex items-center gap-2">
              <input
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
                placeholder="Base URL (optional override)"
                className="flex-1 rounded-xl border px-3 py-2 font-mono text-xs"
                style={fieldStyle}
              />
              <Button variant="primary" size="md" onClick={() => saveBase.mutate()}>
                Save
              </Button>
            </div>
          )}
          <div className="mt-2 text-right">
            <button
              className="text-xs hover:opacity-80"
              style={{ color: "var(--hive-danger)" }}
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
        style={fieldStyle}
      >
        {options.map((m) => (
          <option key={m} value={m}>
            {m}
          </option>
        ))}
      </select>
      <p className="mt-1 text-xs opacity-50">
        Used when a chat has no specific runtime selected. Separate from the local Claude Code model
        (set on its runtime row above).
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
    if (["claude-code", "pi", "aider", "codex", "hermes"].includes(runtimeProvider)) return "local";
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

  // Per-runtime preflight state: { pending } while testing, { result } after.
  const [tests, setTests] = useState<
    Record<string, { pending?: boolean; result?: RuntimeTestResult }>
  >({});
  const runTest = async (id: string) => {
    setTests((t) => ({ ...t, [id]: { pending: true } }));
    try {
      const result = await testRuntime(id);
      setTests((t) => ({ ...t, [id]: { result } }));
    } catch (e) {
      setTests((t) => ({
        ...t,
        [id]: { result: { ok: false, latency_ms: 0, reply: "", error: errMsg(e) } },
      }));
    }
  };

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
      resetRuntimeForm();
      qc.invalidateQueries({ queryKey: ["runtimes"] });
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
    onSuccess: () => qc.invalidateQueries({ queryKey: ["runtimes"] }),
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
          <div key={runtime.id} className="rounded-2xl border px-3 py-3" style={fieldStyle}>
            <div className="flex items-center justify-between gap-3">
              <div>
                <div className="font-medium">{runtime.label}</div>
                <div className="text-xs opacity-50">
                  {runtime.location} · {runtime.provider}
                  {runtime.endpoint ? ` · ${runtime.endpoint}` : ""}
                </div>
                {runtime.provider === "claude-code" && !runtime.isManaged && <ClaudeCodeRowModel />}
              </div>
              <div className="flex items-center gap-2">
                <button
                  onClick={() => runTest(runtime.id)}
                  disabled={tests[runtime.id]?.pending}
                  className="text-xs hover:opacity-80 disabled:opacity-50"
                  title="Send a trivial prompt and verify this runtime answers"
                >
                  {tests[runtime.id]?.pending ? "Testing…" : "Test"}
                </button>
                {runtime.isManaged && (
                  <button onClick={() => startEdit(runtime)} className="text-xs hover:opacity-80">
                    Edit
                  </button>
                )}
                {runtime.isManaged && (
                  <button
                    onClick={() => confirmThen(`Remove runtime "${runtime.label}"?`, () => removeRuntimeMutation.mutate(runtime.id))}
                    className="text-xs hover:opacity-80"
                    style={{ color: "var(--hive-danger)" }}
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
            {tests[runtime.id]?.result && (
              <div className="mt-2 rounded-xl px-3 py-2 text-xs" style={fieldStyle}>
                {tests[runtime.id]!.result!.ok ? (
                  <span style={{ color: "var(--hive-success, #3fb950)" }}>
                    ✓ Responded in {tests[runtime.id]!.result!.latency_ms} ms
                    {tests[runtime.id]!.result!.reply
                      ? ` — "${tests[runtime.id]!.result!.reply.slice(0, 60)}"`
                      : ""}
                  </span>
                ) : (
                  <span style={{ color: "var(--hive-danger)" }}>
                    ✕ {tests[runtime.id]!.result!.error ?? "No response"}
                  </span>
                )}
              </div>
            )}
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
            style={fieldStyle}
          />
          <input
            value={runtimeId}
            onChange={(e) => setRuntimeId(e.target.value)}
            placeholder="ID (optional)"
            readOnly={editingId !== null}
            title={editingId !== null ? "The id is fixed while editing" : undefined}
            className="rounded-xl border px-3 py-2 font-mono text-sm read-only:opacity-60"
            style={fieldStyle}
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
            style={fieldStyle}
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
            style={fieldStyle}
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
            <option value="codex">Codex</option>
            <option value="hermes">Hermes (generic CLI)</option>
          </select>
        </div>
        <input
          value={runtimeEndpoint}
          onChange={(e) => setRuntimeEndpoint(e.target.value)}
          placeholder={
            ["pi", "aider", "codex", "hermes"].includes(runtimeProvider)
              ? "Executable path (blank = found on PATH)"
              : "Endpoint URL"
          }
          className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
          style={fieldStyle}
        />
        {runtimeProvider === "pi" && (
          <>
            <input
              value={runtimeBaseUrl}
              onChange={(e) => setRuntimeBaseUrl(e.target.value)}
              placeholder="Ollama base URL — e.g. http://localhost:11434"
              className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
              style={fieldStyle}
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
          style={fieldStyle}
        />
        <input
          value={runtimeContextWindow}
          onChange={(e) => setRuntimeContextWindow(e.target.value.replace(/[^0-9]/g, ""))}
          inputMode="numeric"
          placeholder="Context window in tokens (optional — e.g. 32768)"
          className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
          style={fieldStyle}
        />
        <p className="text-xs opacity-50">
          Overrides the window the context planner budgets against. Useful for Ollama/custom
          models whose window can't be inferred from the model name.
        </p>
        <div className="flex items-center gap-2 text-sm opacity-75">
          <Switch on={runtimeSupportsTools} onChange={setRuntimeSupportsTools} label="Supports tools" />
          <span>Supports tools</span>
        </div>
        <div className="flex items-center gap-2 text-sm opacity-75">
          <Switch on={runtimeSupportsEmbeddings} onChange={setRuntimeSupportsEmbeddings} label="Supports embeddings" />
          <span>Supports embeddings</span>
        </div>
        <div className="flex items-center gap-2">
          <Button variant="primary" size="md" onClick={() => addRuntimeMutation.mutate()}>
            {editingId ? "Update runtime" : "Add runtime"}
          </Button>
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

// Lets the user pick which model their local Claude Code CLI uses (`--model`).
// Auto-saves on change; applies to the next turn (no restart).
const CLAUDE_MODELS = [
  { value: "", label: "Default (CLI's own setting)" },
  { value: "sonnet", label: "Sonnet" },
  { value: "opus", label: "Opus" },
  { value: "haiku", label: "Haiku" },
];

// Compact model selector shown inline on the Claude Code runtime row. Claude
// Code isn't a config runtime (it's the local `claude` CLI), so its "model" is
// the `--model` alias in settings, not something the generic Edit form touches —
// this edits it in place where the runtime is listed.
function ClaudeCodeRowModel() {
  const qc = useQueryClient();
  const model = useQuery({ queryKey: ["claude-model"], queryFn: getClaudeCodeModel });
  const models = useQuery({ queryKey: ["claude-model-options"], queryFn: listClaudeCodeModels });
  const [custom, setCustom] = useState(false);
  const save = useMutation({
    mutationFn: setClaudeCodeModel,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["claude-model"] });
      qc.invalidateQueries({ queryKey: ["claude-model-options"] });
      qc.invalidateQueries({ queryKey: ["runtimes"] });
    },
    onError: (e) => toast.error(`Couldn't set model: ${errMsg(e)}`),
  });
  const options = models.data?.length ? models.data : CLAUDE_MODELS;
  const current = model.data ?? "";
  const known = options.some((m) => m.value === current);
  const showCustom = custom || (!known && current !== "");
  return (
    <div className="mt-2 flex items-center gap-2">
      <select
        value={showCustom ? "__custom__" : current}
        onChange={(e) => {
          if (e.target.value === "__custom__") setCustom(true);
          else {
            setCustom(false);
            save.mutate(e.target.value);
          }
        }}
        className="rounded-lg border px-2 py-1 text-xs"
        style={fieldStyle}
        aria-label="Claude Code model"
      >
        {options.map((m) => (
          <option key={m.value || "default"} value={m.value}>{`Model: ${m.label}`}</option>
        ))}
        <option value="__custom__">Model: Custom…</option>
      </select>
      {showCustom && (
        <input
          defaultValue={known ? "" : current}
          onBlur={(e) => save.mutate(e.target.value.trim())}
          placeholder="alias or id (e.g. fable)"
          className="rounded-lg border px-2 py-1 text-xs"
          style={fieldStyle}
          aria-label="Custom Claude Code model"
        />
      )}
    </div>
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
            <div key={t.id} className="flex items-center justify-between gap-2 rounded-xl border px-3 py-2" style={fieldStyle}>
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
                className="shrink-0 text-xs hover:opacity-80"
                style={{ color: "var(--hive-danger)" }}
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
          style={fieldStyle}
        />
        <select
          value={runtimeId || rts[0]?.id || ""}
          onChange={(e) => setRuntimeId(e.target.value)}
          className="w-full rounded-xl border px-3 py-2 text-sm"
          style={fieldStyle}
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
          style={fieldStyle}
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
          style={fieldStyle}
        />
        <Button variant="primary" size="md" onClick={() => add.mutate()}>
          Save agent
        </Button>
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
            style={fieldStyle}
          >
            <div className="min-w-0">
              <div className="font-medium">{t.name}</div>
              <div className="mt-0.5 truncate text-xs opacity-55">{t.body}</div>
            </div>
            <button
              className="shrink-0 text-xs hover:opacity-80"
              style={{ color: "var(--hive-danger)" }}
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
          style={fieldStyle}
        />
        <textarea
          value={body}
          onChange={(e) => setBody(e.target.value)}
          placeholder="Prompt text…"
          rows={3}
          className="w-full resize-y rounded-xl border px-3 py-2 text-sm"
          style={fieldStyle}
        />
        <Button variant="primary" size="md" onClick={add}>
          Add template
        </Button>
      </div>
    </Section>
  );
}

/// Customize the instructions behind /summarize and /compact (and the automatic
/// overflow summarization, which reuses the /summarize one). Writes through on
/// blur (D18) — no Save button.
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

  async function commit(nextSummarize: string, nextCompact: string) {
    if (
      nextSummarize === (cmds.data?.summarizePrompt ?? "") &&
      nextCompact === (cmds.data?.compactPrompt ?? "")
    )
      return;
    try {
      await setContextCommands(nextSummarize, nextCompact);
      qc.invalidateQueries({ queryKey: ["context-commands"] });
    } catch (e) {
      toast.error(`Couldn't save context commands: ${errMsg(e)}`);
    }
  }

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
        onBlur={() => void commit(summarize, compact)}
        placeholder={dflt}
        rows={3}
        className={fieldClass}
        style={fieldStyle}
      />
      <label className="block text-sm opacity-70">
        <code>/compact</code> instruction
      </label>
      <textarea
        value={compact}
        onChange={(e) => setCompact(e.target.value)}
        onBlur={() => void commit(summarize, compact)}
        placeholder={dflt}
        rows={3}
        className={fieldClass}
        style={fieldStyle}
      />
      {(summarize.trim() !== "" || compact.trim() !== "") && (
        <button
          onClick={() => {
            setSummarize("");
            setCompact("");
            void commit("", "");
          }}
          className="text-xs underline underline-offset-2 opacity-60 hover:opacity-100"
        >
          Reset to defaults
        </button>
      )}
    </Section>
  );
}
