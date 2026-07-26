import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  children: ReactNode;
  /// Compact ("pane") degrades a single broken pane; "root" is the full-window
  /// crash backstop. Label names the failed region in the compact fallback.
  variant?: "root" | "pane";
  label?: string;
}

interface State {
  error: Error | null;
}

/// Crash backstop. A render throw anywhere below this boundary is caught by
/// `getDerivedStateFromError` (which swaps in the fallback) instead of tearing
/// down the whole React tree to a white screen. `componentDidCatch` logs the
/// stack for diagnostics. All colors route through `--hive-*` tokens.
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("ErrorBoundary caught a render error:", error, info.componentStack);
  }

  reset = () => this.setState({ error: null });

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;

    const message = error.message || String(error);

    // Compact fallback: one broken pane degrades in place; "Try again" clears
    // the boundary so a transient failure can re-render without a full reload.
    if (this.props.variant === "pane") {
      return (
        <div className="flex h-full items-center justify-center p-4">
          <div
            className="max-w-sm rounded-xl border px-4 py-4 text-center text-sm"
            style={{ borderColor: "var(--hive-line)", background: "var(--hive-mist)", color: "var(--hive-ink)" }}
          >
            <div className="font-medium" style={{ color: "var(--hive-danger)" }}>
              {this.props.label ? `${this.props.label} hit a problem.` : "This panel hit a problem."}
            </div>
            <p className="mt-1 text-xs opacity-70">The rest of the app is still working.</p>
            <button
              onClick={this.reset}
              className="mt-3 rounded-lg px-3 py-1.5 text-xs font-medium transition-all hover:brightness-105"
              style={{ background: "var(--hive-accent-cool)", color: "#fff" }}
            >
              Try again
            </button>
            <details className="mt-3 text-left">
              <summary className="cursor-pointer text-xs opacity-55">Error details</summary>
              <pre
                className="mt-2 overflow-x-auto whitespace-pre-wrap rounded-lg p-2 text-[11px] leading-5"
                style={{ background: "var(--hive-overlay)", color: "var(--hive-ink)" }}
              >
                {message}
              </pre>
            </details>
          </div>
        </div>
      );
    }

    // Root fallback: fills the window. A render throw would otherwise leave a
    // blank screen with no way out, so offer an unconditional reload.
    return (
      <div
        className="flex h-full min-h-screen flex-col items-center justify-center p-6"
        style={{ background: "var(--hive-canvas)", color: "var(--hive-ink)" }}
      >
        <div
          className="w-full max-w-md rounded-2xl border p-6 text-center"
          style={{ borderColor: "var(--hive-line)", background: "var(--hive-panel)" }}
        >
          <h1 className="text-lg font-semibold tracking-tight" style={{ color: "var(--hive-danger)" }}>
            Something went wrong
          </h1>
          <p className="mt-2 text-sm opacity-70">
            Hive hit an unexpected error and couldn't finish rendering. Your data is safe on disk —
            reloading usually clears it.
          </p>
          <button
            onClick={() => location.reload()}
            className="mt-4 rounded-xl px-4 py-2 text-sm font-semibold transition-all hover:brightness-105"
            style={{ background: "var(--hive-accent-cool)", color: "#fff" }}
          >
            Reload
          </button>
          <details className="mt-4 text-left">
            <summary className="cursor-pointer text-xs opacity-55">Error details</summary>
            <pre
              className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap rounded-lg p-3 text-[11px] leading-5"
              style={{ background: "var(--hive-mist)", color: "var(--hive-ink)" }}
            >
              {message}
              {error.stack ? `\n\n${error.stack}` : ""}
            </pre>
          </details>
        </div>
      </div>
    );
  }
}

/// Lighter boundary for a single pane / region — one broken pane degrades to a
/// recoverable card instead of taking the whole window down.
export function PaneErrorBoundary({ children, label }: { children: ReactNode; label?: string }) {
  return (
    <ErrorBoundary variant="pane" label={label}>
      {children}
    </ErrorBoundary>
  );
}
