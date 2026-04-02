# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 1.x     | Yes       |

## Reporting a Vulnerability

**Do not open a public GitHub issue for security vulnerabilities.**

Email **hello@bridge.ls** with:

- Description of the vulnerability
- Steps to reproduce
- Impact assessment (what an attacker could achieve)
- Any suggested fix (optional)

## Response Timeline

- **Acknowledgment:** Within 48 hours
- **Initial assessment:** Within 5 business days
- **Fix target:** Within 14 days for critical issues

## Scope

The following areas are in scope for security reports:

- Path traversal in the filesystem provider
- SQL injection in the Postgres provider
- Credential exposure in logs, output, or config handling
- Supply chain issues in dependencies or CI/CD
- Authentication or authorization bypasses (future Bridge Cloud features)

## Recognition

We appreciate responsible disclosure. With your permission, we will credit reporters in the CHANGELOG and release notes.
