# Git integration

Hive treats git as a workspace signal, surfaced in two places: a
**Diff view** for reviewing uncommitted changes, and a compact
**status line** in Settings.

## The Diff view

Switch the canvas to **Diff** (the Chat | Diff tabs in the chat header)
to review every uncommitted change:

- **Changes list** (left, resizable) — each dirty file with `+`/`−`
  line counts and a kind badge (modified / new / deleted / conflict).
- **A real diff editor** (right) — the HEAD version against your
  working tree, **syntax-highlighted** from the file extension, with
  unchanged regions folded. Toggle **Split | Inline** to switch between
  side-by-side and unified layouts. Binary files are flagged instead of
  rendered.
- **Open in your editor** — Hive detects installed editors (VS Code,
  Cursor, Windsurf, Zed, Sublime — via their CLI shims, or the app
  bundle on macOS) and the **Open in…** button launches the selected
  file in the one you choose. Your last choice becomes the default.

This is the human review surface for agent-made changes — read the
diff here, then commit from your own terminal (or have an agent do it,
subject to its [file-access permission mode](settings.md#team)).

## Status line (Settings → Workspace)

**Settings → Workspace** shows the workspace **root path** (which
drives the Diff canvas), a one-line git status — the current
**branch** and a **changed-file count** — and an **Open in editor**
shortcut. That's the extent of the always-visible git readout: there
is no ahead/behind tracking, and no persistent status pill in the
main window.

## Roadmap

A dedicated git pane on the right rail (per-file staging, per-hunk
selection, an inline diff viewer) is a possible future addition but is
**not in the current build**. For anything beyond reviewing the diff,
use your terminal or drive git through an agent.
