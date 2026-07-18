# Security Policy

Hive handles end-to-end-encrypted workspaces, device keypairs, and a relay that
forwards ciphertext. We take vulnerabilities seriously and appreciate coordinated
disclosure.

## Reporting a vulnerability

**Please do not open a public issue for security problems.**

Report privately, whichever you prefer:

- **GitHub private advisory** — the repo's **Security → Report a vulnerability**
  tab (preferred; keeps discussion attached to the fix), or
- **Email** — **m.zazula@gmail.com**. Encrypt if you like; ask for a key.

Please include: affected component (app / relay / crypto / MCP / sync), version
or commit, reproduction steps or a PoC, and impact. We aim to acknowledge within
a few days and will keep you updated on the fix and disclosure timeline.

## Scope

In scope:

- **`hive-core` crypto & signed envelopes** — Ed25519 identity, X25519 key
  agreement, ChaCha20-Poly1305, HPKE per-device sealing, envelope
  verification/quarantine.
- **The relay** ([github.com/honeyhive-ai/relay](https://github.com/honeyhive-ai/relay))
  — entitlement gating, content-blind forwarding (it must never be able to read
  plaintext), the `hrt1` token format.
- **Sync / P2P** — rendezvous, STUN/hole-punch, TURN fallback, workspace-key
  rotation.
- **MCP & tool execution** — the install-is-inert-until-enabled gate, per-tool
  trust, subprocess bridges.
- **Tauri IPC / desktop app** boundaries.

Out of scope: vulnerabilities in third-party model providers or MCP servers you
choose to run; social-engineering; issues requiring a already-compromised device.

## Supported versions

Hive is pre-1.0 and moves fast — fixes land on `main`. Please test against the
latest `main` (or the newest release) before reporting.

## Handling

Valid reports are fixed on `main` with a GitHub Security Advisory and credit to
the reporter (unless you prefer to remain anonymous). Because the desktop client
is a single OSS binary and the relay is self-hostable, please note in your report
whether the issue affects self-hosted deployments so we can advise operators.
