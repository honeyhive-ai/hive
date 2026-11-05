# The `hive serve` dashboard

The iPhone app's dashboard tab shows whatever the host defines. Every host
starts with a built-in **Hive** section about the workspace it serves
(agents, proposals, workflow runs, turns, the host's own status), so the tab
is useful without a home lab or any setup. Beyond that, you describe
sections of panels in `dashboard.toml`; `hive serve` evaluates them and
serves the results at `GET /v1/dashboard`. The phone never talks to
Prometheus, the URLs or the MCP servers in the file. It only gets numbers,
rows, tables and series.

A panel reads from one of four sources: a workspace metric (`hive`),
Prometheus, a JSON URL, or a tool on one of the host's MCP servers that the
file allow-lists by name. The file itself never runs a command.

With no `dashboard.toml`:

- the dashboard is the built-in Hive section;
- if `HIVE_PROMETHEUS_URL` is set, the host adds the lab sections that
  `/v1/lab/status` used to show (power and cost, gateway workers,
  temperatures, and solar when an Enphase exporter reports) below it.
  Sections with no data are left out. `hive dashboard default` prints them as
  TOML for you to edit.

## The built-in Hive section

Shown first on every dashboard unless `dashboard.toml` says otherwise:

| Panel | Metric |
|---|---|
| Agents online, Agents busy | `agents.online`, `agents.busy` |
| Awaiting your review (amber from 1), Open proposals | `proposals.awaiting_me`, `proposals.open` |
| Turns today, Turns, 7 days | `turns.today`, `turns.7d` |
| Workflows running, Awaiting approval (amber from 1), Failed today (red from 1). Only once the workspace has a workflow | `workflows.running`, `workflows.awaiting_approval`, `workflows.failed_today` |
| Phones paired. Only with remote access on | `phones.paired` |
| This host: Hive serve, Relay sync ("Synced 4 s ago"), Remote access, Push | `host.status` |

"Your review" means the identity the phone acts as: this box, or its owner
once the box is enrolled as the owner's device. A proposal awaits you when
it's open, you may vote on it, you haven't, and you didn't write it.

To move or hide it, set a top-level key (before any `[mcp]` table or
`[[section]]`):

```toml
builtin = false      # no Hive section
builtin = "bottom"   # below your sections ("top" or true: above, the default)
```

A `dashboard.toml` written before this existed has no `builtin` key, so it
gets the Hive section above its own sections.

## Where the file lives

`<data dir>/dashboard.toml`: `HIVE_DATA_DIR`, the same directory as
`config.toml` and `serve.token`. Set `HIVE_DASHBOARD_FILE` to use another
path. The host re-reads it on every request, so edits apply without a
restart.

```sh
hive dashboard check                 # validate, evaluate every panel once, print the results
hive dashboard check --file x.toml   # check another file
hive dashboard default               # print the generated lab default as TOML
hive dashboard metrics               # list the hive metrics
```

Run `check` with the server's environment (for example from its env file) so
it uses the same `HIVE_PROMETHEUS_URL`, `HIVE_MCP_CONFIG` and data dir. It
exits non-zero when the file is invalid or any panel has an error. It reads
the workspace from the store but isn't the running server, so the Hive
section's live rows (relay sync, push) read as unknown. Note that `check`
calls every MCP tool the file uses, once.

## Format

```toml
title = "Home lab"   # tab title in the app (default "Dashboard")
refresh = "15s"      # default panel refresh (how long a result is reused)
timeout = "3s"       # default panel timeout (at most 30s)
builtin = true       # the built-in Hive section: true/"top" (default), "bottom", false

[mcp]
allow = ["github/list_pull_requests"]   # MCP tools panels may call, by exact name

[[section]]
title = "Server"
optional = true          # hide the section when none of its panels has data
show_when = "up{job=\"node\"}"   # PromQL: hide the section unless this returns data

[[section.panel]]
id = "cpu"               # optional; default "<section>-<title>" slugs
title = "CPU"
kind = "gauge"           # stat | gauge | timeseries | bar | list | status | text | table
unit = "%"
query = '100 * (1 - avg(rate(node_cpu_seconds_total{mode="idle"}[5m])))'
round = 0                # decimal places
scale = 1                # multiply every value (e.g. 0.001 for mW → W)
min = 0
max = 100
thresholds = [{ value = 0, level = "ok" }, { value = 70, level = "warn" }, { value = 90, level = "critical" }]
refresh = "10s"
timeout = "2s"
```

Durations are `500ms`, `15s`, `5m`, `24h`, `7d` or a number of seconds.
Unknown keys are errors, so a typo doesn't fail silently.

### Sources

| `source` | Set | What it does |
|---|---|---|
| `prometheus` (default) | `query` | An instant query against `HIVE_PROMETHEUS_URL` (timeseries and sparklines use a range query) |
| `http_json` (default when `url` is set) | `url`, `path` | `GET url` (http or https, on the host's network; at most 1 MiB) and read the value at `path`, a JSON pointer such as `/data/0/temp`. No headers or credentials are sent |
| `hive` (default when `metric` is set) | `metric` | A metric of the workspace this host serves, see [Hive metrics](#hive-metrics) |
| `mcp` (default when `tool` is set) | `server`, `tool`, `args`, `path` | Call a tool on one of the host's MCP servers and read its result like a JSON document, see [MCP tools](#mcp-tools) |

### Panel kinds

| `kind` | Data | Extra keys |
|---|---|---|
| `stat` | One number: the first sample of `query`, or the number at `path` | `sparkline = true` (the query over the last hour) or `sparkline = { range = "6h", step = "5m", query = "…" }` |
| `gauge` | One number on `min`…`max` (default 0…100) | `min`, `max`, `thresholds` |
| `timeseries` | 1–3 series over time | `series = [{ name, query }]` (or `path` for http_json), `range` (default `24h`), `step` (default range/60; at most 1000 points). A single `query` is one series named after the panel |
| `bar` / `list` | Label/value rows (bars against `max`, or a plain list) | Prometheus: every sample of `query` is a row, labelled by the `label` label (default: all label values); or `rows = [{ label, query }]`. http_json: `path` to an array of objects (`label` key, default `name`; `value_key`, default `value`) or to an object (each key is a row); or `rows = [{ label, path }]` |
| `status` | Up/down rows | Rows as for `list`. A number is up at or above `up_when` (default 1); a string is up when it equals `up_text` (default ok, up, true, healthy, online, running, pass). When the source fails, every configured row reads down |
| `text` | A line of text | A string at `path`, a number, or a fixed `text = "…"`. An mcp text panel with no `path` shows the tool's text |
| `table` | Columns and rows of cells (`http_json`, `mcp`, `hive`) | `path` to an array; `columns = [{ label, key, align }]`, where `key` is a key of each item or a JSON pointer into it (`/user/login`; default: the label) and `align` is `leading`, `center` or `trailing` (a column of numbers defaults to trailing). No `columns`: the keys of the first item (at most 8). `limit` rows (default 20, at most 200) |

`limit` also caps the rows of `bar`, `list` and `status` panels.

`thresholds` are ascending; a value gets the level of the highest threshold
at or below it (`ok`, `warn` or `critical`), and none below the first. The
app colours values by level.

`unit` is shown after the value. A unit starting with `$` is shown as money:
`unit = "$/h"` reads `$0.046/h`.

For http_json time series, `path` points at `[[t, v], …]` or
`[{t|time|timestamp: …, v|value: …}, …]`; times are Unix seconds, Unix
milliseconds or RFC 3339.

### Hive metrics

`source = "hive"` with `metric = "…"` reads the workspace this host serves,
from its synced event log. `hive dashboard metrics` prints this list.

| Metric | Shape | What |
|---|---|---|
| `agents.total` | number | Agents in the workspace |
| `agents.online` | number | Agents whose host is online. An agent on its owner's own device counts while one of that person's registered devices is online |
| `agents.busy` | number | Agents answering a turn right now (this host's turns, and replies streaming in from other hosts) |
| `agents` | rows | Each agent: up while its host is online, text Busy, Idle or Offline |
| `agents.by_host` | rows | Number of agents per host |
| `hosts` | rows | Each registered host: up while online |
| `proposals.open` | number | Open proposals (not dismissed or superseded) |
| `proposals.awaiting_me` | number | Open proposals you may vote on and haven't, your own excluded |
| `proposals.list` | table | Open proposals, newest first: Proposal, Chat, Approvals |
| `workflows.running` | number | Workflow runs in progress |
| `workflows.awaiting_approval` | number | Runs waiting at an approval gate |
| `workflows.failed_today` | number | Runs that failed since local midnight |
| `workflows.completed_today` | number | Runs that completed since local midnight |
| `workflows` | rows | The four counts above as rows |
| `workflows.list` | table | The latest runs: Workflow, Status, Started |
| `turns.today` | number | Agent replies since local midnight |
| `turns.7d` | number | Agent replies in the last 7 days |
| `turns.by_agent_7d` | rows | Agent replies in the last 7 days, per agent |
| `host.status` | rows | This host: Hive serve, Relay sync, Remote access (when on), Push |
| `sync.age` | number | Seconds since the last successful relay sync |
| `sync.last` | text | The last relay sync, e.g. "Synced 4 s ago" |
| `phones.paired` | number | Phones paired for remote access (`hive remote`) |

A number fits `stat`, `gauge` and `text`, or a row of a list: `rows =
[{ label = "Open", metric = "proposals.open" }, …]`. Rows fit `bar`, `list`
and `status`; tables fit `table`. "Today" starts at local midnight in the
host's time zone, read when `hive serve` starts.

Hive doesn't record token usage, so there are no token or cost metrics.

```toml
[[section]]
title = "Team"

[[section.panel]]
title = "Waiting on me"
metric = "proposals.awaiting_me"
thresholds = [{ value = 1, level = "warn" }]

[[section.panel]]
title = "Review queue"
kind = "table"
metric = "proposals.list"
limit = 5

[[section.panel]]
title = "Who did the work, 7 days"
kind = "bar"
metric = "turns.by_agent_7d"
```

### MCP tools

`source = "mcp"` calls a tool on an MCP server from `HIVE_MCP_CONFIG`, the
same file that gives agents on this host their MCP tools (see
[`deploy/hive-serve/mcp.toml.example`](../deploy/hive-serve/mcp.toml.example)).
Bearer tokens stay in the environment variables that file names.

```toml
[mcp]
allow = ["github/list_pull_requests"]

[[section]]
title = "Code"

[[section.panel]]
title = "Open pull requests"
kind = "table"
source = "mcp"
server = "github"                 # an id in HIVE_MCP_CONFIG
tool = "list_pull_requests"
args = { owner = "acme", repo = "web", state = "open" }
columns = [
  { label = "#", key = "number" },
  { label = "Title", key = "title" },
  { label = "Author", key = "/user/login" },
]
refresh = "5m"
```

How the result is read:

- `isError: true` is a panel error carrying the tool's message.
- `structuredContent`, when the tool returns it, is the document.
- Otherwise the `text` items of `content` (and the `text` of `resource`
  items): one item that parses as JSON is that JSON; several that all parse
  are an array of them; anything else is the text as a JSON string (so a tool
  that answers `42` works for a `stat`). Images and audio are ignored. At
  most 1 MiB.
- From there it works like `http_json`: `path` picks a number or string for
  `stat`, `gauge`, `text` and `status`; an array or object for `list`, `bar`
  and `status` rows (`label`, `value_key`, or `rows = [{ label, path }]`);
  an array for `table`; `series` paths for `timeseries`. A `text` panel with
  no `path` shows the tool's text as is.

**The risk.** MCP has no way to mark a tool read-only, and the host can't
tell a lookup from an action. A panel calls its tool on a timer, with the
host's MCP credentials, for as long as someone has the dashboard open. So:

- every tool must be named exactly in `[mcp] allow` as `"<server>/<tool>"`.
  Wildcards (`github/*`) are rejected, and a panel whose tool isn't listed is
  invalid. Only allow tools that read;
- a tool is called at most once per `refresh` per panel: results (errors
  too) are cached for the refresh, and concurrent requests share one call.
  MCP panels default to `refresh = "60s"` and can't go below `10s`;
- a call is only made while the app asks for the dashboard (no background
  polling when nobody looks), never more often than that;
- each call has its own timeout (default `10s` for MCP panels, at most
  `30s`); a stdio server that doesn't answer in time is killed. A failure is
  that panel's `error`;
- `args` are sent as written; there is no templating.

A stdio server is a process the host starts for each call (as it does for
agent turns), so prefer a long `refresh` for those.

### Errors

Every panel is evaluated on its own with its own timeout. A failure (an
unreachable Prometheus, an empty result, a bad pointer, an HTTP error, an
invalid panel) becomes that panel's `error`; the rest of the dashboard is
unaffected. A failed panel is retried after at most 5 s (an MCP panel after
its `refresh`). A file that isn't valid TOML gives a top-level `error`, which
the app shows, above the built-in Hive section.

## Response (`GET /v1/dashboard`)

Bearer auth like every other `/v1` route.

```json
{
  "title": "Home lab",
  "source": "config",            // "config" (dashboard.toml) | "default" (no file)
  "refreshSeconds": 10,          // the shortest panel refresh: a polling hint
  "updatedAt": "2026-10-27T17:03:11.52Z",
  "error": "…",                  // only when dashboard.toml doesn't parse
  "sections": [{
    "title": "Server",
    "panels": [
      {"id": "cpu", "title": "CPU", "kind": "gauge", "unit": "%", "value": 12, "min": 0, "max": 100,
       "level": "ok", "thresholds": [{"value": 0, "level": "ok"}], "refreshSeconds": 10, "updatedAt": "…"},
      {"id": "server-load-1-m", "title": "Load (1 m)", "kind": "stat", "value": 0.42,
       "sparkline": [{"t": "…", "v": 0.4}], "refreshSeconds": 15, "updatedAt": "…"},
      {"id": "server-network", "title": "Network", "kind": "timeseries", "unit": "Mbit/s",
       "series": [{"name": "In", "points": [{"t": "…", "v": 1.2}]}], "refreshSeconds": 15, "updatedAt": "…"},
      {"id": "server-disk-used", "title": "Disk used", "kind": "bar", "unit": "%", "max": 100,
       "rows": [{"label": "/", "value": 41, "level": "ok"}], "refreshSeconds": 15, "updatedAt": "…"},
      {"id": "models-scrape-targets", "title": "Scrape targets", "kind": "status",
       "rows": [{"label": "node", "up": true, "value": 1}], "refreshSeconds": 15, "updatedAt": "…"},
      {"id": "models-model-server", "title": "Model server", "kind": "status",
       "rows": [{"label": "Model server", "up": false}], "error": "GET failed: connection failed",
       "refreshSeconds": 15, "updatedAt": "…"},
      {"id": "code-open-pull-requests", "title": "Open pull requests", "kind": "table",
       "columns": [{"label": "#", "align": "trailing"}, {"label": "Title"}, {"label": "Author"}],
       "cells": [[128, "Fix login redirect", "alex"], [131, "Bump deps", null]],
       "refreshSeconds": 300, "updatedAt": "…"}
    ]
  }]
}
```

Rows carry `value` (number) or `text` (string), plus `up` for status and
`level` when thresholds apply; the app shows a status row's `text` (e.g.
"Busy", "Synced 4 s ago") in place of Up/Down. Table cells are strings,
numbers, booleans or null; a nested object or array arrives as compact JSON
text. A client should ignore kinds and keys it doesn't know.

## Examples

- No home lab: no file at all gives the Hive section.
  [`deploy/hive-serve/dashboard-team.toml.example`](../deploy/hive-serve/dashboard-team.toml.example)
  adds a review queue from `hive` metrics, open pull requests from an MCP
  tool, and a health check.

- [`deploy/hive-serve/dashboard.toml.example`](../deploy/hive-serve/dashboard.toml.example):
  this box (CPU, memory, load, disks, network from node_exporter) and GPU /
  LLM workers (scrape targets, GPU busy and temperature, llama.cpp generation
  speed, an HTTP health check).
- [`deploy/hive-serve/dashboard-solar.toml.example`](../deploy/hive-serve/dashboard-solar.toml.example):
  an optional solar section (production, house, grid, the share of a load
  covered by solar, energy over 24 h) from an Enphase Envoy exporter.

## Migrating from `/v1/lab/status`

`/v1/lab/status` still works (it answers with `Deprecation: true`) for app
builds that predate the dashboard. Newer apps ask for `/v1/dashboard` and use
`/v1/lab/status` only when the host answers 404. To customise the lab
dashboard, start from the generated default:

```sh
hive dashboard default > "$HIVE_DATA_DIR/dashboard.toml"
hive dashboard check
```
