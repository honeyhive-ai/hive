import { useEffect, useState } from "react";
import {
  setDisplayName,
  setGitEmail,
  pickWorkspaceFolder,
  addWorkspaceToList,
  setWorkspaceRoot,
  addRuntime,
  setClaudeCodeModel,
  updateConnectionSettings,
  getConnectionSettings,
  detectEnvironment,
  githubAccount,
  githubClientConfigured,
  setGithubClientId,
  githubLoginStart,
  githubLoginPoll,
  openExternal,
  createWorkspace,
  type EnvDetectDto,
  type ClaudePermissionMode,
  type DeviceStartDto,
} from "@/lib/ipc";
import { HiveBrandIcon } from "@/components/HiveBrand";

type RuntimeChoice = "claudeCode" | "openai" | "anthropic" | "ollama";

const inputStyle = { borderColor: "var(--hive-line)", background: "var(--hive-panel)" };
const field = "w-full rounded-xl border px-3 py-2.5 text-sm outline-none";

/// First-run wizard: identity → project folder → agent + file access → optional
/// team. Detects what's installed (Claude Code, Ollama, API keys) to default
/// well, and supports any OpenAI-compatible API (OpenAI/OpenRouter/local).
export function Onboarding({ onComplete }: { onComplete: () => void }) {
  const [step, setStep] = useState(1);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [env, setEnv] = useState<EnvDetectDto | null>(null);

  // Step 1 — identity
  const [name, setName] = useState("");
  const [gh, setGh] = useState<{ login: string; name?: string | null } | null>(null);
  const [ghFlow, setGhFlow] = useState<DeviceStartDto | null>(null);
  const [ghConfigured, setGhConfigured] = useState(false);
  const [showClientId, setShowClientId] = useState(false);
  const [clientId, setClientId] = useState("");

  // Step 2 — project
  const [folder, setFolder] = useState<string | null>(null);

  // Step 3 — runtime + access
  const [choice, setChoice] = useState<RuntimeChoice>("claudeCode");
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("https://api.openai.com/v1/chat/completions");
  const [model, setModel] = useState("gpt-4o");
  const [claudeModel, setClaudeModel] = useState(""); // "" = CLI default
  const [letEdit, setLetEdit] = useState(true); // Accept edits by default

  // Step 4 — team (optional)
  const [teamName, setTeamName] = useState("");
  const [relayUrl, setRelayUrl] = useState("");

  useEffect(() => {
    void (async () => {
      try {
        const d = await detectEnvironment();
        setEnv(d);
        if (d.gitName) setName((n) => n || d.gitName!);
        // Recommend the best available runtime.
        setChoice(d.claudeCode ? "claudeCode" : d.openaiEnv ? "openai" : d.ollama ? "ollama" : "anthropic");
      } catch {
        /* detection is best-effort */
      }
      try {
        const acct = await githubAccount();
        if (acct) setGh({ login: acct.login, name: acct.name });
      } catch {
        /* ignore */
      }
      try {
        setGhConfigured(await githubClientConfigured());
      } catch {
        /* ignore */
      }
      try {
        const c = await getConnectionSettings();
        setRelayUrl(c.relayUrl);
      } catch {
        /* ignore */
      }
    })();
  }, []);

  // GitHub device-flow poll (optional sign-in in step 1).
  useEffect(() => {
    if (!ghFlow) return;
    let alive = true;
    let interval = Math.max(ghFlow.interval, 1) * 1000;
    let timer: ReturnType<typeof setTimeout>;
    const tick = async () => {
      if (!alive) return;
      try {
        const r = await githubLoginPoll(ghFlow.deviceCode);
        if (!alive) return;
        if (r.status === "success") {
          setGhFlow(null);
          if (r.account) setGh({ login: r.account.login, name: r.account.name });
          return;
        }
        if (r.status === "denied" || r.status === "expired") {
          setGhFlow(null);
          return;
        }
        if (r.status === "slowDown") interval += 5000;
      } catch {
        /* keep polling */
      }
      if (alive) timer = setTimeout(tick, interval);
    };
    timer = setTimeout(tick, interval);
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [ghFlow]);

  async function startGithub() {
    try {
      const s = await githubLoginStart();
      setGhFlow(s);
      // Open the OS browser via the backend — window.open() doesn't reach it
      // from the Tauri webview. Best-effort; the URL is also shown to copy.
      void openExternal(s.verificationUri).catch(() => {});
    } catch (e) {
      setError(String(e));
    }
  }

  async function pickFolder() {
    try {
      const p = await pickWorkspaceFolder();
      if (p) {
        setFolder(p);
        await addWorkspaceToList(p);
        await setWorkspaceRoot(p);
      }
    } catch (e) {
      setError(String(e));
    }
  }

  const canNext =
    (step === 1 && (gh != null || name.trim().length > 0)) ||
    (step === 2) ||
    (step === 3 && (choice === "claudeCode" || choice === "ollama" || apiKey.trim().length > 0)) ||
    step === 4;

  async function next() {
    setError(null);
    setBusy(true);
    try {
      if (step === 1) {
        if (!gh && name.trim()) {
          await setDisplayName(name.trim());
          if (env?.gitEmail) await setGitEmail(env.gitEmail);
        }
        setStep(2);
      } else if (step === 2) {
        setStep(3);
      } else if (step === 3) {
        await applyRuntime();
        setStep(4);
      } else {
        await finishTeam();
        onComplete();
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function applyRuntime() {
    const c = await getConnectionSettings();
    const permissionMode: ClaudePermissionMode = letEdit ? "acceptEdits" : "default";
    if (choice === "openai") {
      const id = `openai-${Date.now().toString(36)}`;
      await addRuntime(id, "OpenAI-compatible", "openAI", "remote", baseUrl.trim(), model.trim() || "gpt-4o", true, false);
    } else if (choice === "anthropic") {
      await addRuntime("anthropic-api", "Anthropic API", "anthropic", "remote", "https://api.anthropic.com/v1/messages", model.trim() || "claude-sonnet-4-6", true, false);
    } else if (choice === "ollama") {
      await addRuntime("ollama-local", "Ollama (local)", "ollama", "local", "http://localhost:11434", model.trim() || "llama3.1", true, false);
    } else if (choice === "claudeCode") {
      // Claude Code uses the built-in runtime; persist the chosen --model.
      await setClaudeCodeModel(claudeModel);
    }
    const needsKey = choice === "openai" || choice === "anthropic";
    await updateConnectionSettings({
      relayUrl: c.relayUrl,
      room: c.room,
      workspaceKey: null,
      apiKey: needsKey && apiKey.trim() ? apiKey.trim() : null,
      relayAccessToken: null,
      permissionMode,
    });
  }

  async function finishTeam() {
    const c = await getConnectionSettings();
    if (relayUrl.trim() && relayUrl.trim() !== c.relayUrl) {
      await updateConnectionSettings({
        relayUrl: relayUrl.trim(),
        room: c.room,
        workspaceKey: null,
        apiKey: null,
        relayAccessToken: null,
        permissionMode: letEdit ? "acceptEdits" : "default",
      });
    }
    if (relayUrl.trim() && teamName.trim()) {
      await createWorkspace(teamName.trim());
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-6" style={{ background: "var(--hive-canvas)" }}>
      <div className="w-full max-w-md rounded-3xl border p-8" style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}>
        <div className="mb-1 flex items-center gap-3">
          <HiveBrandIcon size={40} />
          <div className="text-xl font-semibold">Welcome to Hive</div>
        </div>
        <div className="mb-4 text-sm opacity-60">
          A local-first workspace where you and your AI agents build side by side — bring your own
          models, keep your code and keys on your machine. Four quick steps:
        </div>
        <div className="mb-5 flex gap-1.5">
          {[1, 2, 3, 4].map((n) => (
            <div key={n} className="h-1 flex-1 rounded-full" style={{ background: n <= step ? "var(--hive-accent-cool)" : "var(--hive-line)" }} />
          ))}
        </div>

        {step === 1 && (
          <div className="space-y-3">
            <div className="text-sm font-medium">Who are you?</div>
            {gh ? (
              <div className="rounded-xl border px-3 py-2.5 text-sm" style={inputStyle}>
                ✓ Signed in as <strong>@{gh.login}</strong>
                {gh.name ? ` (${gh.name})` : ""} — one identity across all your devices.
              </div>
            ) : ghFlow ? (
              <div className="space-y-2 rounded-xl border px-3 py-3 text-sm" style={{ borderColor: "var(--hive-accent-cool)" }}>
                <div>
                  Enter <code className="font-bold tracking-widest">{ghFlow.userCode}</code> at{" "}
                  <button
                    type="button"
                    className="underline hover:opacity-80"
                    style={{ color: "var(--hive-accent-cool)" }}
                    onClick={() => void openExternal(ghFlow.verificationUri).catch(() => {})}
                  >
                    {ghFlow.verificationUri}
                  </button>
                </div>
                <button
                  type="button"
                  className="rounded-lg px-3 py-1.5 text-xs font-semibold text-white hover:brightness-110"
                  style={{ background: "var(--hive-accent-cool)" }}
                  onClick={() => void openExternal(ghFlow.verificationUri).catch(() => {})}
                >
                  Open GitHub ↗
                </button>
                <div className="text-xs opacity-50">Waiting for you to authorize on GitHub…</div>
              </div>
            ) : (
              <>
                {ghConfigured && (
                  <>
                    <button
                      className="flex w-full items-center justify-center gap-2 rounded-xl px-4 py-3 text-sm font-semibold text-white"
                      style={{ background: "#24292f" }}
                      onClick={startGithub}
                    >
                      <GithubGlyph /> Sign in with GitHub
                    </button>
                    <div className="text-center text-xs opacity-50">One identity across all your devices · attributes your commits</div>
                    <div className="flex items-center gap-2 text-xs opacity-40">
                      <div className="h-px flex-1" style={{ background: "var(--hive-line)" }} /> or <div className="h-px flex-1" style={{ background: "var(--hive-line)" }} />
                    </div>
                  </>
                )}
                <input autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="Continue with a name" className={field} style={inputStyle} />
                <div className="text-xs opacity-50">How teammates see you, and how commits are attributed.</div>
                {!ghConfigured &&
                  (showClientId ? (
                    <div className="space-y-2 rounded-xl border p-3" style={inputStyle}>
                      <div className="text-xs opacity-60">
                        Paste a GitHub OAuth App client ID (Developer settings → OAuth Apps, enable
                        Device Flow). No secret needed.
                      </div>
                      <div className="flex gap-2">
                        <input value={clientId} onChange={(e) => setClientId(e.target.value)} placeholder="Iv1.…" className={field + " font-mono text-xs"} style={inputStyle} />
                        <button
                          className="shrink-0 rounded-lg px-3 text-sm font-semibold text-white disabled:opacity-40"
                          style={{ background: "var(--hive-accent-cool)" }}
                          disabled={!clientId.trim()}
                          onClick={async () => {
                            await setGithubClientId(clientId.trim());
                            setGhConfigured(true);
                            setShowClientId(false);
                          }}
                        >
                          Save
                        </button>
                      </div>
                    </div>
                  ) : (
                    <button className="text-xs underline opacity-60 hover:opacity-100" onClick={() => setShowClientId(true)}>
                      Set up GitHub sign-in
                    </button>
                  ))}
              </>
            )}
          </div>
        )}

        {step === 2 && (
          <div className="space-y-3">
            <div className="text-sm font-medium">Open your project</div>
            <div className="text-xs opacity-50">Pick the folder agents should work in (a git repo is ideal).</div>
            <button className="rounded-xl px-3 py-2.5 text-sm font-semibold text-white" style={{ background: "var(--hive-accent-cool)" }} onClick={pickFolder}>
              {folder ? "Choose a different folder…" : "Choose folder…"}
            </button>
            {folder && <div className="truncate rounded-lg border px-3 py-2 font-mono text-xs" style={inputStyle}>✓ {folder}</div>}
            {!folder && <div className="text-xs opacity-50">You can also set this later from the sidebar.</div>}
          </div>
        )}

        {step === 3 && (
          <div className="space-y-3">
            <div className="text-sm font-medium">Choose your agent</div>
            <div className="space-y-1.5">
              <RuntimeOption v="claudeCode" choice={choice} setChoice={setChoice} label="Claude Code" note={env?.claudeCode ? "detected · no API key" : "not found on PATH — install the claude CLI"} />
              {choice === "claudeCode" && (
                <select
                  value={claudeModel}
                  onChange={(e) => setClaudeModel(e.target.value)}
                  className={field}
                  style={inputStyle}
                  aria-label="Claude Code model"
                >
                  <option value="">Model: Default (CLI decides — usually Sonnet)</option>
                  <option value="sonnet">Model: Sonnet</option>
                  <option value="opus">Model: Opus</option>
                  <option value="haiku">Model: Haiku</option>
                </select>
              )}
              <RuntimeOption v="openai" choice={choice} setChoice={setChoice} label="OpenAI-compatible API" note="OpenAI, OpenRouter, or any local OpenAI-style server" />
              <RuntimeOption v="anthropic" choice={choice} setChoice={setChoice} label="Anthropic API key" note={env?.anthropicEnv ? "ANTHROPIC_API_KEY detected" : "claude.ai API key"} />
              {env?.ollama && <RuntimeOption v="ollama" choice={choice} setChoice={setChoice} label="Ollama (local)" note="detected · local models, no key" />}
            </div>

            {choice === "openai" && (
              <div className="space-y-2">
                <input value={apiKey} onChange={(e) => setApiKey(e.target.value)} type="password" placeholder="API key (sk-…, or OpenRouter key)" className={field} style={inputStyle} />
                <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder="Base URL (chat completions endpoint)" className={field + " font-mono text-xs"} style={inputStyle} />
                <input value={model} onChange={(e) => setModel(e.target.value)} placeholder="Model (e.g. gpt-4o)" className={field} style={inputStyle} />
              </div>
            )}
            {choice === "anthropic" && (
              <input value={apiKey} onChange={(e) => setApiKey(e.target.value)} type="password" placeholder="Anthropic API key (sk-ant-…)" className={field} style={inputStyle} />
            )}

            <label className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={letEdit} onChange={(e) => setLetEdit(e.target.checked)} />
              Let agents edit files in my project
            </label>
            <div className="text-xs opacity-50">
              {letEdit ? "Agents can create/modify files (recommended)." : "Read-only — agents can't write files (you can change this later)."}
            </div>
          </div>
        )}

        {step === 4 && (
          <div className="space-y-3">
            <div className="text-sm font-medium">Team up (optional)</div>
            <div className="text-xs opacity-50">Solo? Skip this — add a team anytime from the ＋ in the rail. To sync with others, set a relay and name a team.</div>
            <input value={relayUrl} onChange={(e) => setRelayUrl(e.target.value)} placeholder="Relay URL (blank = solo / local)" className={field + " font-mono text-xs"} style={inputStyle} />
            {relayUrl.trim() && (
              <input value={teamName} onChange={(e) => setTeamName(e.target.value)} placeholder="New team name (optional)" className={field} style={inputStyle} />
            )}
          </div>
        )}

        {error && <p className="mt-3 text-xs" style={{ color: "var(--hive-accent-warm)" }}>{error}</p>}

        <div className="mt-6 flex items-center justify-between">
          <button className="text-xs opacity-50 hover:opacity-100 disabled:opacity-20" disabled={step === 1 || busy} onClick={() => setStep((s) => s - 1)}>
            ← Back
          </button>
          <div className="flex items-center gap-3">
            {(step === 2 || step === 4) && (
              <button className="text-xs opacity-50 hover:opacity-100" disabled={busy} onClick={() => (step === 4 ? onComplete() : setStep(step + 1))}>
                Skip
              </button>
            )}
            <button
              onClick={next}
              disabled={!canNext || busy}
              className="rounded-xl px-5 py-2.5 text-sm font-semibold text-white disabled:opacity-40"
              style={{ background: "var(--hive-accent-cool)" }}
            >
              {busy ? "…" : step === 4 ? "Finish" : "Next"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

function GithubGlyph() {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor" aria-hidden>
      <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0016 8c0-4.42-3.58-8-8-8z" />
    </svg>
  );
}

function RuntimeOption({
  v,
  choice,
  setChoice,
  label,
  note,
}: {
  v: RuntimeChoice;
  choice: RuntimeChoice;
  setChoice: (c: RuntimeChoice) => void;
  label: string;
  note: string;
}) {
  const active = choice === v;
  return (
    <button
      onClick={() => setChoice(v)}
      className="flex w-full items-center justify-between rounded-xl border px-3 py-2.5 text-left text-sm"
      style={{ borderColor: active ? "var(--hive-accent-cool)" : "var(--hive-line)", background: active ? "var(--hive-mist)" : "transparent" }}
    >
      <span className="font-medium">{label}</span>
      <span className="ml-2 text-xs opacity-50">{note}</span>
    </button>
  );
}
