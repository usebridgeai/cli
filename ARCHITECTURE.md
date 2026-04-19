# Architecture

## Overview

Bridge is a single Rust binary with a plugin-ready provider architecture. Agents
can invoke it directly via CLI, and Bridge can also expose generated MCP servers
over stdio. The design stays local-first: no always-on daemon is required.

```
Agent → bridge read <path> --from <provider> [--limit <n>]
            │
            ├── Load bridge.yaml
            ├── Expand ${ENV_VARS}
            ├── Resolve provider by name
            ├── Provider.connect()
            ├── Provider.read(path, options)
            │       ↓
            │   ContextValue { data, metadata }
            │       ↓
            └── JSON → stdout
```

## Project Structure

```
src/
├── main.rs              # Entry point, command dispatch
├── cli.rs               # Clap CLI definition + completions
├── config.rs            # bridge.yaml loading, env var expansion, URI parsing
├── context.rs           # ContextValue, ContextData, ContextEntry types
├── error.rs             # BridgeError enum, JSON error output, URI redaction
├── commands/
│   ├── init.rs          # bridge init
│   ├── connect.rs       # bridge connect <target> --as <name> [--type <provider>]
│   ├── generate.rs      # bridge generate mcp --from openapi|db ...
│   ├── mcp.rs           # bridge mcp serve <manifest>
│   ├── remove.rs        # bridge remove <name>
│   ├── ls.rs            # bridge ls --from <name>
│   ├── read.rs          # bridge read <path> --from <name>
│   ├── status.rs        # bridge status
│   └── update.rs        # bridge update [--check]
├── mcp/
│   ├── manifest.rs      # bridge.mcp/v1 types, YAML serde, validation
│   ├── openapi.rs       # OpenAPI 3.0 loader -> canonical operation model
│   ├── db_introspector.rs # Postgres schema metadata model + introspection
│   ├── db_tool_planner.rs # DB metadata -> deterministic MCP SQL tool plans
│   ├── tool_mapper.rs   # Canonical ops -> MCP tool definitions
│   ├── schema.rs        # Minimal JSON Schema validation for tool inputs
│   ├── executor.rs      # HTTP executor for generated tools
│   ├── sql_executor.rs  # Read-only SQL executor for manifest SQL plans
│   └── runtime.rs       # Stdio JSON-RPC 2.0 MCP server
└── provider/
    ├── mod.rs           # Provider trait, create_provider() registry
    ├── filesystem.rs    # Filesystem provider (path traversal protection)
    ├── postgres.rs      # Postgres provider (SQL injection protection)
    └── sqlite.rs        # SQLite provider

tests/
├── cli_test.rs          # CLI integration tests (all commands)
├── filesystem_test.rs   # Filesystem provider tests
├── mcp_db_test.rs       # DB-backed MCP integration tests (ignored, real Postgres)
├── mcp_generate_test.rs # MCP manifest generation tests
├── mcp_runtime_test.rs  # MCP stdio end-to-end runtime test
├── mcp_unit_test.rs     # MCP parser / mapper / schema unit coverage
├── postgres_test.rs     # Postgres tests (require Docker, run with --ignored)
└── sqlite_test.rs       # SQLite provider tests
```

## Provider Trait

All data sources implement the `Provider` trait:

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> ProviderCapabilities;
    async fn connect(&mut self, config: &ProviderConfig) -> Result<()>;
    async fn read(&self, path: &str, options: ReadOptions) -> Result<ContextValue>;
    async fn list(&self, prefix: Option<&str>) -> Result<Vec<ContextEntry>>;
    async fn health(&self) -> Result<ProviderStatus>;
}
```

New providers are added by implementing this trait and registering in `create_provider()`.

## Security

- **Path traversal:** Filesystem provider uses `canonicalize()` + `starts_with()` to prevent escaping the root directory.
- **SQL injection:** Postgres provider validates identifiers with `^[a-zA-Z_][a-zA-Z0-9_]*$` regex and uses parameterized queries for values.
- **Credential redaction:** URIs with passwords are redacted in all user-facing output via `redact_uri()`.
- **Supply chain:** GitHub Actions are pinned to SHA hashes.

## Adding a Provider

1. Create `src/provider/your_provider.rs`
2. Implement the `Provider` trait
3. Register in `create_provider()` in `src/provider/mod.rs`
4. Add integration tests in `tests/your_provider_test.rs`

## MCP subsystem (src/mcp/)

Bridge also ships an MCP generation + runtime subsystem. The key design constraint:
the manifest (`bridge.mcp/v1`) is the product boundary. Generation and runtime
must share no hidden logic — both operate through the same typed manifest so
Bridge Cloud can host the exact artifact later without reformatting.

```
OpenAPI spec ──► openapi.rs ──► CanonicalOp[] ──► tool_mapper.rs ──► Manifest
                                                                        │
                                                                        ▼
                                                       write: bridge.mcp/v1 YAML
                                                                        │
                                                                        ▼
                            manifest.rs (load + validate) ──► runtime.rs (stdio JSON-RPC 2.0)
                                                                        │
                                                                        ▼
                                                             executor.rs (HTTP call)
```

| File | Responsibility |
| ---- | -------------- |
| `src/mcp/manifest.rs` | `bridge.mcp/v1` types, YAML serde, validation |
| `src/mcp/openapi.rs`  | OpenAPI 3.0 loader → canonical operation model |
| `src/mcp/tool_mapper.rs` | Canonical ops → tool definitions (deterministic naming, MCP annotations) |
| `src/mcp/schema.rs`   | Minimal JSON Schema validation for tool inputs |
| `src/mcp/executor.rs` | HTTP executor — resolves env vars (base URL, bearer) at call time |
| `src/mcp/runtime.rs`  | Newline-delimited JSON-RPC 2.0 MCP server over stdio |

Runtime invariants:

- Logs go to **stderr** only — any stray stdout write would desync the stdio MCP client.
- Env-var resolution happens at `serve` start, so missing secrets fail fast rather than mid-tool-call.
- The first usable OpenAPI `servers` entry becomes `runtime.base_url`; `--base-url-env` remains an override path for per-environment base URLs.
- Local OpenAPI schema refs are inlined into generated manifest schemas so MCP clients and runtime validation do not depend on the original OpenAPI components section.
- Tool input is validated before the HTTP executor is invoked; validation failures surface as tool-level `isError: true`, not JSON-RPC errors.
- Response schemas are best-effort metadata. If a response schema is recursive or otherwise cannot be fully inlined, generation keeps the tool and omits `outputSchema` with a diagnostic instead of dropping the operation.
- Unsupported OpenAPI operations (POST/PUT/PATCH/DELETE in MVP) are reported in the `skipped` output; generation never crashes on them.

## DB-backed MCP flow

The DB generator follows the same manifest-first rule as OpenAPI generation. It does not embed DSNs or bypass the provider layer.

```
bridge generate mcp --from db
            │
            ├── commands/generate.rs
            ├── provider/postgres.rs (resolve named Bridge connection)
            ├── mcp/db_introspector.rs (schema-only metadata)
            ├── mcp/db_tool_planner.rs (deterministic tool planning)
            ├── mcp/manifest.rs (validate + serialize)
            ▼
      bridge.mcp/v1 YAML with connection_ref
            │
            ├── bridge mcp serve <manifest>
            ├── commands/mcp.rs (anchor config lookup to manifest dir)
            ├── mcp/runtime.rs (stdio JSON-RPC)
            └── mcp/sql_executor.rs (read-only parameterized SQL)
```

DB-specific invariants:

- The manifest stores `connection_ref`, not DSNs or secrets.
- Generation and runtime both resolve the database through Bridge's named provider config.
- Introspection is schema-level only; it never reads user data rows.
- Tool planning is deterministic for identical metadata.
- SQL execution is read-only, parameterized, and limited by allowlisted columns, row caps, and statement timeouts.
- `list_*` tools are always generated for supported tables and views; `get_*_by_*` tools are only generated when a deterministic single-column key exists.
- Runtime config discovery starts from the manifest directory, so generated client snippets remain portable across working directories.
