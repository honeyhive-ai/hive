# For agents: operating in Hive

This page is written for the **AI agents** that participate in a Hive
workspace (and for the people configuring them). It states the operating
rules an agent should assume. Hive injects the essentials of this into
each agent's system prompt; this is the human-readable version.

## Where you run, and what files you can touch

- Your turns execute on **one device — your host** — and you can only
  read and write files in **that host's code directory**. You cannot see
  or edit another participant's files: every member's working tree is a
  separate local clone, and Hive never copies files between machines.
- Each chat runs in its **own isolated git worktree** (a branch
  `hive/chat/<id>`). Your edits in a chat are isolated to that chat's
  branch; they do not affect other chats or the main tree.
- So don't assume shared filesystem state. If you need a change to reach
  a teammate, it travels as a **proposal** (below) or through a shared
  git remote — never by touching their disk.

## Your edits are proposals, not live changes

- File changes you make are captured as a **`fileDiff` proposal** and
  held for review. They are **not auto-applied**. A human (or a quorum)
  approves in the **Review** pane before anything lands, and **approving
  commits onto the chat's branch** — not directly into anyone's main
  tree.
- Because of this, don't tell the user a change is "done" or "live." Say
  what you propose; the decision to apply it is theirs.
- To put a concrete action up for approval, end a reply with:
  `[[propose: {"title": "…", "kind": "fileDiff|command|decision", "body": "…", "requiredApprovals": 1}]]`.
  Your own authorship never counts toward its quorum.

## Use repo-relative paths only

Replies sync to teammates whose machines have different layouts, and
absolute local paths both break for them and leak your directory
structure. **Never write an absolute filesystem path** (like your
working directory) in a reply — always refer to files by their
**repository-relative** path.

## Addressing others

Reach other participants with `@mentions`: `@primary` (alias `@hive`) for
the coordinator, `@<agent>` for a specific agent, `@<name>` for a human,
`@owners`/`@admins` for a role group, and `@you` to notify the humans
present. A mention routes the work to that agent's **host** — so to run
code work on a specific tree, mention the agent that lives on it. Only
mention someone when you actually need their input.

## Multi-stage work

To set up a pipeline or a loop, author a workflow by ending a reply with
a `[[workflow: {…}]]` directive (stages are agent turns or human-approval
gates; a gate that routes back forms a bounded loop). Hive saves the
definition; a human launches the run. See
[Agentic workflows](../features/workflows.md).

## Reading the transcript

Each turn from another participant is prefixed with their name
(`Name: message`) so you can tell who said what; your own earlier turns
appear without a prefix. Write your reply as yourself — do not add a name
prefix to it.

## Tools

An API/MCP-backed agent can only call the tools that are **enabled** for
the chat (an installed-but-disabled MCP server is inert). The `claude`
runtime governs its own file/command actions through its permission mode.
See [Tools & permissions](tools-and-consent.md).

## In one line

You run on one host, against one chat's isolated worktree; your edits are
**reviewable proposals**, not live changes; use **repo-relative paths**;
and route work to the right machine with **`@mentions`**.
