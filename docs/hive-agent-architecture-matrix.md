# Hive Agent Architecture Matrix

## Purpose

Hive needs multiple agent modes, not one monolithic "agent" concept.

Each mode should have:

- a clear entry condition
- a bounded execution model
- an explicit state shape
- a defined tool surface
- a permission posture
- a corresponding UI treatment

This matrix defines the expected product contract for Hive v1 and near-term evolution.

## Design Principle

Hive should scale execution power by task weight:

- lightweight intent stays lightweight
- actionable requests become harness-backed
- multi-step work becomes supervised
- long-running work becomes backgrounded
- collaborative decomposition becomes multi-agent

In short:

- `single-turn agent` is reasoning-first
- `single-action harness` is execution-first
- `supervised run` is workflow-first
- `background run` is persistence-first
- `multi-agent team` is coordination-first

## Matrix

| Mode | Primary Use | Execution Shape | Human Involvement | Time Horizon |
| --- | --- | --- | --- | --- |
| `single-turn agent` | explain, suggest, inspect, answer | one model turn, optional retrieval | high | seconds |
| `single-action harness` | one concrete workspace action | one reviewed action | high | seconds to under a minute |
| `supervised run` | multi-step coding task | planned sequence of actions/tasks | medium-high | minutes |
| `background run` | long-running delegated work | detached supervised execution | medium | minutes to hours |
| `multi-agent team` | parallel or specialized collaboration | coordinated subagents with handoffs | medium | minutes to hours |

## 1. Single-Turn Agent

### Purpose

Use when the user needs reasoning, explanation, triage, or planning without immediate workspace mutation.

Examples:

- "Explain this error"
- "What file should I inspect next?"
- "What is the best architecture for this feature?"

### State

Required state:

- active workspace
- active runtime
- current conversation turn
- lightweight context packet
- retrieval mode and attached context items

Should not require:

- active run
- task graph
- queued harness actions
- standing agent team

### Tools

Allowed by default:

- context inspection
- retrieval
- vault search
- file read suggestions

Not expected by default:

- file writes
- command execution
- path mutation

### Permissions

Default posture:

- safe read posture
- no write side effects
- no command side effects

If a tool action is needed mid-turn, the mode should escalate into `single-action harness` or `supervised run` rather than silently broadening permissions.

### UI

Primary surfaces:

- conversation canvas
- context inspector
- runtime visibility

UI expectations:

- no run dashboard required
- no review pane unless a proposal appears
- transcript stays clean and text-forward

### Success Condition

The turn ends with a clear assistant response, question, or recommendation.

## 2. Single-Action Harness

### Purpose

Use when the user asks for one direct device/workspace action that should not require a full agent run.

Examples:

- "Create a directory"
- "Create a file"
- "Rename this file"
- "Run tests"

### State

Required state:

- active workspace
- active runtime
- one action proposal
- one review target
- execution status for that action

Optional state:

- one minimal run wrapper for bookkeeping

Should not require:

- broader task decomposition
- multiple agent personas
- long planning history

### Tools

Expected first-class tools:

- `createDirectory`
- `createFile`
- `moveWorkspacePath`
- `readWorkspaceFile`
- `listWorkspaceFiles`
- `runWorkspaceCommand`
- `proposeFileWrite`

Tool behavior:

- one action is primary
- supporting reads may run automatically if safe
- writes and commands should remain approval-backed unless trusted

### Permissions

Default posture:

- explicit approval before mutation
- scoped to the current action

Allowed trust escalation:

- per-action
- per-task
- per-agent

This mode should be the first place where "trusted once granted" starts to matter operationally.

### UI

Primary surfaces:

- conversation canvas
- review pane
- file pane when a file/diff is involved

UI expectations:

- the action appears as a proposal, not as plain assistant text
- review shows command/path/preview details
- approval and dismissal are explicit

### Success Condition

The action is approved or dismissed, then executed or canceled with a visible result.

## 3. Supervised Run

### Purpose

Use when the work is multi-step and benefits from planning, context evolution, and visible progress.

Examples:

- "Refactor this feature and update tests"
- "Investigate this failure, fix it, and summarize the risk"
- "Implement this UI change and verify the build"

### State

Required state:

- run id
- run objective
- task list
- task dependencies
- assigned agent/runtime per task
- queued/completed/blocked actions
- approval queue
- execution trace
- participating runtimes
- context packet per run

Optional state:

- handoff summaries
- retry state
- latency and health snapshots

### Tools

Expected tools:

- all single-action harness tools
- retrieval expansion
- diff proposal generation
- bounded command execution
- vault access
- runtime-aware planning

Planner behavior:

- may sequence multiple actions
- should prefer safe reads before writes
- should break edits into reviewable chunks

### Permissions

Default posture:

- safe reads may auto-run
- writes and commands require approval unless trusted

Trust scopes should support:

- one action
- one task
- one run
- one agent
- workspace-wide standing grant

### UI

Primary surfaces:

- conversation canvas
- agent pane
- review pane
- files pane
- context inspector

UI expectations:

- transcript shows compact operational milestones
- agent pane owns task and handoff detail
- review pane owns approval queue
- file pane owns diffs and file-linked actions

### Success Condition

The run completes with:

- work executed
- visible outputs
- clear failures if blocked
- a final summary message for the human

## 4. Background Run

### Purpose

Use when the task should continue without the user staying in the foreground.

Examples:

- long test pass
- bug reproduction loop
- dependency upgrade and validation
- slow remote model investigation

### State

Required state:

- persistent run id
- detached execution status
- durable logs
- approval checkpoints
- wake/suspend metadata
- notification state
- resumable context snapshot

Should survive:

- app backgrounding
- client restarts
- mobile continuation

### Tools

Expected tools:

- all supervised-run tools
- monitoring/polling
- scheduled wakeups
- durable artifact capture

Additional runtime needs:

- persistence layer
- background task monitoring
- notification dispatch

### Permissions

Default posture:

- background execution only for tasks already granted enough trust
- destructive or expensive new actions should still pause for approval

This mode should never silently expand privilege just because it is detached.

### UI

Primary surfaces:

- activity view
- run detail view
- notifications
- mobile continuation surfaces

UI expectations:

- run can be left and resumed later
- the conversation should reference the run without requiring the user to keep the agent pane open
- push or in-app notifications should signal completion, failure, or pending approval

### Success Condition

The run continues safely in the background and can be reattached without losing operational history.

## 5. Multi-Agent Team

### Purpose

Use when parallelism or specialization materially improves the work.

Examples:

- planner + coder + reviewer
- research + implementation + validation
- parallel bug triage across subsystems

### State

Required state:

- team id
- agent roster
- role/persona per agent
- runtime per agent
- shared objective
- per-agent task queues
- handoff packets
- shared review state
- shared context plus agent-local context slices

Important distinction:

- shared workspace state
- partially isolated reasoning state

### Tools

Expected tools:

- all supervised-run tools
- inter-agent messaging
- handoff creation
- team assembly and modification
- task reassignment
- runtime-specific specialization

Tool policy:

- not every agent gets every tool
- tool loadout should be role-based and inspectable

### Permissions

Default posture:

- least privilege per agent
- approvals may be granted to a task or specific agent, not automatically the whole team

Important rule:

- trust should remain attributable
- the human must always know which agent executed what

### UI

Primary surfaces:

- conversation canvas with explicit speaker attribution
- agent pane with tasks, handoffs, and team roster
- review pane with agent-linked approvals
- context inspector with shared vs local context visibility

UI expectations:

- runtime and persona must be obvious on every meaningful event
- handoffs should be visible but compact
- operational chatter should not drown the main conversation

### Success Condition

The team behaves like a coordinated engineering unit without becoming opaque.

## Mode Escalation Rules

Hive should move between modes deliberately.

### `single-turn agent` -> `single-action harness`

Escalate when:

- the user asks for one concrete workspace action
- the action is simple and bounded

### `single-action harness` -> `supervised run`

Escalate when:

- one action is not enough
- the task needs planning, verification, or iteration

### `supervised run` -> `background run`

Escalate when:

- the task will take long enough that foreground blocking is wasteful
- the user explicitly delegates and walks away

### `supervised run` -> `multi-agent team`

Escalate when:

- specialization helps
- parallel tasking helps
- the planner recommends decomposition into multiple roles

### De-escalation

Hive should also de-escalate cleanly.

Examples:

- a background run finishes and returns to normal conversation
- a multi-agent team collapses back to a single reviewing agent
- a supervised run ends with a plain conversational summary

## Default Product Policy

Hive should not start in the heaviest mode.

Recommended default:

1. start with `single-turn agent`
2. escalate to `single-action harness` for direct actions
3. escalate to `supervised run` for multi-step work
4. escalate to `background run` when duration justifies detachment
5. escalate to `multi-agent team` when specialization or parallelism justifies coordination

This keeps latency, UI weight, and cognitive load proportional to the task.

## Implementation Mapping

### Near-term in Hive

Already underway:

- `single-turn agent`
- `single-action harness`
- `supervised run`

Partially scaffolded:

- `background run`
- `multi-agent team`

### Required runtime capabilities by mode

| Capability | Single-Turn | Single-Action | Supervised Run | Background Run | Multi-Agent Team |
| --- | --- | --- | --- | --- | --- |
| runtime routing | yes | yes | yes | yes | yes |
| context assembly | yes | yes | yes | yes | yes |
| approval queue | minimal | yes | yes | yes | yes |
| task graph | no | minimal | yes | yes | yes |
| execution trace | no | yes | yes | yes | yes |
| persistence | conversation | conversation + action | run-level | durable detached | durable shared |
| inter-agent handoff | no | no | optional | optional | yes |
| notifications | no | optional | optional | yes | yes |

### Required UI emphasis by mode

| Surface | Single-Turn | Single-Action | Supervised Run | Background Run | Multi-Agent Team |
| --- | --- | --- | --- | --- | --- |
| conversation | primary | primary | primary | summary/reattach | primary with attribution |
| review pane | optional | primary | primary | primary when blocked | primary |
| files pane | optional | task-dependent | task-dependent | task-dependent | task-dependent |
| context inspector | important | important | important | important | critical |
| agent pane | hidden | minimal | important | important | critical |
| activity view | minimal | minimal | useful | critical | critical |

## Product Rule of Thumb

If the user can still think of the request as "one thing," Hive should prefer `single-action harness`.

If the user is really asking for "get this piece of work done," Hive should prefer `supervised run`.

If the user is delegating rather than collaborating live, Hive should prefer `background run`.

If the work naturally divides into roles, Hive should prefer `multi-agent team`.
