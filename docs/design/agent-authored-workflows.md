# Agent-authored workflows

Status: approved direction (owner), implemented on `feat/agent-workflows`,
based on `feat/cli-runtimes` (Antigravity and Copilot runtimes) for one
v1.18.0 release.

## Summary

Workflows keep their run engine (`app/src/workflows.rs` + the pure
scheduler in `hive-core/src/workflow.rs`): DAG of agent stages and approval
gates, crash recovery, cross-device stages, gate proposals, phone approvals.
What changes is **who writes definitions**:

- The hand-built visual DAG editor (`WorkflowBuilder.tsx`, react-flow) is
  removed, along with the "New workflow", "Edit" and preset buttons.
- Agents are the only authors. An agent's workflow is filed as a
  **reviewable proposal** in the Review pane; approving it adds the
  definition. Editing is the same thing: the agent proposes a revision of an
  existing definition, and approving it replaces that definition.
- The Workflows pane becomes read-only for definitions: a readable rendering
  of each definition's stages, plus run, pause/resume, delete, and "ask an
  agent to change this". Runs, gates and history are unchanged.

Today an agent's `[[workflow: …]]` directive saves a definition directly,
with no review. After this change nothing reaches the definition set
without a human vote.

## 1. How an agent proposes a workflow

### One payload, two channels

The payload is the existing directive JSON (it is already documented to
agents, it addresses agents by roster name and it already has a strict
parser + validator, `definition_from_directive` → `hive_core::workflow::validate`):

```json
{"name": "…", "description": "…", "inputLabel": "…",
 "revises": "<existing workflow id or name — optional>",
 "stages": [
   {"id": "slug", "name": "…", "kind": "agent", "agent": "<roster name, omit for primary>",
    "prompt": "… {{input}} … {{nodes.<id>.output}} …", "after": ["<upstream ids>"]},
   {"id": "ok", "kind": "gate", "title": "…", "body": "…", "approvals": 1,
    "onReject": "halt" | {"retryFrom": "<stage id>"}, "after": ["slug"]}
 ]}
```

New field: `revises` (id, or case-insensitive name, of a definition in the
chat). It makes the proposal a revision: on approval the definition keeps its
id, so it replaces the old one in place. Runs already started keep their
frozen copy of the old definition (that is how runs work today).

Channels:

1. **`[[workflow: {…}]]` reply directive — every runtime.** Claude Code,
   Codex, aider, pi, Hermes, the HTTP/API runtimes, and the Antigravity
   (`agy`) and Copilot CLI runtimes from `feat/cli-runtimes` all produce
   reply text, and directives are parsed from reply text in
   `finalize_reply`. This is the universal path and needs nothing per
   runtime. Validation happens when the turn ends; a rejected payload posts
   a system note with the exact error into the chat, which the agent sees
   as context on its next turn and can fix.
2. **`propose_workflow` built-in tool — tool-loop runtimes (local models,
   OpenAI-wire, native Ollama).** Takes the same JSON object as its input.
   It validates **inside the turn** against the chat's roster and existing
   definitions and returns either the validation error (so the model fixes it
   and calls again) or "accepted, it will be filed for review when your reply
   ends". Accepted payloads are appended to the reply as directives, so they
   go through exactly the same filing code as channel 1. The tool is only
   offered when the latest user message asks for a workflow/pipeline/loop
   (the guidance already says agents should only author on explicit
   request), so it does not cost a tool slot on ordinary turns; when offered
   it takes one slot from the cap rather than growing it.

Not done: a Hive-served MCP server exposing `propose_workflow` to CLI
runtimes for in-turn validation. Hive is an MCP *client* today; serving MCP
to every spawned CLI agent is new surface (per-turn stdio server, auth,
lifecycle). The directive path already gives those runtimes the same
capability with one turn of latency on errors. See open question 3.

### Filing

`finalize_reply` (desktop) turns each accepted payload into an
`ActionProposal`:

- `kind = Decision` (deliberately not a new `ProposalKind` variant: older
  peers would fail to decode an unknown enum variant; an unknown *field* is
  ignored by serde),
- new optional field `workflow: Option<WorkflowDefinition>` carrying the
  validated, roster-resolved definition (`id` = the revised definition's id,
  or a fresh one),
- `title` = "Add workflow “X”" / "Revise workflow “X”",
- `body` = a plain-text rendering of the stages (what older peers and the
  phone show),
- `author_actor_id` = the agent (self-approval guard: its own vote can't
  satisfy quorum), `producing_agent_id` = the agent (so "Send to agent to
  revise" re-dispatches it), `required_approvals = 1`.

A system note says "📝 X proposed the workflow “Y” — review it in the
Review pane." Errors post "X suggested a workflow, but it couldn't be filed
(reason)".

### Approval → definition

`ChatService::vote_on_proposal` (shared by the desktop and `hive serve`, so
the phone path gets it too): when a vote moves a proposal that carries a
`workflow` to *approved*, the same call saves the definition
(`WorkflowDefinitionsUpdated`, replace-by-id) and marks the proposal
`Applied`. Definitions are inert (only a human starts a run), so there is no
separate "Implement" step to gate. `implement_proposal` handles the same
case as a fallback (quorum met on a device that didn't apply it, e.g. an
older peer), and `apply_workflow_proposal` is idempotent by definition id.

Rejecting the proposal leaves the definition set untouched. "Send to agent
to revise" works as for any proposal: the agent replies with a new
directive, which files a linked new version.

## 2. The read-only Workflows pane

Per definition:

- name, description, input label, "paused" badge;
- the stages in dependency order, rendered readably: `1. Draft — @Coder`
  with the prompt template in a collapsible block, "after: draft, review",
  gates as `◈ Approve fixes — 2 approvals, on reject: retry from fix`;
- actions: **Run** (input prompt, unchanged), **Pause / Resume** (new
  `disabled` flag on the definition; a paused workflow can't be started),
  **Delete** (confirm), **Ask an agent to change this** (a text box; posts a
  chat message that quotes the current definition as directive JSON with its
  `revises` id and the user's request, addressed to the primary runtime —
  the reply comes back as a revision proposal).

Runs section unchanged: run cards, stage chips with output/error, approve /
reject gates inline, cancel, resume.

Empty state: "No workflows yet. Ask an agent in this chat to set one up —
e.g. 'build a workflow that drafts, reviews, and waits for my approval'."

## 3. Phone

No change needed. Gate approvals (`workflow.approval` push), dashboard
`workflows.*` metrics and run details read runs and definitions, which are
unchanged. A workflow proposal appears in the phone's proposal list as a
decision with the stage rendering as its body; approving it there goes
through `vote_on_proposal` in `hive serve` and adds the definition.

## 4. Migration

None required. `WorkflowDefinitionsUpdated` / `WorkflowRunUpserted` are
unchanged on the wire; existing definitions load, render read-only, run,
pause and delete. The node `x`/`y` canvas positions stay in the hive-core
model (serde-compatible, ignored) and are dropped from the app DTO. The new
fields (`ActionProposal.workflow`, `WorkflowDefinition.disabled`) are
`serde(default)` + skipped when empty, so old events decode and old peers
ignore them.

Older-peer caveats (mixed-version workspaces):

- An older desktop shows a workflow proposal as a decision. Approving it
  there marks it approved but does not add the definition; a newer device's
  Implement (or the next vote on it) applies it. Implementing it *on the
  older peer* sends the body to an agent as an instruction, and that agent
  may re-author it with an old-style directive (which saves directly there).
- An older peer that rewrites the definition set (its own agent directive,
  or delete) re-emits definitions without `disabled`, un-pausing them.

## 5. Deleted vs kept

Deleted:

- `web/src/components/WorkflowBuilder.tsx` and its App.tsx takeover
  (`workflowDraft`, lazy import, roster query, `handleSaveWorkflow`);
- `web/src/lib/workflow.ts` + `workflow.test.ts` (client mirror of the
  validator, layout, slugify — builder-only);
- the `@xyflow/react` dependency and the `.workflow-canvas` CSS;
- Tauri commands `save_workflow`, `add_workflow_preset` and their ipc
  wrappers; `definition_from_dto`; the DTO's `x`/`y`;
- `RightRail`/`WorkflowsPane` `onEditWorkflow` plumbing; "New workflow",
  "Edit", "+ Review gate", "+ Fan-out + vote" buttons;
- the builder screenshot and the "DAG editor"/"Presets" docs sections.

Kept:

- the whole run engine, gates, crash recovery, cross-device stages;
- `list_workflows`, `remove_workflow`, `list_workflow_runs`,
  `start/cancel/resume_workflow_run`;
- `hive_core::workflow::preset_*` (test fixtures and examples; no UI);
- the `[[workflow:]]` directive (now files a proposal instead of saving).

Added: `set_workflow_enabled` command, `propose_workflow` tool,
`ActionProposal.workflow`, `WorkflowDefinition.disabled`,
`ProposalDto.workflow`, `revises`.

Trust note: the review step is enforced by Hive clients (the desktop files
agent output as a proposal), not by event-log authorization.
`WorkflowDefinitionsUpdated` keeps its Contributor floor, so a modified
client with a Contributor key could still write definitions directly, as
before this change. Definitions remain inert (a run needs a person), which
is what bounds that.

Stages target agents by roster name, so an agent on any runtime (including
Antigravity and Copilot) can run a stage; nothing in the engine is
runtime-specific.

## 6. Open questions for the owner

1. **Presets.** Removed with the editor (they were a human authoring path).
   Alternative: keep them as one-click "ask an agent to set up a review-gate
   workflow" suggestions. Cheap to add back either way.
2. **Schedule/trigger.** The engine has no schedule or event trigger; a run
   is always started by a human. Agent-authored definitions don't add one
   here. If wanted, it is an engine feature (a `trigger` on the definition +
   a scheduler that starts runs, which also changes the "only humans start
   runs" safety property) and should be its own design.
3. **In-turn validation for CLI runtimes.** Worth a Hive-served stdio MCP
   server (`propose_workflow`, maybe `propose` too) passed to Claude Code /
   Codex / etc. via their MCP config? Today they get errors one turn later.
4. **Auto-apply on approval.** Implemented as auto-apply (definitions are
   inert). If you want the explicit "Implement" click to stay mandatory for
   consistency with other proposals, it's a one-line change.
5. **Host placement.** Placement is still derived from the stage's agent
   (its owner device / worker), not stated per stage. A per-stage `host`
   field would need engine support; not added.
6. **Worker (hive-cli) replies** don't parse reply directives at all today,
   so an agent running on a headless worker can't propose a workflow from
   there. Pre-existing gap; unchanged.
