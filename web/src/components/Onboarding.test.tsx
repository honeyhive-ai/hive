import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Onboarding } from "./Onboarding";
import * as ipc from "@/lib/ipc";

// Mock the IPC boundary — this is what makes a full UI flow testable headlessly.
vi.mock("@/lib/ipc", () => ({
  setDisplayName: vi.fn(async () => {}),
  setGitEmail: vi.fn(async () => {}),
  pickWorkspaceFolder: vi.fn(async () => "/Users/sam/proj"),
  addWorkspaceToList: vi.fn(async () => ["/Users/sam/proj"]),
  setWorkspaceRoot: vi.fn(async () => {}),
  addRuntime: vi.fn(async () => {}),
  // The claude-code path persists the chosen --model; missing this mock made
  // applyRuntime() throw (undefined is not a function), silently stranding
  // the wizard on step 3 — the long-standing "pre-existing failure".
  setClaudeCodeModel: vi.fn(async () => {}),
  setDefaultModel: vi.fn(async () => {}),
  setGithubClientId: vi.fn(async () => {}),
  openExternal: vi.fn(async () => {}),
  updateConnectionSettings: vi.fn(async () => ({
    relayUrl: "",
    room: "default",
    hasWorkspaceKey: false,
    hasApiKey: false,
    permissionMode: "acceptEdits",
  })),
  getConnectionSettings: vi.fn(async () => ({
    relayUrl: "",
    room: "default",
    hasWorkspaceKey: false,
    hasApiKey: false,
    permissionMode: "default",
  })),
  detectEnvironment: vi.fn(async () => ({
    claudeCode: true,
    ollama: false,
    anthropicEnv: false,
    openaiEnv: false,
    gitName: "Sam Dev",
    gitEmail: "sam@example.com",
  })),
  githubAccount: vi.fn(async () => null),
  githubClientConfigured: vi.fn(async () => false),
  githubLoginStart: vi.fn(),
  githubLoginPoll: vi.fn(),
  joinWorkspace: vi.fn(async () => ({ id: "w", name: "Acme", kind: "room", active: true })),
  redeemShortCode: vi.fn(async () => ({ kind: "workspace", label: "Acme" })),
  probeRelay: vi.fn(async () => ({ status: "ok", detail: "" })),
  probeRelayAt: vi.fn(async () => ({ status: "ok", detail: "" })),
  createRelayUser: vi.fn(async () => ({ userId: "u", userName: "Sam", raw: "tok" })),
}));

beforeEach(() => vi.clearAllMocks());

const clickByName = (name: string | RegExp) =>
  userEvent.click(screen.getByRole("button", { name }));

describe("Onboarding wizard flow", () => {
  it("walks identity → project → agent → team and finishes with accept-edits", async () => {
    const onComplete = vi.fn();
    render(<Onboarding onComplete={onComplete} />);

    // Step 1: name is prefilled from git config.
    const nameInput = await screen.findByPlaceholderText("Continue with a name");
    await waitFor(() => expect((nameInput as HTMLInputElement).value).toBe("Sam Dev"));
    await clickByName("Next");
    expect(ipc.setDisplayName).toHaveBeenCalledWith("Sam Dev");
    expect(ipc.setGitEmail).toHaveBeenCalledWith("sam@example.com");

    // Step 2: skip choosing a folder.
    await screen.findByText("Open your project");
    await clickByName("Next");

    // Step 3: Claude Code is detected + preselected; finish the step.
    await screen.findByText("Choose your agent");
    await clickByName("Next");
    // The claude-code path persists the --model choice ("" = CLI default)…
    await waitFor(() => expect(ipc.setClaudeCodeModel).toHaveBeenCalledWith(""));
    // …and syncs the Primary Runtime's default model so the chat header matches
    // the pick (CLI-default → Sonnet).
    await waitFor(() =>
      expect(ipc.setDefaultModel).toHaveBeenCalledWith("claude-sonnet-4-6"),
    );
    // …and default permission = acceptEdits ("let agents edit files" on).
    await waitFor(() =>
      expect(ipc.updateConnectionSettings).toHaveBeenCalledWith(
        expect.objectContaining({ permissionMode: "acceptEdits", apiKey: null }),
      ),
    );

    // Step 4: "Just me" is the default → Finish clears any relay (local-only).
    await screen.findByText("Team & sync");
    await clickByName("Finish");
    await waitFor(() => expect(onComplete).toHaveBeenCalled());
    expect(ipc.updateConnectionSettings).toHaveBeenCalledWith(
      expect.objectContaining({ relayUrl: "" }),
    );
    // No team-join happened on the solo path.
    expect(ipc.joinWorkspace).not.toHaveBeenCalled();
  });

  it("picks Claude Code + Opus and makes Opus the Primary Runtime default", async () => {
    render(<Onboarding onComplete={vi.fn()} />);

    await screen.findByPlaceholderText("Continue with a name");
    await clickByName("Next"); // identity
    await clickByName("Next"); // project

    await screen.findByText("Choose your agent");
    // Claude Code is preselected; choose Opus in its model dropdown.
    await userEvent.selectOptions(
      screen.getByLabelText("Claude Code model"),
      "opus",
    );
    await clickByName("Next");

    // The CLI --model flag AND the displayed/default Primary Runtime both move to
    // Opus — otherwise the chat header silently falls back to Sonnet.
    await waitFor(() => expect(ipc.setClaudeCodeModel).toHaveBeenCalledWith("opus"));
    await waitFor(() =>
      expect(ipc.setDefaultModel).toHaveBeenCalledWith("claude-opus-4-8"),
    );
  });

  it("adds an OpenAI-compatible runtime with key + base URL", async () => {
    const onComplete = vi.fn();
    render(<Onboarding onComplete={onComplete} />);

    await screen.findByPlaceholderText("Continue with a name");
    await clickByName("Next"); // identity
    await clickByName("Next"); // project (skip-by-next)

    await screen.findByText("Choose your agent");
    await clickByName(/OpenAI-compatible API/);
    await userEvent.type(screen.getByPlaceholderText(/API key/), "sk-test-123");
    await clickByName("Next");

    await waitFor(() =>
      expect(ipc.addRuntime).toHaveBeenCalledWith(
        expect.any(String),
        "OpenAI-compatible",
        "openAI",
        "remote",
        "https://api.openai.com/v1/chat/completions",
        "gpt-4o",
        true,
        false,
      ),
    );
    expect(ipc.updateConnectionSettings).toHaveBeenCalledWith(
      expect.objectContaining({ apiKey: "sk-test-123", permissionMode: "acceptEdits" }),
    );
  });

  it("GitHub sign-in pre-copies the device code instead of auto-opening the browser", async () => {
    const writeText = vi.fn(async () => {});
    Object.assign(navigator, { clipboard: { writeText } });
    (ipc.githubClientConfigured as ReturnType<typeof vi.fn>).mockResolvedValue(true);
    (ipc.githubLoginStart as ReturnType<typeof vi.fn>).mockResolvedValue({
      deviceCode: "dev",
      userCode: "ABCD-1234",
      verificationUri: "https://github.com/login/device",
      interval: 5,
    });

    render(<Onboarding onComplete={vi.fn()} />);
    await screen.findByPlaceholderText("Continue with a name");
    await clickByName(/Sign in with GitHub/);

    // The code is shown and copied to the clipboard — no auto-open.
    await screen.findByText("ABCD-1234");
    expect(writeText).toHaveBeenCalledWith("ABCD-1234");
    expect(ipc.openExternal).not.toHaveBeenCalled();

    // The primary button both copies and opens GitHub.
    await clickByName(/Copy code & open GitHub/);
    expect(ipc.openExternal).toHaveBeenCalledWith("https://github.com/login/device");
  });

  it("blocks Finish when a host relay is unauthorized, allows it once connected", async () => {
    const onComplete = vi.fn();
    // First probe: unauthorized; after a token, ok.
    (ipc.probeRelayAt as ReturnType<typeof vi.fn>)
      .mockResolvedValueOnce({ status: "unauthorized", detail: "token rejected" })
      .mockResolvedValueOnce({ status: "ok", detail: "" });

    render(<Onboarding onComplete={onComplete} />);
    await screen.findByPlaceholderText("Continue with a name");
    await clickByName("Next"); // identity
    await clickByName("Next"); // project
    await screen.findByText("Choose your agent");
    await clickByName("Next"); // runtime → step 4
    await screen.findByText("Team & sync");

    // Choose "Connect to a relay", enter a URL, test → unauthorized.
    await clickByName(/Connect to a relay/);
    await userEvent.type(
      screen.getByPlaceholderText(/Relay URL/),
      "https://relay.example",
    );
    await clickByName("Test connection");
    await screen.findByText("token rejected");
    // Finish is disabled — a broken relay can't complete onboarding.
    expect(screen.getByRole("button", { name: "Finish" })).toBeDisabled();

    // Paste a token + re-test → connected → Finish enabled.
    await userEvent.type(screen.getByPlaceholderText(/access token/i), "good-token");
    await clickByName("Test connection");
    await screen.findByText("✓ Connected");
    expect(screen.getByRole("button", { name: "Finish" })).toBeEnabled();
  });
});
