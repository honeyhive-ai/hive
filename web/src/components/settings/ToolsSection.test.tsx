import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ToolsSection } from "./ToolsSection";
import * as ipc from "@/lib/ipc";
import { promptDialog } from "@/components/Dialog";

vi.mock("@/lib/ipc", () => ({
  listMcpServers: vi.fn(async () => [
    {
      id: "docs",
      transport: "http",
      detail: "http://10.0.0.5:8090/mcp",
      enabled: true,
      isManaged: true,
      hasBearerToken: true,
      oauthConnected: false,
      secretForOtherUrl: false,
    },
    {
      id: "linear",
      transport: "http",
      detail: "https://mcp.linear.app/sse",
      enabled: false,
      isManaged: true,
      hasBearerToken: false,
      oauthConnected: true,
      secretForOtherUrl: false,
    },

    {
      id: "fs",
      transport: "stdio",
      detail: "mcp-fs",
      enabled: false,
      isManaged: true,
      hasBearerToken: false,
      oauthConnected: false,
      secretForOtherUrl: false,
    },
  ]),
  installMcpServer: vi.fn(async () => {}),
  addRemoteMcpServer: vi.fn(async () => {}),
  authorizeMcpServer: vi.fn(async () => {}),
  setMcpOauthClient: vi.fn(async () => {}),
  removeMcpServer: vi.fn(async () => {}),
  setMcpEnabled: vi.fn(async () => {}),
  setMcpBearerToken: vi.fn(async (_id: string, token: string) => token !== ""),
}));

vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

vi.mock("@/components/Dialog", () => ({
  promptDialog: vi.fn(async () => "  new-token  "),
}));

function renderSection() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <ToolsSection />
    </QueryClientProvider>,
  );
}

describe("ToolsSection bearer tokens", () => {
  beforeEach(() => vi.clearAllMocks());

  it("shows token state without ever rendering a token value", async () => {
    renderSection();
    expect(await screen.findByText("· token set")).toBeTruthy();
    expect(screen.getByText("· connected")).toBeTruthy();
    expect(screen.queryByText("· token saved for a different URL")).toBeNull();
    // Token controls only on HTTP servers.
    expect(screen.getByRole("button", { name: "Clear token" })).toBeTruthy();
    expect(screen.getAllByRole("button", { name: "Set token" })).toHaveLength(1);
  });

  it("flags a token saved for a different URL under the same id", async () => {
    vi.mocked(ipc.listMcpServers).mockResolvedValueOnce([
      {
        id: "docs",
        transport: "http",
        detail: "https://elsewhere.example/mcp",
        enabled: true,
        isManaged: true,
        hasBearerToken: true,
        oauthConnected: false,
        secretForOtherUrl: true,
      },
    ]);
    renderSection();
    expect(await screen.findByText("· token saved for a different URL")).toBeTruthy();
    // It can still be cleared or replaced.
    expect(screen.getByRole("button", { name: "Clear token" })).toBeTruthy();
  });

  it("adds an HTTP server with a password-type bearer field", async () => {
    const user = userEvent.setup();
    renderSection();
    await screen.findByText("docs");
    await user.click(screen.getByRole("button", { name: /Add HTTP server/ }));
    const token = screen.getByLabelText(/Bearer token \(optional\)/) as HTMLInputElement;
    expect(token.type).toBe("password");
    await user.type(screen.getByLabelText("Name"), "rdna");
    await user.type(screen.getByLabelText("URL"), "http://10.0.0.9:8090/mcp");
    await user.type(token, "s3cret");
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await waitFor(() =>
      expect(ipc.addRemoteMcpServer).toHaveBeenCalledWith("rdna", "http://10.0.0.9:8090/mcp", "s3cret"),
    );
  });

  it("omits the token when left blank (keeps any saved one)", async () => {
    const user = userEvent.setup();
    renderSection();
    await screen.findByText("docs");
    await user.click(screen.getByRole("button", { name: /Add HTTP server/ }));
    await user.type(screen.getByLabelText("Name"), "open");
    await user.type(screen.getByLabelText("URL"), "http://h/mcp");
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await waitFor(() =>
      expect(ipc.addRemoteMcpServer).toHaveBeenCalledWith("open", "http://h/mcp", undefined),
    );
  });

  it("sets and clears a token on an existing server", async () => {
    const user = userEvent.setup();
    renderSection();
    await screen.findByText("docs");
    await user.click(screen.getByRole("button", { name: "Set token" }));
    await waitFor(() => expect(ipc.setMcpBearerToken).toHaveBeenCalledWith("linear", "  new-token  "));
    expect(promptDialog).toHaveBeenCalledWith(
      expect.stringContaining("linear"),
      expect.objectContaining({ password: true }),
    );
    await user.click(screen.getByRole("button", { name: "Clear token" }));
    await waitFor(() => expect(ipc.setMcpBearerToken).toHaveBeenCalledWith("docs", ""));
  });
});
