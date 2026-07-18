# Your first chat

After onboarding, Hive opens to an empty workspace. Click the `+` next
to **Chats** in the sidebar to start a new chat.

![Empty chat ready for input](../images/first-chat-empty.png){ width="800" }

## Type a message

The composer is at the bottom. A few affordances worth knowing:

- **Primary runtime select (above the input).** Shows which runtime
  answers this chat, and lists every configured runtime so you can
  switch per chat. To address a specific agent or teammate instead,
  type `@` to bring up the mention popover — candidates are `@primary`,
  `@all`, your **agents**, and **members** of the chat. (There are no
  `@file` mentions; attach files with the picker below.)
- **Slash commands.** Type `/` to bring up commands. Selecting one runs
  it instead of sending text: **Summarize conversation**, **Compact
  conversation**, **Insert Linear issues**, **Export chat → Markdown**,
  and **Clear input** — plus any user templates you've saved.
  [`/summarize` and `/compact`](../features/voice-and-slash.md) manage
  the conversation's context window, and `/linear` pulls your
  [Linear issues](../addons/linear.md) into the composer.
- **Attach files.** Use the 📎 **Attach files** button in the composer
  to add files (including images); they're saved and referenced by path
  when you send — handy for "explain this" without pasting.
- **Context at a glance.** The **⬡ pill** in the chat header shows how
  much of the model's context window the next reply will use; click it
  for the full breakdown and controls. See
  [Managing context](../features/voice-and-slash.md).

Press Enter to send. **Shift+Enter** inserts a newline.

## Auto-renamed titles

After the first assistant reply, Hive asks the same runtime for a
3–6 word topic summary and renames the chat. You can override at any
time by clicking the pencil glyph next to the title in the chat header.

## Tools

If your runtime supports tools, the model can call **MCP tools** you've
configured. MCP servers are **inert until you enable them** — add and
switch them on under **Settings → Tools**. Until then a chat runs with
no tools attached.

File access for local CLI agents is governed by Claude Code's
permission mode, set at **Settings → Team → Agent file access**:

- **default** — read-only; the agent's file writes are blocked.
- **acceptEdits** — the agent may create and modify files in your
  workspace.
- **bypassPermissions** — the agent may edit files *and* run shell
  commands without asking.

## Proposals — Review and Implement

When an agent proposes a change that needs sign-off, it lands as a
**proposal** in the **Review** pane on the right rail. Each proposal
shows its approvals (`n/m`), and you can approve or reject it. An
approved proposal only takes effect once quorum is met and a human hits
**Implement** — nothing is applied to the workspace behind your back.
After it runs, the proposal is marked **Implemented**.
