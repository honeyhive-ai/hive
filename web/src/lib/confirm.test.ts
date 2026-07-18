import { describe, it, expect, vi, beforeEach } from "vitest";

// confirmThen delegates to the in-app confirmDialog — window.confirm is a
// no-op in Tauri webviews (the reason the old window.confirm-based tests
// tested a contract the code deliberately abandoned). Mock the dialog and
// let each test control how the user "answers".
const mocks = vi.hoisted(() => ({ confirmDialog: vi.fn() }));
vi.mock("@/components/Dialog", () => ({ confirmDialog: mocks.confirmDialog }));

import { confirmThen } from "./confirm";

beforeEach(() => mocks.confirmDialog.mockReset());

describe("confirmThen", () => {
  it("runs the action when confirmed", async () => {
    mocks.confirmDialog.mockResolvedValue(true);
    const run = vi.fn();
    confirmThen("Sure?", run);
    await vi.waitFor(() => expect(run).toHaveBeenCalledOnce());
  });

  it("does NOT run the action when cancelled", async () => {
    mocks.confirmDialog.mockResolvedValue(false);
    const run = vi.fn();
    confirmThen("Sure?", run);
    // Flush the .then chain before asserting the negative.
    await Promise.resolve();
    await Promise.resolve();
    expect(run).not.toHaveBeenCalled();
  });

  it("passes the message and danger flag to the dialog", () => {
    mocks.confirmDialog.mockResolvedValue(false);
    confirmThen("Delete this?", () => {});
    expect(mocks.confirmDialog).toHaveBeenCalledWith("Delete this?", { danger: true });
  });
});
