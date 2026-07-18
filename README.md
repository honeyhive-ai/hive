# Hive

A collaborative, multi-agent developer workspace: chat with one or many coding
agents, review their proposed changes, and sync the whole workspace across your
devices and teammates — peer-to-peer or through a relay, end-to-end encrypted.

Built with **Rust + [Tauri v2](https://tauri.app) + React/TypeScript**, so one
codebase ships a small native app on **macOS, Linux, and Windows**.

## Highlights

- **Bring-your-own agents/runtimes** — the `claude` CLI (no API key needed),
  Anthropic / OpenAI / OpenRouter / Ollama / any OpenAI-compatible endpoint, and
  subprocess agents (aider, pi).
- **Multi-agent + review** — route work to agents, propose/diff/approve changes,
  quorum voting, reactions.
- **Agentic workflows** — compose agents into DAG pipelines with parallel
  fan-out and human approval gates, in a visual editor — or just ask an
  agent in chat to build the workflow for you; definitions and runs sync
  E2EE, and teammates vote gates from their own devices.
- **MCP, skills & vaults** — Model Context Protocol servers (inert until you
  enable them), reusable skills, and GitHub/GitLab/HTTPS knowledge sources.
- **Multiuser sync** — direct P2P (iroh) with relay fallback; workspaces are
  end-to-end encrypted, so the relay only ever sees ciphertext.
- **GitHub identity** — sign in once; invite teammates by `@handle`; one account,
  many devices.

## Repository layout

```
crates/
  hive-core      domain logic: models, config, crypto/E2EE, MCP wire types,
                 authorization, context budgeting, the event-sourced projector
  hive-runtime   SQLite event store, identity, provider adapters, MCP manager,
                 transports (relay/P2P), sync engine
  hive-proto     shared IPC contract types (serde) → ts-rs-generated TS bindings
  hive-relay     the content-blind relay (axum) — forwarding + entitlement gate
app/             the Tauri app: registers commands/events, owns the runtime
web/             React + TypeScript frontend
addons/          optional agent shims (e.g. aider-mcp)
deploy/          Homebrew tap/cask for distribution
docs/            developer & maintainer docs
docs-site/       published user/admin docs (MkDocs)
```

## Quick start

```bash
# prerequisites: Rust (rustup), Bun (https://bun.sh), and the Tauri deps for your OS
cargo test --workspace            # Rust tests
cd web && bun install && bun run build   # frontend typecheck + build
cargo tauri dev                   # run the app
cargo run -p hive-relay           # (optional) run a local relay
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the dev loop and
[`docs/architecture.md`](docs/architecture.md) for how it fits together.

## Docs

- **Use / self-host Hive** → the published site, built from [`docs-site/`](docs-site/).
- **Work on Hive** → [`docs/`](docs/) (architecture, multiuser, packaging,
  relay deploy, tiering).

## Editions

The app and the reference relay are open source. A managed/enterprise tier adds
server-side membership/RBAC, hosted relays, and org (GHEC) integration — those
paid controls live outside this repo and attach via a small `WriteGuard`
extension seam. See [the tiering model](docs-site/ops/tiering.md).

## License

MIT.
