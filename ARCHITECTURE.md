# Architecture

## Overview

Bridge is a single Rust binary with a plugin-ready provider architecture. Agents invoke it directly via CLI — no server, no daemon, no MCP.

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
│   ├── remove.rs        # bridge remove <name>
│   ├── ls.rs            # bridge ls --from <name>
│   ├── read.rs          # bridge read <path> --from <name>
│   └── status.rs        # bridge status
└── provider/
    ├── mod.rs           # Provider trait, create_provider() registry
    ├── filesystem.rs    # Filesystem provider (path traversal protection)
    └── postgres.rs      # Postgres provider (SQL injection protection)

tests/
├── cli_test.rs          # CLI integration tests (all commands)
├── filesystem_test.rs   # Filesystem provider tests
└── postgres_test.rs     # Postgres tests (require Docker, run with --ignored)
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
