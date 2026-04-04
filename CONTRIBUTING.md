# Contributing to Bridge

We welcome contributions: new providers, bug fixes, documentation improvements.

## Getting Started

1. Fork the repo
2. Create a feature branch: `git checkout -b my-feature`
3. Make your changes
4. Run tests: `cargo test`
5. Open a PR against `main`

See [ARCHITECTURE.md](ARCHITECTURE.md) for how the codebase is structured and how to add a new provider.

## Contributor License Agreement (CLA)

Before your first PR is merged, you must sign our [Contributor License Agreement](CLA.md). This grants Gabriel Beslic and Tomer Liran the right to distribute your contributions under both the AGPL-3.0-only open source license and a commercial license, while you retain full copyright of your work. The CLA also includes a patent grant to protect the project and its users.

This is standard practice for dual-licensed open source projects (Grafana, MinIO, etc.) and is necessary so we can offer commercial licenses to organizations that need them.

To sign: add your name and GitHub handle to the table in [CLA.md](CLA.md) as part of your first PR.

## License

By contributing to Bridge CLI, you agree that your contributions will be licensed under the [GNU Affero General Public License v3.0 only](LICENSE) and that you grant Gabriel Beslic and Tomer Liran the rights described in the [CLA](CLA.md).

## Guidelines

- Follow existing code patterns and style
- Add tests for new functionality
- Keep PRs focused on a single change
- Update documentation if your change affects user-facing behavior
