# Single-Manifest Hosting

This is the deployment shape for the first remotely usable Bridge release:
**one manifest, one database, one team**.

The goal is to host one `bridge.mcp/v1` manifest safely without introducing
tenant routing, control-plane features, or manifest persistence.

## What Runs Where

- One Bridge process serves exactly one manifest.
- That manifest resolves one team-owned secret/config scope.
- DB-backed tools resolve their `connection_ref` from `bridge.yaml` located next
  to the manifest (or in one of that directory's parents).
- You can start the host from any working directory; config resolution is
  anchored to the manifest location, not the shell cwd.

Example layout:

```text
deploy/
├── analytics.mcp.yaml
└── bridge.yaml
```

Example `bridge.yaml` for Postgres:

```yaml
version: "1"
name: analytics
providers:
  analytics:
    type: postgres
    uri: ${DATABASE_URL}
```

SQLite is also supported when the database file is available to the host:

```yaml
version: "1"
name: localdb
providers:
  localdb:
    type: sqlite
    uri: sqlite://./local.db?mode=ro
```

## Startup Checklist

Before starting `bridge mcp serve-http`, make sure:

- The manifest file exists and is valid `bridge.mcp/v1` YAML.
- `bridge.yaml` exists beside the manifest for DB-backed tools.
- Required environment variables are present:
  - `DATABASE_URL` (or whatever env var your DB connection uses in
    `bridge.yaml`)
  - the SQLite database file if the manifest uses a SQLite connection
  - any `runtime.base_url_env` or bearer-token env vars referenced by the
    manifest
- The bind address and externally reachable URL are set for your deployment.

Bridge fails fast at startup if any of those requirements are missing.

## Recommended Command

```bash
export DATABASE_URL=postgres://localhost:5432/analytics
export BRIDGE_MCP_BIND=0.0.0.0:8080
export BRIDGE_MCP_PUBLIC_URL=https://mcp.example.com/team-a

bridge mcp serve-http ./analytics.mcp.yaml
```

Useful settings:

- `--bind` / `BRIDGE_MCP_BIND`: local listen address
- `--public-url` / `BRIDGE_MCP_PUBLIC_URL`: external base URL Bridge advertises
  in health output and startup logs
- `--max-header-bytes` / `BRIDGE_MCP_MAX_HEADER_BYTES`: request header cap
- `--max-body-bytes` / `BRIDGE_MCP_MAX_BODY_BYTES`: request body cap
- `--read-timeout-secs` / `BRIDGE_MCP_READ_TIMEOUT_SECS`: wall-clock budget for
  reading one HTTP request
- `--request-timeout-secs` / `BRIDGE_MCP_REQUEST_TIMEOUT_SECS`: wall-clock
  budget for handling one request after it is read
- `--shutdown-grace-secs` / `BRIDGE_MCP_SHUTDOWN_GRACE_SECS`: drain window on
  shutdown before in-flight requests are aborted

## Network Surface

Bridge serves these endpoints:

- `/mcp`: Streamable HTTP MCP endpoint
- `/healthz`: liveness probe
- `/readyz`: readiness probe

`/healthz` returns 200 while the process is alive. `/readyz` returns 200 when
the host is ready to accept MCP traffic and 503 while it is shutting down.

## Reverse Proxy Expectations

- Terminate TLS at your ingress/proxy or run Bridge behind a platform-managed
  HTTPS load balancer.
- Point readiness checks at `/readyz`.
- Point liveness checks at `/healthz`.
- If Bridge is reachable at a public hostname or path prefix that differs from
  the bind address, set `--public-url` so logs and health output advertise the
  correct external URL.
- Configure inbound auth, TLS policy, and any rate limiting at the hosting
  layer. Those concerns are intentionally outside `bridge.mcp/v1`.

## Operational Behavior

- Structured logs are emitted as JSON lines on stderr.
- Request headers and bodies are capped before they reach MCP handling.
- Slow clients are cut off by the read timeout.
- Long-running request handling is cut off by the request timeout.
- Ctrl-C and `SIGTERM` trigger graceful shutdown with a bounded drain window.

## Not In Scope

This hosted mode does **not** do:

- Multi-tenant routing
- Manifest persistence or config storage services
- Per-tenant control-plane features
- Write-capable DB tooling
