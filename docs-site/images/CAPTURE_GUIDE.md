# Screenshot Capture Guide

A handoff doc for whoever (human or agent) captures the PNGs the docs site
references. It reflects the **current** Rust + Tauri Hive UI (rail + sidebar +
right-rail panes; tabbed Settings; 4-step onboarding). Recipes run top-to-bottom
in one ~15-minute session — later states build on earlier ones.

Each recipe gives: **File path**, **Used by** (docs page), **State** (UI to set
up), **Capture** (shortcut + crop), and **Must be visible** (load-bearing
elements).

> **Filenames are load-bearing.** The docs link these exact names; keep them.
> The bottom section lists *new* surfaces worth capturing for docs pages we
> haven't written yet (optional, no fixed names).

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
4. **Theme:** in onboarding (or **Settings → Appearance** later) pick the
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

---

## 1. `overview.png` — used by `index.md`

**State.** Hive open on a workspace with a few chats and one selected.

**Recipe.**
1. Finish onboarding into the demo workspace.
2. Create 2–3 chats (e.g. "Refactor auth middleware", "Release notes",
   "Flaky test").
3. Open one; send a message and let the model reply (3–4 turns).
4. Open the right rail to the **Tools** pane (🛠) or **Files**.

**Must be visible.** Left **workspace rail**, the **sidebar** (Chats + People/
Agents sections), the transcript (3+ turns), and the right rail.

---

## 2. `onboarding-welcome.png` — used by `getting-started/first-launch.md`

**State.** Onboarding **step 1** (identity), as first shown — before signing in
or typing a name.

**Recipe.** With a clean install (prereq 3), launch Hive. The onboarding card
appears on step 1 ("Who are you?") with the **Sign in with GitHub** button and a
display-name field. Capture the card before interacting.

**Must be visible.** The card headline, the GitHub sign-in button, the
display-name field, the **4-segment** progress bar (step 1 active), and **Next**.

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
the offline demo). Capture before finishing.

**Must be visible.** The provider choice (Claude Code / OpenAI / Anthropic /
Ollama), the relevant field (API key, or the Ollama localhost endpoint), and
**Next**.

---

## 5. `settings-general.png` — used by `features/settings.md`

**State.** Settings open on the **Account** tab (identity/profile). *(The old
"General" tab is now split: identity → Account, theme → Appearance.)*

**Recipe.**
1. Finish onboarding.
2. Open **Settings** from the bottom of the sidebar (or the workspace menu →
   "Workspace Settings…").
3. The tab strip reads **Account · Models · Tools · Team · Workspace ·
   Appearance**; select **Account**.

**Must be visible.** The tab strip (Account active), the **Display name** field,
**Git email**, and the signed-in GitHub account row (or sign-in prompt).

> Want the theme picker too? Capture **Appearance** as a bonus
> (`settings-appearance.png`) — palette swatches + light/dark toggle.

---

## 6. `settings-runtimes.png` — used by `getting-started/configuring-a-runtime.md`

**State.** Settings on the **Models** tab, showing the **LLM providers** section
(condensed) and the models/runtimes list.

**Recipe.**
1. Settings → **Models**.
2. The **LLM providers** section lists only configured providers as compact
   rows, with an **"Add a provider…"** selector below; expand the provider you
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

**State.** Right rail's **Files** pane with a file selected/previewed.

**Recipe.** Open the **Files** pane in the right rail, pick a file (e.g. a
`README.md` in the workspace), and capture.

**Must be visible.** The right-rail pane icons + the Files pane with the tree and
a file preview.

---

## 10. `workspace-bar-git.png` — used by `features/git.md`

**State.** The git status surface for a dirty repo.

**Recipe.**
1. Make the workspace a git repo with changes:
   ```bash
   cd <workspace> && git init && git commit --allow-empty -m initial
   touch a.txt b.txt && git add a.txt   # one staged, one untracked
   ```
2. In Hive, surface the git status (workspace header / status pill — **verify
   where it renders in the current build**) and capture it.

**Must be visible.** The branch name and a non-zero dirty count.

> ⚠️ **Verify against the live UI.** The old top "WorkspaceBar with git pill" was
> reworked in the rewrite; confirm where git status now shows (header pill vs the
> Files/Git pane) and name the shot for that surface.

---

## 11. `invite-pane.png` — used by `features/invites.md`

**State.** Right rail's **People** pane (👥) with an invite/share code generated.

**Recipe.**
1. Be in a **team workspace** (create one from the workspace rail ＋ → "create a
   team", or join one) — invites are per shared workspace.
2. Open the **People** pane (👥) and generate an invite link / short code.

**Must be visible.** The invite controls and the generated link + short code.

> Note: 1:1 collaboration by GitHub username lives in the separate **Friends**
> view (see New surfaces below); this shot is the workspace-invite flow.

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

**State.** Settings → **Team** tab, scrolled to the **Team members** panel, as
a relay admin so the management UI is shown (not the "needs an admin" hint).

**Recipe.**
1. Point Hive at a relay whose admin list includes your GitHub login
   (`HIVE_RELAY_ADMIN_LOGINS`), and sign in with GitHub (Settings → Account).
2. Open **Settings → Team**, scroll to **Team members**.
3. Add one member so the list has a row (and, optionally, capture the one-time
   token reveal callout in a second shot — but redact/blur the token).

**Must be visible.** The **Add member** row (name + GitHub login fields + Add
button) and at least one member row with its token count and the Add token /
Disable / Revoke actions. **Do not show a real token** — blur it if the
one-time reveal is visible.

---

## New surfaces (capture when their docs pages are written)

These shipped after the original guide; no docs reference them yet, so names are
suggestions. Capture them now so the pages can use them.

- **`workspace-rail.png`** — the left rail: the workspace bubble (with a custom
  image icon if set), the active-workspace ring, the ☺ Friends button, and ＋.
- **`providers-add.png`** — Settings → **Models** with the **"Add a provider…"**
  selector open, showing the catalog (OpenAI, OpenRouter, Azure, Ollama, …).
- **`settings-appearance.png`** — Settings → **Appearance**: palette swatches
  (Studio/Harbor/Meadow/Midnight) + light/dark toggle.

---

## Hand-off checklist

After capturing, from `docs-site/images/`:

```bash
ls -1 *.png   # the 13 referenced names below must all exist
# agents-pane.png  collaborators.png  first-chat-empty.png
# invite-pane.png  onboarding-identity.png  onboarding-runtime.png
# onboarding-welcome.png  overview.png  right-rail-files.png  settings-general.png
# settings-runtimes.png  workspace-bar-git.png

# compress
for f in *.png; do pngquant --quality=70-90 --skip-if-larger --strip --output "$f" "$f"; done
```

Preview against the docs, then commit:

```bash
bash scripts/serve-docs.sh && open http://127.0.0.1:8000/hive/
git add docs-site/images/*.png
git commit -m "docs: refresh UI screenshots for the current app"
```

Once all 13 exist, the docs build can be returned to strict mode (re-add
`--strict` to `mkdocs build` in `.github/workflows/docs.yml`).

## Notes for an AI agent (e.g. Codex driving the desktop)

- Hive is a **Tauri** app — one native window titled **`Hive`** wrapping a web
  view. Settings, Friends, and chats are **in-window views** (not separate OS
  windows/sheets), so navigate within the single window; don't hunt for a
  "Settings" child window.
- Onboarding is **4 steps** with a 4-segment progress bar; it only appears on a
  clean install (prereq 3).
- Left-to-right layout: **workspace rail** → **sidebar** (Chats, People, Agents)
  → **main** (transcript / diff) → **right rail** (Tools/People/Vaults/Skills).
- GitHub sign-in uses a device flow: clicking sign-in shows a code + an
  "Open GitHub ↗" button that opens the browser; it resolves once you authorize.
- Pause ~500 ms after each navigation before capturing (UI transitions settle).
- `screencapture -l <window-id>` works given screen-recording permission; get the
  id via `osascript -e 'tell app "System Events" to get id of window 1 of process "Hive"'`.
  Otherwise fall back to `Cmd+Shift+4 + Space + click`.
