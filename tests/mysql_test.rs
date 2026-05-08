// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only

//! MySQL integration tests.
//!
//! These tests require `MYSQL_URL` to be set to a reachable MySQL instance.
//! Run with: cargo test --test mysql_test -- --ignored
//!
//! Example:
//!   MYSQL_URL=mysql://root:root@localhost:3306/bridge_test cargo test --test mysql_test -- --ignored

use assert_cmd::cargo::CommandCargoExt;
use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command as StdCommand, Stdio};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::OnceCell;

static SETUP: OnceCell<()> = OnceCell::const_new();

fn bridge() -> Command {
    Command::cargo_bin("bridge").unwrap()
}

fn bridge_path() -> std::path::PathBuf {
    StdCommand::cargo_bin("bridge")
        .expect("bridge binary built")
        .get_program()
        .into()
}

async fn mysql_url() -> &'static str {
    std::env::var("MYSQL_URL")
        .unwrap_or_else(|_| {
            panic!(
                "MySQL integration tests require MYSQL_URL to point to a reachable MySQL instance \
                 (e.g. mysql://root:root@localhost:3306/bridge_test)"
            )
        })
        .leak()
}

fn setup_mysql(dir: &TempDir, db_url: &str) {
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    bridge()
        .args(["connect", db_url, "--as", "db"])
        .current_dir(dir.path())
        .assert()
        .success();
}

async fn ensure_tables(db_url: &str) {
    let url = db_url.to_string();
    SETUP
        .get_or_init(|| async {
            let pool = sqlx::MySqlPool::connect(&url).await.unwrap();

            for stmt in [
                "DROP TABLE IF EXISTS bridge_test_users",
                "DROP TABLE IF EXISTS bridge_test_empty",
                "DROP TABLE IF EXISTS bridge_test_no_pk",
                "DROP TABLE IF EXISTS bridge_test_composite_pk",
            ] {
                sqlx::query(stmt).execute(&pool).await.unwrap();
            }

            sqlx::query(
                "CREATE TABLE bridge_test_users (
                    id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
                    name TEXT NOT NULL,
                    email TEXT
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "INSERT INTO bridge_test_users (name, email) VALUES
                    ('Alice', 'alice@example.com'),
                    ('Bob', 'bob@example.com'),
                    ('Charlie', NULL)",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "CREATE TABLE bridge_test_empty (
                    id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
                    data TEXT
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query("CREATE TABLE bridge_test_no_pk (data TEXT, value INT)")
                .execute(&pool)
                .await
                .unwrap();

            sqlx::query(
                "CREATE TABLE bridge_test_composite_pk (
                    a INT NOT NULL,
                    b INT NOT NULL,
                    data TEXT,
                    PRIMARY KEY (a, b)
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            pool.close().await;
        })
        .await;
}

// ─── Provider: ls, read, status ──────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_mysql_ls_tables() {
    let db_url = mysql_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_mysql(&dir, db_url);

    let output = bridge()
        .args(["ls", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let entries: Value = serde_json::from_str(&stdout).unwrap();
    let names: Vec<&str> = entries
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();

    assert!(names.contains(&"bridge_test_users"));
    assert!(names.contains(&"bridge_test_empty"));
}

#[tokio::test]
#[ignore]
async fn test_mysql_read_table() {
    let db_url = mysql_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_mysql(&dir, db_url);

    let output = bridge()
        .args(["read", "bridge_test_users", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(value["data"]["type"], "json");
    let rows = value["data"]["content"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["id"], 1);
    assert_eq!(rows[1]["id"], 2);
    assert_eq!(rows[2]["id"], 3);
    assert_eq!(rows[0]["name"], "Alice");
}

#[tokio::test]
#[ignore]
async fn test_mysql_read_table_respects_limit() {
    let db_url = mysql_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_mysql(&dir, db_url);

    let output = bridge()
        .args(["read", "bridge_test_users", "--from", "db", "--limit", "2"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: Value = serde_json::from_str(&stdout).unwrap();
    let rows = value["data"]["content"].as_array().unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], 1);
    assert_eq!(rows[1]["id"], 2);
}

#[tokio::test]
#[ignore]
async fn test_mysql_read_single_row() {
    let db_url = mysql_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_mysql(&dir, db_url);

    let output = bridge()
        .args(["read", "bridge_test_users/2", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(value["data"]["type"], "json");
    assert_eq!(value["data"]["content"]["name"], "Bob");
}

#[tokio::test]
#[ignore]
async fn test_mysql_read_row_not_found() {
    let db_url = mysql_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_mysql(&dir, db_url);

    bridge()
        .args(["read", "bridge_test_users/999", "--from", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("Row not found"));
}

#[tokio::test]
#[ignore]
async fn test_mysql_read_empty_table() {
    let db_url = mysql_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_mysql(&dir, db_url);

    let output = bridge()
        .args(["read", "bridge_test_empty", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: Value = serde_json::from_str(&stdout).unwrap();
    let rows = value["data"]["content"].as_array().unwrap();
    assert_eq!(rows.len(), 0);
}

#[tokio::test]
#[ignore]
async fn test_mysql_read_nonexistent_table() {
    let db_url = mysql_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_mysql(&dir, db_url);

    bridge()
        .args(["read", "bridge_test_nonexistent", "--from", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found in database"));
}

#[tokio::test]
#[ignore]
async fn test_mysql_read_no_primary_key() {
    let db_url = mysql_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_mysql(&dir, db_url);

    bridge()
        .args(["read", "bridge_test_no_pk", "--from", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("no primary key"));
}

#[tokio::test]
#[ignore]
async fn test_mysql_read_composite_pk() {
    let db_url = mysql_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_mysql(&dir, db_url);

    bridge()
        .args(["read", "bridge_test_composite_pk", "--from", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("composite primary key"));
}

#[tokio::test]
#[ignore]
async fn test_mysql_sql_injection_blocked() {
    let db_url = mysql_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_mysql(&dir, db_url);

    bridge()
        .args(["read", "users; DROP TABLE users", "--from", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid_identifier"));
}

#[tokio::test]
#[ignore]
async fn test_mysql_status_health() {
    let db_url = mysql_url().await;

    let dir = TempDir::new().unwrap();
    setup_mysql(&dir, db_url);

    bridge()
        .args(["status"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"connected\": true"));
}

// ─── connect-time verification ────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_mysql_connect_verifies_reachable_uri() {
    let db_url = mysql_url().await;
    let dir = TempDir::new().unwrap();

    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args(["connect", db_url, "--as", "db"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"type\": \"mysql\""))
        .stdout(predicate::str::contains("\"status\": \"connected\""))
        .stdout(predicate::str::contains("\"verified\": true"))
        .stdout(predicate::str::contains("\"latency_ms\""));
}

#[tokio::test]
#[ignore]
async fn test_mysql_connect_fails_on_unreachable_host() {
    let _ = mysql_url().await;
    let dir = TempDir::new().unwrap();

    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args([
            "--timeout",
            "3",
            "connect",
            "mysql://root:root@127.0.0.1:1/bridge_test",
            "--as",
            "bad",
        ])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("connection_verification_failed"));

    let config = std::fs::read_to_string(dir.path().join("bridge.yaml")).unwrap();
    assert!(!config.contains("bad:"));
}

#[tokio::test]
#[ignore]
async fn test_mysql_connect_no_verify_saves_unreachable_target() {
    let _ = mysql_url().await;
    let dir = TempDir::new().unwrap();

    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args([
            "connect",
            "mysql://root:root@127.0.0.1:1/bridge_test",
            "--as",
            "bad",
            "--no-verify",
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"saved_unverified\""))
        .stdout(predicate::str::contains("\"verified\": false"));

    let config = std::fs::read_to_string(dir.path().join("bridge.yaml")).unwrap();
    assert!(config.contains("127.0.0.1:1"));
}

// ─── MCP generation ───────────────────────────────────────────────────────────

static MCP_SETUP: OnceCell<()> = OnceCell::const_new();

async fn ensure_mcp_fixture(db_url: &str) {
    let url = db_url.to_string();
    MCP_SETUP
        .get_or_init(|| async {
            let pool = sqlx::MySqlPool::connect(&url).await.unwrap();

            for stmt in [
                "DROP TABLE IF EXISTS bridge_mcp_orders",
                "DROP TABLE IF EXISTS bridge_mcp_customers",
                "DROP TABLE IF EXISTS bridge_mcp_api_keys",
            ] {
                sqlx::query(stmt).execute(&pool).await.unwrap();
            }

            sqlx::query(
                "CREATE TABLE bridge_mcp_customers (
                    id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
                    email VARCHAR(255) NOT NULL,
                    status VARCHAR(50) NOT NULL,
                    created_at DATETIME NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "INSERT INTO bridge_mcp_customers (email, status, created_at) VALUES
                    ('alice@example.com', 'active', '2026-01-01 10:00:00'),
                    ('bob@example.com', 'inactive', '2026-01-02 11:15:00'),
                    ('carol@example.com', 'active', '2026-01-03 12:30:00')",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "CREATE TABLE bridge_mcp_orders (
                    id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
                    customer_id INT NOT NULL,
                    total DECIMAL(10, 2) NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "INSERT INTO bridge_mcp_orders (customer_id, total) VALUES (1, 19.95), (2, 42.50)",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "CREATE TABLE bridge_mcp_api_keys (
                    token VARCHAR(255) NOT NULL,
                    label VARCHAR(255) NOT NULL,
                    UNIQUE KEY uk_token (token)
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "INSERT INTO bridge_mcp_api_keys (token, label) VALUES
                    ('tok_live_1', 'Primary key'),
                    ('tok_live_2', 'Backup key')",
            )
            .execute(&pool)
            .await
            .unwrap();

            pool.close().await;
        })
        .await;
}

fn setup_mcp_bridge_dir(dir: &TempDir, db_url: &str) {
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    bridge()
        .args(["connect", db_url, "--as", "analytics"])
        .current_dir(dir.path())
        .assert()
        .success();
}

#[tokio::test]
#[ignore]
async fn generate_mcp_from_mysql_produces_manifest_with_expected_tools() {
    let db_url = mysql_url().await;
    ensure_mcp_fixture(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_mcp_bridge_dir(&dir, db_url);
    let out = dir.path().join("analytics.mcp.yaml");

    bridge()
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
            out.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("list_bridge_mcp_customers"))
        .stdout(predicate::str::contains("get_bridge_mcp_customer_by_id"))
        .stdout(predicate::str::contains("get_bridge_mcp_api_key_by_token"));

    let body = std::fs::read_to_string(&out).unwrap();
    assert!(body.contains("kind: bridge.mcp/v1"));
    assert!(body.contains("type: db"));
    assert!(body.contains("dialect: mysql"));
    assert!(body.contains("connection_ref: analytics"));
    assert!(body.contains("type: sql_select"));
    assert!(
        !body.contains("mysql://"),
        "manifest must not embed DSNs: {body}"
    );

    // Regeneration must be deterministic.
    let out2 = dir.path().join("analytics2.mcp.yaml");
    bridge()
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
            out2.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .assert()
        .success();
    let body2 = std::fs::read_to_string(&out2).unwrap();
    assert_eq!(body, body2, "manifest regeneration is not deterministic");
}

// ─── MCP runtime ──────────────────────────────────────────────────────────────

fn send(stdin: &mut impl Write, req: Value) {
    let line = serde_json::to_string(&req).unwrap();
    stdin.write_all(line.as_bytes()).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();
}

fn recv(reader: &mut impl BufRead) -> Value {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

trait WaitTimeoutOrKill {
    fn wait_timeout_or_kill(&mut self, d: Duration);
}

impl WaitTimeoutOrKill for std::process::Child {
    fn wait_timeout_or_kill(&mut self, d: Duration) {
        let start = std::time::Instant::now();
        loop {
            match self.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => {
                    if start.elapsed() > d {
                        let _ = self.kill();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => return,
            }
        }
    }
}

#[tokio::test]
#[ignore]
async fn mcp_serve_exposes_mysql_tools_end_to_end() {
    let db_url = mysql_url().await;
    ensure_mcp_fixture(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_mcp_bridge_dir(&dir, db_url);
    let out = dir.path().join("analytics.mcp.yaml");

    bridge()
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

    // initialize
    send(
        &mut stdin,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    let _ = recv(&mut reader);

    // tools/list
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
    assert!(names.contains(&"list_bridge_mcp_customers"));
    assert!(names.contains(&"get_bridge_mcp_customer_by_id"));
    assert!(names.contains(&"get_bridge_mcp_api_key_by_token"));

    // list_bridge_mcp_customers filtered by status
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "list_bridge_mcp_customers",
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

    // get_bridge_mcp_customer_by_id
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "get_bridge_mcp_customer_by_id",
                "arguments": { "id": 2 }
            }
        }),
    );
    let get_resp = recv(&mut reader);
    assert_eq!(get_resp["result"]["isError"], false);
    let sc = &get_resp["result"]["structuredContent"];
    assert_eq!(sc["found"], true);
    assert_eq!(sc["row"]["email"], "bob@example.com");

    // DECIMAL columns should be returned as strings to avoid precision loss.
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "list_bridge_mcp_orders",
                "arguments": { "order_by": "id" }
            }
        }),
    );
    let orders_resp = recv(&mut reader);
    assert_eq!(orders_resp["result"]["isError"], false);
    let order_rows = orders_resp["result"]["structuredContent"]["rows"]
        .as_array()
        .unwrap();
    assert_eq!(order_rows[0]["total"], "19.95");

    // not-found: get_* for a missing id must return found=false, not an error.
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "get_bridge_mcp_customer_by_id",
                "arguments": { "id": 999999 }
            }
        }),
    );
    let missing_resp = recv(&mut reader);
    assert_eq!(missing_resp["result"]["isError"], false);
    assert_eq!(missing_resp["result"]["structuredContent"]["found"], false);

    // Invalid order_by is rejected as a tool-level error, not a JSON-RPC error.
    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "list_bridge_mcp_customers",
                "arguments": { "order_by": "not_a_column" }
            }
        }),
    );
    let bad_sort = recv(&mut reader);
    assert_eq!(bad_sort["result"]["isError"], true);

    drop(stdin);
    child.wait_timeout_or_kill(Duration::from_secs(2));
}
