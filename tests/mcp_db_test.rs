// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Integration tests for Phase 2: DB → MCP manifest generation and runtime.
// Requires DATABASE_URL pointing at a reachable Postgres instance and is
// gated behind `#[ignore]` just like tests/postgres_test.rs.
//
// Run with: cargo test --test mcp_db_test -- --ignored

use assert_cmd::cargo::CommandCargoExt;
use assert_cmd::Command;
use predicates::str::contains;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command as StdCommand, Stdio};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::OnceCell;

static SETUP: OnceCell<()> = OnceCell::const_new();

fn bridge() -> Command {
    Command::cargo_bin("bridge").expect("bridge binary built")
}

fn bridge_path() -> std::path::PathBuf {
    StdCommand::cargo_bin("bridge")
        .expect("bridge binary built")
        .get_program()
        .into()
}

async fn database_url() -> &'static str {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| panic!("DATABASE_URL must be set for MCP DB integration tests"))
        .leak()
}

/// Seed fixture tables for the Phase-2 tests. Reuses distinct names so it
/// can coexist with postgres_test.rs fixtures in the same database.
async fn ensure_fixture(db_url: &str) {
    let url = db_url.to_string();
    SETUP
        .get_or_init(|| async {
            let pool = sqlx::PgPool::connect(&url).await.unwrap();

            for stmt in [
                "DROP TABLE IF EXISTS bridge_mcp_customers CASCADE",
                "DROP TABLE IF EXISTS bridge_mcp_orders CASCADE",
                "DROP TABLE IF EXISTS bridge_mcp_api_keys CASCADE",
            ] {
                sqlx::query(stmt).execute(&pool).await.unwrap();
            }

            sqlx::query(
                "CREATE TABLE bridge_mcp_customers (
                    id SERIAL PRIMARY KEY,
                    email TEXT NOT NULL,
                    status TEXT NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("COMMENT ON TABLE bridge_mcp_customers IS 'Customers for MCP fixture.'")
                .execute(&pool)
                .await
                .unwrap();

            sqlx::query(
                "INSERT INTO bridge_mcp_customers (email, status, created_at) VALUES
                    ('alice@example.com', 'active', '2026-01-01T10:00:00Z'),
                    ('bob@example.com', 'inactive', '2026-01-02T11:15:00Z'),
                    ('carol@example.com', 'active', '2026-01-03T12:30:00Z')",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "CREATE TABLE bridge_mcp_orders (
                    id SERIAL PRIMARY KEY,
                    customer_id INT NOT NULL,
                    total NUMERIC(10,2) NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO bridge_mcp_orders (customer_id, total) VALUES
                    (1, 19.95),
                    (2, 42.50)",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query(
                "CREATE TABLE bridge_mcp_api_keys (
                    token TEXT NOT NULL,
                    label TEXT NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "CREATE UNIQUE INDEX bridge_mcp_api_keys_token_idx
                    ON bridge_mcp_api_keys (token)",
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

fn setup_bridge_dir(dir: &TempDir, db_url: &str) {
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
async fn generate_mcp_from_db_produces_manifest_with_expected_tools() {
    let db_url = database_url().await;
    ensure_fixture(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_bridge_dir(&dir, db_url);
    let out = dir.path().join("analytics.mcp.yaml");

    bridge()
        .args([
            "generate",
            "mcp",
            "--from",
            "db",
            "--connection",
            "analytics",
            "--schema",
            "public",
            "--name",
            "analytics",
            "--out",
            out.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(contains("list_bridge_mcp_customers"))
        .stdout(contains("get_bridge_mcp_customer_by_id"))
        .stdout(contains("list_bridge_mcp_orders"))
        .stdout(contains("get_bridge_mcp_api_key_by_token"));

    let body = std::fs::read_to_string(&out).unwrap();
    assert!(body.contains("kind: bridge.mcp/v1"));
    assert!(body.contains("type: db"));
    assert!(body.contains("dialect: postgres"));
    assert!(body.contains("connection_ref: analytics"));
    assert!(body.contains("type: sql_select"));
    // No raw DSNs should ever land in the manifest.
    assert!(
        !body.contains("postgres://"),
        "manifest must not embed DSNs: {body}"
    );

    // Regeneration is deterministic.
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

#[tokio::test]
#[ignore]
async fn generate_mcp_from_db_fails_on_missing_connection() {
    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    let out = dir.path().join("missing.mcp.yaml");
    bridge()
        .args([
            "generate",
            "mcp",
            "--from",
            "db",
            "--connection",
            "does_not_exist",
            "--name",
            "x",
            "--out",
            out.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(contains("provider_not_found"));
}

#[tokio::test]
#[ignore]
async fn generate_mcp_from_db_rejects_non_postgres_connection() {
    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    bridge()
        .args(["connect", "sqlite://./x.db", "--as", "local", "--no-verify"])
        .current_dir(dir.path())
        .assert()
        .success();

    let out = dir.path().join("out.yaml");
    bridge()
        .args([
            "generate",
            "mcp",
            "--from",
            "db",
            "--connection",
            "local",
            "--name",
            "x",
            "--out",
            out.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(contains("postgres connections"));
}

// ─── Runtime: serve the generated manifest and drive it over stdio ──────────

#[tokio::test]
#[ignore]
async fn mcp_serve_exposes_db_tools_end_to_end() {
    let db_url = database_url().await;
    ensure_fixture(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_bridge_dir(&dir, db_url);
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

    // Launch `bridge mcp serve <manifest>` in the temp dir so it can resolve
    // the `analytics` connection from bridge.yaml.
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

    // tools/call list_bridge_mcp_customers filtered by status
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
    assert!(rows[0]["created_at"].as_str().is_some());

    // tools/call get_bridge_mcp_customer_by_id
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

    // numeric columns are returned as strings to avoid precision loss
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

    // not-found: a get_* for a missing id must return found=false, not error
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

    // Invalid order_by (not in the sortable allowlist) is rejected at the
    // executor boundary as a tool-level error.
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

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {
                "name": "list_bridge_mcp_customers",
                "arguments": { "order_by": "id", "order_direction": "sideways" }
            }
        }),
    );
    let bad_direction = recv(&mut reader);
    assert_eq!(bad_direction["result"]["isError"], true);

    drop(stdin);
    let _ = child.wait_timeout_or_kill(Duration::from_secs(2));
}

// Tiny helper because std::process::Child doesn't expose wait_timeout.
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
