// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only

use assert_cmd::cargo::CommandCargoExt;
use assert_cmd::Command;
use predicates::str::contains;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command as StdCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn bridge() -> Command {
    Command::cargo_bin("bridge").expect("bridge binary built")
}

fn bridge_path() -> std::path::PathBuf {
    StdCommand::cargo_bin("bridge")
        .expect("bridge binary built")
        .get_program()
        .into()
}

async fn create_sqlite_fixture(dir: &TempDir) -> String {
    let db_path = dir.path().join("local.db");
    let db_path_str = db_path.to_string_lossy().to_string();
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{db_path_str}?mode=rwc"))
        .await
        .unwrap();

    sqlx::query(
        "CREATE TABLE customers (
            id INTEGER PRIMARY KEY,
            email TEXT NOT NULL,
            status TEXT NOT NULL,
            total NUMERIC,
            active BOOLEAN,
            created_at TEXT
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO customers (email, status, total, active, created_at) VALUES
            ('alice@example.com', 'active', 19.95, 1, '2026-01-01T10:00:00Z'),
            ('bob@example.com', 'inactive', 42.50, 0, '2026-01-02T11:15:00Z'),
            ('carol@example.com', 'active', 7.00, 1, '2026-01-03T12:30:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query("CREATE TABLE api_keys (token TEXT NOT NULL UNIQUE, label TEXT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO api_keys (token, label) VALUES
            ('tok_live_1', 'Primary'),
            ('tok_live_2', 'Backup')",
    )
    .execute(&pool)
    .await
    .unwrap();

    pool.close().await;
    db_path_str
}

fn setup_bridge_dir(dir: &TempDir, db_path: &str) {
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    bridge()
        .args(["connect", &format!("sqlite://{db_path}"), "--as", "localdb"])
        .current_dir(dir.path())
        .assert()
        .success();
}

fn setup_bridge_dir_with_relative_sqlite_uri(dir: &TempDir) {
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    bridge()
        .args(["connect", "sqlite://./local.db", "--as", "localdb"])
        .current_dir(dir.path())
        .assert()
        .success();
}

fn generate_sqlite_manifest(dir: &TempDir) -> std::path::PathBuf {
    let out = dir.path().join("localdb.mcp.yaml");
    bridge()
        .args([
            "generate",
            "mcp",
            "--from",
            "db",
            "--connection",
            "localdb",
            "--name",
            "localdb",
            "--out",
            out.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .assert()
        .success();
    out
}

#[tokio::test]
async fn generate_mcp_from_sqlite_produces_manifest_with_expected_tools() {
    let dir = TempDir::new().unwrap();
    let db_path = create_sqlite_fixture(&dir).await;
    setup_bridge_dir(&dir, &db_path);
    let out = dir.path().join("localdb.mcp.yaml");

    bridge()
        .args([
            "generate",
            "mcp",
            "--from",
            "db",
            "--connection",
            "localdb",
            "--name",
            "localdb",
            "--out",
            out.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(contains("list_customers"))
        .stdout(contains("get_customer_by_id"))
        .stdout(contains("get_api_key_by_token"));

    let body = std::fs::read_to_string(&out).unwrap();
    assert!(body.contains("kind: bridge.mcp/v1"));
    assert!(body.contains("type: db"));
    assert!(body.contains("dialect: sqlite"));
    assert!(body.contains("schema: main"));
    assert!(body.contains("connection_ref: localdb"));
    assert!(body.contains("type: sql_select"));
    assert!(
        !body.contains("sqlite://"),
        "manifest must not embed SQLite URIs"
    );
}

#[tokio::test]
async fn mcp_serve_exposes_sqlite_tools_end_to_end() {
    let dir = TempDir::new().unwrap();
    let db_path = create_sqlite_fixture(&dir).await;
    setup_bridge_dir(&dir, &db_path);
    let out = dir.path().join("localdb.mcp.yaml");

    bridge()
        .args([
            "generate",
            "mcp",
            "--from",
            "db",
            "--connection",
            "localdb",
            "--name",
            "localdb",
            "--out",
            out.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .assert()
        .success();

    let mut child = StdCommand::new(bridge_path())
        .args(["mcp", "serve", out.to_str().unwrap()])
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bridge mcp serve");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    fn send(stdin: &mut impl Write, req: Value) {
        let line = serde_json::to_string(&req).unwrap();
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    fn recv(reader: &mut impl BufRead) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    let _ = recv(&mut reader);

    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    );
    let list_resp = recv(&mut reader);
    let names: Vec<&str> = list_resp["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"list_customers"));
    assert!(names.contains(&"get_customer_by_id"));
    assert!(names.contains(&"get_api_key_by_token"));

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "list_customers",
                "arguments": { "status": "active", "order_by": "id", "order_direction": "asc" }
            }
        }),
    );
    let call_resp = recv(&mut reader);
    assert_eq!(call_resp["result"]["isError"], false);
    let rows = call_resp["result"]["structuredContent"]["rows"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["email"], "alice@example.com");
    assert_eq!(rows[1]["email"], "carol@example.com");
    assert_eq!(rows[0]["total"], "19.95");

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "get_customer_by_id",
                "arguments": { "id": 2 }
            }
        }),
    );
    let get_resp = recv(&mut reader);
    assert_eq!(get_resp["result"]["isError"], false);
    assert_eq!(
        get_resp["result"]["structuredContent"]["row"]["email"],
        "bob@example.com"
    );

    drop(stdin);
    let _ = wait_timeout_or_kill(&mut child, Duration::from_secs(2));
}

#[tokio::test]
async fn mcp_serve_resolves_relative_sqlite_uri_from_manifest_config_root() {
    let dir = TempDir::new().unwrap();
    let _db_path = create_sqlite_fixture(&dir).await;
    setup_bridge_dir_with_relative_sqlite_uri(&dir);
    let out = generate_sqlite_manifest(&dir);
    let other_cwd = TempDir::new().unwrap();

    let mut child = StdCommand::new(bridge_path())
        .args(["mcp", "serve", out.to_str().unwrap()])
        .current_dir(other_cwd.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bridge mcp serve");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    send_jsonrpc(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    let _ = recv_jsonrpc(&mut reader);

    send_jsonrpc(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "get_customer_by_id",
                "arguments": { "id": 1 }
            }
        }),
    );
    let resp = recv_jsonrpc(&mut reader);
    assert_eq!(resp["result"]["isError"], false);
    assert_eq!(
        resp["result"]["structuredContent"]["row"]["email"],
        "alice@example.com"
    );

    drop(stdin);
    wait_timeout_or_kill(&mut child, Duration::from_secs(2));
}

#[tokio::test]
async fn mcp_serve_http_calls_sqlite_tool_from_relative_uri() {
    let dir = TempDir::new().unwrap();
    let _db_path = create_sqlite_fixture(&dir).await;
    setup_bridge_dir_with_relative_sqlite_uri(&dir);
    let out = generate_sqlite_manifest(&dir);
    let other_cwd = TempDir::new().unwrap();

    let server = ServerGuard::spawn(&out, other_cwd.path());
    let init = post_jsonrpc(
        server.addr,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");

    let call = post_jsonrpc(
        server.addr,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "list_customers",
                "arguments": { "status": "active", "order_by": "id", "order_direction": "asc" }
            }
        }),
    );
    assert_eq!(call["result"]["isError"], false);
    let rows = call["result"]["structuredContent"]["rows"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["email"], "alice@example.com");
}

fn send_jsonrpc(stdin: &mut impl Write, req: Value) {
    let line = serde_json::to_string(&req).unwrap();
    stdin.write_all(line.as_bytes()).unwrap();
    stdin.write_all(b"\n").unwrap();
}

fn recv_jsonrpc(reader: &mut impl BufRead) -> Value {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

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
    let sep = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response must have header terminator");
    let status_line =
        std::str::from_utf8(&raw[..raw.iter().position(|&b| b == b'\r').unwrap()]).unwrap();
    assert!(
        status_line.starts_with("HTTP/1.1 200"),
        "expected HTTP 200, got: {status_line}"
    );
    serde_json::from_slice(&raw[sep + 4..]).expect("response body is JSON")
}

struct ServerGuard {
    child: Child,
    addr: SocketAddr,
}

impl ServerGuard {
    fn spawn(manifest: &Path, cwd: &Path) -> Self {
        let port = free_port();
        let bind = format!("127.0.0.1:{port}");
        let addr: SocketAddr = bind.parse().unwrap();
        let child = StdCommand::new(bridge_path())
            .args([
                "mcp",
                "serve-http",
                manifest.to_str().unwrap(),
                "--bind",
                &bind,
            ])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
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

fn wait_timeout_or_kill(child: &mut std::process::Child, d: Duration) {
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {
                if start.elapsed() > d {
                    let _ = child.kill();
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return,
        }
    }
}
