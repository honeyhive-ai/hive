"""aider-mcp — expose aider's CLI as MCP tools.

The Hive client treats this as a normal MCP stdio server (configured
in `hive.config.toml` under `[[mcp_servers]]`). Tools surfaced:

  - aider_edit: run aider with a prompt against one or more files.
  - aider_review: ask aider to summarize / critique a file.
  - aider_commit: have aider create a commit message + commit.
  - aider_undo: revert aider's last edit.

The shim doesn't try to be clever — it shells out to `aider` and
returns whatever stdout/stderr says. Hive's consent flow gates each
call.

Hive can also use aider as a first-class runtime (set
`provider = "aider"` in a `[[runtimes]]` block). That shape makes
aider the LLM driving a chat. This shim is the complement: it
makes aider a *tool* that another runtime can call — useful when
your chat agent is Claude or GPT-4 but you want to hand off the
code-edit step to aider. Both shapes are valid; this addon
supports the tool shape.
"""

__version__ = "0.1.0"
