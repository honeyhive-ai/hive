# Linear & remote MCP servers (OAuth)

Beyond stdio and plain-HTTP [MCP servers](mcp.md), Hive speaks the modern
**Streamable-HTTP** MCP transport with **OAuth 2.1 + PKCE**. That lets
agents talk to hosted MCP services that authenticate you in the browser —
[Linear](https://linear.app) being the headline example.

Everything lives in **Settings → Tools**.

## Why OAuth

Plain-HTTP MCP servers use a static bearer token from an env var. Hosted
providers like Linear instead want a real OAuth handshake: you authorize
Hive once in your browser, and Hive holds short-lived tokens that it
**refreshes automatically**. No long-lived secret sits in your config.

The flow uses a **loopback redirect** — `http://127.0.0.1:51736/callback`
— so the browser hands the authorization code back to the Hive app
running on your machine.

## Add Linear

There's a one-click **"Add Linear (issues)"** preset under Settings →
Tools. It adds a server pointed at `mcp.linear.app/sse`. You still have to
**enable** and **Connect** it (below).

### 1. Create a Linear OAuth app

Linear is a *confidential* provider, so you supply your own OAuth app's
credentials.

1. In Linear, go to **Settings → API → OAuth apps** and create one.
2. Set the **redirect URI** to exactly:

   ```text
   http://127.0.0.1:51736/callback
   ```

3. Grant scopes: **`read`**, **`write`**, **`issues:create`**.
4. Note the app's **Client ID** and **Client secret**.

### 2. Add → enable → Connect

1. **Add Linear (issues)** from Settings → Tools.
2. **Enable** the server. As with every MCP server, install/add is inert —
   enabling is what wires it into chats. See the security note below.
3. Click **Connect** on the server row. On the **first** Connect, Hive
   prompts for the OAuth app's **Client ID** (and **Client secret**, since
   Linear is confidential). It then opens your browser for the OAuth
   consent screen.
4. Approve in the browser. The code returns to the loopback redirect and
   Hive stores the tokens, refreshing them as needed.

Once it's connected **and** enabled, your agents can **read, create, and
update Linear issues** through the server's tools.

## The /linear command

The composer's `/` menu includes **/linear**, which pulls **your Linear
issues into the conversation context** — handy for "summarize my open
issues" or "draft a comment on the top one" without the agent having to
fetch them tool-call by tool-call.

## Connecting other OAuth MCP servers

The Linear preset is just a convenience. Any Streamable-HTTP MCP server
that supports OAuth 2.1 + PKCE works the same way:

1. Add the server with its `…/sse` (or HTTP) endpoint — manually or via
   [Install from the internet](mcp.md#install-from-the-internet).
2. Enable it.
3. Click **Connect** and provide the Client ID (plus secret for
   confidential providers) when prompted.

The redirect URI you register with the provider is always
`http://127.0.0.1:51736/callback`.

## Security: enabling is the gate

!!! warning "Inert until enabled"
    The same MCP security model applies: a server you add does **nothing**
    until you **enable** it, and Connect only runs the OAuth flow you
    initiate. Tokens are scoped to the OAuth app you create and can be
    revoked from the provider side at any time. Per-tool trust toggles in
    the [Tools pane](mcp.md#trust-per-tool) still gate individual calls.
