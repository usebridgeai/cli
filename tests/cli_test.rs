// Bridge CLI - One CLI. Any storage. Every agent.
// Copyright (c) 2026 Gabriel Beslic
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

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn bridge() -> Command {
    Command::cargo_bin("bridge").unwrap()
}

#[test]
fn test_help() {
    bridge()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("unified CLI"));
}

#[test]
fn test_init_creates_config() {
    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"created\""));

    assert!(dir.path().join("bridge.yaml").exists());
}

#[test]
fn test_init_already_exists() {
    let dir = TempDir::new().unwrap();
    // First init
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    // Second init
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"already_exists\""));
}

#[test]
fn test_connect_filesystem() {
    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args(["connect", "file://./data", "--as", "files"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"type\": \"filesystem\""))
        .stdout(predicate::str::contains("\"status\": \"connected\""));
}

#[test]
fn test_connect_postgres_uri() {
    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args(["connect", "postgres://localhost:5432/db", "--as", "mydb"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"type\": \"postgres\""));
}

#[test]
fn test_connect_invalid_uri() {
    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args(["connect", "baduri", "--as", "x"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid_uri"));
}

#[test]
fn test_connect_duplicate_overwrites() {
    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args(["connect", "file://./v1", "--as", "files"])
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args(["connect", "file://./v2", "--as", "files"])
        .current_dir(dir.path())
        .assert()
        .success();

    // Verify config has the new URI
    let config = std::fs::read_to_string(dir.path().join("bridge.yaml")).unwrap();
    assert!(config.contains("file://./v2"));
    assert!(!config.contains("file://./v1"));
}

#[test]
fn test_remove_provider() {
    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();
    bridge()
        .args(["connect", "file://./data", "--as", "files"])
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args(["remove", "files"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"removed\""));
}

#[test]
fn test_remove_nonexistent() {
    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args(["remove", "nonexistent"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("provider_not_found"));
}

#[test]
fn test_no_config_error() {
    let dir = TempDir::new().unwrap();
    bridge()
        .args(["connect", "file://./data", "--as", "files"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("bridge init"));
}

#[test]
fn test_all_outputs_are_json() {
    let dir = TempDir::new().unwrap();

    // init output is valid JSON
    let output = bridge()
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&stdout).is_ok(),
        "init output not valid JSON: {stdout}"
    );

    // connect output is valid JSON
    let output = bridge()
        .args(["connect", "file://./data", "--as", "files"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&stdout).is_ok(),
        "connect output not valid JSON: {stdout}"
    );

    // remove output is valid JSON
    let output = bridge()
        .args(["remove", "files"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&stdout).is_ok(),
        "remove output not valid JSON: {stdout}"
    );

    // error output is valid JSON
    let output = bridge()
        .args(["remove", "nonexistent"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&stderr).is_ok(),
        "error output not valid JSON: {stderr}"
    );
}
