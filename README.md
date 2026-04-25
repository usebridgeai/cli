# Bridge CLI

<p align="center">
  <img src="assets/banner.png" alt="Bridge CLI" width="100%" />
</p>

<p align="center">
  <a href="https://github.com/usebridgeai/cli/actions/workflows/ci.yml"><img src="https://github.com/usebridgeai/cli/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0--only-blue.svg" alt="License: AGPL-3.0-only" /></a>
  <a href="https://github.com/usebridgeai/cli/releases/latest"><img src="https://img.shields.io/badge/version-1.0.0-green.svg" alt="Version" /></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey.svg" alt="Platform" />
  <a href="https://bridge.ls"><img src="https://img.shields.io/badge/website-bridge.ls-blueviolet.svg" alt="Website" /></a>
</p>

**Any storage. Any agent. One CLI**

Bridge gives AI agents a single interface to read structured context from any storage backend. One config file, one binary, JSON on stdout. The missing layer between your agent framework and your data.

## The Problem

AI agents need context from storage, but every backend speaks a different language. Today you write custom SDK calls for Postgres, different code for S3, different code again for your vector store. When you switch backends, you rewrite the integration.

Bridge is the [`rclone`](https://rclone.org/) for agent context. One interface, any storage, structured JSON that any framework can consume.

## Quick Start

The examples below assume the referenced directory, file, and database already exist.

```bash
# Initialize a project
bridge init

# Connect data sources
bridge connect file://./docs --as files
bridge connect sqlite://./local.db --as localdb

# List contents
bridge ls --from files
bridge ls --from localdb

# Read context
bridge read README.md --from files

# Check health
bridge status
```

`bridge connect` verifies the target by default. If you want to save a connection before the directory, database, or service is reachable, add `--no-verify`. Re-run with `--force` to replace an existing provider name.

Bridge can also generate and serve MCP servers from OpenAPI specs and existing SQL database connections. See the MCP sections below for end-to-end examples.

### What agents see

All output is JSON on stdout. Agents parse it directly.

`bridge read README.md --from files` returns:

```json
{
  "data": {
    "type": "text",
    "content": "# Hello\n\nFile contents here."
  },
  "metadata": {
    "source": "filesystem",
    "path": "README.md",
    "content_type": "text/markdown",
    "size": 28
  }
}
```

`bridge ls --from db` returns:

```json
[
  { "name": "users", "path": "users", "entry_type": "table" },
  { "name": "orders", "path": "orders", "entry_type": "table" }
]
```

Errors go to stderr as JSON with non-zero exit codes. Agents read stdout for data, stderr for errors.

## Install

**macOS / Linux:**

```bash
curl -fsSL https://bridge.ls/install | sh
```

**Windows (PowerShell):**

```powershell
irm https://bridge.ls/install | iex
```

**Homebrew (macOS):**

```bash
brew install usebridgeai/tap/bridge
```

**From source (requires Rust):**

```bash
cargo install --path .
```

## How It Works

Bridge is a single Rust binary. No server, no daemon, no token cost.

```
Agent → bridge read <path> --from <provider>
            │
            ├── Load bridge.yaml
            ├── Resolve provider by name
            ├── Provider.read(path)
            │       ↓
            │   ContextValue { data, metadata }
            │       ↓
            └── JSON → stdout
```

The agent calls Bridge like any CLI tool, gets JSON back, done. New providers are added by implementing a `Provider` trait, one file each.

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full design.

## Why Not Just...

**...write custom SDK calls?** One `bridge.yaml`, one `bridge read`, any source. No per-backend integration code. Add a new data source with `bridge connect`, not a new dependency.

**...use MCP servers?** MCP servers run inside the agent's token loop. Every tool call costs tokens. Bridge runs outside: one CLI call, structured JSON back, done. Cheaper, faster, and works with any agent framework, not just MCP hosts.

## Providers

| Provider   | URI                       | `ls` returns          | `read` returns                                |
| ---------- | ------------------------- | --------------------- | --------------------------------------------- |
| Filesystem | `file://./path`           | Files and directories | File contents (text, JSON, or base64)         |
| SQLite     | `sqlite://./local.db`     | Tables                | All rows (`users`) or single row (`users/42`) |
| Postgres   | `postgres://host:port/db` | Tables                | All rows (`users`) or single row (`users/42`) |

## Commands

| Command                            | Description                        |
| ---------------------------------- | ---------------------------------- |
| `bridge init`                      | Create a `bridge.yaml` config file |
| `bridge connect <target> --as <name> [--type <provider>] [--force] [--no-verify]` | Add and verify a data source connection |
| `bridge remove <name>`             | Remove a data source               |
| `bridge status`                    | Show health of all connections     |
| `bridge ls --from <name>`          | List contents (files, tables)      |
| `bridge read <path> --from <name>` | Read context from a source         |
| `bridge generate mcp --from openapi <spec> --name <n> --out <file>` | Generate a bridge.mcp/v1 manifest from an OpenAPI spec |
| `bridge generate mcp --from db --connection <name> --schema <schema> --name <n> --out <file>` | Generate a bridge.mcp/v1 manifest from a Postgres or SQLite connection |
| `bridge mcp serve <manifest>`      | Serve an MCP manifest as a live MCP server over stdio |
| `bridge mcp serve-http <manifest>` | Serve an MCP manifest remotely over HTTP |

## MCP servers from OpenAPI

Bridge can turn any OpenAPI spec into a live MCP server — no hand-written MCP code required.

### Happy path

```bash
# 1. Generate a manifest from an OpenAPI spec.
bridge generate mcp \
  --from openapi ./openapi.yaml \
  --name petstore \
  --base-url-env PETSTORE_BASE_URL \
  --out ./petstore.mcp.yaml

# 2. Serve it as an MCP server over stdio.
export PETSTORE_BASE_URL=https://petstore.example.com
bridge mcp serve ./petstore.mcp.yaml
```

Generation prints a ready-to-paste MCP client config snippet. Pipe stdin/stdout into any MCP-compatible client (Claude Desktop, Cursor, etc.).

If the OpenAPI spec includes a usable `servers` entry, `--base-url-env` is optional. In that case Bridge stores the first usable server URL in the manifest as a fallback, and a `--base-url-env` value becomes an environment-specific override.

### With bearer auth

```bash
bridge generate mcp \
  --from openapi ./openapi.yaml \
  --name github \
  --base-url-env GITHUB_API_BASE_URL \
  --bearer-env GITHUB_TOKEN \
  --out ./github.mcp.yaml

export GITHUB_API_BASE_URL=https://api.github.com
export GITHUB_TOKEN=ghp_xxx
bridge mcp serve ./github.mcp.yaml
```

Secrets are **never** written to the manifest — only the env var name is stored. `bridge mcp serve` fails fast with a clear error if a required env var is missing.

Bridge inlines local OpenAPI schema refs into generated tool input schemas, so MCP clients and runtime validation do not depend on the original OpenAPI components section. Response schemas are best-effort metadata: recursive response models may omit `outputSchema`, but the tool is still generated and callable.

MVP scope: OpenAPI 3.0 input, GET operations only, stdio transport, bearer-token auth. POST/PUT/PATCH/DELETE are reported as skipped and left as additive follow-on work.

## MCP servers from SQL databases

Bridge can also turn an existing Bridge Postgres or SQLite connection into a read-only MCP server. The manifest stays secret-free and reuses the named connection from `bridge.yaml` at runtime.

### Happy path

```bash
# 1. Connect a Postgres database once.
bridge connect postgres://localhost:5432/analytics --as analytics

# 2. Generate a manifest from the selected schema.
bridge generate mcp \
  --from db \
  --connection analytics \
  --schema public \
  --name analytics \
  --out ./analytics.mcp.yaml

# 3. Serve it as an MCP server over stdio.
bridge mcp serve ./analytics.mcp.yaml
```

SQLite works the same way:

```bash
bridge connect sqlite://./local.db --as localdb

bridge generate mcp \
  --from db \
  --connection localdb \
  --name localdb \
  --out ./localdb.mcp.yaml

bridge mcp serve ./localdb.mcp.yaml
```

Generation prints a ready-to-paste MCP client config snippet. The snippet uses an absolute manifest path so it can be registered in MCP clients directly.

Generated DB tools are intentionally conservative:

- `list_*` tools support safe equality filters, pagination, and allowlisted sorting.
- `get_*_by_*` tools are generated only when Bridge can prove a deterministic single-column lookup key.
- The runtime executes generated, parameterized `SELECT` plans only, with server-side row caps. Postgres also applies statement timeouts and read-only transactions.

Manifests never contain DSNs or secrets. They store a `connection_ref`, and `bridge mcp serve` resolves `bridge.yaml` relative to the manifest location so the same generated artifact can be launched from another working directory.

MVP scope: Postgres and SQLite, one selected schema/database namespace, tables and views, `list_*` plus deterministic `get_*_by_*`, stdio and Streamable HTTP transports, read-only execution. Raw SQL, writes, and multi-table query planning are intentionally out of scope.

## Hosted MCP over HTTP

Bridge can also host a single `bridge.mcp/v1` manifest over Streamable HTTP for
team use:

```bash
export DATABASE_URL=postgres://localhost:5432/analytics
export BRIDGE_MCP_BIND=0.0.0.0:8080
export BRIDGE_MCP_PUBLIC_URL=https://mcp.example.com/team-a

bridge mcp serve-http ./analytics.mcp.yaml
```

For hosted SQLite, make sure the database file is mounted into the process/container and prefer a read-only URI such as `sqlite://./local.db?mode=ro`.

Hosted mode is intentionally scoped to **one manifest, one database, one
team**. The manifest remains the execution artifact; deployment details stay in
flags and environment variables.

What hosted mode provides:

- `/mcp` for the MCP Streamable HTTP endpoint
- `/healthz` and `/readyz` for load balancers and orchestration
- Graceful shutdown on `SIGTERM`/Ctrl-C with a bounded drain window
- Request header/body size caps and read/handling timeouts
- Structured JSON logs on stderr
- Fast startup failures when the manifest is invalid, `bridge.yaml` is missing,
  or required env vars for DB/OpenAPI execution are not set

Key `serve-http` settings:

- `--bind` or `BRIDGE_MCP_BIND`
- `--public-url` or `BRIDGE_MCP_PUBLIC_URL`
- `--max-header-bytes` / `BRIDGE_MCP_MAX_HEADER_BYTES`
- `--max-body-bytes` / `BRIDGE_MCP_MAX_BODY_BYTES`
- `--read-timeout-secs` / `BRIDGE_MCP_READ_TIMEOUT_SECS`
- `--request-timeout-secs` / `BRIDGE_MCP_REQUEST_TIMEOUT_SECS`
- `--shutdown-grace-secs` / `BRIDGE_MCP_SHUTDOWN_GRACE_SECS`

For the deployment model, reverse-proxy expectations, and startup checklist,
see [docs/single-manifest-hosting.md](docs/single-manifest-hosting.md).

## Configuration

Bridge uses a `bridge.yaml` file in the project root:

```yaml
version: "1"
name: my-project
providers:
  files:
    type: filesystem
    uri: file://./docs
  localdb:
    type: sqlite
    uri: sqlite://./local.db
  db:
    type: postgres
    uri: ${DATABASE_URL}
```

Environment variables are supported with `${VAR_NAME}` syntax. For shared or production setups, prefer `${VAR_NAME}` references so the real secret stays outside `bridge.yaml`.

Bridge supports two setup patterns:

- For quick local setup, pass a reachable literal URI such as `file://./docs`, `sqlite://./local.db`, or `postgres://localhost:5432/mydb`.
- For safer shared or production setups, pass a bare environment variable name such as `DATABASE_URL` together with `--type postgres`. Bridge writes `uri: ${DATABASE_URL}` into `bridge.yaml` and resolves the real value at runtime.

`bridge connect` verifies new connections by default. If the target is not reachable yet, pass `--no-verify` to save the config anyway. If you need to replace an existing provider with the same name, re-run with `--force`.

SQLite is also supported through `sqlite://./local.db?mode=rwc` when you explicitly want SQLite to create the file on first use.

Bridge reads environment variables from the process environment when commands run. It does not automatically load a `.env` file for you.

## Security

- **Path traversal protection:** Filesystem provider uses `canonicalize()` + `starts_with()` to block directory escape
- **SQL injection protection:** SQLite and Postgres providers validate identifiers with strict regex and use parameterized row reads
- **Credential redaction:** URIs with passwords are redacted in all user-facing output
- **Supply chain:** GitHub Actions pinned to SHA hashes
- **Testing:** Integration coverage across the CLI, filesystem, SQLite, and Postgres providers (including Docker-based Postgres tests in CI)

## Roadmap

- [x] Filesystem provider
- [x] Postgres provider
- [x] Cross-platform binaries (macOS, Linux, Windows)
- [x] Shell completions (bash, zsh, fish, PowerShell)
- [x] Structured JSON output with metadata
- [x] Environment variable expansion in config
- [x] SQLite provider
- [ ] Write support (`bridge write`)
- [ ] S3 provider
- [ ] Vector store providers (Qdrant, Pinecone)

## Bridge Cloud

Bridge CLI is local-first and always will be. But what if your agents could share context with other agents, across teams and organizations?

**Bridge Cloud** will let you publish context slices from your local Bridge and grant permissioned access to external agents. Your data stays in your storage backends. Bridge Cloud handles discovery, auth, and access control.

Interested? Star this repo and visit [bridge.ls](https://bridge.ls) to get notified.

## Contributing

Bridge CLI is open source under the [AGPL-3.0-only](LICENSE) license. We welcome contributions: new providers, bug fixes, documentation improvements. All contributors must sign our [Contributor License Agreement](CLA.md) before their first PR is merged.

See [ARCHITECTURE.md](ARCHITECTURE.md) for how the codebase is structured and how to add a new provider.

## Testing

Run the default test suite with:

```bash
cargo test
```

Run the Postgres integration tests with:

```bash
DATABASE_URL=your-real-postgres-url cargo test --test postgres_test -- --ignored --nocapture
```

The Postgres tests require `DATABASE_URL` to point to a reachable Postgres instance. They create and reset `bridge_test_*` tables as part of setup, so use a dedicated local or test database.

## Shell Completions

Run `bridge completions <shell>` for bash, zsh, fish, or PowerShell. The install script sets up completions automatically.

## Uninstall

```bash
# macOS / Linux
rm -rf ~/.bridge

# Windows (PowerShell)
Remove-Item -Recurse $env:USERPROFILE\.bridge
```

Then remove the PATH entry from your shell profile.

## Licensing

Bridge CLI is licensed under the [GNU Affero General Public License v3.0 only](LICENSE) (SPDX: `AGPL-3.0-only`).

- **Open source use:** The full AGPL-3.0 applies. If you modify Bridge CLI and provide it as a network service, you must make your source code available under the same license.
- **Commercial licensing:** If AGPL does not work for your organization, commercial licenses are available. Contact hello@bridge.ls.
- **Contributor License Agreement:** All contributors must sign our [CLA](CLA.md) before their first PR is merged. This enables dual-licensing while contributors retain full copyright of their work.
- **Name usage:** See [TRADEMARK.md](TRADEMARK.md) for guidelines on using the Bridge CLI name and logo.

## License

[AGPL-3.0-only](LICENSE) -- Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
