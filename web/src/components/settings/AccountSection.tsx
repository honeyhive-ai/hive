import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  getAppSettings,
  setDisplayName,
  setAvatar,
  setGitEmail,
  githubAccount,
  githubClientConfigured,
  setGithubClientId,
  githubLoginStart,
  openExternal,
  githubLoginPoll,
  githubLogout,
} from "@/lib/ipc";
import { Button, Section, fieldStyle } from "@/components/ui";
import { toast, errMsg } from "@/components/Toast";
import { Avatar } from "@/components/Avatar";
import { fileToAvatarDataUrl } from "@/lib/avatar";

/// Account page: your identity (name, git email, avatar) plus the GitHub
/// sign-in that federates it across devices. Changes apply immediately.
export function AccountSection() {
  return (
    <>
      <IdentitySection />
      <GithubSection />
    </>
  );
}

// Identity writes through on blur — no Save button (D18). A keystroke only
// re-renders this section, not the query-bearing siblings.
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

  async function commitName() {
    const next = name.trim() || "You";
    if (next === (settings.data?.displayName ?? "")) return;
    try {
      await setDisplayName(next);
      qc.invalidateQueries({ queryKey: ["settings"] });
    } catch (e) {
      toast.error(`Couldn't save display name: ${errMsg(e)}`);
    }
  }

  async function commitEmail() {
    if (emailLocked) return;
    const next = email.trim();
    if (next === (settings.data?.gitEmail ?? "")) return;
    try {
      await setGitEmail(next);
      qc.invalidateQueries({ queryKey: ["settings"] });
    } catch (e) {
      toast.error(`Couldn't save git email: ${errMsg(e)}`);
    }
  }

  async function pickAvatar(file: File | undefined) {
    if (!file) return;
    try {
      const url = await fileToAvatarDataUrl(file);
      await setAvatar(url);
      qc.invalidateQueries({ queryKey: ["settings"] });
    } catch (e) {
      toast.error(`Couldn't set avatar: ${errMsg(e)}`);
    }
  }
  async function clearAvatar() {
    try {
      await setAvatar(null);
      qc.invalidateQueries({ queryKey: ["settings"] });
    } catch (e) {
      toast.error(`Couldn't clear avatar: ${errMsg(e)}`);
    }
  }

  return (
    <Section title="Identity">
      <div className="mb-4 flex items-center gap-3">
        <label className="group/av relative cursor-pointer" title="Upload an avatar">
          <Avatar name={settings.data?.displayName || name || "You"} url={settings.data?.avatarUrl} kind="human" size={52} />
          <span
            className="absolute inset-0 flex items-center justify-center rounded-full text-[10px] font-semibold opacity-0 transition-opacity group-hover/av:opacity-100"
            style={{ background: "color-mix(in srgb, var(--hive-ink) 45%, transparent)", color: "var(--hive-canvas)" }}
          >
            Edit
          </span>
          <input
            type="file"
            accept="image/*"
            className="hidden"
            onChange={(e) => {
              const f = e.target.files?.[0];
              e.target.value = "";
              void pickAvatar(f);
            }}
          />
        </label>
        <div className="text-xs opacity-60">
          <div>Your avatar is shown to everyone in the workspace.</div>
          {settings.data?.avatarUrl && (
            <button onClick={() => void clearAvatar()} className="mt-1 underline opacity-80 hover:opacity-100">
              Remove
            </button>
          )}
        </div>
      </div>
      <label className="block text-sm opacity-70">Display name</label>
      <input
        value={name}
        onChange={(e) => setName(e.target.value)}
        onBlur={() => void commitName()}
        className="w-full rounded-xl border px-3 py-2 text-sm"
        style={fieldStyle}
      />
      <label className="mt-2 block text-sm opacity-70">Git email</label>
      <input
        value={emailLocked ? (account.data?.email ?? email) : email}
        onChange={(e) => setEmail(e.target.value)}
        onBlur={() => void commitEmail()}
        readOnly={emailLocked}
        placeholder="you@example.com"
        className="w-full rounded-xl border px-3 py-2 font-mono text-sm"
        style={{ ...fieldStyle, opacity: emailLocked ? 0.6 : 1 }}
      />
      <p className="text-xs opacity-50">
        {emailLocked
          ? "From your GitHub account — sign out below to set it by hand."
          : "Used to credit you as the commit author when an agent does work you asked for — even if it runs on a teammate's machine."}
      </p>
      <span className="text-xs opacity-50">Device: {settings.data?.deviceName ?? "…"}</span>
    </Section>
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
    },
    onError: (e) => toast.error(errMsg(e)),
  });
  const logout = useMutation({
    mutationFn: githubLogout,
    onSuccess: () => qc.invalidateQueries({ queryKey: ["github-account"] }),
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
    <Section title="GitHub">
      {acct ? (
        <div
          className="flex items-center justify-between gap-3 rounded-xl border px-3 py-2.5"
          style={fieldStyle}
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
          <button className="shrink-0 text-xs hover:opacity-80" style={{ color: "var(--hive-danger)" }} onClick={() => logout.mutate()}>
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
              style={fieldStyle}
            />
            <Button variant="primary" size="md" onClick={() => saveClient.mutate()}>
              Save
            </Button>
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
