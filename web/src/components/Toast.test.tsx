import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act, fireEvent } from "@testing-library/react";
import { ToastHost, toast, errMsg } from "./Toast";

describe("errMsg", () => {
  it("unwraps Error, string, and objects", () => {
    expect(errMsg(new Error("boom"))).toBe("boom");
    expect(errMsg("plain")).toBe("plain");
    expect(errMsg({ code: 42 })).toBe('{"code":42}');
  });
});

describe("toast store + host", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    // Flush any pending auto-dismiss timers so the module store is clean.
    act(() => vi.runAllTimers());
    vi.useRealTimers();
  });

  it("renders a dispatched toast and auto-dismisses after its TTL", () => {
    render(<ToastHost />);
    act(() => {
      toast.success("Saved");
    });
    expect(screen.getByText("Saved")).toBeInTheDocument();
    // success TTL is 3s.
    act(() => vi.advanceTimersByTime(3000));
    expect(screen.queryByText("Saved")).not.toBeInTheDocument();
  });

  it("errors linger longer than successes", () => {
    render(<ToastHost />);
    act(() => {
      toast.error("Nope");
    });
    act(() => vi.advanceTimersByTime(3000));
    // Still visible at 3s (error TTL is 7s).
    expect(screen.getByText("Nope")).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(4000));
    expect(screen.queryByText("Nope")).not.toBeInTheDocument();
  });

  it("invokes a toast action and then dismisses it", () => {
    render(<ToastHost />);
    const run = vi.fn();
    act(() => {
      toast.error("Failed", { label: "Retry", run });
    });
    // fireEvent is synchronous — avoids userEvent's delay loop under fake timers.
    act(() => {
      fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    });
    expect(run).toHaveBeenCalledOnce();
    expect(screen.queryByText("Failed")).not.toBeInTheDocument();
  });
});
