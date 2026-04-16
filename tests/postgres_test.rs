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

// ─── connect-time verification ───────────────────────────────────────────────

/// Replace the database name (everything after the last '/') in a postgres URL.
/// Used to build a URL that points at the same host/port/credentials but a
/// database that does not exist on the server.
fn replace_database_name(url: &str, new_db: &str) -> String {
    match url.rfind('/') {
        Some(pos) => {
            // Trim any existing query string from the db portion.
            let base = &url[..=pos];
            format!("{base}{new_db}")
        }
        None => panic!("malformed DATABASE_URL: {url}"),
    }
}

#[tokio::test]
#[ignore]
async fn test_pg_connect_verifies_reachable_uri() {
    let db_url = test_database_url().await;
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
        .stdout(predicate::str::contains("\"type\": \"postgres\""))
        .stdout(predicate::str::contains("\"status\": \"connected\""))
        .stdout(predicate::str::contains("\"verified\": true"))
        .stdout(predicate::str::contains("\"latency_ms\""));
}

#[tokio::test]
#[ignore]
async fn test_pg_connect_fails_verification_on_unreachable_host() {
    // Must have a working DATABASE_URL env so we know we're actually running
    // in the postgres test suite; we ignore its value and target port 1 which
    // will be refused fast on localhost.
    let _ = test_database_url().await;

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
            "postgres://postgres:postgres@127.0.0.1:1/postgres",
            "--as",
            "bad",
        ])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("connection_verification_failed"));

    // Nothing should have been written to bridge.yaml
    let config = std::fs::read_to_string(dir.path().join("bridge.yaml")).unwrap();
    assert!(!config.contains("bad:"));
}

#[tokio::test]
#[ignore]
async fn test_pg_connect_fails_verification_on_missing_database() {
    let db_url = test_database_url().await;
    let bogus = replace_database_name(db_url, "bridge_definitely_not_a_real_db_9f3c1");

    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args(["--timeout", "5", "connect", &bogus, "--as", "bad"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("connection_verification_failed"));

    let config = std::fs::read_to_string(dir.path().join("bridge.yaml")).unwrap();
    assert!(!config.contains("bad:"));
}

#[tokio::test]
#[ignore]
async fn test_pg_connect_no_verify_saves_unreachable_target() {
    let _ = test_database_url().await;

    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args([
            "connect",
            "postgres://postgres:postgres@127.0.0.1:1/postgres",
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

#[tokio::test]
#[ignore]
async fn test_pg_connect_env_var_target_verifies_when_var_is_set() {
    let db_url = test_database_url().await;
    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    // Use a custom env var name so we don't collide with the ambient DATABASE_URL
    // (which is used by the harness). We pass it explicitly to the child process.
    bridge()
        .env("BRIDGE_TEST_PG_URL", db_url)
        .args([
            "connect",
            "BRIDGE_TEST_PG_URL",
            "--type",
            "postgres",
            "--as",
            "db",
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"uri\": \"${BRIDGE_TEST_PG_URL}\"",
        ))
        .stdout(predicate::str::contains("\"verified\": true"));

    // The saved URI must remain the template, not the expanded value.
    let config = std::fs::read_to_string(dir.path().join("bridge.yaml")).unwrap();
    assert!(config.contains("uri: ${BRIDGE_TEST_PG_URL}"));
}

#[tokio::test]
#[ignore]
async fn test_pg_connect_env_var_target_saved_unverified_when_var_unset() {
    let _ = test_database_url().await;

    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .env_remove("BRIDGE_TEST_PG_UNSET_URL")
        .args([
            "connect",
            "BRIDGE_TEST_PG_UNSET_URL",
            "--type",
            "postgres",
            "--as",
            "db",
        ])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"saved_unverified\""))
        .stdout(predicate::str::contains("\"verified\": false"))
        .stdout(predicate::str::contains("BRIDGE_TEST_PG_UNSET_URL"));
}

#[tokio::test]
#[ignore]
async fn test_pg_connect_duplicate_requires_force() {
    let db_url = test_database_url().await;
    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    // First connect: succeeds and verifies against the live DB.
    bridge()
        .args(["connect", db_url, "--as", "db"])
        .current_dir(dir.path())
        .assert()
        .success();

    // Second connect to the same name: fails with provider_already_exists.
    bridge()
        .args(["connect", db_url, "--as", "db"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("provider_already_exists"));

    // With --force: succeeds and re-verifies.
    bridge()
        .args(["connect", db_url, "--as", "db", "--force"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"verified\": true"));
}

#[tokio::test]
#[ignore]
async fn test_pg_connect_does_not_save_on_verification_failure() {
    // End-to-end guarantee: a failed verification leaves bridge.yaml untouched,
    // so subsequent `bridge status` sees no provider for that name.
    let _ = test_database_url().await;

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
            "postgres://postgres:postgres@127.0.0.1:1/postgres",
            "--as",
            "ghost",
        ])
        .current_dir(dir.path())
        .assert()
        .failure();

    let output = bridge()
        .args(["status"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let providers = parsed["providers"].as_object().unwrap();
    assert!(
        !providers.contains_key("ghost"),
        "provider should not be present after failed verification: {stdout}"
    );
}
