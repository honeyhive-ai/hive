# `hive.config.toml` reference

Hive reads one TOML file per workspace, at the workspace root, via
`hive-core::config`. The Settings → **Models** and **Tools** sections write
back to it through the encoder, so hand-edited and GUI-edited files
round-trip cleanly.

> **Note:** the encoder doesn't preserve comments. Hand-add commentary in a
> sibling `README.md` if you need it documented next to the config.

> **Connection settings live elsewhere.** The relay URL, room, workspace key,
> API key, and Claude permission mode are edited at runtime in
> Settings → Team and persist to a `settings.json` in the app data
> dir (not this file); the `[transport]` keys below only *seed* them on first
> launch. The theme is stored per-device. See [Settings](../features/settings.md).

> **Managed in-app, not in this file.** Vaults, skills, and the
> install-from-the-internet MCP catalog are managed through the right-rail
> **Vaults**, **Skills**, and **Tools** panes and stored in the workspace
> event log / `.hive/*.json` catalogs — there are no `[[vaults]]` or
> `[[skills]]` blocks in `hive.config.toml`. Runtime pools and retrieval
> tuning are likewise not read from this file today.

## Full example

Every key below is one the loader (`RawConfig`) actually parses. Anything not
listed here is silently ignored.

```toml
[app]
name = "Hive"
local_mode = false
sync_mode = "workspace"          # local | workspace | account
default_runtime = "anthropic"
default_model = "claude-sonnet-4-5"

[transport]
kind = "relay"                   # local | relay | lan

[transport.relay]
endpoint = "wss://relay.hive.example/v1"
account_token_env = "HIVE_RELAY_TOKEN"

[sync]
enabled = true
server = ""
device_name = "alice-macbook"
end_to_end_encryption = true

[permissions]
default_policy = "always_ask"    # one_action | chat | workspace | always_ask
allow_network = true

[permissions.presets.default]
read_files = true
write_files = true
run_commands = true
access_vaults = true
access_remote_runtime = true

[[runtimes]]
id = "anthropic"
name = "Anthropic"
kind = "remote"                  # local | remote
provider = "anthropic"           # ollama | openai | anthropic | openrouter | custom | hive-daemon | aider | pi | claude-code
endpoint = "https://api.anthropic.com"
models = ["claude-sonnet-4-5", "claude-opus-4-7"]
preferred_model = "claude-sonnet-4-5"
supports_embeddings = false
supports_tools = true
performance_score = 9.5
cost_per_1m_input_tokens_usd = 3.0

[[runtimes]]
id = "aider"
name = "Aider"
kind = "local"
provider = "aider"
endpoint = "/usr/local/bin/aider"
preferred_model = "gpt-4o"
supports_tools = true

[[mcp_servers]]
id = "filesystem"
transport = "stdio"              # stdio | http
command = "/usr/local/bin/mcp-filesystem"
args = ["--workspace", "."]
enabled = false

[chat_defaults]
permission_preset = "default"
retrieval_mode = "manual"
runtime_pool = ""
show_context_panel = true
show_activity_panel = true
```

## Section reference

### `[app]`

| Key                | Type    | Notes                                          |
|--------------------|---------|------------------------------------------------|
| `name`             | string  | Display label for the workspace                 |
| `local_mode`       | bool    | When true, transport defaults to local-disk     |
| `sync_mode`        | string  | `local` / `workspace` / `account`               |
| `default_runtime`  | string  | `id` of the runtime used for new chats          |
| `default_model`    | string  | Model ID to seed new chats with                 |

### `[transport]`

`kind` selects which transport carries envelopes:

- `local` — append to the local SQLite event log only. No network.
- `relay` — also push to the configured relay (`[transport.relay]`).
- `lan` — bonjour-discovered peers. **Roadmap item** — LAN discovery isn't
  implemented in the current build, and there is no `[transport.lan]` config
  block. See [LAN discovery](../networking/lan.md).

### `[transport.relay]`

Routes the full envelope log through a relay.

| Key                 | Type   | Notes                                          |
|---------------------|--------|------------------------------------------------|
| `endpoint`          | string | Relay base URL (`wss://…/v1`); REST calls derive `https://…/v1` |
| `account_token_env` | string | Env var holding the relay access token to seed |

Only `endpoint` and `account_token_env` are read from this block; the live
relay URL, room, and workspace key are edited in Settings → Team.

### `[sync]`

| Key                     | Type   | Notes                                     |
|-------------------------|--------|-------------------------------------------|
| `enabled`               | bool   | Whether sync is on                        |
| `server`                | string | Sync server (usually left empty)          |
| `device_name`           | string | Human label for this device               |
| `end_to_end_encryption` | bool   | E2EE for synced envelopes                 |

### `[permissions]` / `[permissions.presets.NAME]`

`default_policy` is the trust scope for new actions
(`always_ask` / `one_action` / `chat` / `workspace`). `allow_network` gates
network access. Each named preset carries the boolean capability flags
`read_files`, `write_files`, `run_commands`, `access_vaults`, and
`access_remote_runtime`. The active preset for a chat comes from
`chat_defaults.permission_preset`.

### `[[runtimes]]`

One block per LLM or CLI agent. See
[Configuring a runtime](../getting-started/configuring-a-runtime.md).

| Key                             | Type     | Notes                                             |
|---------------------------------|----------|---------------------------------------------------|
| `id`                            | string   | Workspace-unique identifier (required)            |
| `name`                          | string   | Display label                                     |
| `provider`                      | string   | `ollama` / `openai` / `anthropic` / `openrouter` / `custom` / `hive-daemon` / `aider` / `pi` / `claude-code` (required) |
| `kind`                          | string   | `local` / `remote` (default `remote`)             |
| `endpoint`                      | string   | HTTP base URL, or the binary path for CLI agents  |
| `metrics_endpoint`              | string   | Optional metrics URL                              |
| `models`                        | [string] | Available model IDs                               |
| `preferred_model`               | string   | Default model (falls back to first of `models`)   |
| `model_provider_id`             | string   | Optional sub-provider id (e.g. for `pi`)          |
| `model_base_url`                | string   | Optional OpenAI-compatible base URL               |
| `keep_alive`                    | string   | Optional keep-alive hint                          |
| `supports_embeddings`           | bool     | Whether the runtime can embed                     |
| `supports_tools`                | bool     | Whether the runtime accepts tool calls            |
| `context_window`                | int      | Optional context-window size                      |
| `performance_score`             | float    | Heuristic quality score                           |
| `cost_per_1m_input_tokens_usd`  | float    | Cost hint                                         |

> There is **no `api_key_env` key**. API keys are supplied through Settings
> (persisted in `settings.json`) or a provider environment variable, not this
> file.

### `[[mcp_servers]]`

Model Context Protocol servers. See [MCP servers](../addons/mcp.md).

| Key         | Type     | Notes                                            |
|-------------|----------|--------------------------------------------------|
| `id`        | string   | Workspace-unique identifier (required)           |
| `transport` | string   | `stdio` (default) or `http`                      |
| `command`   | string   | For stdio — the binary Hive spawns               |
| `args`      | [string] | Arguments passed to `command`                    |
| `url`       | string   | For http — the endpoint Hive talks to            |
| `enabled`   | bool     | Inert until true; **default `false`**            |

### `[chat_defaults]`

Defaults applied to new chats:

| Key                   | Type   | Notes                                    |
|-----------------------|--------|------------------------------------------|
| `permission_preset`   | string | Which `[permissions.presets.X]` to apply |
| `retrieval_mode`      | string | `none` / `manual` / `always`             |
| `runtime_pool`        | string | Optional pool id (pools aren't defined in this file) |
| `show_context_panel`  | bool   | Open the Context pane on new chats       |
| `show_activity_panel` | bool   | Open the Activity pane on new chats      |

## What the loader silently ignores

- Unknown keys in known sections (forward-compatibility for schema additions).
- Unknown top-level tables (e.g. `[[vaults]]`, `[[skills]]`, `[[runtime_pools]]`,
  `[retrieval]`) — these are managed in-app, not through this file.

The loader and encoder live in `hive-core::config`.
