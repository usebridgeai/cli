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

**One CLI. Any storage. Every agent.**

Bridge gives AI agents a single interface to read structured context from any storage backend. One config file, one binary, JSON on stdout. The missing layer between your agent framework and your data.

## The Problem

AI agents need context from storage, but every backend speaks a different language. Today you write custom SDK calls for Postgres, different code for S3, different code again for your vector store. When you switch backends, you rewrite the integration.

Bridge is the [`rclone`](https://rclone.org/) for agent context. One interface, any storage, structured JSON that any framework can consume.

## Quick Start

```bash
# Initialize a project
bridge init

# Connect data sources
bridge connect file://./docs --as files
bridge connect postgres://localhost:5432/mydb --as db

# List contents
bridge ls --from files
bridge ls --from db

# Read context
bridge read README.md --from files
bridge read users --from db
bridge read users/42 --from db

# Check health
bridge status
```

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
curl -fsSL https://raw.githubusercontent.com/usebridgeai/cli/main/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/usebridgeai/cli/main/install.ps1 | iex
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
| Postgres   | `postgres://host:port/db` | Tables                | All rows (`users`) or single row (`users/42`) |

## Commands

| Command                            | Description                        |
| ---------------------------------- | ---------------------------------- |
| `bridge init`                      | Create a `bridge.yaml` config file |
| `bridge connect <uri> --as <name>` | Add a data source connection       |
| `bridge remove <name>`             | Remove a data source               |
| `bridge status`                    | Show health of all connections     |
| `bridge ls --from <name>`          | List contents (files, tables)      |
| `bridge read <path> --from <name>` | Read context from a source         |

## Configuration

Bridge uses a `bridge.yaml` file in the project root:

```yaml
version: "1"
name: my-project
providers:
  files:
    type: filesystem
    uri: file://./docs
  db:
    type: postgres
    uri: ${DATABASE_URL}
```

Environment variables are supported with `${VAR_NAME}` syntax. Keep secrets out of the config file.

## Security

- **Path traversal protection:** Filesystem provider uses `canonicalize()` + `starts_with()` to block directory escape
- **SQL injection protection:** Postgres provider validates identifiers with strict regex and uses parameterized queries
- **Credential redaction:** URIs with passwords are redacted in all user-facing output
- **Supply chain:** GitHub Actions pinned to SHA hashes
- **Testing:** 56 tests across CLI integration, filesystem, and Postgres (including Docker-based Postgres tests in CI)

## Roadmap

- [x] Filesystem provider
- [x] Postgres provider
- [x] Cross-platform binaries (macOS, Linux, Windows)
- [x] Shell completions (bash, zsh, fish, PowerShell)
- [x] Structured JSON output with metadata
- [x] Environment variable expansion in config
- [ ] Write support (`bridge write`)
- [ ] SQLite provider
- [ ] S3 provider
- [ ] Vector store providers (Qdrant, Pinecone)

## Bridge Cloud

Bridge CLI is local-first and always will be. But what if your agents could share context with other agents, across teams and organizations?

**Bridge Cloud** will let you publish context slices from your local Bridge and grant permissioned access to external agents. Your data stays in your storage backends. Bridge Cloud handles discovery, auth, and access control.

Interested? Star this repo and visit [bridge.ls](https://bridge.ls) to get notified.

## Contributing

Bridge CLI is open source under the [AGPL-3.0-only](LICENSE) license. We welcome contributions: new providers, bug fixes, documentation improvements. All contributors must sign our [Contributor License Agreement](CLA.md) before their first PR is merged.

See [ARCHITECTURE.md](ARCHITECTURE.md) for how the codebase is structured and how to add a new provider.

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
