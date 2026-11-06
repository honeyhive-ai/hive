# Agentic workflows

Workflows turn a chat's agents into a **pipeline**: a set of stages wired
into a DAG, where each stage is either an **agent turn** or a **human
approval gate**. Stages with no unmet dependencies run **in parallel**;
gates pause the run until enough people approve.

Everything a workflow produces lives in the chat itself — stage prompts and
replies are ordinary messages, gates are ordinary proposals in the Review
pane — and the workflow definitions and run records sync **end-to-end
encrypted** with the workspace, so teammates watch a run progress live and
can vote its gates from their own devices.

Agents write workflows; people approve, run and pause them. There is no
hand-built editor: you describe the pipeline you want in the chat, an
agent drafts the definition, and it arrives in the **Review** pane as a
proposal. Approving it adds the workflow to the chat's **Workflows** pane.

!!! info "Changed in 1.18"
    Earlier versions had a visual DAG editor and two presets in the
    Workflows pane, and an agent's workflow was saved without review. The
    editor and presets are gone, and every agent-authored workflow (new or
    revised) now goes through a proposal first. Workflows you already have
    keep working unchanged: they stay listed, run, and can be paused,
    deleted or revised by an agent.

## Getting a workflow

Ask any agent in the chat, for example *"build me a workflow that triages
new issues, drafts fixes, and holds them for my approval"*. The agent
replies with a definition, and Hive:

1. **Checks it**: stage ids, dependencies (no cycles), template references
   that point upstream, agent names that exist in the chat's roster, gate
   rules. If something is wrong, the agent is told why. Local-model agents
   using Hive's built-in tools get the error inside the same turn (through
   the `propose_workflow` tool) and fix it before replying; other runtimes
   see the error as a note in the chat and fix it on their next turn.
2. **Files it as a proposal** in the **Review** pane: *Add workflow "…"*,
   with the stages listed in the order they run, who runs each one, its
   prompt, and how each gate decides.
3. **Adds it when approved.** The approving vote adds the definition to the
   **Workflows** pane. The authoring agent's own vote never counts. Reject
   it, or add a change request and **Send to agent to revise**, and the
   agent files a new version.

Approving a workflow doesn't run anything. A run only starts when someone
presses **Run**. Agents get planning authority, never execution authority.

Workflow proposals show up on the phone too (as a decision with the
stages as its text), and approving one there adds the workflow the same
way. See [Hive on your phone](phone-app.md).

## The Workflows pane

Open **Workflows** in the right rail. For each workflow you get:

- the name, description and a **paused** badge when it's paused;
- **show stages**: each stage in order, the agent that runs it (or the
  primary runtime), what it runs after, its prompt template, and for a
  gate its approval count and what a rejection does;
- **Run**: asks for the run input (the workflow's *input label*) and
  starts it;
- **Pause / Resume**: a paused workflow stays listed but can't be started.
  Runs already going are unaffected;
- **Delete**: removes the definition. Past runs stay in the history;
- **Ask an agent to change this**: type what should change. Hive posts a
  message to the chat with your request and the current definition, the
  agent answers with a revision, and the revision arrives in the Review
  pane as *Revise workflow "…"*. Approving it replaces the old definition
  in place (same workflow, new stages). Runs already started keep the
  version they started with.

Below the definitions, **Runs** shows every run with a chip per stage
(see [Running](#running)).

## Stages, dependencies and templates

- **Agent stages** run one turn of an agent from the chat's roster, or of
  the chat's primary runtime when no agent is named. Any runtime works:
  Claude Code, Codex, aider, pi, local models, and the Antigravity and
  Copilot subscription CLIs ([Bring-your-own agents](../addons/byo-agents.md)).
  A stage whose agent lives on another member's device or a worker runs
  there.
- **Prompt templates**: `{{input}}` is the run input;
  `{{nodes.<id>.output}}` is the full output of an upstream stage.
- **Gate stages** create a proposal when reached: a title and body, a
  required approval count (quorum), and a rejection policy: halt the run,
  or retry from an earlier stage.
- **Dependencies** (`after`) make the DAG. Stages whose dependencies are
  done run in parallel. A dependency without a template reference is still
  useful as pure ordering: gates have no output, stages that both edit the
  working tree need serializing, and a cheap check can short-circuit an
  expensive stage (a failed dependency skips everything downstream).

## Loops (iterate until good)

Workflows are DAGs, but a **gate that routes back** gives you a controlled loop.
Set a review gate's rejection policy to **route to an earlier stage** instead of
_halt_: rejecting then re-runs that stage (and everything after it) and pauses at
the gate again, so you can cycle **fix → recheck** as many times as you need, and
**approve** to move on. Because a person (or a quorum) decides each gate, the loop
can't run away on its own — that approval is the exit.

Author loops well: keep the loop body small (the stage to redo plus maybe one
after it), and have the redo stage read the reviewer's notes
(`{{nodes.review.output}}`) so each pass acts on the feedback rather than
retrying blind. Prefer one loop per workflow and keep the rest linear.

`@hive` knows this pattern: ask it to *"draft, have the reviewer critique,
and loop until I approve"* and it proposes the gate-routes-back workflow.

## The definition format

What an agent writes (in a `[[workflow: {…}]]` reply directive, or as the
input of the `propose_workflow` tool) looks like this:

```text
[[workflow: {
  "name": "Bugfix with review",
  "inputLabel": "Which bug?",
  "stages": [
    {"id": "repro",  "prompt": "Reproduce and diagnose: {{input}}"},
    {"id": "fix",    "agent": "Coder",
     "prompt": "Fix the bug given this diagnosis:\n{{nodes.repro.output}}",
     "after": ["repro"]},
    {"id": "review", "kind": "gate", "title": "Ship fix for: {{input}}",
     "body": "{{nodes.fix.output}}", "approvals": 1,
     "onReject": {"retryFrom": "fix"}, "after": ["fix"]}
  ]
}]]
```

Stages address agents by their **roster name** (omit `agent` for the
primary runtime), `after` declares dependencies, and `onReject` is
`"halt"` or `{"retryFrom": "<stage id>"}`. A revision adds
`"revises": "<workflow id or name>"` and carries the whole new definition.
You never need to write this yourself, but it's what you're approving.

## Recipes

Some shapes that work well. Describe one to an agent and it proposes the
definition:

**Cost-tiered fixing.** A free local model does the wide, cheap work; the
expensive frontier model only runs on what survives.

| Stage | Kind | Runs after | Notes |
|---|---|---|---|
| `triage` | agent (local Ollama) | — | "Rank these failures by likely root cause: {{input}}" |
| `fix` | agent (Claude) | `triage` | Works from `{{nodes.triage.output}}` |
| `ship` | gate | `fix` | 1 approval, reject retries `fix` |

**Cross-vendor adversarial review.** The reviewer is a *different* model
family than the author, so they don't share blind spots — something
single-vendor tools can't express.

| Stage | Kind | Runs after | Notes |
|---|---|---|---|
| `draft` | agent (Claude) | — | Implements `{{input}}` |
| `attack` | agent (another vendor/model) | `draft` | "Find what's wrong with: {{nodes.draft.output}}. Be adversarial." |
| `repair` | agent (Claude) | `attack` | Fixes what the attack found |
| `gate` | gate | `repair` | Human ships it |

**Spec fan-out with a team gate.** Three independent drafts, a judge, and
a quorum of two teammates before anything counts as decided — run it in a
shared workspace and both approvals can come from different devices.

| Stage | Kind | Runs after | Notes |
|---|---|---|---|
| `draft-1..3` | agents | — | Parallel: "Propose an API design (variant N): {{input}}" |
| `judge` | agent | all three | Compares, declares a winner |
| `adopt` | gate | `judge` | `approvals: 2` — real team sign-off |

**Docs pipeline.** Draft → fact-check against the vaults loaded in the
session → editorial gate; rejection reroutes to the draft stage with the
checker's notes already in the transcript.

## Running

Hit **Run**, give it an input, and watch the run card: one chip per stage,
colored by state; click a chip for its output, error and attempts. Stage turns stream into
the transcript under a `[Workflow · name → stage]` header. Awaiting gates
show approve/reject right on the card (they're also in the Review pane).
Cancel stops a run between stages; **Resume** restarts a run that was
interrupted (say, by an app restart); finished stages keep their outputs.
Gate approvals also reach your phone as a push notification when a
`hive serve` host is set up ([Hive on your phone](phone-app.md)).

## Current limits

- Only a person starts a run; there is no schedule or event trigger for
  workflows (use [Scheduled agents](scheduling.md) for timed single
  turns).
- Definitions belong to the chat they're created in.
- Parallel stages share the chat transcript, so siblings can see each
  other's prompts in context.
- In a workspace with devices on older Hive versions, an older desktop
  shows a workflow proposal as a plain decision: approving it there
  doesn't add the workflow until a device on 1.18 or later applies it
  (its **Implement** button does).
