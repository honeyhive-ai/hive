# Per-workspace issue / ticket integration

Status: design. Goal — let a Hive workspace **plug into an existing ticketing
system** (GitHub Issues/PRs, Linear, Jira, …) so agents can do **CRUD on the
items**: read the backlog, open issues, comment, move status, link PRs, close.

The key decision (from the product discussion): **don't build a tracker — plug
into one.** Hive already has the machinery for this — the **MCP framework** with
per-tool trust — so an integration is mostly *configuration + capability gating*,
not new infrastructure.

## Why MCP is the right seam

Hive already:

- runs **MCP servers** per workspace (stdio + http), **inert until enabled**,
  with **per-tool trust toggles** (see [MCP servers](../docs-site/addons/mcp.md));
- has a **multi-turn tool loop** so an agent can call a tool, read the result,
  and continue;
- holds a **GitHub OAuth token** already (the directory/identity work).

Every major tracker either ships an MCP server or has a thin API we can wrap:

| Tracker | Connector | Notes |
|---|---|---|
| **GitHub Issues/PRs** | GitHub's MCP server (or our own thin wrapper over the REST/GraphQL API) | **First connector** — we already have the user's GitHub token; zero extra auth. Natural for a dev tool. |
| **Linear** | Linear's MCP server / GraphQL | "just plug in Linear"; OAuth or API key per workspace |
| **Jira** | community/Atlassian MCP server | API token per workspace |
| **(generic)** | any MCP server exposing item CRUD | falls out for free |

So the feature is: **a small abstraction over "an issue source," concrete
connectors behind it, surfaced as MCP tools, governed by RBAC.**

## Model

A workspace may have zero or more **issue sources**, configured in
`hive.config.toml` next to `[[mcp_servers]]` (or as a typed `[[issue_source]]`
that *expands into* an MCP server config, so there's one execution path):

```toml
[[issue_source]]
kind   = "github"          # github | linear | jira | mcp
repo    = "honeyhive-ai/hive"   # github: owner/repo
# auth: github reuses the signed-in account token; others take a key ref
# token_env = "LINEAR_API_KEY"  # for linear/jira
enabled = false            # inert until enabled, like every MCP server
```

Each source advertises a normalized **tool surface** the agents see:

- `issues.list(query)` / `issues.get(id)` — read
- `issues.create({title, body, labels, assignee})` — create
- `issues.update(id, {…})` / `issues.comment(id, body)` — update
- `issues.transition(id, status)` — move state (open/in-progress/done)
- `pr.link(issue_id, pr)` / `pr.status(pr)` — GitHub PR glue

Connectors map these onto the tracker's native API (GitHub issues, Linear
mutations, Jira transitions). Reads can also feed **context/vaults** so an agent
"knows the backlog" without an explicit tool call.

## RBAC gating (ties into the entitlement work)

CRUD on someone's real backlog is privileged, so it rides the same controls as
the rest of the admin model:

- **Per-tool MCP trust** (already built) — writes (`create`/`update`/
  `transition`) start untrusted; a human approves the tool, per workspace.
- **Capability gate** — once the [RBAC capabilities](../docs-site/ops/managed-service.md)
  land, `issues.create`/`transition`/etc. require a `manage_integrations` (or a
  finer `issues.write`) capability; `Viewer`s get read-only tools.
- **Consent-on-demand** — the existing built-in-tool consent flow surfaces "Agent
  wants to create issue *X* in *repo*" for approval, with the agreement-gated
  execution loop for anything that mutates the tracker.

So an agent proposing "I'll file these 3 issues and move HIVE-42 to In Review"
goes through the same review/approval path as a code edit — no silent writes to
your tracker.

## Phasing

1. **GitHub Issues/PRs connector (first).** Reuse the signed-in GitHub token;
   wrap REST/GraphQL behind the normalized tool surface; read-only first, then
   gated writes. Highest value for the least new auth.
2. **`[[issue_source]]` config + Settings UI.** Typed config that expands to an
   MCP server; a Settings → Integrations pane (enable/trust per tool).
3. **Linear connector.** Via Linear's MCP server / GraphQL, API key per
   workspace.
4. **Jira + generic MCP.** Any tracker with an MCP server drops in.
5. **Backlog-as-context.** Optionally surface open items into the agent's context
   budget so planning sees the live backlog.

## Tier placement

The connectors themselves are **free/core** (they're just MCP integrations).
What's **paid** is the *governance* around agent writes to a shared tracker —
capability-based RBAC, org policy ("agents may file but not close"), and the
audit trail of agent-made changes — which sits in the same managed/enterprise
relay surface as the rest of [`tiering.md`](../docs-site/ops/tiering.md).
