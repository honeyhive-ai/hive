import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  installMcpServer,
  addRemoteMcpServer,
  authorizeMcpServer,
  setMcpOauthClient,
  listMcpServers,
  removeMcpServer,
  setMcpEnabled,
  setMcpBearerToken,
} from "@/lib/ipc";
import { Button, Field, FormDisclosure, Section, Switch, fieldStyle } from "@/components/ui";
import { IconPlus } from "@/lib/icons";
import { toast, errMsg } from "@/components/Toast";
import { promptDialog } from "@/components/Dialog";

/// Tools & MCP: install and configure MCP servers. Settings owns installing +
/// configuring; the chat Tools pane links here and only attaches/selects.
export function ToolsSection() {
  const qc = useQueryClient();
  const servers = useQuery({ queryKey: ["mcp"], queryFn: listMcpServers });
  const [source, setSource] = useState("");
  const toggle = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) => setMcpEnabled(id, enabled),
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
  // Add an HTTP server by URL, optionally with a static bearer token. The
  // token is sent once, stored on this device only, and never read back.
  const [addOpen, setAddOpen] = useState(false);
  const [httpId, setHttpId] = useState("");
  const [httpUrl, setHttpUrl] = useState("");
  const [httpToken, setHttpToken] = useState("");
  const addHttp = useMutation({
    mutationFn: () =>
      addRemoteMcpServer(httpId.trim(), httpUrl.trim(), httpToken.trim() || undefined),
    onSuccess: () => {
      const id = httpId.trim();
      setHttpId("");
      setHttpUrl("");
      setHttpToken("");
      setAddOpen(false);
      qc.invalidateQueries({ queryKey: ["mcp"] });
      toast.success(`${id} added — enable it to use its tools.`);
    },
    onError: (e) => toast.error(`Couldn't add server: ${errMsg(e)}`),
  });
  const setToken = useMutation({
    mutationFn: async ({ id, clear }: { id: string; clear: boolean }) => {
      if (clear) return setMcpBearerToken(id, "");
      const token = await promptDialog(`Bearer token for "${id}"`, {
        placeholder: "Stored on this device only",
        password: true,
      });
      if (token === null || !token.trim()) return null; // cancelled
      return setMcpBearerToken(id, token);
    },
    onSuccess: (set, { id }) => {
      if (set === null) return;
      qc.invalidateQueries({ queryKey: ["mcp"] });
      toast.success(set ? `Token saved for ${id}.` : `Token removed for ${id}.`);
    },
    onError: (e) => toast.error(`Couldn't save token: ${errMsg(e)}`),
  });
  const hasLinear = (servers.data ?? []).some((s) => s.id === "linear");
  return (
    <Section title="MCP servers">
      <p className="text-xs opacity-50">
        Servers stay inert until enabled. Add a hosted server (e.g. Linear) and Connect to authorize
        it, or install a workspace-scoped entry from a manifest URL or GitHub reference.
      </p>
      {!hasLinear && (
        <Button onClick={() => addLinear.mutate()} disabled={addLinear.isPending} className="self-start">
          <IconPlus size={15} /> Add Linear (issues)
        </Button>
      )}
      <div className="flex gap-2">
        <input
          value={source}
          onChange={(e) => setSource(e.target.value)}
          placeholder="owner/repo/mcp.json or https://…"
          className="flex-1 rounded-xl border px-3 py-2 font-mono text-sm"
          style={fieldStyle}
        />
        <Button variant="primary" size="md" onClick={() => install.mutate()}>
          Install
        </Button>
      </div>
      <FormDisclosure label="Add HTTP server" open={addOpen} onToggle={() => setAddOpen((v) => !v)}>
        <form
          className="flex flex-col gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            addHttp.mutate();
          }}
        >
          <Field label="Name">
            <input
              value={httpId}
              onChange={(e) => setHttpId(e.target.value)}
              placeholder="docs"
              className="rounded-xl border px-3 py-2 text-sm"
              style={fieldStyle}
            />
          </Field>
          <Field label="URL">
            <input
              value={httpUrl}
              onChange={(e) => setHttpUrl(e.target.value)}
              placeholder="https://host:8090/mcp"
              className="rounded-xl border px-3 py-2 font-mono text-sm"
              style={fieldStyle}
            />
          </Field>
          <Field
            label="Bearer token (optional)"
            hint="Kept on this device only — never synced, exported or written to hive.config.toml."
          >
            <input
              type="password"
              autoComplete="off"
              value={httpToken}
              onChange={(e) => setHttpToken(e.target.value)}
              placeholder="For servers protected by a fixed token"
              className="rounded-xl border px-3 py-2 font-mono text-sm"
              style={fieldStyle}
            />
          </Field>
          <Button
            type="submit"
            variant="primary"
            size="md"
            className="self-start"
            disabled={addHttp.isPending || !httpId.trim() || !httpUrl.trim()}
          >
            Add server
          </Button>
        </form>
      </FormDisclosure>
      {servers.data?.length === 0 && <p className="text-sm opacity-50">None configured.</p>}
      {(servers.data ?? []).map((s) => (
        <div
          key={s.id}
          className="flex items-center justify-between rounded-xl border px-3 py-2"
          style={fieldStyle}
        >
          <span>
            <span className="font-medium">{s.id}</span>{" "}
            <span className="text-xs opacity-50">
              [{s.transport}] {s.detail}
            </span>
            {s.hasBearerToken && (
              <span className="ml-2 text-xs opacity-70" title="A static bearer token is saved on this device">
                · token set
              </span>
            )}
            {!s.hasBearerToken && s.oauthConnected && (
              <span className="ml-2 text-xs opacity-70">· connected</span>
            )}
            {s.secretForOtherUrl && (
              <span
                className="ml-2 text-xs opacity-70"
                title="This server's URL changed since its token was saved, so the token is not sent. Set the token again or reconnect."
              >
                · token saved for a different URL
              </span>
            )}
          </span>
          <span className="flex items-center gap-3">
            {s.transport === "http" && (
              <button
                className="text-xs underline opacity-70 hover:opacity-100 disabled:opacity-40"
                disabled={setToken.isPending}
                onClick={() => setToken.mutate({ id: s.id, clear: s.hasBearerToken })}
                title={
                  s.hasBearerToken
                    ? "Remove the saved bearer token"
                    : "Save a static bearer token (stored on this device only)"
                }
              >
                {s.hasBearerToken ? "Clear token" : "Set token"}
              </button>
            )}
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
            <Switch
              on={s.enabled}
              onChange={(v) => toggle.mutate({ id: s.id, enabled: v })}
              label={`Enable ${s.id}`}
            />
            {s.isManaged && (
              <button
                className="text-xs hover:opacity-80"
                style={{ color: "var(--hive-danger)" }}
                onClick={() => remove.mutate(s.id)}
              >
                Remove
              </button>
            )}
          </span>
        </div>
      ))}
    </Section>
  );
}
