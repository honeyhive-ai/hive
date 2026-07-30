# Screenshots & diagrams

The docs reference these image files. This directory holds the
*contract* for what each page expects; see
[`CAPTURE_GUIDE.md`](CAPTURE_GUIDE.md) for step-by-step recipes
(exact UI state, what must be visible, crop). Capture the shots,
drop them into this folder, and the docs render.

## Required screenshots (embedded by docs pages)

| File | What to capture |
|------|------------------|
| `onboarding-welcome.png` | Onboarding step 1 (identity), as first shown |
| `onboarding-identity.png` | Onboarding step 1 with the display name / handle set |
| `onboarding-runtime.png` | Onboarding step 3 (connect a model) |
| `settings-general.png` | Settings, **Account** tab |
| `settings-runtimes.png` | Settings, **Models & runtimes** tab, one provider + runtime |
| `settings-team-members.png` | Settings → **Team sync**, Team members panel (relay admin) |
| `agents-pane.png` | Right rail **Tools** pane, Workspace Agents section |
| `right-rail.png` | Right rail with the **Tools** pane active (icon strip visible) |
| `invite-pane.png` | Right rail **People** pane, invite link + short code generated |
| `first-chat-empty.png` | Empty new chat, composer focused |
| `collaborators.png` | **Friends** view (rail ☺): add-by-username, request, presence |

## Optional / gallery + new 1.0 surfaces

| File | What to capture |
|------|------------------|
| `overview.png` | Full window: rail + sidebar + transcript + right rail (gallery hero) |
| `onboarding-team.png` | Onboarding step 4 (Team & sync: local / join / relay) |
| `workspace-bar-git.png` | Git status surface (header / Folder & Git) — verify placement |
| `workflows-pane.png` / `workflow-builder.png` | Workflows pane + DAG builder |
| `multi-agent-thread.png` | Two agents taking consecutive turns in one chat |
| `schedules.png` | Settings → Schedules, one schedule configured |

## Required diagrams

| File | What it shows |
|------|----------------|
| `rendezvous-flow.png` | Sequence diagram: Alice publishes → Bob lookup → P2P UDP |

Diagrams can be drawn in Excalidraw, Mermaid (rendered to PNG),
draw.io — anything that exports a static image.

## Conventions

- **Resolution**: capture at 2× retina (Cmd+Shift+4, drag) and let
  the docs render scale it down via `{ width="…" }` attributes.
- **Theme**: use the **Midnight** palette (dark) so screenshots are consistent.
- **Format**: PNG. Compress with `pngquant` before committing.
- **Crop**: tight to the relevant pane; leave 8–12 pixels of dark
  panel as breathing room.

## Where each is referenced

```bash
grep -rn '!\[' ../*.md ../*/*.md | grep images/
```

This lists every image reference across the docs.
