# Changelog

All notable changes to Bridge CLI will be documented in this file.

## [Unreleased]

### Features

- **MCP generation from OpenAPI.** `bridge generate mcp --from openapi <spec>` produces a versioned `bridge.mcp/v1` manifest from any OpenAPI 3.0 spec.
- **MCP runtime.** `bridge mcp serve <manifest>` runs the manifest as a live MCP server over stdio (JSON-RPC 2.0), implementing `initialize`, `tools/list`, and `tools/call` with MCP structuredContent + text fallback.
- **Manifest-first architecture.** Generation and runtime share a single typed contract so Bridge Cloud can host the same manifest later without format changes.
- **Environment-driven auth.** Base URL and bearer tokens are env-var references in the manifest; secrets are never persisted. When an OpenAPI spec includes `servers`, Bridge stores the first usable server URL as a manifest fallback.
- **Schema-aware generation.** Local OpenAPI schema refs are inlined into generated input schemas, and recursive response schemas fall back gracefully by omitting `outputSchema` instead of dropping the tool.

## [1.0.0] - 2026-03-30

Initial public release.

### Features

- **Core CLI.** `bridge init`, `connect`, `remove`, `status`, `ls`, `read` commands.
- **Filesystem provider.** Read files and directories with path traversal protection.
- **Postgres provider.** Read tables and rows by primary key with SQL injection protection.
- **bridge.yaml.** Project-level configuration with `${ENV_VAR}` support for secrets.
- **JSON output.** All commands output structured JSON on stdout for agent consumption.
- **Install scripts.** `curl | sh` installer for macOS/Linux and PowerShell installer for Windows. Downloads pre-built binary, verifies SHA-256 checksum, adds to PATH, and installs shell completions.
- **Shell completions.** `bridge completions <shell>` for bash, zsh, fish, and PowerShell.
- **Cross-platform.** Pre-built binaries for macOS (x86_64, ARM64), Linux (x86_64), and Windows (x86_64).
- **Checksum verification.** Every release includes `checksums.txt` with SHA-256 hashes.
- **CI/CD.** GitHub Actions for testing (with Postgres service) and releasing binaries.
- **Supply chain security.** GitHub Actions pinned to SHA hashes.

### Licensing

- Licensed under AGPL-3.0-only with AGPL Section 7(e) name protection for "Bridge CLI."
- Dual-licensing model: open source AGPL-3.0-only, commercial licenses available.
- CLA with patent grant for contributors.
