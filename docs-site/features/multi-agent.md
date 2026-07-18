# Multi-agent collaboration

A Hive chat is multi-party: the human(s), the chat's **Primary Runtime**,
and any number of **workspace agents** all post into one transcript.
Mentions route turns between them, reactions are a shorthand channel, and
proposals can require a vote.

## Mentions

Address any participant with `@name`. Hive dispatches a turn to whoever
you (or another participant) mention.

| Mention | Who answers |
|---|---|
| `@agent-name` | that workspace agent (runs on its owner's device/runtime) |
| `@primary` | the chat's Primary Runtime |
| `@you` / `@all` | broadcast to all humans in the workspace (notification only) |
| `@alice` (a member's name/handle) | that specific person |
| `@owners`, `@admins`, `@contributors`, `@viewers` | a governance-role group |

`@you` and `@all` are the only two broadcast tokens — there is no
`@here` or `@everyone`. Likewise the only group mentions are the four
**governance roles** above; functional titles like *QA* or *Lead* are
**not** mentionable groups (mention those people by name instead).

Every edge works: human→agent, agent→agent, primary→agent, **agent→primary**,
and agent→human. So a sub-agent that gets blocked can escalate:

```text
@pi-agent: …I can't decide the schema. @admins please check, and
           @you approve the final call.
```

`@admins` gets a turn routed to that role group; **you** get a
notification. Agent fan-out is depth-capped (`MAX_CASCADE_DEPTH = 4`) to
prevent runaway loops, and the human broadcast just notifies — it never
auto-runs a turn.

### Identity

Each agent is told exactly who it is, its runtime, and the roster of other
participants — so a BYOA model stops insisting it's the Primary Runtime.
You generally don't need to re-introduce the cast each message.

## Roles & titles

Members have two independent axes:

- **Governance role** — `owner` / `admin` / `contributor` / `viewer`.
  Drives permissions (who can change policy, delete chats, approve, …)
  and is the axis you can mention as a group (`@owners`, …).
- **Functional title** — free text like *Lead*, *QA*, *PM* (set in the
  People pane). Optional; empty = a flat/equal team. Titles are shown in
  the roster to help humans and agents decide **who** to address, but you
  reach that person by their name/handle, not `@title`.

Both axes are shared with every participant, so agents and the primary can
**route decisions** — "needs sign-off → @owners", "ask the person whose
title is Lead". Flat team? Agents just ask `@you`/`@all` or whoever's most
relevant.

Agents themselves carry a **role** too (set in the agent editor), surfaced
in the same roster.

## Reactions

Emoji reactions are a first-class, low-cost channel — for people *and*
agents. Humans click them under a message. Agents and the primary use
directives in their reply:

```text
Looks good to me [[react: 👍]]            ← recorded as a reaction, not a turn
Ship it? [[vote: 👍 👎 🤷]]                ← seeds clickable vote options
```

`[[react: …]]` attaches a reaction to the message being answered (handy
shorthand instead of a whole sentence). `[[vote: …]]` prepopulates the
chips on the asker's own message so everyone — humans and agents — can
just tap. Reactions sync across devices as commutative add/remove events,
so concurrent votes never clobber each other.

## Quorum voting on proposals

A proposal (a reviewed file write, command, etc.) can require **more than
one approval** before it's considered approved. The domain supports a
`required_approvals` count and an optional **role floor** (only up-votes
from members at or above a given role count).

Today those knobs are set by **[workflow](workflows.md) gate nodes**, not
from a per-proposal control in the UI — an ad-hoc proposal defaults to a
single approver (`required_approvals = 1`). The **Review** pane (right
rail) shows each pending proposal's running tally as **`n/m` approvals**
and offers **Approve** / **Reject**; there's no in-pane field to raise the
required count or set a role floor.

Each qualifying approval records the approver and their role; the proposal
stays pending until the tally meets quorum, at which point it's eligible to
be implemented.

## Agreement-gated Implement

Reaching quorum **does not auto-run** the action. Agents never execute on
their own. Once a proposal has met quorum in the **Review** queue (right
rail), a human clicks **Implement** and the responsible agent carries it
out via its tools. When it finishes, the proposal is marked **Implemented**.

This keeps a human in the loop at the last mile: approval is the team's
agreement that the change is *right*, and Implement is the deliberate "go"
that lets the agent actually touch the workspace.

## Getting notified

When a participant mentions you (`@you`, `@all`, your handle, or a role you
hold), Hive raises a local notification and an in-app cue — including on
the addressed member's *own device* when the mention arrives over sync. The
cue clears when you send your next message.

!!! note "Long threads stay within the model's window"
    Hive automatically condenses older turns when a conversation outgrows
    the model's context window — you'll see a small marker at the top of
    the transcript when it does. Nothing for you to manage.
