"""MCP stdio server that wraps aider.

Hive talks to this process over stdin/stdout in JSON-RPC 2.0 (the MCP
wire format). Each `tools/call` request gets translated into an aider
CLI invocation; aider's stdout/stderr come back as the tool result.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from dataclasses import dataclass
from typing import Any


# ---------------------------------------------------------------------------
# Tool registry
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class ToolDescriptor:
    name: str
    description: str
    input_schema: dict[str, Any]


TOOLS: list[ToolDescriptor] = [
    ToolDescriptor(
        name="aider_edit",
        description=(
            "Run aider against one or more files with a natural-language "
            "prompt. aider plans the edit, applies it, and (by default) "
            "commits. Requires user approval on the Hive side."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Workspace-relative paths to give aider as context.",
                },
                "prompt": {
                    "type": "string",
                    "description": "Instruction for aider, e.g. 'add a regression test for the off-by-one in parseRange'.",
                },
                "auto_commit": {
                    "type": "boolean",
                    "description": "If false, aider will edit but not commit. Defaults to true.",
                },
            },
            "required": ["files", "prompt"],
        },
    ),
    ToolDescriptor(
        name="aider_review",
        description=(
            "Ask aider to review a file and surface issues. Read-only — "
            "no edits are written."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "Workspace-relative path to review.",
                },
                "focus": {
                    "type": "string",
                    "description": "Optional area of concern (security, performance, style, ...).",
                },
            },
            "required": ["file"],
        },
    ),
    ToolDescriptor(
        name="aider_commit",
        description=(
            "Have aider craft a commit message for the currently-staged "
            "changes and commit them. Requires user approval."
        ),
        input_schema={
            "type": "object",
            "properties": {
                "hint": {
                    "type": "string",
                    "description": "Optional context for the commit message.",
                },
            },
        },
    ),
    ToolDescriptor(
        name="aider_undo",
        description="Revert aider's most recent commit. Requires user approval.",
        input_schema={
            "type": "object",
            "properties": {},
        },
    ),
]

TOOLS_BY_NAME = {t.name: t for t in TOOLS}


# ---------------------------------------------------------------------------
# aider invocation
# ---------------------------------------------------------------------------


def _aider_binary() -> str:
    return os.environ.get("AIDER_BINARY", "aider")


def _run_aider(args: list[str], cwd: str | None = None) -> dict[str, Any]:
    """Run aider with the given args; return MCP content blocks."""
    try:
        result = subprocess.run(
            [_aider_binary(), *args],
            cwd=cwd,
            capture_output=True,
            text=True,
            check=False,
        )
    except FileNotFoundError as exc:
        return _text_block(
            f"aider not on PATH ({exc}). Install with `pip install aider-chat`."
        )

    body = result.stdout.strip() or result.stderr.strip()
    if result.returncode != 0:
        return _text_block(f"aider exited {result.returncode}\n\n{body}")
    return _text_block(body or "(aider produced no output)")


def _text_block(text: str) -> dict[str, Any]:
    return {"content": [{"type": "text", "text": text}]}


def _call_aider_edit(args: dict[str, Any]) -> dict[str, Any]:
    files = args.get("files") or []
    prompt = args.get("prompt") or ""
    auto_commit = args.get("auto_commit", True)

    cli = [
        "--no-pretty",
        "--yes-always",
        "--no-stream",
        "--message",
        prompt,
    ]
    if auto_commit is False:
        cli.append("--no-auto-commits")
    cli.extend(files)
    return _run_aider(cli)


def _call_aider_review(args: dict[str, Any]) -> dict[str, Any]:
    file = args.get("file") or ""
    focus = args.get("focus") or "general"
    prompt = (
        f"Review {file} focusing on {focus}. Do not modify the file — list "
        "issues only as a bulleted summary."
    )
    return _run_aider(
        [
            "--no-pretty",
            "--yes-always",
            "--no-stream",
            "--no-auto-commits",
            "--message",
            prompt,
            file,
        ]
    )


def _call_aider_commit(args: dict[str, Any]) -> dict[str, Any]:
    hint = args.get("hint")
    cli = ["--commit"]
    if hint:
        cli.extend(["--message", hint])
    return _run_aider(cli)


def _call_aider_undo(_args: dict[str, Any]) -> dict[str, Any]:
    return _run_aider(["--undo"])


DISPATCH = {
    "aider_edit": _call_aider_edit,
    "aider_review": _call_aider_review,
    "aider_commit": _call_aider_commit,
    "aider_undo": _call_aider_undo,
}


# ---------------------------------------------------------------------------
# MCP JSON-RPC loop
# ---------------------------------------------------------------------------


def _reply(message_id: Any, result: dict[str, Any]) -> str:
    return json.dumps({"jsonrpc": "2.0", "id": message_id, "result": result})


def _error(message_id: Any, code: int, message: str) -> str:
    return json.dumps(
        {
            "jsonrpc": "2.0",
            "id": message_id,
            "error": {"code": code, "message": message},
        }
    )


def _handle(payload: dict[str, Any]) -> str | None:
    method = payload.get("method")
    message_id = payload.get("id")
    params = payload.get("params") or {}

    if method == "initialize":
        return _reply(
            message_id,
            {
                "protocolVersion": "2024-11-05",
                "serverInfo": {"name": "aider-mcp", "version": "0.1.0"},
                "capabilities": {"tools": {}},
            },
        )

    if method == "tools/list":
        return _reply(
            message_id,
            {
                "tools": [
                    {
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": t.input_schema,
                    }
                    for t in TOOLS
                ]
            },
        )

    if method == "tools/call":
        name = params.get("name")
        arguments = params.get("arguments") or {}
        if name not in DISPATCH:
            return _error(message_id, -32602, f"unknown tool: {name}")
        result = DISPATCH[name](arguments)
        return _reply(message_id, result)

    if method == "ping":
        return _reply(message_id, {})

    if message_id is None:
        # Notification — no reply expected.
        return None

    return _error(message_id, -32601, f"method not found: {method}")


def serve() -> None:
    """Read line-delimited JSON-RPC from stdin, reply on stdout."""
    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        try:
            payload = json.loads(raw)
        except json.JSONDecodeError as exc:
            sys.stdout.write(
                _error(None, -32700, f"parse error: {exc}") + "\n"
            )
            sys.stdout.flush()
            continue
        response = _handle(payload)
        if response is not None:
            sys.stdout.write(response + "\n")
            sys.stdout.flush()
