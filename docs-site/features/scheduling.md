# Scheduled agents

Hive can fire a prompt at an agent on a timer — every N minutes, or once
a day at a set time. Each fire opens a **fresh chat**, posts your prompt,
and the chat's runtime answers. Useful for a daily standup digest, a
periodic dependency-audit run, or any recurring "go check this" task.

Open **Settings → Schedules** to manage them. There are no schedules by
default; the scheduler stays dormant until you add one.

## Create a schedule

Each schedule captures:

- **Label** — a name for the schedule (shows in the list).
- **Prompt** — the message posted into the new chat when it fires.
- **Cadence** — either an **interval** ("every N minutes") **or** a
  **daily time** (`HH:MM`).
- **Runtime** (optional) — which configured runtime answers. Leave it
  unset to use the workspace's default runtime.

!!! warning "Daily times are UTC"
    The `HH:MM` daily time is interpreted in **UTC**, not your local
    timezone. A schedule set to `09:00` fires at 09:00 UTC. Convert from
    your local time when you set it.

## What happens on each fire

When a schedule triggers, Hive:

1. Opens a brand-new chat in the current workspace.
2. Posts the schedule's prompt as the first message.
3. Lets the assigned (or default) runtime answer, exactly as if you'd
   typed it yourself — including any tool calls, which still go through
   the usual [consent flow](../concepts/tools-and-consent.md).

Because every fire is its own chat, runs don't pile up in one ever-growing
transcript, and you can read each result independently.

## Toggle and remove

Each row in the list has an on/off toggle and a remove control:

- **Toggle off** — keeps the schedule but stops it firing.
- **Remove** — deletes it.

## How the timer works

The scheduler runs on a **30-second tick**. An interval schedule fires
once its interval has elapsed since the last run; a daily schedule fires
on the first tick at or after its `HH:MM` (UTC) each day. Hive must be
running for a schedule to fire — schedules don't wake the app or run while
it's quit.
