# `web` — Hive frontend

The **React 18 + TypeScript** frontend, built with **Vite**. It runs inside the
[Tauri desktop shell](../app/README.md) (not a standalone web server) and talks
to the Rust backend over IPC.

## Layout

- `src/components/` — the UI (chat, diff view, right-rail Review/Workflows panes,
  onboarding, settings, workflow builder, …).
- `src/lib/ipc.ts` — the **Tauri IPC** layer: invokes backend commands and
  subscribes to emitted events. Other `src/lib/` modules cover mentions, slash
  commands, templates, workflow helpers, theme, and attachments.
- `src/bindings/` — **generated** TypeScript types, mirrored from the Rust DTOs
  in [`hive-proto`](../crates/hive-proto/README.md). Do not hand-edit; regenerate
  with `cargo test -p hive-proto export_bindings`.
- Data fetching uses **TanStack Query**; routing uses TanStack Router.

## Dev commands

```bash
bun install        # or: npm install
bun run dev        # Vite dev server (usually driven via `cargo tauri dev`)
bun run build      # tsc --noEmit typecheck + vite build
bun run test       # vitest
bun run typecheck  # tsc --noEmit
```

> **Node.js is a prerequisite for `bun install` here.** `esbuild` is a
> `trustedDependency`, so bun runs its postinstall (`node install.js`). With no
> `node` on PATH that script fails and bun **removes esbuild** rather than leaving
> a broken install — quietly: `bun install` reports success, and the breakage only
> surfaces later as vitest failing with `Cannot find package 'esbuild'` (which
> points at the symptom, not the cause). If you can't install Node, install
> without postinstall scripts instead:
>
> ```bash
> bun install --ignore-scripts
> ```

The full app runs via `cargo tauri dev` from the repo root, which serves this
frontend and the Rust backend together.
