# Vaults & reference material

A **vault** is a pointer to reference material — a style guide, an API
spec, an architecture doc — that Hive injects into the model's context
for every reply in the chat. Add the source once; every teammate in the
workspace gets it automatically via the (end-to-end-encrypted) event
log, and each peer fetches the content independently.

Vaults are for *stable background knowledge*. For code in the working
folder, agents read files directly (or use `@file` in the composer);
for live systems, use an MCP server.

## Adding a vault

Open the right rail → **Vaults** pane (book icon) and add a source:

| Kind | Reference | Fetched from |
|------|-----------|--------------|
| **GitHub** | `owner/repo/path/to/file.md` (+ optional branch) | `raw.githubusercontent.com` |
| **GitLab** | `group/project` + path (+ optional branch) | `gitlab.com/…/-/raw/…` |
| **HTTPS** | any URL returning text | that URL |

**Preview** shows the first ~2 KB of what the model will see — and
re-fetches the source, so it doubles as a refresh after upstream edits.

## How the content reaches the model

- On each reply, the runtime assembles a `[Reference vaults]` section
  and appends it to the system prompt, after the roster and skills.
- Content is fetched **once per app run** and cached in memory; use
  **Preview** to re-fetch after upstream changes, or restart Hive.
- Caps keep vaults from crowding out conversation: the first
  **3 sources** are injected, **~6,000 characters each** (truncated
  with a marker beyond that). A failed fetch degrades to a labeled
  *unavailable* line — it never blocks the reply.
- The Context pane (⬡) shows how many vaults are attached to the chat.

Point vaults at *files*, not repository roots — a repo URL isn't a
text document. If you need whole-repo knowledge, expose it through an
MCP server with search tools instead.

## Sharing & authorization

Vault sources are workspace state: adding or removing one emits a
signed `vaultSourceAdded` / `vaultSourceRemoved` event that syncs to
every peer. Changing the source list requires the **contributor**
role or higher (the same content-write threshold as skills and
proposals). Each peer fetches content
themselves; nothing but the pointer crosses the wire, and the relay
only ever sees ciphertext.

Sources must be reachable by every peer — currently that means public
URLs (or URLs on a network all peers can reach). Per-peer credentials
for private sources are on the roadmap.

## Roadmap

Planned, not yet implemented:

- **Private sources** — per-peer tokens (e.g. a `HIVE_GH_TOKEN` env
  var) so private repos work without sharing credentials.
- **Local folders / Obsidian libraries** as per-peer vault kinds.
- **On-disk caching + indexing** (semantic search over large vaults)
  instead of whole-file injection.
- **More kinds** — Notion, Google Drive, S3. Extend the `VaultSource`
  enum in `hive-core` and add a fetcher arm in
  `hive-runtime/src/vault_fetcher.rs`; the Settings UI picks up new
  kinds from the enum. Or expose the source through an MCP server
  (`search_vault`-style tools) — that works today.
