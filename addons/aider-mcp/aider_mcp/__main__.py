"""aider-mcp entrypoint.

Usage:
    aider-mcp                       # speak MCP over stdio
    python -m aider_mcp             # same
"""

from .server import serve


if __name__ == "__main__":
    serve()
