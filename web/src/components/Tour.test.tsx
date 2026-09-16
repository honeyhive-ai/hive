import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Tour, type TourStep } from "./Tour";

function steps(overrides: Partial<TourStep>[] = []): TourStep[] {
  const base: TourStep[] = [
    { title: "Intro", body: "Welcome" },
    { title: "Chats", body: "Chats live here", anchor: "sidebar", before: vi.fn() },
    { title: "Done", body: "All set" },
  ];
  return base.map((s, i) => ({ ...s, ...(overrides[i] ?? {}) }));
}

describe("Tour", () => {
  it("shows the first step and a Start button", () => {
    render(<Tour steps={steps()} onClose={vi.fn()} />);
    expect(screen.getByText("Intro")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Start" })).toBeTruthy();
    // No Back on the first step.
    expect(screen.queryByRole("button", { name: "Back" })).toBeNull();
  });

  it("advances through steps and runs a step's before() side effect", async () => {
    const user = userEvent.setup();
    const before = vi.fn();
    render(<Tour steps={steps([{}, { before }])} onClose={vi.fn()} />);

    await user.click(screen.getByRole("button", { name: "Start" }));
    expect(screen.getByText("Chats")).toBeTruthy();
    // The step that just mounted drove the UI via before().
    expect(before).toHaveBeenCalled();
    // Back now exists.
    expect(screen.getByRole("button", { name: "Back" })).toBeTruthy();
  });

  it("Skip closes without completing", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<Tour steps={steps()} onClose={onClose} />);
    await user.click(screen.getByRole("button", { name: "Skip" }));
    expect(onClose).toHaveBeenCalledWith(false);
  });

  it("Done on the last step completes", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<Tour steps={steps()} onClose={onClose} />);
    await user.click(screen.getByRole("button", { name: "Start" })); // → step 2
    await user.click(screen.getByRole("button", { name: "Next" })); // → last step
    await user.click(screen.getByRole("button", { name: "Done" }));
    expect(onClose).toHaveBeenCalledWith(true);
  });

  it("Escape skips the tour", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<Tour steps={steps()} onClose={onClose} />);
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledWith(false);
  });
});
