// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Minimal hand-rolled HTTP/1.1 transport helpers for the MCP HTTP adapter.
// Everything in this file is about moving bytes on and off a TCP socket —
// parsing a request, writing a response, enforcing size and time budgets.
// Nothing here knows about MCP, JSON-RPC, origins, or sessions; those live
// one level up in `mcp::http`.
//
// The split exists so the adapter layer (routing, policy, service dispatch)
// stays readable as Streamable HTTP grows: future work for session IDs,
// Accept negotiation (`text/event-stream`), Last-Event-ID resumption, and SSE
// framing can land in this module without touching the MCP logic.
//
// The wire layer is intentionally small and permissive rather than a
// general-purpose HTTP library — it only handles what the MCP endpoint
// actually needs (Content-Length bodies, one request per connection,
// `Connection: close` on every response).
//
// Invariants:
// - Requests are bounded in both byte size and wall-clock time.
// - Responses always terminate the connection after the body is written.
// - Header names are parsed case-insensitively (per RFC 7230) and exposed
//   through `Headers::get`, which is the single entry point for future
//   protocol-header work (session IDs, Accept negotiation, etc.).

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Byte caps and wall-clock budget for reading a single request off the wire.
/// Centralized here so the limits are visible next to the parser they bound.
#[derive(Clone, Copy, Debug)]
pub struct ReadLimits {
    pub max_header_bytes: usize,
    pub max_body_bytes: usize,
    pub timeout: Duration,
}

/// A parsed HTTP request. Only carries what the adapter currently needs —
/// method, path, headers, body. `http_version` and trailers are deliberately
/// omitted; they'd be dead fields today.
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Headers,
    pub body: Vec<u8>,
}

/// Case-insensitive header collection. Backed by a `Vec` because request
/// header counts are tiny and duplicate-key semantics matter (future work may
/// need to read both `Accept` values in a multi-value header).
#[derive(Default)]
pub struct Headers {
    entries: Vec<(String, String)>,
}

impl Headers {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn push(&mut self, name: String, value: String) {
        self.entries.push((name, value));
    }
}

/// A response ready to be written to the wire. Bodies are materialized up
/// front so write_response can emit a single `Content-Length` without chunked
/// encoding (chunked is deferred to the SSE path when it lands).
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(status: u16, value: serde_json::Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: serde_json::to_vec(&value).unwrap_or_else(|_| b"null".to_vec()),
        }
    }

    pub fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: body.as_bytes().to_vec(),
        }
    }

    pub fn empty(status: u16) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: Vec::new(),
        }
    }
}

/// Parse errors that are worth turning into a specific HTTP status. `Io`
/// surfaces underlying socket failures unchanged so the caller can log them.
pub enum WireError {
    Status(u16, String),
    Io(std::io::Error),
}

impl From<std::io::Error> for WireError {
    fn from(e: std::io::Error) -> Self {
        WireError::Io(e)
    }
}

/// Read one request, enforcing `limits.timeout` across both the header and
/// body phases. Without the wall-clock cap, a slow client trickling bytes
/// below the size caps could pin a tokio task indefinitely.
pub async fn read_request(
    stream: &mut TcpStream,
    limits: &ReadLimits,
) -> std::result::Result<Request, WireError> {
    match tokio::time::timeout(limits.timeout, read_request_inner(stream, limits)).await {
        Ok(r) => r,
        Err(_) => Err(WireError::Status(408, "request timeout".into())),
    }
}

async fn read_request_inner(
    stream: &mut TcpStream,
    limits: &ReadLimits,
) -> std::result::Result<Request, WireError> {
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        if buf.len() > limits.max_header_bytes {
            return Err(WireError::Status(431, "headers too large".into()));
        }
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Err(WireError::Status(
                400,
                "client closed before headers".into(),
            ));
        }
        buf.extend_from_slice(&tmp[..n]);
    };

    let header_bytes = &buf[..header_end];
    let header_text = std::str::from_utf8(header_bytes)
        .map_err(|_| WireError::Status(400, "non-utf8 headers".into()))?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| WireError::Status(400, "missing request line".into()))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| WireError::Status(400, "missing method".into()))?
        .to_string();
    let path = parts
        .next()
        .ok_or_else(|| WireError::Status(400, "missing path".into()))?
        .to_string();

    let mut headers = Headers::default();
    let mut content_length: usize = 0;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value
                .parse()
                .map_err(|_| WireError::Status(400, "bad content-length".into()))?;
        }
        headers.push(name.to_string(), value.to_string());
    }

    if content_length > limits.max_body_bytes {
        return Err(WireError::Status(413, "body too large".into()));
    }

    let body_start = header_end + 4;
    let mut body = buf.split_off(body_start);
    while body.len() < content_length {
        let need = content_length - body.len();
        let mut chunk = vec![0u8; need.min(8192)];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(WireError::Status(400, "truncated body".into()));
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);

    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

/// Serialize and flush `resp`, then shut the socket down. Connections are
/// single-shot today; keep-alive is deferred until SSE forces us to keep the
/// socket open for server-initiated events.
pub async fn write_response(stream: &mut TcpStream, resp: &Response) -> std::io::Result<()> {
    let reason = status_reason(resp.status);
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        resp.status,
        reason,
        resp.body.len()
    );
    // Only include a Content-Type when there's something to describe. 204 and
    // 202-with-empty-body omit it, matching what RFC 7231 recommends.
    if !resp.body.is_empty() || resp.status == 200 {
        head.push_str(&format!("Content-Type: {}\r\n", resp.content_type));
    }
    head.push_str("\r\n");

    stream.write_all(head.as_bytes()).await?;
    if !resp.body.is_empty() {
        stream.write_all(&resp.body).await?;
    }
    stream.flush().await?;
    let _ = stream.shutdown().await;
    Ok(())
}

pub fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

pub fn status_reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_end_detection() {
        assert_eq!(find_header_end(b"GET / HTTP/1.1\r\n\r\n"), Some(14));
        assert_eq!(find_header_end(b"partial\r\n"), None);
    }

    #[test]
    fn status_reasons_cover_common_codes() {
        for c in [200, 202, 204, 400, 403, 404, 405, 408, 413, 431] {
            assert_ne!(status_reason(c), "Error");
        }
    }

    #[test]
    fn headers_get_is_case_insensitive_and_preserves_original_case() {
        let mut h = Headers::default();
        h.push("Content-Type".into(), "application/json".into());
        h.push("Mcp-Session-Id".into(), "abc".into());
        assert_eq!(h.get("content-type"), Some("application/json"));
        assert_eq!(h.get("CONTENT-TYPE"), Some("application/json"));
        assert_eq!(h.get("mcp-session-id"), Some("abc"));
        assert_eq!(h.get("x-missing"), None);
    }
}
