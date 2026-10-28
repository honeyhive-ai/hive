# The `hive serve` dashboard

The iPhone app's dashboard tab shows whatever the host defines. You describe
sections of panels in `dashboard.toml`; `hive serve` runs the queries and
serves the results at `GET /v1/dashboard`. The phone never talks to
Prometheus or the URLs in the file. It only gets numbers, rows and series.
Nothing in the file runs a command: a panel reads from Prometheus or from a
JSON URL, nothing else.

With no `dashboard.toml`:

- if `HIVE_PROMETHEUS_URL` is set, the host generates a default dashboard
  with what `/v1/lab/status` used to show (power and cost, gateway workers,
  temperatures, and solar when an Enphase exporter reports). Sections with no
  data are left out. `hive dashboard default` prints it as TOML for you to
  edit;
- otherwise the dashboard is empty, and the app hides the tab.

## Where the file lives

`<data dir>/dashboard.toml`: `HIVE_DATA_DIR`, the same directory as
`config.toml` and `serve.token`. Set `HIVE_DASHBOARD_FILE` to use another
path. The host re-reads it on every request, so edits apply without a
restart.

```sh
hive dashboard check                 # validate, evaluate every panel once, print the results
hive dashboard check --file x.toml   # check another file
hive dashboard default               # print the generated default as TOML
```

Run `check` with the server's environment (for example from its env file) so
it uses the same `HIVE_PROMETHEUS_URL` and data dir. It exits non-zero when
the file is invalid or any panel has an error.

## Format

```toml
title = "Home lab"   # tab title in the app (default "Dashboard")
refresh = "15s"      # default panel refresh (how long a result is reused)
timeout = "3s"       # default panel timeout (at most 30s)

[[section]]
title = "Server"
optional = true          # hide the section when none of its panels has data
show_when = "up{job=\"node\"}"   # PromQL: hide the section unless this returns data

[[section.panel]]
id = "cpu"               # optional; default "<section>-<title>" slugs
title = "CPU"
kind = "gauge"           # stat | gauge | timeseries | bar | list | status | text
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

### Panel kinds

| `kind` | Data | Extra keys |
|---|---|---|
| `stat` | One number: the first sample of `query`, or the number at `path` | `sparkline = true` (the query over the last hour) or `sparkline = { range = "6h", step = "5m", query = "…" }` |
| `gauge` | One number on `min`…`max` (default 0…100) | `min`, `max`, `thresholds` |
| `timeseries` | 1–3 series over time | `series = [{ name, query }]` (or `path` for http_json), `range` (default `24h`), `step` (default range/60; at most 1000 points). A single `query` is one series named after the panel |
| `bar` / `list` | Label/value rows (bars against `max`, or a plain list) | Prometheus: every sample of `query` is a row, labelled by the `label` label (default: all label values); or `rows = [{ label, query }]`. http_json: `path` to an array of objects (`label` key, default `name`; `value_key`, default `value`) or to an object (each key is a row); or `rows = [{ label, path }]` |
| `status` | Up/down rows | Rows as for `list`. A number is up at or above `up_when` (default 1); a string is up when it equals `up_text` (default ok, up, true, healthy, online, running, pass). When the source fails, every configured row reads down |
| `text` | A line of text | A string at `path`, a number, or a fixed `text = "…"` |

`thresholds` are ascending; a value gets the level of the highest threshold
at or below it (`ok`, `warn` or `critical`), and none below the first. The
app colours values by level.

`unit` is shown after the value. A unit starting with `$` is shown as money:
`unit = "$/h"` reads `$0.046/h`.

For http_json time series, `path` points at `[[t, v], …]` or
`[{t|time|timestamp: …, v|value: …}, …]`; times are Unix seconds, Unix
milliseconds or RFC 3339.

### Errors

Every panel is evaluated on its own with its own timeout. A failure (an
unreachable Prometheus, an empty result, a bad pointer, an HTTP error, an
invalid panel) becomes that panel's `error`; the rest of the dashboard is
unaffected. A failed panel is retried after at most 5 s. A file that isn't
valid TOML gives an empty dashboard with a top-level `error`, which the app
shows.

## Response (`GET /v1/dashboard`)

Bearer auth like every other `/v1` route.

```json
{
  "title": "Home lab",
  "source": "config",            // "config" | "default" | "none"
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
       "refreshSeconds": 15, "updatedAt": "…"}
    ]
  }]
}
```

Rows carry `value` (number) or `text` (string), plus `up` for status and
`level` when thresholds apply. A client should ignore kinds and keys it
doesn't know.

## Examples

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
