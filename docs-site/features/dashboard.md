# Host dashboard

A `hive serve` host can serve a **dashboard** for the phone app: numbers,
rows, tables and charts about your workspace and anything else the host can
reach. The phone shows it in its dashboard tab. The host builds every panel
itself and answers `GET /v1/dashboard` (bearer auth, like every other `/v1`
route); the phone never talks to Prometheus, a JSON URL or an MCP server
directly. It only receives the results.

Every host starts with a built-in **Hive** section about the workspace it
serves, so the tab is useful with no setup at all. Anything more is
described in a `dashboard.toml` file on the host.

See [Hive on your phone](phone-app.md) for the app, and
[Running hive serve as your own device](../ops/serve-box.md) for setting up `hive serve`.

## The built-in Hive section

Shown first on every dashboard unless `dashboard.toml` says otherwise:

| Panel | What it shows |
|---|---|
| **Agents online**, **Agents busy** | Agents whose host is online, and agents answering a turn right now |
| **Awaiting your review** (amber from 1), **Open proposals** | Proposals you may vote on and haven't (your own excluded), and all open proposals |
| **Turns today**, **Turns, 7 days** | Agent replies since local midnight, and over the last 7 days |
| **Workflows running**, **Awaiting approval** (amber from 1), **Failed today** (red from 1) | Shown only once the workspace has a workflow |
| **Phones paired** | Shown only when remote access is on (`hive remote`) |
| **This host** | Hive serve, Relay sync (for example "Synced 4 s ago"), Remote access (when on), Push |

"Your review" means the identity the phone acts as: the host itself, or
its owner once the host is enrolled as one of the owner's devices.

To move or hide the section, set `builtin` at the top of `dashboard.toml`,
before any `[mcp]` table or `[[section]]`:

```toml
builtin = false      # no Hive section
builtin = "bottom"   # below your own sections
# builtin = true or "top": above them (the default)
```

## Where the file lives

The host reads `dashboard.toml` from its data directory (`HIVE_DATA_DIR`,
default `~/.hive`), next to `config.toml`. Set `HIVE_DASHBOARD_FILE` to use
another path. The file is read again on every request, so edits apply
without restarting `hive serve`.

With no `dashboard.toml`, the dashboard is the built-in Hive section. If
`HIVE_PROMETHEUS_URL` is set, the host also adds a generated home-lab
dashboard below it (power, workers, temperatures and so on; sections with no
data are left out). `hive dashboard default` prints that generated
dashboard as TOML, a good starting point to edit.

## Commands

```sh
hive dashboard check                   # validate the file, evaluate every panel once, print the results
hive dashboard check --file other.toml # check another file
hive dashboard metrics                 # list the hive metrics panels can show
hive dashboard default                 # print the generated default dashboard as TOML
```

Run `hive dashboard check` with the server's environment (for example its
env file), so it uses the same `HIVE_DATA_DIR`, `HIVE_PROMETHEUS_URL` and
`HIVE_MCP_CONFIG`. It exits non-zero when the file is invalid or any panel
has an error. `check` reads the workspace from the host's store but isn't
the running server, so the live rows of the Hive section (relay sync, push)
read as unknown. It also **calls every MCP tool the file uses, once**.

## An example

```toml
title = "Team"        # tab title in the app (default "Dashboard")
refresh = "30s"       # how long a panel's result is reused (default for every panel)
builtin = true        # the built-in Hive section on top

[mcp]
allow = ["github/list_pull_requests"]   # every MCP tool a panel may call, by exact name

[[section]]
title = "Review"

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
title = "Replies per agent, 7 days"
kind = "bar"
metric = "turns.by_agent_7d"

[[section]]
title = "Code"
optional = true       # hide the section while none of its panels has data

[[section.panel]]
title = "Open pull requests"
kind = "table"
source = "mcp"
server = "github"     # a server id in HIVE_MCP_CONFIG
tool = "list_pull_requests"
args = { owner = "acme", repo = "web", state = "open" }
columns = [
  { label = "#", key = "number" },
  { label = "Title", key = "title" },
  { label = "Author", key = "/user/login" },
]
refresh = "5m"

[[section]]
title = "Services"
optional = true

[[section.panel]]
title = "API"
kind = "status"
url = "http://192.0.2.10:8080/healthz"
path = "/status"
```

More examples ship in the repository under
[`deploy/hive-serve/`](https://github.com/honeyhive-ai/hive/tree/main/deploy/hive-serve):
`dashboard-team.toml.example` (no home lab needed),
`dashboard.toml.example` (a server and GPU workers from Prometheus) and
`dashboard-solar.toml.example`.

## Format

Top-level keys: `title`, `refresh` (default panel refresh), `timeout`
(default panel timeout, at most `30s`), `builtin`, and the `[mcp]` table.
Each `[[section]]` has a `title`, optional `optional = true` (hide it when
none of its panels has data) and optional `show_when` (a PromQL query; the
section is hidden unless it returns data). Each `[[section.panel]]` has a
`title`, a `kind`, a source, and optional `id`, `unit`, `round` (decimal
places), `scale` (multiply every value), `min`, `max`, `thresholds`,
`refresh`, `timeout` and `limit`.

Durations are written `500ms`, `15s`, `5m`, `24h`, `7d`, or a number of
seconds. **Unknown keys are errors**, so a typo doesn't fail silently.

`thresholds` are ascending; a value gets the level (`ok`, `warn` or
`critical`) of the highest threshold at or below it, and the app colours it
by level. A `unit` starting with `$` reads as money (`unit = "$/h"` shows
`$0.046/h`).

### Sources

| `source` | Keys | Reads |
|---|---|---|
| `prometheus` (the default) | `query` | An instant query against `HIVE_PROMETHEUS_URL` (time series and sparklines use a range query) |
| `http_json` (default when `url` is set) | `url`, `path` | `GET url` (http or https, from the host's network, at most 1 MiB) and the value at `path`, a JSON pointer such as `/data/0/temp`. No headers or credentials are sent |
| `hive` (default when `metric` is set) | `metric` | A metric of the workspace this host serves (see [below](#hive-metrics)) |
| `mcp` (default when `tool` is set) | `server`, `tool`, `args`, `path` | A tool on one of the host's MCP servers, read like a JSON document (see [MCP tool panels](#mcp-tool-panels)) |

### Panel kinds

| `kind` | Shows | Notable keys |
|---|---|---|
| `stat` | One number | `sparkline = true` (the query over the last hour) or `sparkline = { range = "6h", step = "5m" }` |
| `gauge` | One number between `min` and `max` (default 0 to 100) | `min`, `max`, `thresholds` |
| `timeseries` | One to three series over time | `series = [{ name, query }]` (or `path` for JSON), `range` (default `24h`), `step` (at most 1000 points) |
| `bar`, `list` | Label/value rows | Every sample of a Prometheus query, a JSON array or object at `path`, or `rows = [{ label, query }]` / `rows = [{ label, path }]` |
| `status` | Up/down rows | Rows as for `list`; a number is up at or above `up_when` (default 1), a string is up when it equals `up_text` (default ok, up, true, healthy, online, running, pass) |
| `text` | One line of text | A string at `path`, a number, or a fixed `text = "…"` |
| `table` | Columns and rows of cells (`http_json`, `mcp` and `hive` sources) | `path` to an array; `columns = [{ label, key, align }]` |

**Table panels.** `key` is a key of each item or a JSON pointer into it
(`/user/login`); it defaults to the label. `align` is `leading`, `center` or
`trailing` (a column of numbers defaults to trailing). Without `columns`,
the table uses the keys of the first item (at most 8). `limit` sets the
number of rows: default 20, at most 200. `limit` also caps the rows of
`bar`, `list` and `status` panels.

### Hive metrics

`source = "hive"` with `metric = "…"` reads the workspace this host serves,
from its synced event log. `hive dashboard metrics` prints this list.

| Metric | Shape | What |
|---|---|---|
| `agents.total` | number | Agents in the workspace |
| `agents.online` | number | Agents whose host is online |
| `agents.busy` | number | Agents answering a turn right now |
| `agents` | rows | Each agent: up while its host is online, with Busy, Idle or Offline |
| `agents.by_host` | rows | Number of agents per host |
| `hosts` | rows | Each registered host: up while online |
| `proposals.open` | number | Open proposals (not dismissed or superseded) |
| `proposals.awaiting_me` | number | Open proposals you may vote on and haven't, your own excluded |
| `proposals.list` | table | Open proposals, newest first: proposal, chat, approvals |
| `workflows.running` | number | Workflow runs in progress |
| `workflows.awaiting_approval` | number | Runs waiting at an approval gate |
| `workflows.failed_today` | number | Runs that failed since local midnight |
| `workflows.completed_today` | number | Runs that completed since local midnight |
| `workflows` | rows | The four counts above, as rows |
| `workflows.list` | table | The latest runs: workflow, status, started |
| `turns.today` | number | Agent replies since local midnight |
| `turns.7d` | number | Agent replies in the last 7 days |
| `turns.by_agent_7d` | rows | Agent replies in the last 7 days, per agent |
| `host.status` | rows | This host: Hive serve, Relay sync, Remote access (when on), Push |
| `sync.age` | number | Seconds since the last successful relay sync |
| `sync.last` | text | The last relay sync, for example "Synced 4 s ago" |
| `phones.paired` | number | Phones paired for remote access |

A number fits `stat`, `gauge` and `text`, or one row of a list
(`rows = [{ label = "Open", metric = "proposals.open" }]`). Rows fit `bar`,
`list` and `status`; tables fit `table`. "Today" starts at local midnight in
the host's time zone, read when `hive serve` starts. Hive doesn't record
token usage, so there are no token or cost metrics.

## MCP tool panels

`source = "mcp"` calls a tool on one of the MCP servers in the host's
`HIVE_MCP_CONFIG`, the same file that gives agents on this host their MCP
tools. Bearer tokens stay in the environment variables that file names.

How the tool's result is read:

- A result marked `isError` becomes the panel's error, with the tool's
  message.
- If the tool returns `structuredContent`, that is the document.
- Otherwise the text of the result is used: one text item that parses as
  JSON is that JSON, several that all parse are an array of them, and
  anything else is the plain text (so a tool that answers `42` works for a
  `stat`). Images and audio are ignored. At most 1 MiB.
- From there, `path`, `rows`, `series` and `columns` work as for
  `http_json`. A `text` panel with no `path` shows the tool's text as is.

!!! warning "MCP panels call tools on a timer, with the host's credentials"
    MCP has no way to mark a tool read-only, so the host can't tell a lookup
    from an action. A panel calls its tool every time its result expires,
    with the host's MCP credentials, for as long as someone has the
    dashboard open, and nobody approves each call. The safeguards:

    - **Every tool must be allow-listed** by exact name in `[mcp] allow`, as
      `"<server>/<tool>"`. Wildcards such as `github/*` are rejected, and a
      panel whose tool isn't listed is invalid. **Only allow tools that read.**
    - A tool is called at most once per `refresh` per panel. Results (errors
      too) are cached for the refresh, and concurrent requests share one
      call. MCP panels default to `refresh = "60s"` and can't go below `10s`.
    - Calls are made only while the app asks for the dashboard. There is no
      background polling when nobody is looking.
    - Each call has its own timeout (default `10s` for MCP panels, at most
      `30s`). A stdio server that doesn't answer in time is stopped.
    - `args` are sent exactly as written; there is no templating.
    - `hive dashboard check` calls each tool once.

    A stdio server is started for each call, so give stdio-backed panels a
    long `refresh`.

## Errors

Each panel is evaluated on its own, with its own timeout. A failure (an
unreachable Prometheus, an empty result, a bad JSON pointer, an HTTP error,
an invalid panel) becomes that panel's error and the rest of the dashboard
is unaffected. A failed panel is retried after at most 5 seconds (an MCP
panel after its `refresh`). A file that isn't valid TOML shows one error at
the top of the dashboard, above the built-in Hive section.

The full specification, including the JSON that `GET /v1/dashboard`
returns, is in
[`docs/dashboard.md`](https://github.com/honeyhive-ai/hive/blob/main/docs/dashboard.md).
