// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Streamable-HTTP adapter for the MCP service core. Hosts a single manifest on
// a TCP socket and speaks MCP's Streamable HTTP transport: a single endpoint
// that accepts JSON-RPC over HTTP POST and returns JSON responses.
//
// This hosted path also owns the operational behavior that does not belong in
// the manifest itself: request size/time budgets, health/readiness endpoints,
// graceful shutdown, origin checks, and structured logs.

mod wire;

use crate::error::{BridgeError, Result};
use crate::mcp::manifest::Manifest;
use crate::mcp::runtime::build_local_executors;
use crate::mcp::service::{ExecutorBundle, McpService};
use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};
use std::future::Future;
use std::net::SocketAddr;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use url::Url;
use wire::{ReadLimits, Request, Response, WireError};

/// The single MCP endpoint path. Fixed here so clients can be configured
/// without reading config — matches the Streamable HTTP convention.
const MCP_PATH: &str = "/mcp";
const HEALTH_PATH: &str = "/healthz";
const READINESS_PATH: &str = "/readyz";

#[derive(Clone, Debug)]
pub struct HostedHttpConfig {
    bind: SocketAddr,
    public_url: Option<String>,
    read_limits: ReadLimits,
    request_timeout: Duration,
    shutdown_grace_period: Duration,
}

impl HostedHttpConfig {
    pub fn new(
        bind: String,
        public_url: Option<String>,
        max_header_bytes: usize,
        max_body_bytes: usize,
        read_timeout_secs: u64,
        request_timeout_secs: u64,
        shutdown_grace_secs: u64,
    ) -> Result<Self> {
        if max_header_bytes == 0 {
            return Err(BridgeError::McpRuntime(
                "--max-header-bytes must be greater than 0".into(),
            ));
        }
        if max_body_bytes == 0 {
            return Err(BridgeError::McpRuntime(
                "--max-body-bytes must be greater than 0".into(),
            ));
        }
        if read_timeout_secs == 0 {
            return Err(BridgeError::McpRuntime(
                "--read-timeout-secs must be greater than 0".into(),
            ));
        }
        if request_timeout_secs == 0 {
            return Err(BridgeError::McpRuntime(
                "--request-timeout-secs must be greater than 0".into(),
            ));
        }
        if shutdown_grace_secs == 0 {
            return Err(BridgeError::McpRuntime(
                "--shutdown-grace-secs must be greater than 0".into(),
            ));
        }

        let bind = bind
            .parse()
            .map_err(|e| BridgeError::McpRuntime(format!("invalid --bind '{bind}': {e}")))?;
        let public_url = match public_url {
            Some(raw) => Some(normalize_public_url(&raw)?),
            None => None,
        };

        Ok(Self {
            bind,
            public_url,
            read_limits: ReadLimits {
                max_header_bytes,
                max_body_bytes,
                timeout: Duration::from_secs(read_timeout_secs),
            },
            request_timeout: Duration::from_secs(request_timeout_secs),
            shutdown_grace_period: Duration::from_secs(shutdown_grace_secs),
        })
    }

    fn endpoint_urls(&self, local: SocketAddr) -> EndpointUrls {
        let base = self
            .public_url
            .clone()
            .unwrap_or_else(|| format!("http://{local}"));
        EndpointUrls {
            mcp: join_public_url(&base, MCP_PATH),
            health: join_public_url(&base, HEALTH_PATH),
            readiness: join_public_url(&base, READINESS_PATH),
        }
    }
}

#[derive(Clone, Debug)]
struct EndpointUrls {
    mcp: String,
    health: String,
    readiness: String,
}

struct HostedState {
    manifest_name: String,
    urls: EndpointUrls,
    ready: AtomicBool,
}

/// Serve `manifest` over MCP Streamable HTTP on `config.bind`.
pub async fn serve(
    manifest: Manifest,
    config: HostedHttpConfig,
    timeout_secs: u64,
    config_dir: &Path,
    origin_policy: OriginPolicy,
) -> Result<()> {
    let executors = build_local_executors(&manifest, config_dir, timeout_secs)?;
    serve_with_executors(manifest, executors, config, origin_policy).await
}

/// Lower-level entry that takes an already-built `ExecutorBundle`. Used by
/// tests (and by any future host that wants to inject its own executors).
pub async fn serve_with_executors(
    manifest: Manifest,
    executors: ExecutorBundle,
    config: HostedHttpConfig,
    origin_policy: OriginPolicy,
) -> Result<()> {
    let service = Arc::new(McpService::new(manifest, executors)?);
    let origin_policy = Arc::new(origin_policy);

    let listener = TcpListener::bind(config.bind)
        .await
        .map_err(|e| BridgeError::McpRuntime(format!("bind {} failed: {e}", config.bind)))?;
    let local = listener
        .local_addr()
        .map_err(|e| BridgeError::McpRuntime(format!("local_addr failed: {e}")))?;
    let urls = config.endpoint_urls(local);
    let state = Arc::new(HostedState {
        manifest_name: service.manifest().name.clone(),
        urls: urls.clone(),
        ready: AtomicBool::new(true),
    });

    log_event(
        "info",
        "server_started",
        json!({
            "component": "mcp_http",
            "manifest": service.manifest().name,
            "tool_count": service.manifest().tools.len(),
            "bind": config.bind.to_string(),
            "local_addr": local.to_string(),
            "mcp_url": urls.mcp,
            "health_url": urls.health,
            "readiness_url": urls.readiness,
            "max_header_bytes": config.read_limits.max_header_bytes,
            "max_body_bytes": config.read_limits.max_body_bytes,
            "read_timeout_ms": config.read_limits.timeout.as_millis() as u64,
            "request_timeout_ms": config.request_timeout.as_millis() as u64,
            "shutdown_grace_ms": config.shutdown_grace_period.as_millis() as u64,
        }),
    );

    let mut shutdown = shutdown_signal()?;
    let mut tasks = JoinSet::new();

    loop {
        tokio::select! {
            signal = &mut shutdown => {
                state.ready.store(false, Ordering::SeqCst);
                log_event(
                    "info",
                    "shutdown_requested",
                    json!({
                        "component": "mcp_http",
                        "manifest": state.manifest_name,
                        "signal": signal,
                    }),
                );
                break;
            }
            accept_result = listener.accept() => {
                let (stream, peer) = accept_result
                    .map_err(|e| BridgeError::McpRuntime(format!("accept failed: {e}")))?;
                let service = service.clone();
                let policy = origin_policy.clone();
                let state = state.clone();
                let config = config.clone();
                tasks.spawn(async move {
                    if let Err(e) = handle_connection(stream, service, policy, state, config).await {
                        log_event(
                            "error",
                            "connection_error",
                            json!({
                                "component": "mcp_http",
                                "peer": peer.to_string(),
                                "error": e.to_string(),
                            }),
                        );
                    }
                });
            }
            joined = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(result) = joined {
                    log_join_result(result);
                }
            }
        }
    }

    drop(listener);

    match tokio::time::timeout(config.shutdown_grace_period, async {
        while let Some(result) = tasks.join_next().await {
            log_join_result(result);
        }
    })
    .await
    {
        Ok(()) => {
            log_event(
                "info",
                "server_stopped",
                json!({
                    "component": "mcp_http",
                    "manifest": state.manifest_name,
                    "graceful": true,
                }),
            );
        }
        Err(_) => {
            let dropped = tasks.len();
            tasks.abort_all();
            while let Some(result) = tasks.join_next().await {
                log_join_result(result);
            }
            log_event(
                "warn",
                "shutdown_timeout",
                json!({
                    "component": "mcp_http",
                    "manifest": state.manifest_name,
                    "dropped_connections": dropped,
                }),
            );
        }
    }

    Ok(())
}

async fn handle_connection(
    mut stream: TcpStream,
    service: Arc<McpService>,
    policy: Arc<OriginPolicy>,
    state: Arc<HostedState>,
    config: HostedHttpConfig,
) -> Result<()> {
    let started = Instant::now();
    let peer = stream.peer_addr().ok().map(|addr| addr.to_string());

    let req = match wire::read_request(&mut stream, &config.read_limits).await {
        Ok(r) => r,
        Err(WireError::Status(code, msg)) => {
            log_event(
                "warn",
                "request_rejected",
                json!({
                    "component": "mcp_http",
                    "peer": peer,
                    "status": code,
                    "reason": msg,
                }),
            );
            return write(&mut stream, Response::text(code, &msg)).await;
        }
        Err(WireError::Io(e)) => return Err(BridgeError::McpRuntime(format!("read: {e}"))),
    };

    if let Some(origin) = req.headers.get("origin") {
        if !policy.is_allowed(origin) {
            let response = Response::text(403, "origin not allowed");
            log_request(&req, &response, &peer, started.elapsed(), Some(origin));
            return write(&mut stream, response).await;
        }
    }

    let response =
        match tokio::time::timeout(config.request_timeout, route(&service, &req, &state)).await {
            Ok(resp) => resp,
            Err(_) => Response::text(408, "request handling timeout"),
        };

    log_request(
        &req,
        &response,
        &peer,
        started.elapsed(),
        req.headers.get("origin"),
    );
    write(&mut stream, response).await
}

async fn write(stream: &mut TcpStream, resp: Response) -> Result<()> {
    wire::write_response(stream, &resp)
        .await
        .map_err(|e| BridgeError::McpRuntime(format!("write: {e}")))
}

async fn route(service: &McpService, req: &Request, state: &HostedState) -> Response {
    let path = request_path(&req.path);

    if matches!(path, HEALTH_PATH | "/health") {
        return route_probe(req.method.as_str(), health_response(state));
    }
    if matches!(path, READINESS_PATH | "/ready") {
        return route_probe(req.method.as_str(), readiness_response(state));
    }

    if path != MCP_PATH {
        return Response::text(404, "not found");
    }

    if !state.ready.load(Ordering::SeqCst) {
        return Response::text(503, "server is shutting down");
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

fn route_probe(method: &str, resp: Response) -> Response {
    match method {
        "GET" => resp,
        "HEAD" => Response::empty(resp.status),
        "OPTIONS" => Response::empty(204),
        _ => Response::empty(405),
    }
}

fn health_response(state: &HostedState) -> Response {
    Response::json(
        200,
        json!({
            "ok": true,
            "status": "ok",
            "manifest": state.manifest_name,
            "mcp_url": state.urls.mcp,
            "health_url": state.urls.health,
            "readiness_url": state.urls.readiness,
        }),
    )
}

fn readiness_response(state: &HostedState) -> Response {
    let ready = state.ready.load(Ordering::SeqCst);
    Response::json(
        if ready { 200 } else { 503 },
        json!({
            "ok": ready,
            "status": if ready { "ready" } else { "shutting_down" },
            "manifest": state.manifest_name,
            "mcp_url": state.urls.mcp,
            "health_url": state.urls.health,
            "readiness_url": state.urls.readiness,
        }),
    )
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

fn request_path(raw: &str) -> &str {
    raw.split('?').next().unwrap_or(raw)
}

fn normalize_public_url(raw: &str) -> Result<String> {
    let raw = raw.trim();
    let url = Url::parse(raw)
        .map_err(|e| BridgeError::McpRuntime(format!("invalid --public-url '{raw}': {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(BridgeError::McpRuntime(format!(
            "invalid --public-url '{raw}': only http:// and https:// URLs are supported"
        )));
    }
    if url.host_str().is_none() {
        return Err(BridgeError::McpRuntime(format!(
            "invalid --public-url '{raw}': host is required"
        )));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(BridgeError::McpRuntime(format!(
            "invalid --public-url '{raw}': query strings and fragments are not supported"
        )));
    }

    Ok(url.to_string().trim_end_matches('/').to_string())
}

fn join_public_url(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

fn shutdown_signal() -> Result<Pin<Box<dyn Future<Output = &'static str> + Send>>> {
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|e| BridgeError::McpRuntime(format!("install SIGTERM handler failed: {e}")))?;
        Ok(Box::pin(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => "ctrl_c",
                _ = sigterm.recv() => "sigterm",
            }
        }))
    }
    #[cfg(not(unix))]
    {
        Ok(Box::pin(async move {
            let _ = tokio::signal::ctrl_c().await;
            "ctrl_c"
        }))
    }
}

fn log_request(
    req: &Request,
    resp: &Response,
    peer: &Option<String>,
    elapsed: Duration,
    origin: Option<&str>,
) {
    log_event(
        "info",
        "request_complete",
        json!({
            "component": "mcp_http",
            "peer": peer,
            "method": req.method,
            "path": request_path(&req.path),
            "status": resp.status,
            "duration_ms": elapsed.as_millis() as u64,
            "content_length": req.body.len(),
            "origin": origin,
        }),
    );
}

fn log_join_result(result: std::result::Result<(), tokio::task::JoinError>) {
    if let Err(e) = result {
        log_event(
            "error",
            "connection_task_failed",
            json!({
                "component": "mcp_http",
                "error": e.to_string(),
            }),
        );
    }
}

fn log_event(level: &str, event: &str, fields: Value) {
    let mut record = serde_json::Map::new();
    record.insert(
        "ts".into(),
        Value::String(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)),
    );
    record.insert("level".into(), Value::String(level.to_string()));
    record.insert("event".into(), Value::String(event.to_string()));
    if let Value::Object(obj) = fields {
        record.extend(obj);
    }
    eprintln!("{}", Value::Object(record));
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
    }

    #[test]
    fn hosted_config_normalizes_public_url() {
        let cfg = HostedHttpConfig::new(
            "127.0.0.1:8080".into(),
            Some("HTTPS://Example.com/team-a/".into()),
            32 * 1024,
            1024 * 1024,
            15,
            30,
            10,
        )
        .unwrap();

        assert_eq!(
            cfg.public_url.as_deref(),
            Some("https://example.com/team-a")
        );
        let urls = cfg.endpoint_urls("127.0.0.1:8080".parse().unwrap());
        assert_eq!(urls.mcp, "https://example.com/team-a/mcp");
        assert_eq!(urls.health, "https://example.com/team-a/healthz");
        assert_eq!(urls.readiness, "https://example.com/team-a/readyz");
    }

    #[test]
    fn hosted_config_rejects_non_http_public_url() {
        let err = HostedHttpConfig::new(
            "127.0.0.1:8080".into(),
            Some("ftp://example.com".into()),
            32 * 1024,
            1024 * 1024,
            15,
            30,
            10,
        )
        .unwrap_err();

        assert!(err.to_string().contains("only http:// and https:// URLs"));
    }
}
