// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// End-to-end tests for `bridge mcp serve-http`: spawn the CLI as a subprocess,
// drive it over HTTP/1.1 against the Streamable HTTP endpoint, and assert
// the JSON-RPC envelope for initialize, tools/list, and a DB-backed
// tools/call. The DB-backed case is gated behind `#[ignore]` and requires
// DATABASE_URL, matching tests/mcp_db_test.rs.

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as StdCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::sync::OnceCell;

fn fixture_path(rel: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(rel)
        .to_string_lossy()
        .into_owned()
}

fn bridge_bin() -> PathBuf {
    StdCommand::cargo_bin("bridge")
        .unwrap()
        .get_program()
        .into()
}

/// Grab a free localhost port by binding to :0 and immediately dropping the
/// listener. There's an inherent race between drop and the child binding, but
/// in practice it's reliable enough for tests and avoids hard-coding a port.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

fn wait_for_listen(addr: SocketAddr, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("server did not start listening on {addr} within {timeout:?}");
}

/// POST a JSON-RPC request to the MCP endpoint and return the decoded JSON
/// response body. The server always uses `Connection: close`, so we can read
/// until EOF instead of parsing Content-Length.
fn post_jsonrpc(addr: SocketAddr, req: &Value) -> Value {
    let body = serde_json::to_vec(req).unwrap();
    let request = format!(
        "POST /mcp HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Content-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );

    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(&body).unwrap();
    stream.flush().unwrap();

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();

    // Split headers / body at the first CRLFCRLF.
    let sep = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response must have header terminator");
    let status_line = std::str::from_utf8(&raw[..raw.iter().position(|&b| b == b'\r').unwrap()])
        .unwrap()
        .to_string();
    assert!(
        status_line.starts_with("HTTP/1.1 200"),
        "expected HTTP 200, got: {status_line}"
    );
    let body_bytes = &raw[sep + 4..];
    serde_json::from_slice(body_bytes).expect("response body is JSON")
}

fn post_with_origin(addr: SocketAddr, origin: &str, body: &[u8]) -> (String, Vec<u8>) {
    let request = format!(
        "POST /mcp HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Content-Type: application/json\r\n\
         Origin: {origin}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let sep = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let status = std::str::from_utf8(&raw[..raw.iter().position(|&b| b == b'\r').unwrap()])
        .unwrap()
        .to_string();
    (status, raw[sep + 4..].to_vec())
}

fn post_plain(addr: SocketAddr, body: &[u8]) -> (String, Vec<u8>) {
    let request = format!(
        "POST /mcp HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let sep = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let status = std::str::from_utf8(&raw[..raw.iter().position(|&b| b == b'\r').unwrap()])
        .unwrap()
        .to_string();
    (status, raw[sep + 4..].to_vec())
}

fn generate_petstore_manifest(out: &Path) {
    let status = StdCommand::cargo_bin("bridge")
        .unwrap()
        .args([
            "generate",
            "mcp",
            "--from",
            "openapi",
            &fixture_path("fixtures/openapi/petstore.yaml"),
            "--name",
            "petstore",
            "--base-url-env",
            "BRIDGE_TEST_PETSTORE_BASE_URL",
            "--out",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "generate failed");
}

struct ServerGuard {
    child: Child,
    addr: SocketAddr,
}

impl ServerGuard {
    fn spawn(manifest: &Path, extra_env: &[(&str, &str)], cwd: Option<&Path>) -> Self {
        Self::spawn_with_args(manifest, extra_env, cwd, &[])
    }

    fn spawn_with_args(
        manifest: &Path,
        extra_env: &[(&str, &str)],
        cwd: Option<&Path>,
        extra_args: &[&str],
    ) -> Self {
        let port = free_port();
        let bind = format!("127.0.0.1:{port}");
        let addr: SocketAddr = bind.parse().unwrap();

        let mut cmd = StdCommand::new(bridge_bin());
        cmd.args([
            "mcp",
            "serve-http",
            manifest.to_str().unwrap(),
            "--bind",
            &bind,
        ])
        .args(extra_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        let child = cmd.spawn().unwrap();
        wait_for_listen(addr, Duration::from_secs(10));
        Self { child, addr }
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn http_initialize_and_tools_list_over_openapi_manifest() {
    let tmp = TempDir::new().unwrap();
    let manifest = tmp.path().join("petstore.mcp.yaml");
    generate_petstore_manifest(&manifest);

    // No backend needed for initialize / tools/list — the manifest just needs
    // to validate and the HTTP executor can be built lazily.
    let server = ServerGuard::spawn(
        &manifest,
        &[("BRIDGE_TEST_PETSTORE_BASE_URL", "http://localhost:1")],
        None,
    );

    let init = post_jsonrpc(
        server.addr,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        }),
    );
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
    assert!(init["result"]["serverInfo"]["name"]
        .as_str()
        .unwrap()
        .contains("petstore"));

    let list = post_jsonrpc(
        server.addr,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    );
    let tools = list["result"]["tools"].as_array().unwrap();
    assert!(!tools.is_empty());
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names
        .iter()
        .any(|n| n.contains("getPetById") || n.contains("get_pet_by_id")));
}

#[test]
fn http_rejects_unknown_paths_and_methods() {
    let tmp = TempDir::new().unwrap();
    let manifest = tmp.path().join("petstore.mcp.yaml");
    generate_petstore_manifest(&manifest);
    let server = ServerGuard::spawn(
        &manifest,
        &[("BRIDGE_TEST_PETSTORE_BASE_URL", "http://localhost:1")],
        None,
    );

    // Unknown path → 404
    let mut s = TcpStream::connect_timeout(&server.addr, Duration::from_secs(2)).unwrap();
    s.write_all(b"GET /nope HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let head = std::str::from_utf8(&raw[..12]).unwrap();
    assert!(head.starts_with("HTTP/1.1 404"), "got: {head}");

    // Parse error → 400 with JSON-RPC parse error
    let (status, body) = post_plain(server.addr, b"not json");
    assert!(status.starts_with("HTTP/1.1 400"), "got: {status}");
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], -32700);
}

#[test]
fn http_allow_origin_flag_extends_default_policy() {
    // Exercises the CLI surface: `--allow-origin` values passed through clap,
    // into OriginPolicy, and observable over the wire. Ensures the flag isn't
    // silently dropped by argument parsing or command wiring.
    let tmp = TempDir::new().unwrap();
    let manifest = tmp.path().join("petstore.mcp.yaml");
    generate_petstore_manifest(&manifest);

    let trusted = "https://studio.example.test";
    let server = ServerGuard::spawn_with_args(
        &manifest,
        &[("BRIDGE_TEST_PETSTORE_BASE_URL", "http://localhost:1")],
        None,
        &[
            "--allow-origin",
            trusted,
            // Second occurrence proves the flag is repeatable (Vec<String>).
            "--allow-origin",
            "https://second.example.test",
        ],
    );

    let body =
        serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
            .unwrap();

    // Origin matching the first allowlist entry → 200.
    let (status, _) = post_with_origin(server.addr, trusted, &body);
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "allowlisted origin '{trusted}' got: {status}"
    );

    // Origin matching the second entry → 200 (repeatability).
    let (status, _) = post_with_origin(server.addr, "https://second.example.test", &body);
    assert!(status.starts_with("HTTP/1.1 200"), "got: {status}");

    // Loopback still allowed by default alongside the custom allowlist.
    let (status, _) = post_with_origin(server.addr, "http://127.0.0.1:8080", &body);
    assert!(status.starts_with("HTTP/1.1 200"), "got: {status}");

    // An unrelated cross-site origin is still rejected — the flag extends the
    // default policy, it doesn't replace it with "allow anything".
    let (status, _) = post_with_origin(server.addr, "https://evil.example", &body);
    assert!(status.starts_with("HTTP/1.1 403"), "got: {status}");
}

#[test]
fn http_slow_client_gets_disconnected_and_does_not_pin_task() {
    // The server's request-read budget is 15s. We can't wait that long in a
    // unit test, so we instead assert the *behavioural* contract under load:
    // ten half-open sockets that never send a full request line must not
    // prevent concurrent well-formed requests from completing. Before the
    // read-timeout fix, this test would still pass for other reasons — so we
    // additionally assert that each slow socket eventually gets closed by
    // the server (Slowloris would hold it open indefinitely).
    let tmp = TempDir::new().unwrap();
    let manifest = tmp.path().join("petstore.mcp.yaml");
    generate_petstore_manifest(&manifest);
    let server = ServerGuard::spawn(
        &manifest,
        &[("BRIDGE_TEST_PETSTORE_BASE_URL", "http://localhost:1")],
        None,
    );

    let mut slow: Vec<TcpStream> = (0..10)
        .map(|_| {
            let s = TcpStream::connect_timeout(&server.addr, Duration::from_secs(2)).unwrap();
            // Trickle a partial request line — no CRLF ever, no full headers.
            let mut s2 = s.try_clone().unwrap();
            s2.write_all(b"POST /mcp HTTP/1.1\r\nHost: x\r\n").unwrap();
            s2.flush().unwrap();
            s
        })
        .collect();

    // Well-formed client completes normally while slow sockets are pending.
    let init = post_jsonrpc(
        server.addr,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");

    // Each slow socket must be closed by the server within the read-timeout
    // window. We cap the wait at 20s to stay below CI patience but above the
    // 15s server-side budget. Read-side EOF proves the server dropped it.
    for s in &mut slow {
        s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let mut buf = [0u8; 512];
        let n = s.read(&mut buf).unwrap_or(0);
        // Server closes with a 408 + Connection: close, so read returns >0
        // then EOF. A hung server would hit the read timeout and return err.
        assert!(n > 0, "server did not close slow socket within budget");
    }
}

#[test]
fn http_rejects_cross_site_origin_for_dns_rebinding_protection() {
    let tmp = TempDir::new().unwrap();
    let manifest = tmp.path().join("petstore.mcp.yaml");
    generate_petstore_manifest(&manifest);
    let server = ServerGuard::spawn(
        &manifest,
        &[("BRIDGE_TEST_PETSTORE_BASE_URL", "http://localhost:1")],
        None,
    );

    let body =
        serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
            .unwrap();

    // Cross-site Origin → 403
    let (status, _) = post_with_origin(server.addr, "https://evil.example", &body);
    assert!(status.starts_with("HTTP/1.1 403"), "got: {status}");

    // Hostname that merely *contains* "localhost" must not bypass the check.
    let (status, _) = post_with_origin(server.addr, "http://localhost.evil.example", &body);
    assert!(status.starts_with("HTTP/1.1 403"), "got: {status}");

    // Loopback Origin → 200
    let (status, _) = post_with_origin(server.addr, "http://localhost:8080", &body);
    assert!(status.starts_with("HTTP/1.1 200"), "got: {status}");
}

#[test]
fn http_notifications_produce_202_no_body() {
    let tmp = TempDir::new().unwrap();
    let manifest = tmp.path().join("petstore.mcp.yaml");
    generate_petstore_manifest(&manifest);
    let server = ServerGuard::spawn(
        &manifest,
        &[("BRIDGE_TEST_PETSTORE_BASE_URL", "http://localhost:1")],
        None,
    );

    let body = serde_json::to_vec(
        &json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
    )
    .unwrap();
    let (status, body_bytes) = post_plain(server.addr, &body);
    assert!(status.starts_with("HTTP/1.1 202"), "got: {status}");
    assert!(body_bytes.is_empty());
}

// ─── DB-backed tools/call (requires DATABASE_URL) ───────────────────────────

static SETUP: OnceCell<()> = OnceCell::const_new();

async fn database_url() -> &'static str {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| panic!("DATABASE_URL must be set for HTTP MCP DB tests"))
        .leak()
}

async fn ensure_fixture(db_url: &str) {
    let url = db_url.to_string();
    SETUP
        .get_or_init(|| async {
            let pool = sqlx::PgPool::connect(&url).await.unwrap();
            sqlx::query("DROP TABLE IF EXISTS bridge_mcp_http_customers CASCADE")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE bridge_mcp_http_customers (
                    id SERIAL PRIMARY KEY,
                    email TEXT NOT NULL,
                    status TEXT NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO bridge_mcp_http_customers (email, status) VALUES
                    ('alice@example.com', 'active'),
                    ('bob@example.com', 'inactive')",
            )
            .execute(&pool)
            .await
            .unwrap();
            pool.close().await;
        })
        .await;
}

fn bridge_assert() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("bridge").unwrap()
}

#[tokio::test]
#[ignore]
async fn http_db_tools_call_round_trips_over_http() {
    let db_url = database_url().await;
    ensure_fixture(db_url).await;

    let dir = TempDir::new().unwrap();
    bridge_assert()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    bridge_assert()
        .args(["connect", db_url, "--as", "analytics"])
        .current_dir(dir.path())
        .assert()
        .success();

    let manifest = dir.path().join("analytics.mcp.yaml");
    bridge_assert()
        .args([
            "generate",
            "mcp",
            "--from",
            "db",
            "--connection",
            "analytics",
            "--name",
            "analytics",
            "--out",
            manifest.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .assert()
        .success();

    // Hosted mode must resolve bridge.yaml relative to the *manifest location*,
    // not the process cwd. Launch the server from a sibling temp dir with no
    // bridge.yaml of its own: if the binary ever regresses back to
    // current-directory lookup, this test fails with provider_not_found
    // instead of silently hiding the regression.
    let other_cwd = TempDir::new().unwrap();
    assert!(
        !other_cwd.path().join("bridge.yaml").exists(),
        "precondition: alternate cwd must not contain a bridge.yaml"
    );
    let server = ServerGuard::spawn(&manifest, &[], Some(other_cwd.path()));

    let init = post_jsonrpc(
        server.addr,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");

    let list = post_jsonrpc(
        server.addr,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    );
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"list_bridge_mcp_http_customers"));

    let call = post_jsonrpc(
        server.addr,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "list_bridge_mcp_http_customers",
                "arguments": { "status": "active", "order_by": "id", "order_direction": "asc" }
            }
        }),
    );
    assert_eq!(call["result"]["isError"], false);
    let rows = call["result"]["structuredContent"]["rows"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["email"], "alice@example.com");
}
