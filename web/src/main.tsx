import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { App } from "@/App";
import "@/styles.css";
// NOTE: Monaco is intentionally NOT imported here — it's the bulk of the JS
// bundle. It loads lazily with the Diff view (App.tsx + DiffView.tsx) so the
// app starts light.

// Sane defaults: with the library default (staleTime 0) every component mount
// refetches, so e.g. switching Settings tabs re-fired getAppSettings on each
// tab — each an IPC round-trip (and previously a git shell-out). A short
// staleTime + no refetch-on-focus stops that storm; live data is still pushed
// via explicit invalidateQueries after mutations and the sync event.
const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      refetchOnWindowFocus: false,
      retry: 1,
    },
  },
});

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>
  </React.StrictMode>,
);
