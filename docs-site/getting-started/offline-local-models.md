# Offline setup with local models

Hive can run fully offline against open-weight models on your own
hardware. This page describes the setup we measured in Hive's lab: a
llama.cpp gateway on a ROCm GPU box, a CPU worker for quick jobs, and an
MCP server for documents, web search and memory. It covers which model to
use for which job and the settings that make a small local model a usable
agent.

The numbers below come from that lab (AMD GPUs, llama.cpp ROCm builds,
models from 300M to 35B). Your hardware will give different absolute
numbers, but the comparisons should hold.

## The short version

1. Serve models with **llama.cpp behind an OpenAI-compatible gateway**
   and add it in Hive with the **Local gateway (llama.cpp,
   OpenAI-compatible)** preset.
2. Use **gpt-oss 20B** for agent loops, **gemma4 12B** for chat,
   **LFM2 8B-A1B** as the Quick tasks model, and **Qwen3.6 35B-A3B**
   when you want the best answer.
3. Turn on **Supports tools** and connect an MCP server with
   `search_docs`, `web_search` and `fetch_url`. **Retrieve documents;
   don't paste them.** For vaults, turn on **Vault retrieval** in
   Settings → Models.

## The gateway

Ollama works with Hive (it has its own provider), but llama.cpp served
directly was much faster in our runs:

| Measured | llama.cpp (ROCm) vs Ollama |
|---|---|
| Prompt processing (prefill) | **5.8×** faster |
| Decode, upstream GGUF vs Ollama's model blob | **2×** faster |

Prefill dominates agent turns, because every round re-sends the system
prompt, the tools and the conversation. So the OpenAI-compatible path
(provider `custom`) is the fast path for local models.

Our gateway is a small OpenAI-compatible proxy in front of several
`llama-server` workers. It routes by the `model` field. It exposes
`/v1/chat/completions`, `/v1/embeddings` and `/v1/rerank`. Each GPU worker
runs:

- **4 parallel slots of 32k tokens each.** Hive plans against the
  per-slot window, not the model's trained maximum. Without an override,
  Hive asks a `llama-server` for its per-slot window (`/props`). A gateway
  without `/props` falls back to Hive's model table, which uses 32k for
  the local families below. Set **Context window** on the runtime if your
  slots are a different size.
- **q8_0 KV cache.** This roughly halves KV memory compared with f16, so
  four 32k slots fit next to the weights.
- **Host-memory prompt cache.** Prompt prefixes evicted from a slot are
  kept in host RAM and restored when the same prefix comes back.

A worker launched along these lines (check the flag names against your
llama.cpp build):

```bash
llama-server -m gpt-oss-20b.gguf --jinja \
  --parallel 4 --ctx-size 131072 \
  --cache-type-k q8_0 --cache-type-v q8_0 --flash-attn on \
  --cache-ram 8192 --host 0.0.0.0 --port 8081
```

`--ctx-size` is shared by all slots, so 131072 / 4 = 32k per slot.
`--jinja` uses the model's own chat template, which tool calling needs.

!!! warning "Route by prefix when one model runs on several workers"
    Hive keeps each chat's prompt byte-stable from turn to turn, so a
    turn can reuse the server's cache of the turn before it. In our
    measurements, the second turn of a chat sent straight to one
    `llama-server` reused 97–98% of its prompt from cache. This held
    with plain chat, with tools offered, and after compaction. The same
    turns sent through a gateway that routes by load reused nothing.
    When two workers are equally idle, a load-based policy alternates
    between them, so each turn lands on the worker that does not hold
    the previous turn's prefix. If the same model runs on more than one
    worker, use a cache-aware (prefix) routing policy, or give each
    worker its own model name.

### Adding it in Hive

**Settings → Models → Add runtime → Preset… → Local gateway (llama.cpp,
OpenAI-compatible)** fills in:

- provider `custom`, no API key
- endpoint `http://<host>:8080/v1/chat/completions`; replace `<host>`
  with the gateway's address (a Tailscale IP works)
- **Supports tools** on

Add one runtime per model you want to use (each sends a different
`model` name). When you type the model name, the form shows Hive's
**recommended role** for that model family.

The equivalent `hive.config.toml` block:

```toml
[[runtimes]]
id = "gateway-gpt-oss"
name = "gpt-oss 20B (gateway)"
provider = "custom"
kind = "local"
endpoint = "http://100.64.0.10:8080/v1/chat/completions"
preferred_model = "gpt-oss:20b"
supports_tools = true
# context_window = 32768   # only if the server can't report it
# max_tool_rounds = 24     # default 24, max 100
```

## Which model for which job

| Role | Model | Why (measured) |
|---|---|---|
| Agent loops | **gpt-oss 20B** | 2.2× gemma4's speed in agent loops, and it kept 100% of the facts through compaction. |
| Chat | **gemma4 12B** | Good everyday model. With `web_search` and `fetch_url` it went from **0/11 to 9/11** on questions about current versions. |
| Quick jobs (titles, classification) | **LFM2 8B-A1B** on CPU | Answers in **0.2–0.4 s** without touching the GPU. |
| Quality | **Qwen3.6 35B-A3B** | Use it when the answer matters more than speed. |
| Embeddings | **EmbeddingGemma 300M** | Retrieval; see below. |
| Reranking | **bge-reranker-v2-m3** | Retrieval; see below. |

Models to avoid for some jobs:

- **LFM2.5** always reasons, so it's slow for quick jobs. It also lost
  about a third of the facts when it wrote compaction summaries. Hive
  gives it no recommended role.
- Don't use the quick-jobs model for summaries. Summaries need a strong
  model; Hive writes them with the chat's own runtime.

### Reasoning

Reasoning models spend output tokens thinking before they answer.

- **Give them room.** With a 160-token answer budget, reasoning models
  ran out mid-thought. They need **at least 1024 tokens**. Hive sends no
  `max_tokens` on the OpenAI wire, so the server's slot is the only limit.
- **gpt-oss always reasons.** When the runtime's thinking switch is off,
  Hive sends `reasoning_effort: "low"` to keep the reasoning short.
- **gemma4 and Qwen3** can turn thinking on and off. Hive sends
  `chat_template_kwargs: {"enable_thinking": false}` unless you turn on
  **Let the model think** for the runtime. Thinking is off by default.
- **LFM2** doesn't reason, so Hive sends no thinking fields for it.

Hive sends these fields only to `custom` and `hive-daemon` runtimes,
because hosted APIs reject fields they don't know. If a server rejects
them anyway, Hive retries the request without them.

## Tools and MCP

Local models do much better with tools than without them. Turn on
**Supports tools** for the runtime (the gateway preset already does).
For an HTTP runtime this offers two kinds of tools:

- **Hive's built-in file tools** (`read_file`, `list_dir`, `search`,
  `edit_file`, `write_file`, optionally `run_command`). In a git
  workspace, a turn reads the chat's folder until it first writes or
  runs a command. At that point Hive creates a worktree for the turn,
  and the turn's edits come back as a proposal for you to review. A turn
  that only reads never creates a worktree. The switches are in
  **Settings → Permissions → Local model file tools**.
- **Tools from your enabled MCP servers.**

The file tools send the contents of the files the model reads to the
runtime's endpoint. Hive offers them only when the endpoint's host is on
this machine or a private network:

- loopback, `localhost`, and `*.localhost`
- RFC 1918 addresses (`10.x`, `172.16–31.x`, `192.168.x`)
- Tailscale addresses (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`)
- link-local addresses
- `*.local`, `*.internal`, and `*.home.arpa` names

Tailscale MagicDNS names (`*.ts.net`) and single-label names (`gpu-box`)
don't count, because a `*.ts.net` name can be public through Tailscale
Funnel and a single label can be a public domain. Use the machine's
Tailscale or LAN IP address instead, or turn on the opt-in below. Hive
reads the host the same way its HTTP client does, so an endpoint such as
`http://example.com\@127.0.0.1/` counts as `example.com`.

For any other endpoint, turn on **Allow workspace file tools for this
endpoint** on the runtime. Without it, the model gets only your MCP tools.
The switch is saved on this device only, for that endpoint: it is never
read from or written to `hive.config.toml`, so a shared or cloned config
can't turn it on, and pointing the runtime at another endpoint turns it
off.
Hosted APIs (OpenAI, OpenRouter, Azure) never get the tool loop.

The tool loop allows 24 rounds by default (set **Max tool rounds** on
the runtime). When the conversation gets close to the context window,
Hive stops offering tools so the model has to answer.

Small models sometimes repeat a call they have already made, such as
reading the same file again. Hive doesn't run a repeated call again: it
returns the earlier result and asks the model to move on. Anything that
changes the workspace, like an edit or a command, counts as a change, so
reading a file again after editing it works as normal. If the model
repeats one call three times, Hive ends the loop and asks for an
answer.

On the last round, Hive tells the model the tool budget is used up and
asks for an answer from what it has. With an OpenAI-compatible server,
that round still lists the tools but sends `tool_choice: "none"`, so
llama.cpp can reuse its cached prompt instead of processing the whole
conversation again. For models that return their reasoning separately,
like gpt-oss, Hive sends each round's reasoning back with that round's
tool calls. In lab tests this helped gpt-oss keep track of ids across
steps.

A useful MCP server for offline work offers:

| Tool | Use |
|---|---|
| `search_docs` | Search your indexed documents (embed, then rerank) |
| `web_search`, `fetch_url` | Current information, when the machine is online |
| `remember`, `recall` | Long-term memory across chats |
| `index_document`, `list_collections` | Add documents to the index and list collections |

Hive's lab uses a server of this shape (rdnamcp), served over
Streamable HTTP. Add it as an `http` MCP server; see
[MCP servers](../addons/mcp.md).

!!! note "Static bearer tokens"
    For a server protected by a fixed token, use **Settings → Tools →
    Add HTTP server** and fill in **Bearer token (optional)**, or click
    **Set token** on an existing HTTP server. The token stays on this
    device; see [MCP servers](../addons/mcp.md#static-bearer-tokens).

## Quick tasks model

**Settings → Models → Quick tasks model** chooses the runtime used for
chat titles. Point it at a small, fast model that doesn't reason (LFM2
8B-A1B on CPU answered in 0.2–0.4 s). If that runtime fails or times
out, Hive falls back to the chat's own runtime.

## Compaction

When a conversation outgrows the window, Hive folds the oldest messages
into a summary. Two design choices come from our measurements:

- **The summary goes after the system prompt, not inside it.** The
  system prompt stays byte-identical on every turn, so the server reuses
  its cached prefix. Putting the summary after the system prompt gave
  **75% cache reuse and a 1.8 s first token**. Rewriting the system
  prompt to include the summary gave **0% reuse and 5.6 s**. Stable
  prefixes made agent sessions **2.7× faster** overall.
- **Compaction is rare and done in batches.** Each summary took **20–34
  s** to generate. Hive compacts only after the kept history passes 85%
  of the budget, then moves the boundary far enough that it fits in 50%.
  Turns between compactions reuse the same summary without another
  model call.

Hive saves each chat's summary on this device, so after a restart it
reuses the summary instead of writing a new one. The saved summary
stays out of the synced chat log. If a message that the summary covers
is edited or removed, Hive writes a new summary.

If the summarizer fails, the turn still goes ahead. It uses the
previous summary, or a note that earlier messages were left out. Hive
then waits before it tries the summarizer again: after 2 more turns or 2
minutes, whichever comes first. Each failure in a row doubles the wait,
up to 16 turns or 30 minutes.

Summaries are written by the chat's own runtime. Use a strong model for
chats that run long; gpt-oss 20B kept every fact through compaction.

## Retrieval

Retrieval gave the model the same answer quality as handing it the
right file, at **1/5 of the tokens** and **3× faster**. Pasting about 26k
tokens of documentation was **8× slower** and often overflowed a 32k
slot.

The pipeline that worked best:

1. Embed the question with **EmbeddingGemma 300M**.
2. Take the **top 20** passages by vector similarity.
3. Rerank those 20 with **bge-reranker-v2-m3**.
4. Give the model the **top 5** passages.

This runs inside the MCP server's `search_docs` tool, so the model
calls it when it needs it. On a llama.cpp gateway the reranker endpoint
is `/v1/rerank` (`{model, query, documents, top_n}` in,
`results[{index, relevance_score}]` out).

### Vault retrieval

Hive can run the same pipeline over the vaults attached to a chat. Set
it up in **Settings → Models → Vault retrieval**:

| Field | Gateway value |
|---|---|
| Embeddings URL | `http://<host>:8080/v1` |
| Embeddings model | `embeddinggemma-300m` |
| Rerank URL (optional) | `http://<host>:8080/v1` |
| Rerank model | `bge-reranker-v2-m3` |
| Passages per message | 5 |
| Candidates to rerank | 20 |

Use **Test** to check both endpoints. With retrieval on, each message
is used as the query, and the top passages are added to the end of
that message, under a "Retrieved from reference vaults" heading. The
system prompt stays the same from turn to turn, so the server's prompt
cache keeps working. Embeddings are cached in memory, so after the
first message only the question is embedded. The cache is cleared when
Hive restarts.

With retrieval off, or when it fails or times out, vaults are pasted
into the system prompt instead, capped at 3 sources of 6000 characters
each. The endpoints are called without an API key, so they must be
keyless, as a local gateway is.

## Hardware takeaways

- **Retrieve, don't paste.** It used fewer tokens, ran faster, and
  didn't overflow slots.
- **Run retrieval on the GPU.** Embedding and reranking sit on the
  critical path of every `search_docs` call and every vault-retrieval
  message.
- **A small embedder plus a reranker is enough.** A 300M embedder with
  a reranker matched handing the model the right file.
- **Use a model that doesn't reason for quick jobs.** A small CPU model
  (LFM2 8B-A1B) handles titles and classification in under half a second
  and leaves the GPU to the chat model.
- **Alert before thermal limits.** Long agent sessions keep the GPU
  busy for minutes at a time. Set monitoring alerts below the card's
  throttle temperature so you see a problem before it slows down
  generation.
