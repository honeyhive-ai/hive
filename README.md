<div align="center">

<img src="assets/branding/hive-app-icon-1024.png" alt="Hive" width="112" height="112">

# Hive

**A collaborative, multi-agent workspace for developers.**

Chat with one or many coding agents, review their proposed changes, and sync the
whole workspace across your devices and teammates — peer-to-peer or through a
relay, end-to-end encrypted. Your code and keys stay on your machine.

[![CI](https://github.com/honeyhive-ai/hive/actions/workflows/ci.yml/badge.svg)](https://github.com/honeyhive-ai/hive/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/honeyhive-ai/hive?include_prereleases&sort=semver&label=release)](https://github.com/honeyhive-ai/hive/releases/latest)
[![Docs](https://img.shields.io/badge/docs-apiaryhq.ai-4c8bf5)](https://docs.apiaryhq.ai/)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)

[**Download**](#install) · [**Documentation**](https://docs.apiaryhq.ai/) · [**Build from source**](#build-from-source) · [**Contributing**](CONTRIBUTING.md)

</div>

---

Hive is a small native desktop app built with **Rust + [Tauri v2](https://tauri.app) +
React/TypeScript**, so one codebase ships on **macOS, Linux, and Windows**. It's
local-first: agents run against your working tree, your model keys never leave
the device, and anything that syncs to teammates is end-to-end encrypted — the
relay only ever sees ciphertext.

## Highlights

- **Bring your own agents & runtimes** — the `claude` CLI (no API key needed),
  Anthropic / OpenAI / OpenRouter / Ollama / any OpenAI-compatible endpoint, and
  subprocess agents (aider, pi). Model lists come from each runtime, not a
  hardcoded set.
- **Multi-agent + review** — route work to one or many agents, then
  propose / diff / approve their changes, with quorum voting and reactions.
- **Agentic workflows** — compose agents into DAG pipelines with parallel
  fan-out and human approval gates in a visual editor, or just ask an agent in
  chat to build the workflow for you. Definitions and runs sync E2EE, and
  teammates vote gates from their own devices.
- **MCP, skills & vaults** — Model Context Protocol servers (inert until you
  enable them), reusable skills, and GitHub / GitLab / HTTPS knowledge sources.
- **Real-time collaboration** — direct P2P ([iroh](https://iroh.computer)) with
  relay fallback; end-to-end encrypted workspaces shared by GitHub `@handle`.
- **GitHub identity** — sign in once; invite teammates by handle; one account,
  many devices.

## Install

Grab the installer for your platform from the
[**latest release**](https://github.com/honeyhive-ai/hive/releases/latest):

| Platform | Download |
| --- | --- |
| **macOS** (Apple Silicon) | `Hive_*_aarch64.dmg` |
| **Linux** | `Hive_*_amd64.AppImage`, `.deb`, or `.rpm` |
| **Windows** | `Hive_*_x64-setup.exe` or `.msi` |

On first launch, a four-step onboarding sets up your identity, project folder,
agent/runtime, and (optionally) a team relay — no config files required.

> **Status.** Hive is under active development and current releases are tagged as
> previews (`0.2.x`). Installers are unsigned for now, so macOS/Windows may warn
> on first open. A Homebrew cask is being prepared under
> [`deploy/homebrew`](deploy/homebrew).

## Documentation

Full user and self-hosting docs live at **[docs.apiaryhq.ai](https://docs.apiaryhq.ai/)**
(source in [`docs-site/`](docs-site/)):

- [First launch](https://docs.apiaryhq.ai/getting-started/first-launch/) ·
  [Configuring a runtime](https://docs.apiaryhq.ai/getting-started/configuring-a-runtime/) ·
  [First chat](https://docs.apiaryhq.ai/getting-started/first-chat/)
- [Multi-agent](https://docs.apiaryhq.ai/features/multi-agent/) ·
  [Workflows](https://docs.apiaryhq.ai/features/workflows/) ·
  [Collaboration](https://docs.apiaryhq.ai/features/collaboration/)
- [Self-hosting a relay](https://docs.apiaryhq.ai/networking/self-host/) ·
  [Tools & permissions](https://docs.apiaryhq.ai/concepts/tools-and-consent/)

## Build from source

Prerequisites: **Rust** (via [rustup](https://rustup.rs)), **[Bun](https://bun.sh)**,
the [Tauri v2 system deps](https://tauri.app/start/prerequisites/) for your OS,
and the [`tauri-cli`](https://tauri.app/reference/cli/) (`cargo install tauri-cli`).

```bash
cargo test --workspace              # Rust crates + the Tauri app
cd web && bun install && bun run build   # frontend: typecheck (tsc) + build
cargo tauri dev                     # run the app (frontend + backend)
cargo run -p hive-relay             # optional: a local relay on :8443
```

Packaged installers are produced by [`scripts/build.sh`](scripts/build.sh) (e.g.
`./scripts/build.sh mac`). See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the full
dev loop and [`docs/architecture.md`](docs/architecture.md) for how the pieces
fit together.

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
deploy/          Homebrew cask + distribution
docs/            developer & maintainer docs
docs-site/       published user/admin docs (MkDocs)
```

## Editions

The desktop app and the reference relay are open source (this repo). A
managed / enterprise tier adds server-side membership & RBAC, hosted relays, and
org (GHEC) integration — those paid controls live outside this repo and attach
via a small `WriteGuard` extension seam, so self-hosters keep a fully functional,
content-blind relay. See [the tiering model](docs-site/ops/tiering.md).

## Contributing & security

Issues and PRs are welcome — start with [`CONTRIBUTING.md`](CONTRIBUTING.md) and
our [Code of Conduct](CODE_OF_CONDUCT.md). To report a vulnerability, follow
[`SECURITY.md`](SECURITY.md) (please don't open a public issue for security bugs).

## License

[MIT](LICENSE).
