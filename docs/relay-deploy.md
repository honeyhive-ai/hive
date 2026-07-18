# Hosting the relay in the cloud

The relay now lives in its own repository:

> **[github.com/honeyhive-ai/relay](https://github.com/honeyhive-ai/relay)** (MIT)

Full deploy instructions — Docker, Fly.io, environment variables, TLS, optional
token gating, and Postgres/HA — live in that repo's **README** and
`deploy/fly.toml`. It's a single portable container, so you can run it on any VM,
managed container host (Fly, Render, Railway, Cloud Run), or Kubernetes; the only
requirements are a public HTTPS URL and (for the snapshot store) a small volume.

Quick start:

```bash
docker build -t hive-relay https://github.com/honeyhive-ai/relay.git
docker run -d -p 8443:8443 -v hive-data:/data -e HIVE_RELAY_DATA_DIR=/data hive-relay
curl http://localhost:8443/v1/health   # → ok
```

Then point each device at the resulting HTTPS URL (Settings → **Multiuser sync**,
or `HIVE_RELAY_URL`). Always set a shared `HIVE_WORKSPACE_KEY` so traffic is
end-to-end encrypted (the relay only ever sees ciphertext).

> This monorepo keeps a Rust reference relay (`crates/hive-relay`) used only as an
> in-process fixture for the client's tests — it is **not** the deploy target. Use
> the relay repo above for anything you run in production.
