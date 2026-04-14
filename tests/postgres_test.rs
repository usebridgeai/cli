// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License version 3
// as published by the Free Software Foundation.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

//! Postgres integration tests.
//!
//! These tests require `DATABASE_URL` to be set to a reachable Postgres instance.
//! Run with: cargo test -- --ignored
//!
//! In CI, use a Postgres service container and set DATABASE_URL instead.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;
use tokio::sync::OnceCell;

static SETUP: OnceCell<()> = OnceCell::const_new();

fn bridge() -> Command {
    Command::cargo_bin("bridge").unwrap()
}

async fn test_database_url() -> &'static str {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| {
            panic!(
                "Postgres integration tests require DATABASE_URL to point to a reachable Postgres instance"
            )
        })
        .leak()
}

/// Set up a temp dir with bridge.yaml pointing to Postgres.
fn setup_pg(dir: &TempDir, db_url: &str) {
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

/// Create test tables once (async-safe via tokio::sync::OnceCell).
async fn ensure_tables(db_url: &str) {
    let url = db_url.to_string();
    SETUP
        .get_or_init(|| async {
            let pool = sqlx::PgPool::connect(&url).await.unwrap();

            sqlx::query("DROP TABLE IF EXISTS bridge_test_users CASCADE")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("DROP TABLE IF EXISTS bridge_test_empty CASCADE")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("DROP TABLE IF EXISTS bridge_test_no_pk CASCADE")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("DROP TABLE IF EXISTS bridge_test_composite_pk CASCADE")
                .execute(&pool)
                .await
                .unwrap();

            sqlx::query(
                "CREATE TABLE bridge_test_users (
                    id SERIAL PRIMARY KEY,
                    name TEXT NOT NULL,
                    email TEXT
                )",
            )
            .execute(&pool)
            .await
            .unwrap();

            sqlx::query("INSERT INTO bridge_test_users (name, email) VALUES ('Alice', 'alice@example.com'), ('Bob', 'bob@example.com'), ('Charlie', NULL)")
                .execute(&pool)
                .await
                .unwrap();

            sqlx::query(
                "CREATE TABLE bridge_test_empty (id SERIAL PRIMARY KEY, data TEXT)",
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
                    a INT,
                    b INT,
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

#[tokio::test]
#[ignore]
async fn test_pg_ls_tables() {
    let db_url = test_database_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_pg(&dir, db_url);

    let output = bridge()
        .args(["ls", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let entries: serde_json::Value = serde_json::from_str(&stdout).unwrap();
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
async fn test_pg_read_table() {
    let db_url = test_database_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_pg(&dir, db_url);

    let output = bridge()
        .args(["read", "bridge_test_users", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(value["data"]["type"], "json");
    let rows = value["data"]["content"].as_array().unwrap();
    assert_eq!(rows.len(), 3);

    // Verify ORDER BY — ids should be 1, 2, 3
    assert_eq!(rows[0]["id"], 1);
    assert_eq!(rows[1]["id"], 2);
    assert_eq!(rows[2]["id"], 3);
    assert_eq!(rows[0]["name"], "Alice");
}

#[tokio::test]
#[ignore]
async fn test_pg_read_table_respects_limit() {
    let db_url = test_database_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_pg(&dir, db_url);

    let output = bridge()
        .args(["read", "bridge_test_users", "--from", "db", "--limit", "2"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let rows = value["data"]["content"].as_array().unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], 1);
    assert_eq!(rows[1]["id"], 2);
}

#[tokio::test]
#[ignore]
async fn test_pg_read_table_allows_zero_limit() {
    let db_url = test_database_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_pg(&dir, db_url);

    let output = bridge()
        .args(["read", "bridge_test_users", "--from", "db", "--limit", "0"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let rows = value["data"]["content"].as_array().unwrap();

    assert_eq!(rows.len(), 0);
}

#[tokio::test]
#[ignore]
async fn test_pg_read_single_row() {
    let db_url = test_database_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_pg(&dir, db_url);

    let output = bridge()
        .args(["read", "bridge_test_users/2", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(value["data"]["type"], "json");
    assert_eq!(value["data"]["content"]["name"], "Bob");
}

#[tokio::test]
#[ignore]
async fn test_pg_read_single_row_ignores_limit() {
    let db_url = test_database_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_pg(&dir, db_url);

    let output = bridge()
        .args([
            "read",
            "bridge_test_users/2",
            "--from",
            "db",
            "--limit",
            "0",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(value["data"]["type"], "json");
    assert_eq!(value["data"]["content"]["id"], 2);
    assert_eq!(value["data"]["content"]["name"], "Bob");
}

#[tokio::test]
#[ignore]
async fn test_pg_read_row_not_found() {
    let db_url = test_database_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_pg(&dir, db_url);

    bridge()
        .args(["read", "bridge_test_users/999", "--from", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("Row not found"));
}

#[tokio::test]
#[ignore]
async fn test_pg_read_empty_table() {
    let db_url = test_database_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_pg(&dir, db_url);

    let output = bridge()
        .args(["read", "bridge_test_empty", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let rows = value["data"]["content"].as_array().unwrap();
    assert_eq!(rows.len(), 0);
}

#[tokio::test]
#[ignore]
async fn test_pg_read_nonexistent_table() {
    let db_url = test_database_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_pg(&dir, db_url);

    bridge()
        .args(["read", "bridge_test_nonexistent", "--from", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found in database"));
}

#[tokio::test]
#[ignore]
async fn test_pg_read_no_primary_key() {
    let db_url = test_database_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_pg(&dir, db_url);

    bridge()
        .args(["read", "bridge_test_no_pk", "--from", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("no primary key"));
}

#[tokio::test]
#[ignore]
async fn test_pg_read_composite_pk() {
    let db_url = test_database_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_pg(&dir, db_url);

    bridge()
        .args(["read", "bridge_test_composite_pk", "--from", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("composite primary key"));
}

#[tokio::test]
#[ignore]
async fn test_pg_sql_injection_blocked() {
    let db_url = test_database_url().await;
    ensure_tables(db_url).await;

    let dir = TempDir::new().unwrap();
    setup_pg(&dir, db_url);

    // Attempt SQL injection via table name
    bridge()
        .args(["read", "users; DROP TABLE users", "--from", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid_identifier"));
}

#[tokio::test]
#[ignore]
async fn test_pg_status_health() {
    let db_url = test_database_url().await;

    let dir = TempDir::new().unwrap();
    setup_pg(&dir, db_url);

    bridge()
        .args(["status"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"connected\": true"));
}
