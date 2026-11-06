# Hive on your phone

The Hive iPhone app is a thin client for one person's workspace. The phone
doesn't run Hive and holds no workspace keys. It talks to **`hive serve`**, a
Hive device you run on an always-on box, over a small JSON + Server-Sent
Events API (`/v1/…`). The box keeps the end-to-end encryption keys, syncs with
your relay and runs the agents you send to it; the phone reads and writes
through it.

> The app's source lives in its own repository. The `hive serve` side and the
> API contract the app uses are in this repo
> ([`docs/ios-app.md`](https://github.com/honeyhive-ai/hive/blob/main/docs/ios-app.md)),
> so any client can implement them.

To set up the box and pair a phone, see
[Remote access for your phone](../networking/remote-access.md). To make the
box act as you rather than as a separate member, see
[Running hive serve as your own device](../ops/serve-box.md).

## Chats

- The chat list shows each chat's title, a preview of the newest message, an
  unread count and up to five recent participants. Chats are filed under their
  channels, in the desktop's sidebar order.
- Start a chat or send a message from the phone. `@mentions` route exactly as
  on the desktop: `@agent` wakes that agent, `@hive` the chat's primary
  runtime. See [Multi-agent collaboration](multi-agent.md).
- Replies stream live. A reply produced on the `hive serve` box streams token
  by token; one produced on another device (your desktop, a worker) appears
  once it syncs, a few seconds later.
- Unread counts and read marks belong to the box, not to each device:
  opening a chat on the phone marks it read there, and your desktop keeps its
  own read state.

## People, channels and chat details

- **People** lists the workspace's human members with their role, title and
  avatar. The box's own identity is marked as you.
- **Channels** shows the channel tree, including each channel's drop-in chat.
- **Chat details** is a read-only view of the desktop's right-hand panel: who
  is in the chat, which agents it uses and where each one runs, who answers an
  untagged message (and whether that device is online right now), a rough
  context-window estimate, recent workflow runs, and the chat's skills and
  vaults.

!!! note "Who answers `@hive`"
    Only a desktop runs a chat's primary runtime (`@hive`). Chat details says
    whether your desktop is available. If it's closed, the turn waits and is
    answered when the desktop opens. For chats the phone creates to be
    answered by your desktop, enroll the box as
    [your own device](../ops/serve-box.md).

You can also move an existing agent to a different host from the phone.
Creating agents and editing runtimes stay on the desktop.

## Reviewing proposals

Proposals (an agent's file changes, a command, a decision) appear on the
phone with the same collaboration state as the desktop's Review card:

- **Votes, with who and role.** Each member's latest vote shows their name and
  workspace role, alongside the required approvals, the qualifying approvals
  so far and whether quorum is met.
- **Approve or reject.** The phone votes through the same core call as the
  desktop, at the box's real roster role. Quorum, the role floor and the
  no-self-approval rule apply unchanged; a role below the floor gets an error
  and nothing is written.
- **Refinement thread.** Read the proposal's notes and add your own. Mark a
  note as a change request.
- **Request changes.** *Send to agent to revise* bundles the change requests
  (or every note, if none is marked), records it on the thread and sends the
  instruction to the agent that produced the proposal. When that agent files
  its next version in the chat, the new version is linked to the old one and
  the old one leaves the open list.

Approving a file-diff proposal from the phone doesn't apply it, unless the
box is set to (see [`HIVE_SERVE_APPLY_ON_APPROVE`](../ops/serve-box.md#apply-on-approve)).
As on the desktop, **Implement** applies it.

## Workflow approvals

A workflow run that reaches an approval gate files a decision proposal for it.
It shows up on the phone like any other proposal, and approving or rejecting
it there votes on the gate exactly as the desktop's Review pane does. Workflow
proposals (an agent's suggested new workflow) appear too, as a decision with
the stages as its text. See [Workflows](workflows.md).

## Push notifications

With [remote access through a relay](../networking/remote-access.md) set up,
`hive serve` can wake the phone with a notification. The content is end-to-end
encrypted between the box and the phone: the relay and Apple see only
ciphertext.

| Notification | When |
|---|---|
| *Name* mentioned you | a message from an agent or another member `@mentions` you, `@you`, `@all` or a role group you're in |
| *Workflow* needs approval | a workflow run's approval gate is waiting (again on each rerouted attempt) |
| *Agent* replied / hit an error | an agent reply finished (not your own messages, and not a reply that already notified you as a mention) |
| Change ready for review | a proposal is waiting for review (a workflow gate is announced once, as an approval, not also here) |
| New phone paired | a new phone was paired with the box |

Tapping a notification opens its chat.

- **Previews are on by default.** A notification shows who did what as the
  title and, for replies and mentions, the first 120 characters of the message
  as the body. Markdown is flattened to plain text first: a table becomes
  `Header: value` lines, list items get a bullet, and headings, fences and
  link URLs are dropped.
- **Previews are per phone.** Turn them off in the app's notification
  settings and that phone gets only the title ("coder replied"). Other phones
  keep their own setting.
- **No duplicates while you're looking.** While the app is open and following
  the live event stream, the box doesn't push to that phone.
- **Only news.** Rapid bursts in one chat collapse into one notification per
  10 seconds, there are per-phone and host-wide rate limits, and history that
  synced in late (older than 10 minutes when `hive serve` started) never
  pushes.

!!! warning "Push needs your own APNs key today"
    Apple accepts a push only when it is signed with a key from the team that
    signed the app. With a self-hosted relay, that means running the relay
    with your own APNs key and building the app under the same Apple team. See
    [Remote access → Push through your own relay](../networking/remote-access.md#push-through-your-own-relay).

Mentions also work on the desktop; see
[Mentions & notifications](mentions-and-notifications.md).

## Appearance

The app can follow the box's theme. Pick **Match host** in the app's
appearance settings and it reads `GET /v1/appearance` from the box, which
answers a palette (`pollen`, `studio`, `harbor`, `slate`, `meadow` or
`obsidian`) and a mode (`auto`, `light` or `dark`).

The box decides each value on its own, in this order:

1. its own settings, `HIVE_SERVE_PALETTE` and `HIVE_SERVE_APPEARANCE`;
2. the desktop app's saved theme, if the desktop app is installed on the same
   machine (read on every request, so a change on the desktop shows up the
   next time the phone asks);
3. neither: the app uses its default palette.

## Dashboard

The box can also serve a dashboard for the phone: a built-in Hive section
(agents, proposals, workflow runs, turns, the host's status) plus panels you
define in `dashboard.toml` on the box. The app hides its dashboard tab when
the dashboard is empty. Format and examples:
[`docs/dashboard.md`](https://github.com/honeyhive-ai/hive/blob/main/docs/dashboard.md).
