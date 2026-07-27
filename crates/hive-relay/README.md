# `hive-relay`

A thin, **content-blind** envelope-forwarding relay (axum + tokio): clients post
opaque, end-to-end-encrypted event envelopes keyed by workspace, and peers fetch
everything after a cursor. It also offers a rendezvous board (STUN candidates for
P2P) and ephemeral presence. It never sees plaintext — the workspace key lives
only on devices ([`hive-core::e2ee`](../hive-core/README.md)).

## ⚠️ This is not the production relay

The **production relay is a separate Go repo**,
**[github.com/honeyhive-ai/relay](https://github.com/honeyhive-ai/relay)** —
content-blind, hardened, durable, and self-hostable. It carries the same `/v1`
contract and `hrt1` token format.

This Rust crate is retained only as:

- the **behavioral oracle** / reference implementation, and
- an **in-process loopback fixture** for the Rust sync client's tests (a
  [`hive-runtime`](../hive-runtime/README.md) dev-dependency).

It is **not** built into the app and **not** deployed. Its state is in-memory.
**Don't add features here** — change the Go repo. To self-host, follow
[Self-hosting a relay](https://docs.apiaryhq.ai/networking/self-host/).
