# Screenshot Capture Guide

A handoff doc for whoever (human or agent) captures the PNGs the docs site
references. It reflects the **current 1.0 Rust + Tauri Hive UI** (rail + sidebar
+ right-rail panes; tabbed Settings; **5-step** onboarding). Recipes run
top-to-bottom in one ~15-minute session — later states build on earlier ones.

Each recipe gives: **File path**, **Used by** (docs page), **State** (UI to set
up), **Capture** (shortcut + crop), and **Must be visible** (load-bearing
elements).

> **Filenames are load-bearing.** The docs link these exact names; keep them.
> A ⚠️ marks something that changed since the last capture pass and should be
> **verified against the live build** before you shoot — the rewrite moved a few
> surfaces. The bottom section lists *new 1.0 surfaces* (Workflows, multi-agent,
> CLI, scheduling) whose docs pages don't embed a screenshot yet; capture them
> so we can wire them in.

---

## Prerequisites

1. **Build + install the app:**
   ```bash
   ./scripts/build.sh mac           # produces target/release/bundle/dmg/Hive_*.dmg
   ```
   Install the `.dmg`, or run a dev build with `cargo tauri dev` (the chrome is
   identical; a packaged build avoids the dev toolbar).
2. **A working model** so chats/consent shots can stream. Easiest is a local
   Ollama (`ollama serve` + a pulled model) so captures don't burn API quota;
   an `ANTHROPIC_API_KEY` also works.
3. **Start from a clean install** so onboarding runs and the roster is empty.
   Quit Hive, then move its data aside (reversible):
   ```bash
   mv ~/Library/Application\ Support/com.hive.desktop \
      ~/Library/Application\ Support/com.hive.desktop.bak-$(date +%s)
   ```
4. **Theme:** in onboarding (step 5) or **Settings → Appearance** later, pick the
   **Midnight** palette in **dark** mode so every shot is consistent.
5. **Window size:** ~1440 × 900 logical. Drag to size, or try
   `osascript -e 'tell app "Hive" to set bounds of window 1 to {0,0,1440,900}'`.
6. **Capture:** `Cmd+Shift+4` then `Space` → click a window for a tight crop, or
   `Cmd+Shift+4` + drag for a region. PNGs land on the Desktop.
7. **Output dir:** `docs-site/images/`. Rename + move each capture after taking it.

### Conventions

- **PNG, 2× retina** (default). Docs render with `{ width="…" }` so they scale.
- **Compress before committing:** `pngquant --quality=70-90`.
- **Crop tight** with ~8–12 px breathing room.
- **No personal data.** Use display name **Alice** (`@alice`), a generic path
  like `/Users/alice/Projects/demo`, and test-only invite/access codes. If you
  sign in with GitHub for a shot, prefer a throwaway/test account.

### Current UI reference (verified against source, 1.0)

- **Onboarding — 5 steps** with a 5-segment progress bar:
  1. **Identity** ("Who are you?" — GitHub sign-in or display name)
  2. **Workspace** (pick/confirm a folder)
  3. **Model** (Claude Code / OpenAI / Anthropic / Ollama)
  4. **Team & sync** (Just me · Join a team · Connect to a relay)
  5. **Appearance** ("Make it yours" — palette + light/dark)
- **Right-rail panes** (icon rail, left→right): **Tools · Context · Review ·
  People · Vaults · Skills · Workflows · Activity**. *(There is no longer a
  standalone "Files" pane — see recipe #9.)*
- **Settings tabs** (top-to-bottom in the left nav): **Account · Appearance ·
  Folder & Git · Team sync · Schedules · Models & runtimes · Tools & MCP ·
  Permissions · Updates & data · Danger zone**.
- **Layout, left→right:** workspace rail → sidebar (Chats, People, Agents,
  Workflows) → main (transcript / diff) → right rail.

---

## 1. `overview.png` — gallery hero (`images/README.md`)

> ⚠️ **Optional / gallery only.** `index.md` no longer embeds this shot; it's
> referenced only by the image gallery. Capture it for the README/gallery, but
> it isn't required for the docs build.

**State.** Hive open on a workspace with a few chats and one selected.

**Recipe.**
1. Finish onboarding into the demo workspace.
2. Create 2–3 chats (e.g. "Refactor auth middleware", "Release notes",
   "Flaky test").
3. Open one; send a message and let the model reply (3–4 turns).
4. Open the right rail to the **Tools** pane (🛠) or **Context**.

**Must be visible.** Left **workspace rail**, the **sidebar** (Chats + People/
Agents/Workflows sections), the transcript (3+ turns), and the right rail.

---

## 2. `onboarding-welcome.png` — used by `getting-started/first-launch.md`

**State.** Onboarding **step 1** (identity), as first shown — before signing in
or typing a name.

**Recipe.** With a clean install (prereq 3), launch Hive. The onboarding card
appears on step 1 ("Who are you?") with the **Sign in with GitHub** button and a
display-name field. Capture the card before interacting.

**Must be visible.** The card headline, the GitHub sign-in button, the
display-name field, the **5-segment** progress bar (step 1 active), and **Next**.

---

## 3. `onboarding-identity.png` — used by `features/onboarding.md`

**State.** Onboarding step 1 with identity set — either signed in with GitHub
(shows `@handle`, email locked) or the display-name field filled with **Alice**.

**Recipe.** Continue from #2. For the no-network version, just type **Alice** in
the display-name field and capture. (If demoing GitHub: click **Sign in with
GitHub** → an "Open GitHub ↗" button + code appear; after authorizing it flips
to "Signed in as @…".)

**Must be visible.** The identity step with a name/handle populated and **Next**
enabled.

---

## 4. `onboarding-runtime.png` — used by `getting-started/first-launch.md`

**State.** Onboarding **step 3** (connect a model).

**Recipe.** From step 1 click **Next** to step 2 (workspace), pick/confirm a
folder, **Next** to step 3. Choose a provider — **Anthropic** (or **Ollama** for
the offline demo). Capture before advancing.

**Must be visible.** The provider choice (Claude Code / OpenAI / Anthropic /
Ollama), the relevant field (API key, or the Ollama localhost endpoint), and
**Next**.

---

## 4b. `onboarding-team.png` — used by `features/onboarding.md` *(new step)*

**State.** Onboarding **step 4** ("Team & sync"), the collaboration choice added
in 1.0.

**Recipe.** From step 3 click **Next**. Step 4 offers three modes: **Just me**
(local-only), **Join a team** (invite / short code), **Connect to a relay**
(relay URL + token). Select **Connect to a relay** so the URL + access-token
fields and the **Test connection** button show; capture before finishing.

**Must be visible.** The three mode options with their descriptions, the
relay-URL / access-token fields for the selected mode, and **Test connection**.
Use a demo relay URL — **no real tokens** (blur if present).

> This page (`features/onboarding.md`) currently embeds only
> `onboarding-identity.png`; add this shot when you refresh that page to cover
> the new team step.

---

## 5. `settings-general.png` — used by `features/settings.md`

**State.** Settings open on the **Account** tab (identity/profile).

**Recipe.**
1. Finish onboarding.
2. Open **Settings** from the bottom of the sidebar (or the workspace menu →
   "Workspace Settings…").
3. The left nav lists **Account · Appearance · Folder & Git · Team sync ·
   Schedules · Models & runtimes · Tools & MCP · Permissions · Updates & data ·
   Danger zone**; select **Account**.

**Must be visible.** The tab nav (Account active), the **Display name** field,
**Git email**, and the signed-in GitHub account row (or sign-in prompt).

> Want the theme picker too? Capture **Appearance** as a bonus
> (`settings-appearance.png`) — palette swatches + light/dark toggle.

---

## 6. `settings-runtimes.png` — used by `getting-started/configuring-a-runtime.md`

**State.** Settings on the **Models & runtimes** tab, showing the **LLM
providers** section and the models/runtimes list.

**Recipe.**
1. Settings → **Models & runtimes**.
2. The **LLM providers** section lists configured providers as compact rows,
   with an **"Add a provider…"** selector below; expand the provider you
   configured during onboarding so its key/base-URL fields show.
3. The **models/runtimes** list below shows the runtime created in onboarding.

**Must be visible.** The "Add a provider…" selector, at least one configured
provider row (expanded), and the runtimes list with one entry.

---

## 7. `agents-pane.png` — used by `concepts/agents.md`

**State.** Right rail's **Tools** pane (🛠), showing the Workspace Agents section
with one agent.

**Recipe.**
1. Open the right rail and select **Tools** (🛠) from the pane icons.
2. Add an agent: handle `coder`, runtime = your configured provider; save.

**Must be visible.** The Tools pane, the Workspace Agents section, and the
`@coder` row with its runtime.

---

## 9. `right-rail-files.png` — used by `features/right-rail.md`

> ⚠️ **Verify against the live UI — the "Files" pane was removed.** The current
> right-rail panes are **Tools · Context · Review · People · Vaults · Skills ·
> Workflows · Activity**; there is no standalone "Files" pane. The
> "read what the agent is reading" surface now lives under **Tools** (file
> access) / **Context**. Before shooting: decide whether to (a) retarget this
> shot to the current file/context surface and keep the name, or (b) rename it
> and update `features/right-rail.md`, which still describes a "Files" pane.

**State.** The right-rail pane that shows workspace files / what the agent reads.

**Recipe.** Open that pane, select a file (e.g. a `README.md` in the workspace),
and capture the tree + preview.

**Must be visible.** The right-rail pane icons + the file tree and a file preview.

---

## 10. `workspace-bar-git.png` — gallery / `features/git.md`

> ⚠️ **Optional + verify.** `features/git.md` does **not** currently embed this
> shot (only the gallery lists it), and the top "WorkspaceBar with git pill" was
> reworked in the rewrite. Git status now surfaces via **Settings → Folder &
> Git** and the workspace header. Confirm where it renders in the current build
> and name the shot for that surface before capturing.

**State.** The git status surface for a dirty repo.

**Recipe.**
1. Make the workspace a git repo with changes:
   ```bash
   cd <workspace> && git init && git commit --allow-empty -m initial
   touch a.txt b.txt && git add a.txt   # one staged, one untracked
   ```
2. Surface the git status (workspace header, or **Settings → Folder & Git**) and
   capture it.

**Must be visible.** The branch name and a non-zero dirty count.

---

## 11. `invite-pane.png` — used by `features/invites.md`

**State.** Right rail's **People** pane (👥) with an invite/share code generated.

**Recipe.**
1. Be in a **team workspace** (create one from the workspace rail ＋ → "create a
   team", or join one) — invites are per shared workspace.
2. Open the **People** pane (👥) and generate an invite link / short code.

**Must be visible.** The invite controls and the generated link + short code.

> Note: 1:1 collaboration by GitHub username lives in the separate **Friends**
> view (rail ☺); this shot is the workspace-invite flow.

---

## 12. `first-chat-empty.png` — used by `getting-started/first-chat.md`

**State.** A new empty chat with the composer focused.

**Recipe.** Click **＋** by **Chats** in the sidebar; the transcript shows the
empty state; click the composer.

**Must be visible.** The sidebar with the new chat selected, the empty-transcript
hint, and the composer (target chip + attach + input + Send).

---

## 13. `collaborators.png` — used by `features/collaborators.md`

**State.** The **Friends** view (left rail ☺): add-by-GitHub-username, an
incoming request, and connected friends with presence dots.

**Recipe.** Sign in with a test GitHub account, open the **Friends** view (rail
☺), and set up one incoming request + one connected friend.

**Must be visible.** The "Add by GitHub username" field, an incoming request row
(accept/decline), and a friends list with presence dots (online/away/offline).

---

## 14. `settings-team-members.png` — used by `features/settings.md`

**State.** Settings → **Team sync** tab, scrolled to the **Team members** panel,
as a relay admin so the management UI is shown (not the "needs an admin" hint).

**Recipe.**
1. Point Hive at a relay whose admin list includes your GitHub login
   (`HIVE_RELAY_ADMIN_LOGINS`), and sign in with GitHub (Settings → Account).
2. Open **Settings → Team sync**, scroll to **Team members**.
3. Add one member so the list has a row (and, optionally, capture the one-time
   token reveal callout in a second shot — but redact/blur the token).

**Must be visible.** The **Add member** row (name + GitHub login fields + Add
button) and at least one member row with its token count and the Add token /
Disable / Revoke actions. **Do not show a real token** — blur it if the
one-time reveal is visible.

---

## New 1.0 surfaces (capture so we can wire them into their pages)

These shipped in 1.0 and their docs pages exist but don't embed a screenshot
yet. Capture with the suggested names; we'll add the `![…]` refs when the pages
are refreshed.

### `workflows-pane.png` + `workflow-builder.png` — for `features/workflows.md`

The flagship 1.0 feature — the DAG workflow editor with gates.

**Recipe.**
1. Open **Workflows** — from the right-rail **Workflows** pane (flow icon) or the
   sidebar **Workflows** nav row.
2. `workflows-pane.png`: the pane listing saved workflows + recent runs (with run
   status badges). Seed one saved workflow + one completed run first.
3. Click **New workflow** (or edit an existing one) to open the builder;
   `workflow-builder.png` shows the node/step editor: per-step agent assignment,
   dependencies, and **gate** conditions.

**Must be visible.** *(pane)* the workflow list + a run with a status badge;
*(builder)* the "New/Edit workflow" header, ≥2 steps with agent assignments, and
a gate. ⚠️ Verify the exact builder layout (node canvas vs. step list) against
the live build.

### `multi-agent-thread.png` — for `features/multi-agent.md`

**Recipe.** In a chat, `@`-mention two agents so they take consecutive turns
(the autonomous cascade). Capture a transcript where two `@agent` turns follow
each other with their avatars/handles.

**Must be visible.** ≥2 distinct agent turns in one thread with responder handles.

### `headless-cli.png` — for `concepts/headless-agents.md`

**Recipe.** A terminal running the CLI daemon, e.g. `hive worker` streaming
claimed turns (or `hive --help`). This is a **terminal** shot, not the app.

**Must be visible.** The `hive` command and a couple of lines of its output.

### `schedules.png` — for `features/scheduling.md`

**Recipe.** **Settings → Schedules** with one schedule configured (cron/interval
+ target). Capture the list with a row.

**Must be visible.** The schedule row (trigger + target + enabled toggle).

### Bonus (no page yet, nice to have)

- **`workspace-rail.png`** — the left rail: workspace bubble (custom image icon
  if set), active-workspace ring, ☺ Friends button, and ＋.
- **`providers-add.png`** — Settings → **Models & runtimes** with the **"Add a
  provider…"** selector open (OpenAI, OpenRouter, Azure, Ollama, …).
- **`settings-appearance.png`** — Settings → **Appearance**: palette swatches
  (Studio/Harbor/Meadow/Midnight) + light/dark toggle.

> `rendezvous-flow.png` (in the gallery) is a **diagram**, not a UI screenshot —
> it's out of scope for this capture pass.

---

## Hand-off checklist

The docs **build** needs the shots embedded by real pages. Nine are still
missing (two — `onboarding-welcome`, `onboarding-identity` — already exist):

```bash
cd docs-site/images && ls -1 *.png

# Referenced by docs pages (11 total; ✅ = already captured):
#   agents-pane.png            collaborators.png        first-chat-empty.png
#   invite-pane.png            onboarding-identity.png ✅ onboarding-runtime.png
#   onboarding-welcome.png ✅   right-rail-files.png     settings-general.png
#   settings-runtimes.png      settings-team-members.png
#
# Gallery-only / optional (not required by the build):
#   overview.png   workspace-bar-git.png   rendezvous-flow.png (diagram)

# compress
for f in *.png; do pngquant --quality=70-90 --skip-if-larger --strip --output "$f" "$f"; done
```

Preview against the docs, then commit:

```bash
bash scripts/serve-docs.sh && open http://127.0.0.1:8000/hive/
git add docs-site/images/*.png
git commit -m "docs: refresh UI screenshots for the 1.0 app"
```

Once the nine referenced shots exist, the docs build can return to strict mode
(re-add `--strict` to `mkdocs build` in `.github/workflows/docs.yml`).

## Notes for an AI agent (e.g. Codex driving the desktop)

- Hive is a **Tauri** app — one native window titled **`Hive`** wrapping a web
  view. Settings, Friends, chats, and the Workflow builder are **in-window
  views** (not separate OS windows/sheets), so navigate within the single
  window; don't hunt for a "Settings" child window.
- Onboarding is **5 steps** with a 5-segment progress bar; it only appears on a
  clean install (prereq 3).
- Left-to-right layout: **workspace rail** → **sidebar** (Chats, People, Agents,
  Workflows) → **main** (transcript / diff) → **right rail** (Tools · Context ·
  Review · People · Vaults · Skills · Workflows · Activity).
- GitHub sign-in uses a device flow: clicking sign-in shows a code + an
  "Open GitHub ↗" button that opens the browser; it resolves once you authorize.
- Pause ~500 ms after each navigation before capturing (UI transitions settle).
- `screencapture -l <window-id>` works given screen-recording permission; get the
  id via `osascript -e 'tell app "System Events" to get id of window 1 of process "Hive"'`.
  Otherwise fall back to `Cmd+Shift+4 + Space + click`.
