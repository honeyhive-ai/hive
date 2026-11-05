import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { addRuntime, detectCliRuntimes, listRuntimes, type DetectedCliDto } from "@/lib/ipc";
import { Button, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";

const LOGIN_TEXT: Record<DetectedCliDto["login"], string> = {
  loggedIn: "signed in",
  notLoggedIn: "not signed in",
  unknown: "sign-in not checked",
};

/// Subscription CLIs (Google `agy`, GitHub `copilot`): what's installed on this
/// machine, whether it's signed in, and a one-click "Add runtime". Keyless —
/// the login stays in the CLI's own config / OS keychain; Hive only runs it.
export function CliRuntimesPanel() {
  const qc = useQueryClient();
  const [probe, setProbe] = useState(false);
  const clis = useQuery({
    queryKey: ["cli-runtimes", probe],
    queryFn: () => detectCliRuntimes(probe),
  });
  const runtimes = useQuery({ queryKey: ["runtimes"], queryFn: listRuntimes });
  const [models, setModels] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);

  async function add(c: DetectedCliDto) {
    if (!c.provider) return;
    setBusy(c.id);
    try {
      const model = (models[c.id] ?? "").trim();
      const id = model ? `${c.id}-${model}`.toLowerCase().replace(/[^a-z0-9]+/g, "-") : c.id;
      const name = model ? `${c.label} · ${model}` : c.label;
      await addRuntime(id, name, c.provider, "local", "", model, true, false);
      await qc.invalidateQueries({ queryKey: ["runtimes"] });
      toast.success(`Added ${name} — pick it from the route pill in any chat.`);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setBusy(null);
    }
  }

  const list = clis.data ?? [];
  const added = (provider: string | null) => !!provider && (runtimes.data ?? []).some((r) => r.provider === provider);

  return (
    <div className="mb-3 rounded-2xl border p-3" style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)" }}>
      <div className="flex items-center justify-between gap-2">
        <div className="text-sm font-medium">Subscription CLIs</div>
        <div className="flex gap-1.5">
          <Button size="sm" variant="ghost" disabled={clis.isFetching} onClick={() => void clis.refetch()}>
            {clis.isFetching ? "Scanning…" : "Re-scan"}
          </Button>
          {list.some((c) => c.id === "antigravity" && c.installed) && (
            <Button
              size="sm"
              variant="ghost"
              disabled={clis.isFetching}
              title="Runs `agy models` (read-only, uses no quota) to confirm the Google sign-in"
              onClick={() => (probe ? void clis.refetch() : setProbe(true))}
            >
              Check Google sign-in
            </Button>
          )}
        </div>
      </div>
      <p className="mt-0.5 text-xs opacity-55">
        Use a Google AI Pro / Ultra or GitHub Copilot subscription through its official CLI. Hive runs the CLI
        headless with your permission setting; the login stays in the CLI — Hive never sees it.
      </p>
      {clis.isError && <p className="mt-2 text-xs opacity-70">{errMsg(clis.error)}</p>}
      <div className="mt-2 space-y-2">
        {list.map((c) => (
          <div key={c.id} data-testid={`cli-${c.id}`} className="rounded-xl border px-3 py-2 text-sm" style={fieldStyle}>
            <div className="flex flex-wrap items-center justify-between gap-2">
              <span>
                <span className="font-medium">{c.label}</span>{" "}
                <span className="font-mono text-xs opacity-50">
                  {c.binary}
                  {c.version ? ` ${c.version}` : ""}
                </span>
              </span>
              {!c.installed ? (
                <span className="text-xs opacity-55">not installed</span>
              ) : !c.supported ? (
                <span className="text-xs opacity-55">not usable for subscriptions</span>
              ) : added(c.provider) ? (
                <span className="text-xs opacity-55">runtime added</span>
              ) : (
                <Button size="sm" variant="primary" disabled={busy === c.id} onClick={() => void add(c)}>
                  Add runtime
                </Button>
              )}
            </div>
            {c.installed && c.supported && (
              <div className="mt-1 text-xs">
                <span
                  style={{
                    color:
                      c.login === "loggedIn"
                        ? "var(--hive-accent-cool)"
                        : c.login === "notLoggedIn"
                          ? "var(--hive-danger)"
                          : undefined,
                  }}
                >
                  {LOGIN_TEXT[c.login]}
                </span>
                {c.loginDetail && <span className="opacity-55"> · {c.loginDetail}</span>}
                {c.login !== "loggedIn" && (
                  <div className="mt-0.5 opacity-70">
                    To sign in, run <code>{c.loginCommand}</code> in a terminal on this machine.
                  </div>
                )}
              </div>
            )}
            {c.installed && c.supported && !added(c.provider) && (
              <>
                <input
                  value={models[c.id] ?? ""}
                  onChange={(e) => setModels((m) => ({ ...m, [c.id]: e.target.value }))}
                  placeholder="Model (blank = the CLI's default)"
                  list={c.models.length ? `cli-models-${c.id}` : undefined}
                  className="mt-1.5 w-full rounded-lg border px-2 py-1 font-mono text-xs"
                  style={fieldStyle}
                />
                {c.models.length > 0 && (
                  <datalist id={`cli-models-${c.id}`}>
                    {c.models.map((m) => (
                      <option key={m} value={m} />
                    ))}
                  </datalist>
                )}
              </>
            )}
            {!c.installed && (
              <div className="mt-1 text-xs opacity-60">
                Install: <code>{c.installCommand}</code>
              </div>
            )}
            {c.note && <div className="mt-1 text-xs opacity-55">{c.note}</div>}
          </div>
        ))}
      </div>
    </div>
  );
}
