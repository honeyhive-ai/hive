import { describe, it, expect, vi, beforeAll } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { Modal } from "./ui";

// jsdom lays nothing out, so every element reports zero client rects and the
// trap's visibility filter would see an empty modal. Pretend everything is
// visible.
beforeAll(() => {
  Object.defineProperty(HTMLElement.prototype, "getClientRects", {
    value: () => [{ width: 1, height: 1 }],
    configurable: true,
  });
});

function TwoButtonModal({ onClose }: { onClose: () => void }) {
  return (
    <Modal onClose={onClose}>
      <button>First</button>
      <button>Last</button>
    </Modal>
  );
}

describe("Modal", () => {
  it("focuses the first control on open", () => {
    render(<TwoButtonModal onClose={() => {}} />);
    expect(screen.getByText("First")).toHaveFocus();
  });

  it("respects a child's autoFocus over the first control", () => {
    render(
      <Modal onClose={() => {}}>
        <button>First</button>
        <button autoFocus>Preferred</button>
      </Modal>,
    );
    expect(screen.getByText("Preferred")).toHaveFocus();
  });

  it("traps Tab: cycles last → first and first → last", async () => {
    const user = userEvent.setup();
    render(<TwoButtonModal onClose={() => {}} />);
    screen.getByText("Last").focus();
    await user.tab();
    expect(screen.getByText("First")).toHaveFocus();
    await user.tab({ shift: true });
    expect(screen.getByText("Last")).toHaveFocus();
  });

  it("closes on Escape without needing focus inside the panel", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<TwoButtonModal onClose={onClose} />);
    (document.activeElement as HTMLElement | null)?.blur();
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("closes on overlay click but not on panel click", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<TwoButtonModal onClose={onClose} />);
    await user.click(screen.getByText("First"));
    expect(onClose).not.toHaveBeenCalled();
    await user.click(screen.getByRole("dialog").parentElement!);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("restores focus to the trigger on close", async () => {
    const user = userEvent.setup();
    function Host() {
      const [open, setOpen] = useState(false);
      return (
        <div>
          <button onClick={() => setOpen(true)}>Open</button>
          {open && (
            <Modal onClose={() => setOpen(false)}>
              <button onClick={() => setOpen(false)}>Close</button>
            </Modal>
          )}
        </div>
      );
    }
    render(<Host />);
    await user.click(screen.getByText("Open"));
    expect(screen.getByText("Close")).toHaveFocus();
    await user.click(screen.getByText("Close"));
    expect(screen.getByText("Open")).toHaveFocus();
  });

  it("with stacked modals, Escape only closes the top one", async () => {
    const user = userEvent.setup();
    // Like DialogHost: the second modal opens later, from inside the first.
    function Host() {
      const [outer, setOuter] = useState(true);
      const [inner, setInner] = useState(false);
      return (
        <div>
          {outer && (
            <Modal onClose={() => setOuter(false)}>
              <button onClick={() => setInner(true)}>Open inner</button>
            </Modal>
          )}
          {inner && (
            <Modal onClose={() => setInner(false)}>
              <button>Inner</button>
            </Modal>
          )}
        </div>
      );
    }
    render(<Host />);
    await user.click(screen.getByText("Open inner"));
    await user.keyboard("{Escape}");
    expect(screen.queryByText("Inner")).not.toBeInTheDocument();
    expect(screen.getByText("Open inner")).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(screen.queryByText("Open inner")).not.toBeInTheDocument();
  });
});
