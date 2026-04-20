// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Streamable-HTTP adapter for the MCP service core. Hosts a single manifest on
// a TCP socket and speaks MCP's Streamable HTTP transport: a single endpoint
// that accepts JSON-RPC over HTTP POST and returns JSON responses. SSE fan-out
// and GET streams are deferred — see ENG-51.
//
// The HTTP wire format (request parsing, response writing, size/time budgets)
// lives in `wire`. This module is the MCP side of the transport: it assembles
// the service, enforces Origin policy, routes to the MCP endpoint, and
// dispatches JSON-RPC into `McpService`. Keeping the split means adding
// session-id handling, Accept negotiation, and SSE framing later only touches
// the wire module and one or two functions here.
//
// Hosted operation is single-manifest by design; multi-tenant routing lives in
// a separate ticket.

mod wire;

use crate::error::{BridgeError, Result};
use crate::mcp::manifest::Manifest;
use crate::mcp::runtime::build_local_executors;
use crate::mcp::service::{ExecutorBundle, McpService};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use wire::{ReadLimits, Request, Response, WireError};

/// The single MCP endpoint path. Fixed here so clients can be configured
/// without reading config — matches the Streamable HTTP convention.
const MCP_PATH: &str = "/mcp";

/// Default read budget: 32 KiB headers, 1 MiB body, 15 s wall clock. JSON-RPC
/// payloads are small and almost always delivered in a single TCP segment, so
/// a short timeout is safe and cuts off Slowloris-style stalls cleanly.
const READ_LIMITS: ReadLimits = ReadLimits {
    max_header_bytes: 32 * 1024,
    max_body_bytes: 1024 * 1024,
    timeout: Duration::from_secs(15),
};

/// Serve `manifest` over MCP Streamable HTTP on `bind`. Binds the socket and
/// accepts connections until the future is cancelled.
pub async fn serve(
    manifest: Manifest,
    bind: SocketAddr,
    timeout_secs: u64,
    config_dir: &Path,
    origin_policy: OriginPolicy,
) -> Result<()> {
    let executors = build_local_executors(&manifest, config_dir, timeout_secs)?;
    serve_with_executors(manifest, executors, bind, origin_policy).await
}

/// Lower-level entry that takes an already-built `ExecutorBundle`. Used by
/// tests (and by any future host that wants to inject its own executors).
pub async fn serve_with_executors(
    manifest: Manifest,
    executors: ExecutorBundle,
    bind: SocketAddr,
    origin_policy: OriginPolicy,
) -> Result<()> {
    let service = Arc::new(McpService::new(manifest, executors)?);
    let origin_policy = Arc::new(origin_policy);

    let listener = TcpListener::bind(bind)
        .await
        .map_err(|e| BridgeError::McpRuntime(format!("bind {bind} failed: {e}")))?;
    let local = listener
        .local_addr()
        .map_err(|e| BridgeError::McpRuntime(format!("local_addr failed: {e}")))?;

    eprintln!(
        "bridge mcp: serving '{}' with {} tool(s) over HTTP at http://{}{}",
        service.manifest().name,
        service.manifest().tools.len(),
        local,
        MCP_PATH
    );

    loop {
        let (stream, _peer) = listener
            .accept()
            .await
            .map_err(|e| BridgeError::McpRuntime(format!("accept failed: {e}")))?;
        let service = service.clone();
        let policy = origin_policy.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, service, policy).await {
                eprintln!("bridge mcp http: connection error: {e}");
            }
        });
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    service: Arc<McpService>,
    policy: Arc<OriginPolicy>,
) -> Result<()> {
    let req = match wire::read_request(&mut stream, &READ_LIMITS).await {
        Ok(r) => r,
        Err(WireError::Status(code, msg)) => {
            return write(&mut stream, Response::text(code, &msg)).await;
        }
        Err(WireError::Io(e)) => return Err(BridgeError::McpRuntime(format!("read: {e}"))),
    };

    // Origin check runs before anything touches the service so a rejected
    // browser request can't reach `initialize` and fingerprint the server.
    if let Some(origin) = req.headers.get("origin") {
        if !policy.is_allowed(origin) {
            return write(&mut stream, Response::text(403, "origin not allowed")).await;
        }
    }

    let response = route(&service, &req).await;
    write(&mut stream, response).await
}

async fn write(stream: &mut TcpStream, resp: Response) -> Result<()> {
    wire::write_response(stream, &resp)
        .await
        .map_err(|e| BridgeError::McpRuntime(format!("write: {e}")))
}

async fn route(service: &McpService, req: &Request) -> Response {
    // Strip a query string if present. Session-id / stream-id handling is
    // deferred until we implement server-initiated streams.
    let path = req.path.split('?').next().unwrap_or(&req.path);
    if path != MCP_PATH {
        return Response::text(404, "not found");
    }

    match req.method.as_str() {
        "POST" => handle_post(service, &req.body).await,
        // GET on the MCP endpoint is reserved for SSE streams; we don't open
        // one yet, so signal "no stream available" rather than 405.
        "GET" => Response::empty(405),
        "OPTIONS" => Response::empty(204),
        _ => Response::empty(405),
    }
}

async fn handle_post(service: &McpService, body: &[u8]) -> Response {
    let value: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            return Response::json(
                400,
                json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("parse error: {e}") }
                }),
            );
        }
    };

    // Streamable HTTP allows batching: a JSON array of JSON-RPC messages in a
    // single POST. Dispatch each and return the array of responses (dropping
    // `None` entries from notifications). An empty result set becomes 202
    // Accepted with no body, matching the spec.
    match value {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                if let Some(resp) = service.handle_jsonrpc(item).await {
                    out.push(resp);
                }
            }
            if out.is_empty() {
                Response::empty(202)
            } else {
                Response::json(200, Value::Array(out))
            }
        }
        other => match service.handle_jsonrpc(other).await {
            Some(resp) => Response::json(200, resp),
            None => Response::empty(202),
        },
    }
}

/// Policy for the `Origin` request header. The Streamable HTTP spec mandates
/// Origin validation on every request to prevent DNS rebinding: an attacker's
/// page at evil.example can rebind its own hostname to 127.0.0.1 and then fire
/// cross-origin fetches at a local MCP server. Without this check a browser
/// tab on the operator's workstation is a viable entry point.
///
/// Requests with no `Origin` header are allowed — non-browser MCP clients
/// (Claude Desktop, SDK-based agents, curl) don't send one, and CSRF/DNS
/// rebinding both require a browser context that does.
#[derive(Clone, Debug)]
pub struct OriginPolicy {
    extra: Vec<String>,
}

impl OriginPolicy {
    /// Default policy: accept only `http(s)://localhost` and
    /// `http(s)://127.0.0.1` (any port) plus anything the operator passes via
    /// `--allow-origin`. This is safe for the default `127.0.0.1:8080` bind
    /// and refuses browser traffic from arbitrary sites when the operator
    /// exposes the port on `0.0.0.0`.
    ///
    /// Allowlist entries are normalized at construction (see
    /// `normalize_origin`) so small operator variations — trailing slashes,
    /// default port numbers, uppercase hostnames — match the browser-sent
    /// Origin, which is always emitted in canonical form.
    pub fn new(extra: Vec<String>) -> Self {
        let extra = extra
            .into_iter()
            .filter_map(|o| normalize_origin(&o))
            .collect();
        Self { extra }
    }

    fn is_allowed(&self, origin: &str) -> bool {
        let Some(normalized) = normalize_origin(origin) else {
            return false;
        };
        if self.extra.iter().any(|o| o == &normalized) {
            return true;
        }
        is_loopback_origin(&normalized)
    }
}

impl Default for OriginPolicy {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

/// Reduce an Origin header value to canonical `scheme://host[:port]` form so
/// string comparison doesn't depend on operator-entered quirks. Returns `None`
/// for values that aren't `http`/`https` origins — those can never be allowed.
///
/// Normalizations applied:
/// - Scheme and host are lowercased.
/// - Any path (including a lone trailing slash) is dropped. An Origin per
///   RFC 6454 has no path; tolerating `http://x/` avoids silent mismatches.
/// - Default ports are dropped (`:80` on http, `:443` on https).
/// - IPv6 literals keep their brackets.
fn normalize_origin(raw: &str) -> Option<String> {
    let raw = raw.trim();
    // Scheme is ASCII-only, so a case-insensitive prefix check is safe on raw
    // bytes; everything after the `://` delimiter stays as-is.
    let (scheme, rest) = if raw.len() >= 7 && raw[..7].eq_ignore_ascii_case("http://") {
        ("http", &raw[7..])
    } else if raw.len() >= 8 && raw[..8].eq_ignore_ascii_case("https://") {
        ("https", &raw[8..])
    } else {
        return None;
    };

    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() {
        return None;
    }

    let (host_raw, port) = if let Some(stripped) = authority.strip_prefix('[') {
        let (inner, tail) = stripped.split_once(']')?;
        let port = tail.strip_prefix(':');
        (format!("[{inner}]"), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (authority.to_string(), None),
        }
    };

    let host = host_raw.to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }

    let default_port = match scheme {
        "http" => "80",
        "https" => "443",
        _ => unreachable!(),
    };

    Some(match port {
        None => format!("{scheme}://{host}"),
        Some(p) if p == default_port || p.is_empty() => format!("{scheme}://{host}"),
        Some(p) => {
            if !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            format!("{scheme}://{host}:{p}")
        }
    })
}

fn is_loopback_origin(normalized: &str) -> bool {
    let rest = normalized
        .strip_prefix("http://")
        .or_else(|| normalized.strip_prefix("https://"));
    let Some(authority) = rest else { return false };
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        match stripped.split_once(']') {
            Some((inner, _)) => inner,
            None => return false,
        }
    } else {
        authority.split(':').next().unwrap_or("")
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_allows_loopback_origins() {
        let p = OriginPolicy::default();
        assert!(p.is_allowed("http://localhost"));
        assert!(p.is_allowed("http://localhost:8080"));
        assert!(p.is_allowed("http://127.0.0.1:3000"));
        assert!(p.is_allowed("https://localhost:8080"));
        assert!(p.is_allowed("http://[::1]:8080"));
    }

    #[test]
    fn default_policy_rejects_arbitrary_and_rebinding_origins() {
        let p = OriginPolicy::default();
        assert!(!p.is_allowed("http://evil.example"));
        assert!(!p.is_allowed("https://evil.example:8080"));
        assert!(!p.is_allowed("http://localhost.evil.example"));
        assert!(!p.is_allowed("http://127.0.0.1.evil.example"));
        assert!(!p.is_allowed("file://"));
        assert!(!p.is_allowed("null"));
    }

    #[test]
    fn explicit_allowlist_extends_defaults() {
        let p = OriginPolicy::new(vec!["https://studio.bridge.ls".into()]);
        assert!(p.is_allowed("https://studio.bridge.ls"));
        assert!(p.is_allowed("http://127.0.0.1:8080"));
        assert!(!p.is_allowed("https://other.example"));
    }

    #[test]
    fn normalize_strips_default_ports() {
        assert_eq!(
            normalize_origin("http://example.com:80").as_deref(),
            Some("http://example.com")
        );
        assert_eq!(
            normalize_origin("https://example.com:443").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            normalize_origin("http://example.com:8080").as_deref(),
            Some("http://example.com:8080")
        );
    }

    #[test]
    fn normalize_strips_trailing_slash_and_path() {
        assert_eq!(
            normalize_origin("http://example.com/").as_deref(),
            Some("http://example.com")
        );
        assert_eq!(
            normalize_origin("http://example.com/not/an/origin").as_deref(),
            Some("http://example.com")
        );
    }

    #[test]
    fn normalize_lowercases_scheme_and_host() {
        assert_eq!(
            normalize_origin("HTTPS://Studio.Example.COM").as_deref(),
            Some("https://studio.example.com")
        );
    }

    #[test]
    fn normalize_keeps_ipv6_brackets() {
        assert_eq!(
            normalize_origin("http://[::1]:8080").as_deref(),
            Some("http://[::1]:8080")
        );
        assert_eq!(
            normalize_origin("http://[::1]:80").as_deref(),
            Some("http://[::1]")
        );
    }

    #[test]
    fn normalize_rejects_non_http_schemes_and_junk() {
        assert!(normalize_origin("file:///etc/passwd").is_none());
        assert!(normalize_origin("null").is_none());
        assert!(normalize_origin("").is_none());
        assert!(normalize_origin("http://").is_none());
        assert!(normalize_origin("http://example.com:abc").is_none());
    }

    #[test]
    fn policy_matches_across_equivalent_origin_forms() {
        let p = OriginPolicy::new(vec!["HTTPS://Studio.Example.com:443/".into()]);
        assert!(p.is_allowed("https://studio.example.com"));
        assert!(p.is_allowed("https://Studio.Example.com"));
        assert!(p.is_allowed("https://studio.example.com:443"));
        assert!(!p.is_allowed("https://studio.example.com:8443"));
        assert!(!p.is_allowed("http://studio.example.com"));
    }
}
