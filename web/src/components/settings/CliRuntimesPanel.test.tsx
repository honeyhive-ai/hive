import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { CliRuntimesPanel } from "./CliRuntimesPanel";
import * as ipc from "@/lib/ipc";

const base = {
  path: null,
  version: null,
  loginDetail: "",
  supported: true,
  note: "",
  models: [] as string[],
};

vi.mock("@/lib/ipc", () => ({
  detectCliRuntimes: vi.fn(async (probe: boolean) => [
    {
      ...base,
      id: "antigravity",
      provider: "antigravity",
      label: "Google Antigravity CLI",
      binary: "agy",
      installed: true,
      version: "1.5.1",
      login: probe ? "loggedIn" : "unknown",
      loginDetail: probe ? "signed in — 2 models available" : "",
      loginCommand: "agy",
      installCommand: "curl -fsSL https://antigravity.google/cli/install.sh | bash",
      models: probe ? ["gemini-3.8-flash-low", "gemini-4-argon-high"] : [],
    },
    {
      ...base,
      id: "copilot",
      provider: "copilot",
      label: "GitHub Copilot CLI",
      binary: "copilot",
      installed: true,
      version: "1.0.106-6",
      login: "notLoggedIn",
      loginCommand: "copilot login",
      installCommand: "brew install --cask copilot-cli",
    },
    {
      ...base,
      id: "gemini",
      provider: null,
      label: "Gemini CLI",
      binary: "gemini",
      installed: true,
      supported: false,
      login: "unknown",
      loginCommand: "agy",
      installCommand: "",
      note: "Gemini CLI stopped serving Google AI Pro sign-ins on 2026-06-18.",
    },
  ]),
  listRuntimes: vi.fn(async () => []),
  addRuntime: vi.fn(async () => {}),
}));

vi.mock("@/components/Toast", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
  errMsg: (e: unknown) => String(e),
}));

function renderPanel() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <CliRuntimesPanel />
    </QueryClientProvider>,
  );
}

describe("CliRuntimesPanel", () => {
  beforeEach(() => vi.clearAllMocks());

  it("shows versions, sign-in state and the exact login command", async () => {
    renderPanel();
    const copilot = await screen.findByTestId("cli-copilot");
    expect(within(copilot).getByText(/1\.0\.106-6/)).toBeTruthy();
    expect(within(copilot).getByText("not signed in")).toBeTruthy();
    expect(within(copilot).getByText("copilot login")).toBeTruthy();
    const gemini = screen.getByTestId("cli-gemini");
    expect(within(gemini).getByText("not usable for subscriptions")).toBeTruthy();
    expect(within(gemini).queryByText("Add runtime")).toBeNull();
    expect(ipc.detectCliRuntimes).toHaveBeenCalledWith(false);
  });

  it("probes the Google sign-in only on request, then adds a keyless runtime with the chosen model", async () => {
    const user = userEvent.setup();
    renderPanel();
    const agy = await screen.findByTestId("cli-antigravity");
    expect(within(agy).getByText("sign-in not checked")).toBeTruthy();
    await user.click(screen.getByText("Check Google sign-in"));
    await waitFor(() => expect(ipc.detectCliRuntimes).toHaveBeenCalledWith(true));
    await waitFor(() => expect(within(screen.getByTestId("cli-antigravity")).getByText("signed in")).toBeTruthy());

    const row = screen.getByTestId("cli-antigravity");
    await user.type(within(row).getByPlaceholderText(/Model/), "gemini-3.8-flash-low");
    await user.click(within(row).getByText("Add runtime"));
    await waitFor(() =>
      expect(ipc.addRuntime).toHaveBeenCalledWith(
        "antigravity-gemini-3-8-flash-low",
        "Google Antigravity CLI · gemini-3.8-flash-low",
        "antigravity",
        "local",
        "",
        "gemini-3.8-flash-low",
        true,
        false,
      ),
    );
  });
});
