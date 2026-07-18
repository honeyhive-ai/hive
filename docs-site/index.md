# ![Hive](images/hive-lockup-light.svg#only-light){ .hero-lockup }![Hive](images/hive-lockup-dark.svg#only-dark){ .hero-lockup }

**A shared LLM workspace for developers.** Bring your own subscription
(Claude, OpenAI, OpenRouter, Ollama, aider, pi, or any local agent),
collaborate with peers on real work — code, commits, reviews — and
keep your data local-first.

---

## Why Hive

Most "AI for code" tools assume one developer, one model, one
machine. Hive flips that:

- **Multiple peers, one workspace.** Alice's Claude subscription and
  Bob's Ollama instance can both participate in the same chat,
  reviewing each other's proposals.
- **Bring your own agent.** Aider, pi, Claude Code, or any CLI agent
  you trust can act as a workspace participant. Hive doesn't recreate
  agent logic — it hosts what you bring.
- **Local-first, P2P sync.** Workspaces sync directly between peers
  over a tiny optional relay. No central server holds your content.
- **Nothing runs behind your back.** Installed MCP servers stay inert
  until you enable them, Claude Code respects its own permission mode,
  and agent changes arrive as proposals you review before implementing.

---

## What's in here

<div class="grid cards" markdown>

-   :material-rocket-launch:{ .lg } **Getting started**

    First launch, configuring runtimes, your first chat.

    [:octicons-arrow-right-24: Start here](getting-started/first-launch.md)

-   :material-graph:{ .lg } **Concepts**

    Workspaces, agents, MCP tools, permissions, identity.

    [:octicons-arrow-right-24: Read the model](concepts/workspaces.md)

-   :material-server-network:{ .lg } **Networking & relay**

    How peers find each other; setting up your own relay.

    [:octicons-arrow-right-24: Networking](networking/rendezvous.md)

-   :material-puzzle:{ .lg } **Addon agents**

    Wire aider, pi, or any subprocess agent as a workspace participant.

    [:octicons-arrow-right-24: BYO agents](addons/byo-agents.md)

</div>

---

## Built with

Hive is a **Tauri v2** desktop app — a Rust backend (`crates/hive-core`,
`hive-runtime`, `hive-relay`, plus `app/`) with a React + TypeScript
frontend (`web/`). One codebase ships **macOS, Windows, and Linux**. State is an
append-only event log in SQLite; replies stream from your chosen runtime (the
`claude` CLI by default — bring your own subscription).

## Project status

Pre-release. The multi-runtime workspace + relay-forwarded multiuser sync are in
place. Build from source with `cargo tauri build` (see
[Building the dist](ops/build.md)); run a relay from
[github.com/honeyhive-ai/relay](https://github.com/honeyhive-ai/relay) (or
`cargo run -p hive-relay` locally against the in-repo reference crate).
Release builds are **unsigned** for now — run from source or click through your
OS's first-launch prompt.

The reference relay is MIT-licensed and lives at `crates/hive-relay/` (a small
axum service). Direct peer-to-peer sync is implemented with **iroh** (see the
**Direct peers (P2P)** section in Settings) and falls back to relay forwarding
when a direct connection can't be established. The collaborative-text CRDT is a
tracked follow-up.
