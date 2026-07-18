# Screenshots & diagrams

The docs reference these image files. They're not committed yet —
this directory holds the *contract* for what each page expects.
Run `dist/Hive.app`, capture the screenshots listed below, drop
them into this folder, and the docs render.

## Required screenshots

| File | What to capture |
|------|------------------|
| `overview.png` | Full Hive window with sidebar + transcript + right rail, sample data |
| `onboarding-welcome.png` | Onboarding step 1 (welcome) |
| `onboarding-identity.png` | Onboarding step 2 (display name + handle) |
| `onboarding-runtime.png` | Onboarding step 4 (runtime picker) |
| `settings-general.png` | Settings sheet, General tab focused |
| `settings-runtimes.png` | Settings sheet, Runtimes tab with one configured |
| `agents-pane.png` | Tools pane (right rail) with Workspace Agents section |
| `right-rail-files.png` | Right rail with Files pane active, a file open |
| `workspace-bar-git.png` | Top workspace bar with git pill showing dirty count |
| `invite-pane.png` | People pane with Workspace Invites expanded, code generated |
| `first-chat-empty.png` | Empty new chat ready for input |

## Required diagrams

| File | What it shows |
|------|----------------|
| `rendezvous-flow.png` | Sequence diagram: Alice publishes → Bob lookup → P2P UDP |

Diagrams can be drawn in Excalidraw, Mermaid (rendered to PNG),
draw.io — anything that exports a static image.

## Conventions

- **Resolution**: capture at 2x retina (Cmd+Shift+4, drag) and let
  the docs render scale it down via `{ width="…" }` attributes.
- **Theme**: use the Midnight theme so screenshots are consistent.
- **Format**: PNG. Compress with `pngquant` before committing.
- **Crop**: tight to the relevant pane; leave 8-12 pixels of dark
  panel as breathing room.

## Where each is referenced

```bash
grep -rn '!\[' ../*.md ../*/*.md | grep images/
```

This lists every image reference across the docs.
