# Changelog

All notable changes to Bridge CLI will be documented in this file.

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
