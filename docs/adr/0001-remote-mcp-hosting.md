# ADR 0001: Remote MCP Hosting Boundaries for `bridge.mcp/v1`

- Status: Accepted
- Date: 2026-04-20
- Deciders: Bridge core team

## Context

Bridge generates and serves MCP servers locally today. The generated artifact is
the `bridge.mcp/v1` manifest, consumed by `bridge mcp serve` over stdio JSON-RPC.
We are preparing to host these manifests remotely (Bridge Cloud) so agents can
reach them over the network without running the CLI locally.

Before writing hosting code, we need to lock down what belongs inside the
manifest versus what belongs in hosting/deployment configuration. Without this
boundary, the manifest will accumulate environment-specific fields and stop
being portable across local, CI, and cloud execution.

## Decision

### 1. `bridge.mcp/v1` remains the execution artifact

The manifest is the single source of truth for *what* an MCP server exposes:
tool definitions, input/output schemas, HTTP operation bindings, SQL plans,
connection refs, and validation rules. Both local `bridge mcp serve` and the
remote host execute the same manifest bytes. Generation and runtime must share
no hidden logic — anything the remote host needs to run the server correctly
must either be in the manifest or resolvable from a named connection the host
already knows about.

The manifest stays transport-agnostic. It describes tools, not how they are
served.

### 2. Hosting, transport, and deployment stay out of the manifest

The following belong to hosting/deployment config (outside `bridge.mcp/v1`) and
will never be added to the manifest schema:

- Transport selection (stdio, HTTP, SSE, WebSocket)
- Bind address, port, public hostname
- TLS material, certificate sources, cipher policy
- Authentication and authorization for inbound MCP clients (API keys, OAuth,
  mTLS, tenant identity)
- Rate limits, quotas, concurrency caps, request timeouts enforced by the host
- Routing, load balancing, path prefixes, ingress rules
- Secret-manager wiring — how `${ENV_VAR}` references in the manifest are
  resolved at runtime (AWS Secrets Manager, GCP Secret Manager, Vault, env
  files). The manifest keeps referencing env-var names; the host decides how
  those names are populated.
- Observability backends (log/metric/trace sinks), audit storage
- Autoscaling, replica counts, scheduling constraints
- Tenant identity and tenant-to-manifest mapping

The manifest may continue to declare *logical* constraints it needs enforced
(e.g. read-only SQL, row caps, allowlisted columns, statement timeouts) because
those are properties of the tool contract, not of the deployment.

### 3. First remote release is single-manifest, single-tenant

The first hosted release ships with this boundary:

- One deployed host runs exactly one `bridge.mcp/v1` manifest.
- One tenant owns that host. No cross-tenant request routing inside a single
  host process.
- Auth is a single inbound credential scheme configured at the host, not per
  tool and not per caller identity inside the manifest.
- Secrets referenced by the manifest are resolved from one secret scope owned
  by that tenant.
- Upgrades replace the manifest atomically; there is no partial-manifest
  rollout in v1.

This keeps the blast radius small and lets us validate that the
manifest-as-artifact contract holds end-to-end before we add multi-tenant
concerns.

### 4. Multi-tenant hosting is a later phase

Multi-tenant hosting — one host process serving many manifests, per-tenant
auth, per-tenant secret scopes, per-tool authorization policy, tenant-aware
rate limits — is explicitly out of scope for the first release. When we take
it on, the decision record for that phase must re-examine whether any of it
leaks into the manifest; the default answer is still no.

## Explicit in/out of manifest

| Concern                               | In `bridge.mcp/v1`? |
| ------------------------------------- | ------------------- |
| Tool names, descriptions, annotations | Yes                 |
| Tool input/output JSON schemas        | Yes                 |
| HTTP operation binding (method, path) | Yes                 |
| SQL plan (read-only, parameterized)   | Yes                 |
| `connection_ref` (named connection)   | Yes                 |
| Env-var *names* for base URL / auth   | Yes                 |
| Row caps, column allowlists, timeouts | Yes (tool contract) |
| Transport (stdio vs HTTP vs SSE)      | No                  |
| Bind address / port / hostname        | No                  |
| TLS certificates and policy           | No                  |
| Inbound auth scheme and credentials   | No                  |
| Secret-manager backend + wiring       | No                  |
| Rate limits, quotas, concurrency      | No                  |
| Routing, ingress, load balancing      | No                  |
| Tenant identity                       | No                  |
| Observability sinks                   | No                  |

## First hosted release boundary

- Scope: single `bridge.mcp/v1` manifest per host, single tenant.
- Transport: one inbound transport chosen at host config time, not in manifest.
- Auth: one inbound credential scheme configured at the host.
- Secrets: manifest keeps env-var references; host resolves them from one
  tenant-owned secret scope.
- Upgrades: atomic manifest replacement; no partial rollout, no per-tool
  versioning across tenants.
- Not in this release: multi-manifest hosts, multi-tenant routing, per-caller
  authorization, per-tool auth policy, tenant-aware quotas.

## Consequences

- The manifest stays portable: the same bytes run under `bridge mcp serve`
  locally and under the remote host.
- Hosting code owns a real surface (transport, TLS, auth, secrets, limits).
  That surface will need its own config schema, separate from `bridge.mcp/v1`.
- Features that feel manifest-shaped (per-tenant auth, per-tool rate limits)
  must resist the pull until multi-tenant hosting is designed end-to-end.
- If a future need genuinely belongs in the manifest, extending it requires a
  new ADR that revisits this boundary explicitly.

## Out of scope

- Runtime code changes. This ADR documents the boundary only; implementation
  of the remote host lands in follow-up issues.
