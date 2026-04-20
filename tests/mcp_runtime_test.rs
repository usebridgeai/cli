// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// End-to-end runtime test: spin up a tiny HTTP backend, generate a manifest
// against a matching OpenAPI spec, launch `bridge mcp serve` as a subprocess,
// drive it over stdio with JSON-RPC, and assert the tool-call envelope.

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Command as StdCommand, Stdio};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn fixture_path(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    p.to_string_lossy().into_owned()
}

/// Minimal HTTP/1.1 server that answers `GET /pets/{id}` with a canned JSON
/// payload. Intentionally hand-rolled to avoid pulling a test-only web server
/// into the workspace.
fn start_mock_backend() -> (SocketAddr, std::sync::mpsc::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(false).unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel::<()>();

    thread::spawn(move || {
        for stream in listener.incoming() {
            if shutdown_rx.try_recv().is_ok() {
                break;
            }
            let Ok(mut stream) = stream else { continue };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();

            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            // Drain headers.
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    break;
                }
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }

            let body = json!({ "id": "42", "name": "Fido", "tag": "dog" }).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    (addr, shutdown_tx)
}

fn generate_manifest(out: &std::path::Path) {
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

fn send_and_recv(
    stdin: &mut std::process::ChildStdin,
    stdout: &mut BufReader<std::process::ChildStdout>,
    msg: &Value,
) -> Option<Value> {
    let line = format!("{msg}\n");
    stdin.write_all(line.as_bytes()).unwrap();
    stdin.flush().unwrap();
    // Read a single response line. Notifications produce nothing — caller must
    // only call this for requests with an `id`.
    let mut buf = String::new();
    let n = stdout.read_line(&mut buf).unwrap();
    if n == 0 {
        return None;
    }
    Some(serde_json::from_str(&buf).unwrap())
}

#[test]
fn mcp_server_serves_generated_tool_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let manifest = tmp.path().join("petstore.mcp.yaml");
    generate_manifest(&manifest);

    let (addr, _shutdown) = start_mock_backend();
    let base = format!("http://{addr}");

    let mut child = StdCommand::cargo_bin("bridge")
        .unwrap()
        .env("BRIDGE_TEST_PETSTORE_BASE_URL", &base)
        .args(["mcp", "serve", manifest.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // 1. initialize handshake.
    let init = send_and_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "0" }
            }
        }),
    )
    .expect("initialize response");
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
    assert!(init["result"]["serverInfo"]["name"]
        .as_str()
        .unwrap()
        .contains("petstore"));

    // 2. notifications/initialized — notification, no response.
    stdin
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\",\"params\":{}}\n",
        )
        .unwrap();
    stdin.flush().unwrap();

    // 3. tools/list must include our GET tools.
    let list = send_and_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }),
    )
    .expect("tools/list response");
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names
        .iter()
        .any(|n| n.contains("getPetById") || n.contains("get_pet_by_id")));
    // No POST tools must have been generated.
    assert!(!names.iter().any(|n| n.contains("adopt")));

    let pet_tool = names
        .iter()
        .find(|n| n.contains("getPetById") || n.contains("get_pet_by_id"))
        .unwrap()
        .to_string();

    // 4. tools/call getPetById → expect ok:true and body with name=Fido.
    let call = send_and_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": pet_tool,
                "arguments": { "petId": "42" }
            }
        }),
    )
    .expect("tools/call response");

    let structured = &call["result"]["structuredContent"];
    assert_eq!(structured["ok"], Value::Bool(true));
    assert_eq!(structured["status"], json!(200));
    assert_eq!(structured["body"]["name"], json!("Fido"));
    assert_eq!(call["result"]["isError"], Value::Bool(false));

    // 5. tools/call with missing required param → tool-level error.
    let bad = send_and_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": { "name": pet_tool, "arguments": {} }
        }),
    )
    .expect("error response");
    assert_eq!(bad["result"]["isError"], Value::Bool(true));

    // 6. unknown tool → tool-level error, not RPC error.
    let unknown = send_and_recv(
        &mut stdin,
        &mut stdout,
        &json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": { "name": "does_not_exist", "arguments": {} }
        }),
    )
    .expect("unknown tool response");
    assert!(unknown["error"].is_object() || unknown["result"]["isError"] == Value::Bool(true));

    drop(stdin);
    let _ = child.wait();
}
