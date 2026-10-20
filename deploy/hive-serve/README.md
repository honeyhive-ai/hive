# hive serve: the home host for the iOS app

`hive serve` runs on an always-on Linux box on your tailnet. It is an ordinary
Hive device in your workspace: it syncs the E2EE event log through the relay
like the desktop app, runs agents you send to it on your own model gateway, and
gives the iPhone app a small HTTP + Server-Sent Events API on the box's
Tailscale address. The phone holds only a bearer token. The workspace key and
the device keys stay on the box. Design and API contract: [`docs/ios-app.md`](../../docs/ios-app.md).

Files here:

| File | Goes to |
|---|---|
| `hive-serve.service` | `~/.config/systemd/user/hive-serve.service` |
| `hive-serve.env.example` | `~/.config/hive-serve/hive-serve.env` (mode 0600, then edit) |
| `runtimes.toml.example` | `$HIVE_DATA_DIR/runtimes.toml` |
| `mcp.toml.example` | `~/.config/hive-serve/mcp.toml` (optional) |

## 1. Build on the box

```sh
# Rust toolchain, if the box has none (installs to ~/.cargo, no root needed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
. "$HOME/.cargo/env"
sudo apt-get install -y build-essential pkg-config git   # C toolchain for bundled SQLite

git clone https://github.com/honeyhive-ai/hive.git ~/src/hive   # or pull your branch
cd ~/src/hive
cargo build --release -p hive-cli
install -Dm755 target/release/hive ~/.local/bin/hive
```

Only the `hive-cli` crate builds. The Tauri app and its web UI aren't needed on
the box.

## 2. Configure

```sh
install -Dm600 deploy/hive-serve/hive-serve.env.example ~/.config/hive-serve/hive-serve.env
$EDITOR ~/.config/hive-serve/hive-serve.env        # HIVE_DATA_DIR, HIVE_SERVE_LISTEN, ntfy, ...
tailscale ip -4                                     # the address for HIVE_SERVE_LISTEN
```

For the CLI commands below, load the same environment into your shell:

```sh
set -a; . ~/.config/hive-serve/hive-serve.env; set +a
```

## 3. Join the workspace

You run this step yourself. It needs your workspace invite, and that invite
carries the workspace's E2EE key.

1. In the desktop app, open the workspace and use **Copy invite**. It gives you
   a `hivews1:…` code. Send it to the box over a channel you trust, such as
   SSH. Don't use chat or email.
2. On the box:

   ```sh
   hive enroll 'hivews1:…'                  # add --token <hrt1…> if your relay needs an access token
   hive join <invite-code>                  # only on relays that enforce membership
   hive sync                                # pull the workspace
   hive whoami                              # this box's account/device ids
   ```

`enroll` writes the connection, including the key, to
`$HIVE_DATA_DIR/config.toml`. Keep that directory private (the unit sets
`UMask=0077`).

On first start, `hive serve` adds itself to the roster at the **Contributor**
role. That is enough to chat, vote on proposals and move agents between hosts.
It also registers itself as a host, so it appears in the desktop's host list
under `HIVE_SERVE_LABEL`. A proposal whose `approvalRoleFloor` sits above
Contributor needs an Owner/Admin to promote this member in the desktop's
People pane before the phone's vote counts. Defining workspace runtimes
(`hive add-runtime`) needs Admin.

## 4. Models, repos and tools

- **Runtimes.** Copy `runtimes.toml.example` to `$HIVE_DATA_DIR/runtimes.toml`
  and point it at your gateway. Agents you send to this host use the runtime
  listed there (the `*` entry catches all), unless a workspace runtime with the
  agent's runtime id exists.
- **Repos.** Clone the checkouts agents should work on under `HIVE_REPOS_DIR`
  and bind one:

  ```sh
  git clone <url> "$HIVE_REPOS_DIR/hive"
  hive bind-repo hive                  # the workspace's checkout
  hive bind-repo other --chat <id>     # one chat on a different checkout
  hive repos                           # show bindings
  ```

  The binding stays on this box and never syncs, the same as the desktop's
  workspace folder. A turn reads the chat's base worktree
  (`.hive/worktrees/chat-<id>`). Its first write cuts a per-turn worktree, and
  the diff comes back as a review proposal. Nothing is written to the checkout
  itself. Turns in a chat with no bound checkout get no file tools.
- **MCP.** `HIVE_MCP_CONFIG` names a TOML file of MCP servers (see
  `mcp.toml.example`). Bearer tokens come from the environment variables it
  names.
- `run_command` is off unless `HIVE_BUILTIN_RUN_COMMAND=1`.

## 5. Run

```sh
install -Dm644 deploy/hive-serve/hive-serve.service ~/.config/systemd/user/hive-serve.service
systemctl --user daemon-reload
systemctl --user enable --now hive-serve
sudo loginctl enable-linger "$USER"     # start at boot, keep running after logout
journalctl --user -u hive-serve -f
```

Health check from any tailnet device: `curl http://<tailscale-ip>:8787/v1/health`.

## 6. Pair the phone

```sh
hive pair        # prints a QR code and a hive://pair?... link for the configured address
```

Scan the QR code with the iPhone camera, or paste the link into the app. The
link contains the API token, so run `hive pair` in your own terminal. Don't run
it in the unit, because the journal would keep the token. To revoke the phone,
delete `$HIVE_DATA_DIR/serve.token` and restart. A new token is generated, and
you pair again.

## Sending agents here

From the phone, open **Agents**, pick an agent and choose this host
(`PUT /v1/agents/{id}/host`). From a terminal, run
`hive set-agent-host <agent> <host-id>`. After that, any `@agent` mention in
the workspace runs on this box. The desktop shows the agent's host as this
box's label.
