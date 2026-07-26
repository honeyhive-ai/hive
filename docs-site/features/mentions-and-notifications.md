# Mentions & notifications

Hive tells you when you're wanted without making you scan every chat.
An `@`-mention that names **you** raises a desktop notification and
lights up the channel — and the chat — in the sidebar until you read it.

> This page is about being *notified*. For the full routing table —
> which mention wakes an agent, the Primary Runtime, or a role group —
> see [Multi-agent collaboration → Mentions](multi-agent.md#mentions).

## What counts as a mention of *you*

A message pings you when it isn't yours and it carries any of:

| Mention | Reaches you because… |
|---|---|
| `@you` / `@all` | the two human-broadcast tokens |
| `@yourname` / `@name#N` | it names you by handle (the `#N` disambiguates duplicates) |
| `@owners` `@admins` `@contributors` `@viewers` | it names the governance role you hold |

Agent mentions (`@agent-name`) never ping a human — they route a turn
to that agent. An *unanswered* agent mention is **queued work**, shown
in the [Review pane](right-rail.md), not as a personal highlight.

## Desktop notification

When a message you didn't write mentions you, Hive raises one system
notification — "*Name* mentioned you" — per message. Whether the app is
focused or in the background, and whether the message was typed locally
or arrived over sync, the trigger is the same.

## Sidebar highlights

The same mention lights up the sidebar so it's still there after the
notification banner fades:

- the **channel name** turns bold with a small dot, and
- the specific **chat row** shows the unread dot.

The channel dot lives on the header, so it's visible **even when the
channel is collapsed** and its chats are hidden. A channel lights up if
any chat under it — its main conversation or a focused child chat — holds
an unread mention.

## Reading clears it

Opening the chat marks it read and clears the highlight; any new message
that arrives while you're looking at it stays read. There's nothing to
dismiss manually.

**Read state is per device.** Whether *you've* seen a mention is local
UI state — it lives on your machine, never on the synced workspace log.
Each of your devices tracks its own read cursor, and other members' apps
highlight (or don't) entirely independently of yours. Nothing about what
you've read is shared with the workspace or the relay.
