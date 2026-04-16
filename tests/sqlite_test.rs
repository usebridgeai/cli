// Bridge CLI - One CLI. Any storage. Every agent.
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

//! SQLite integration tests.
//!
//! These tests create a temporary SQLite database and exercise the full CLI
//! pipeline: connect, ls, read, status. No external services required.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use tempfile::TempDir;

fn bridge() -> Command {
    Command::cargo_bin("bridge").unwrap()
}

/// Create a SQLite test database with sample tables and return the file path.
fn create_test_db(dir: &Path) -> String {
    let db_path = dir.join("test.db");
    let db_path_str = db_path.to_str().unwrap().to_string();

    // Use the sqlite3 CLI-independent approach: connect via sqlx at runtime.
    // We'll use a small helper binary approach — but simpler: just use the
    // rusqlite-compatible sqlx blocking approach isn't available, so we create
    // the DB via a short-lived tokio runtime.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=rwc", db_path_str))
            .await
            .unwrap();

        sqlx::query(
            "CREATE TABLE users (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                email TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO users (id, name, email) VALUES
                (1, 'Alice', 'alice@example.com'),
                (2, 'Bob', 'bob@example.com'),
                (3, 'Charlie', NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query("CREATE TABLE empty_table (id INTEGER PRIMARY KEY, data TEXT)")
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query("CREATE TABLE no_pk (data TEXT, value INTEGER)")
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query(
            "CREATE TABLE composite_pk (
                a INTEGER,
                b INTEGER,
                data TEXT,
                PRIMARY KEY (a, b)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE typed_data (
                id INTEGER PRIMARY KEY,
                int_val INTEGER,
                real_val REAL,
                text_val TEXT,
                blob_val BLOB,
                bool_val BOOLEAN
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE affinity_data (
                id INTEGER PRIMARY KEY,
                decimal_val DECIMAL(10,2),
                json_val JSON,
                varchar_val VARCHAR(255)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO typed_data (id, int_val, real_val, text_val, blob_val, bool_val)
             VALUES (1, 42, 3.14, 'hello', X'DEADBEEF', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            r#"INSERT INTO affinity_data (id, decimal_val, json_val, varchar_val)
               VALUES (1, 12.34, '{"a":1}', 'hello')"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        pool.close().await;
    });

    db_path_str
}

/// Set up a temp dir with bridge.yaml pointing to a SQLite database.
fn setup_sqlite(dir: &TempDir, db_path: &str) {
    let uri = format!("sqlite://{db_path}");
    setup_sqlite_with_uri(dir, &uri);
}

/// Set up a temp dir with bridge.yaml pointing to a specific SQLite URI.
fn setup_sqlite_with_uri(dir: &TempDir, uri: &str) {
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    bridge()
        .args(["connect", &uri, "--as", "db"])
        .current_dir(dir.path())
        .assert()
        .success();
}

// ── connect ──────────────────────────────────────────────────────────

#[test]
fn test_sqlite_connect() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());

    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args(["connect", &format!("sqlite://{db_path}"), "--as", "db"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"type\": \"sqlite\""))
        .stdout(predicate::str::contains("\"status\": \"connected\""));
}

#[test]
fn test_sqlite_connect_rejects_missing_file_and_hints_at_rwc() {
    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args([
            "connect",
            "sqlite:///tmp/bridge_nonexistent_12345.db",
            "--as",
            "db",
        ])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("connection_verification_failed"))
        .stderr(predicate::str::contains("mode=rwc"));
}

#[test]
fn test_sqlite_connect_saves_config_for_nonexistent_file_with_no_verify() {
    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args([
            "connect",
            "sqlite:///tmp/bridge_nonexistent_12345.db",
            "--as",
            "db",
            "--no-verify",
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"type\": \"sqlite\""))
        .stdout(predicate::str::contains("\"status\": \"saved_unverified\""));
}

// ── ls ───────────────────────────────────────────────────────────────

#[test]
fn test_sqlite_ls_tables() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    let output = bridge()
        .args(["ls", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let entries: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let names: Vec<&str> = entries
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();

    assert!(names.contains(&"users"));
    assert!(names.contains(&"empty_table"));
    assert!(names.contains(&"no_pk"));
    assert!(names.contains(&"composite_pk"));
    assert!(names.contains(&"typed_data"));
    assert!(names.contains(&"affinity_data"));
    // sqlite internal tables should be filtered out
    assert!(!names.iter().any(|n| n.starts_with("sqlite_")));
}

// ── read table ───────────────────────────────────────────────────────

#[test]
fn test_sqlite_read_table() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    let output = bridge()
        .args(["read", "users", "--from", "db"])
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

#[test]
fn test_sqlite_read_table_respects_limit() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    let output = bridge()
        .args(["read", "users", "--from", "db", "--limit", "2"])
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

#[test]
fn test_sqlite_read_table_allows_zero_limit() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    let output = bridge()
        .args(["read", "users", "--from", "db", "--limit", "0"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let rows = value["data"]["content"].as_array().unwrap();
    assert_eq!(rows.len(), 0);
}

// ── read single row ──────────────────────────────────────────────────

#[test]
fn test_sqlite_read_single_row() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    let output = bridge()
        .args(["read", "users/2", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(value["data"]["type"], "json");
    assert_eq!(value["data"]["content"]["name"], "Bob");
}

#[test]
fn test_sqlite_read_single_row_ignores_limit() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    let output = bridge()
        .args(["read", "users/2", "--from", "db", "--limit", "0"])
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

#[test]
fn test_sqlite_read_single_row_with_mode_ro() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite_with_uri(&dir, &format!("sqlite://{db_path}?mode=ro"));

    let output = bridge()
        .args(["read", "users/2", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(value["data"]["content"]["id"], 2);
    assert_eq!(value["data"]["content"]["name"], "Bob");
}

// ── error cases ──────────────────────────────────────────────────────

#[test]
fn test_sqlite_read_row_not_found() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    bridge()
        .args(["read", "users/999", "--from", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("Row not found"));
}

#[test]
fn test_sqlite_read_empty_table() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    let output = bridge()
        .args(["read", "empty_table", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let rows = value["data"]["content"].as_array().unwrap();
    assert_eq!(rows.len(), 0);
}

#[test]
fn test_sqlite_read_nonexistent_table() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    bridge()
        .args(["read", "nonexistent", "--from", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found in database"));
}

#[test]
fn test_sqlite_read_no_primary_key() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    bridge()
        .args(["read", "no_pk", "--from", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("no primary key"));
}

#[test]
fn test_sqlite_read_composite_pk() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    bridge()
        .args(["read", "composite_pk", "--from", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("composite primary key"));
}

// ── SQL injection ────────────────────────────────────────────────────

#[test]
fn test_sqlite_sql_injection_blocked() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    bridge()
        .args(["read", "users; DROP TABLE users", "--from", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid_identifier"));
}

// ── type mapping ─────────────────────────────────────────────────────

#[test]
fn test_sqlite_type_mapping() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    let output = bridge()
        .args(["read", "typed_data/1", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let content = &value["data"]["content"];

    assert_eq!(content["int_val"], 42);
    assert_eq!(content["text_val"], "hello");
    // real_val should be a number close to 3.14
    assert!(content["real_val"].as_f64().unwrap() > 3.13);
    assert!(content["real_val"].as_f64().unwrap() < 3.15);
    // bool_val was inserted as 1 with BOOLEAN affinity
    assert_eq!(content["bool_val"], true);
    // blob_val was inserted as X'DEADBEEF' — should be base64-encoded
    assert_eq!(content["blob_val"], "3q2+7w==");
}

#[test]
fn test_sqlite_declared_affinity_mapping() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    let output = bridge()
        .args(["read", "affinity_data/1", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let content = &value["data"]["content"];

    assert!(content["decimal_val"].as_f64().unwrap() > 12.33);
    assert!(content["decimal_val"].as_f64().unwrap() < 12.35);
    assert_eq!(content["json_val"], "{\"a\":1}");
    assert_eq!(content["varchar_val"], "hello");
}

// ── null handling ────────────────────────────────────────────────────

#[test]
fn test_sqlite_null_values() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    let output = bridge()
        .args(["read", "users/3", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let content = &value["data"]["content"];

    assert_eq!(content["name"], "Charlie");
    assert!(content["email"].is_null());
}

// ── health ───────────────────────────────────────────────────────────

#[test]
fn test_sqlite_status_health() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    bridge()
        .args(["status"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"connected\": true"));
}

#[test]
fn test_sqlite_status_with_mode_rwc_creates_database() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("created.db");
    let db_path_str = db_path.to_str().unwrap().to_string();
    setup_sqlite_with_uri(&dir, &format!("sqlite://{db_path_str}?mode=rwc"));

    bridge()
        .args(["status"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"connected\": true"));

    assert!(db_path.exists());
}

// ── JSON output ──────────────────────────────────────────────────────

#[test]
fn test_sqlite_all_outputs_are_json() {
    let dir = TempDir::new().unwrap();
    let db_path = create_test_db(dir.path());
    setup_sqlite(&dir, &db_path);

    // ls output is valid JSON
    let output = bridge()
        .args(["ls", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&stdout).is_ok(),
        "ls output not valid JSON: {stdout}"
    );

    // read output is valid JSON
    let output = bridge()
        .args(["read", "users", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&stdout).is_ok(),
        "read output not valid JSON: {stdout}"
    );

    // error output is valid JSON
    let output = bridge()
        .args(["read", "nonexistent", "--from", "db"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&stderr).is_ok(),
        "error output not valid JSON: {stderr}"
    );
}
