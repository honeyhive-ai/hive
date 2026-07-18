# Skills

A **skill** is a reusable instruction bundle — a `SKILL.md` with a name,
a short description, and a body of guidance the model should follow when
the task calls for it. Skills install from the internet and their
instructions are injected into agent and primary-runtime context.

Use a skill when you want to teach the workspace *how to do something*
(fill PDF forms, write release notes a certain way, follow a house code
style) without repeating the instructions in every message.

## Install from the internet

In the right-rail **Skills** pane, **Install from the internet** fetches a
`SKILL.md` and stores the skill in a per-workspace catalog (`.hive/skills.json`).
The same reference forms as MCP work:

```text
https://raw.githubusercontent.com/owner/repo/main/pdf/SKILL.md
https://github.com/owner/repo/blob/main/pdf/SKILL.md   # blob URLs become raw
owner/repo/pdf/SKILL.md                                # shorthand, ref defaults to main
https://skills.sh/some/skill                           # any https host
```

## SKILL.md format

YAML-ish frontmatter (all optional) followed by the instruction body:

```markdown
---
name: PDF Filler
description: Fill PDF forms from structured data
allowed-tools: Read, Write, Bash
---
# PDF Filler

Use `pdftk` to fill forms. Always validate the output with
`pdftk … dump_data_fields` before returning.
```

- `name` → the skill's display name (falls back to the folder name).
- Everything after the frontmatter is the **instructions** body.
- Other frontmatter keys (`description`, `allowed-tools`) are advisory and
  aren't stored on the skill — Hive keeps only the name and the instruction
  body (plus the source URL).

No frontmatter is fine — the whole file becomes the instructions and the
URL's folder name is used as the skill name.

## How skills reach the model

Skills loaded into a chat are injected in full into the participants' system
prompt. They appear under a shared heading:

```text
Loaded skills (follow these):

## PDF Filler
<the skill's instruction body>

## Some Other Skill
<its instruction body>
```

Each skill is a `## <name>` block. There is **no autoload toggle and no
per-scope setting** — every skill loaded into the chat is injected in full;
the model follows the ones relevant to the task at hand. Remove a skill from
the Skills pane if you don't want it in context.

## Where it lives

Installed skills are workspace-level: stored in `.hive/skills.json`,
seeded into every chat in the workspace, and surviving relaunch. Remove a
skill from the **Skills** pane to drop it from the catalog.

!!! note "Public skills only, for now"
    Skill fetches are unauthenticated, so private-repo `SKILL.md` files
    aren't supported yet. Host the skill at a public raw URL or via
    skills.sh.
