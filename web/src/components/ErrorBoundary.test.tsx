import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { ErrorBoundary, PaneErrorBoundary } from "./ErrorBoundary";

// A child that always throws on render, to trip the boundary.
function Boom(): never {
  throw new Error("kaboom");
}

describe("ErrorBoundary", () => {
  it("renders the root fallback (message, Reload, error text) when a child throws", () => {
    // React logs the caught error; silence it so the test output stays clean.
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    render(
      <ErrorBoundary variant="root">
        <Boom />
      </ErrorBoundary>,
    );
    expect(screen.getByText("Something went wrong")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reload" })).toBeInTheDocument();
    expect(screen.getByText(/kaboom/)).toBeInTheDocument();
    spy.mockRestore();
  });

  it("renders children unchanged when nothing throws", () => {
    render(
      <ErrorBoundary variant="root">
        <div>safe content</div>
      </ErrorBoundary>,
    );
    expect(screen.getByText("safe content")).toBeInTheDocument();
  });

  it("renders a recoverable, labelled pane fallback", () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    render(
      <PaneErrorBoundary label="Tools">
        <Boom />
      </PaneErrorBoundary>,
    );
    expect(screen.getByText("Tools hit a problem.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
    spy.mockRestore();
  });
});
