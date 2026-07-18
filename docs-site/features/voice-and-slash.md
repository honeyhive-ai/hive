# Managing context

The model can only see so much at once — its **context window**. Hive
manages that budget for you automatically, shows you where it stands at
all times, and gives you explicit controls when you want them.

## The context pill

The chat header shows a live pill — **⬡ 42%** — with the share of the
model's window the *next* reply will use (system prompt + the messages
that fit). It turns **amber at 80%**. Click it to open the **Context
pane** in the right rail.

## The Context pane (⬡)

The full breakdown, updated live while replies stream:

- **Budget bar** — tokens planned vs. the model's window.
- **What's in the prompt** — system prompt, kept history, loaded
  skills (with their token cost), attached vaults.
- **What fell out** — how many older messages overflowed and were
  folded into an automatic summary.
- **One-click unloading** — remove a skill or unmount a vault right
  from the pane to reclaim budget.

## `/summarize` and `/compact`

Long conversations eventually outgrow any window. Hive **auto-summarizes
overflow** — older messages are collapsed into a running summary that
rides in the system prompt — but two `/` commands give you the explicit
version (type `/` in the composer; both are tagged **Context**):

### /summarize

Posts a model-written **summary of the conversation so far** as a new
message. The transcript is **kept** — nothing is removed. Use it for a
recap, or to hand a newcomer the gist.

### /compact

**Collapses the conversation into a single summary checkpoint.** The
existing messages are removed and the summary becomes the new head of
the chat, freeing context immediately. This is the explicit version of
the automatic overflow handling.

!!! warning "/compact removes messages"
    Hive confirm-gates it. The earlier messages are gone from the active
    conversation afterward — reach for `/summarize` if you want to keep
    the full history.

## Customizing the instructions

Both commands follow an instruction you can rewrite in
**Settings → Models → Context commands**:

- Two text fields — one per command — with the built-in default shown
  as the placeholder. Blank = default.
- Want `/compact` to preserve code snippets verbatim, or `/summarize`
  to output only decisions and action items? Write it there.
- The `/summarize` instruction **also guides the automatic overflow
  summarization**, so long-chat memory follows the same rules you set.

Changes apply immediately (persisted in `settings.json`); no restart.

## Sizing the window itself

Hive infers the context window from the model name (Claude, GPT-4o, …).
For **Ollama / LM Studio / custom endpoints** it can't, so set it
yourself:

- **Settings → Models → Add runtime → "Context window in tokens"**, or
- `context_window = 32768` on the runtime in `hive.config.toml`.

The planner budgets against your number — the pill and pane reflect it.

## What else occupies the window

Everything the pane counts, and where to manage it:

| Occupant | Managed in |
|---|---|
| [Skills](../addons/skills.md) | Skills pane (✦) / Context pane |
| [Vaults](../concepts/vaults.md) | Vaults pane / Context pane |
| Attachments & `@file` | The composer — you choose what to include |
| Conversation history | Automatic windowing + `/compact` |

See [Your first chat](../getting-started/first-chat.md) for the rest of
the composer's `/` menu.
