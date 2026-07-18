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
  createWorkspace: vi.fn(async () => ({ id: "w", name: "Acme", kind: "room", active: true })),
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
    // …and default permission = acceptEdits ("let agents edit files" on).
    await waitFor(() =>
      expect(ipc.updateConnectionSettings).toHaveBeenCalledWith(
        expect.objectContaining({ permissionMode: "acceptEdits", apiKey: null }),
      ),
    );

    // Step 4: solo (no relay) → Finish.
    await screen.findByText("Team up (optional)");
    await clickByName("Finish");
    await waitFor(() => expect(onComplete).toHaveBeenCalled());
    expect(ipc.createWorkspace).not.toHaveBeenCalled();
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
});
