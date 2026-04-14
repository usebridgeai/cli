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

use tempfile::TempDir;

// We test the filesystem provider through the CLI binary to avoid
// needing to expose internal types in integration tests.
use assert_cmd::Command;
use predicates::prelude::*;

fn bridge() -> Command {
    Command::cargo_bin("bridge").unwrap()
}

fn setup_with_fixtures(dir: &TempDir) {
    // Init bridge
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    // Create fixtures inside the temp dir
    let fixtures = dir.path().join("fixtures");
    std::fs::create_dir_all(fixtures.join("nested")).unwrap();
    std::fs::write(fixtures.join("hello.md"), "# Hello\n\nWorld\n").unwrap();
    std::fs::write(fixtures.join("data.json"), r#"{"key": "value", "num": 42}"#).unwrap();
    std::fs::write(fixtures.join("empty.txt"), "").unwrap();
    std::fs::write(fixtures.join("nested/deep.txt"), "deep content").unwrap();
    std::fs::write(fixtures.join("binary.bin"), &[0x00, 0x01, 0xFF, 0xFE]).unwrap();

    // Connect filesystem provider
    let uri = format!("file://{}", fixtures.display());
    bridge()
        .args(["connect", &uri, "--as", "files"])
        .current_dir(dir.path())
        .assert()
        .success();
}

#[test]
fn test_ls_lists_files() {
    let dir = TempDir::new().unwrap();
    setup_with_fixtures(&dir);

    let output = bridge()
        .args(["ls", "--from", "files"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let entries: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let arr = entries.as_array().unwrap();

    // Should have 5 items: binary.bin, data.json, empty.txt, hello.md, nested/
    assert_eq!(arr.len(), 5);

    // Check types
    let names: Vec<&str> = arr.iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"hello.md"));
    assert!(names.contains(&"nested"));
    assert!(names.contains(&"data.json"));

    // nested should be a directory
    let nested = arr.iter().find(|e| e["name"] == "nested").unwrap();
    assert_eq!(nested["entry_type"], "directory");
}

#[test]
fn test_read_markdown() {
    let dir = TempDir::new().unwrap();
    setup_with_fixtures(&dir);

    bridge()
        .args(["read", "hello.md", "--from", "files"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"type\": \"text\""))
        .stdout(predicate::str::contains("# Hello"))
        .stdout(predicate::str::contains(
            "\"content_type\": \"text/markdown\"",
        ));
}

#[test]
fn test_read_json_file() {
    let dir = TempDir::new().unwrap();
    setup_with_fixtures(&dir);

    let output = bridge()
        .args(["read", "data.json", "--from", "files"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(value["data"]["type"], "json");
    assert_eq!(value["data"]["content"]["key"], "value");
    assert_eq!(value["data"]["content"]["num"], 42);
}

#[test]
fn test_read_binary_file() {
    let dir = TempDir::new().unwrap();
    setup_with_fixtures(&dir);

    let output = bridge()
        .args(["read", "binary.bin", "--from", "files"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(value["data"]["type"], "binary");
    assert_eq!(value["data"]["encoding"], "base64");

    // Verify base64 round-trip
    let b64_content = value["data"]["content"].as_str().unwrap();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64_content)
        .unwrap();
    assert_eq!(decoded, vec![0x00, 0x01, 0xFF, 0xFE]);
}

#[test]
fn test_read_empty_file() {
    let dir = TempDir::new().unwrap();
    setup_with_fixtures(&dir);

    let output = bridge()
        .args(["read", "empty.txt", "--from", "files"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(value["data"]["type"], "text");
    assert_eq!(value["data"]["content"], "");
}

#[test]
fn test_read_nested_file() {
    let dir = TempDir::new().unwrap();
    setup_with_fixtures(&dir);

    bridge()
        .args(["read", "nested/deep.txt", "--from", "files"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("deep content"));
}

#[test]
fn test_read_filesystem_ignores_limit() {
    let dir = TempDir::new().unwrap();
    setup_with_fixtures(&dir);

    bridge()
        .args(["read", "hello.md", "--from", "files", "--limit", "1"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("# Hello"))
        .stdout(predicate::str::contains("\"type\": \"text\""));
}

#[test]
fn test_read_nonexistent_file() {
    let dir = TempDir::new().unwrap();
    setup_with_fixtures(&dir);

    bridge()
        .args(["read", "nonexistent.txt", "--from", "files"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("provider_error"));
}

#[test]
fn test_path_traversal_blocked() {
    let dir = TempDir::new().unwrap();
    setup_with_fixtures(&dir);

    bridge()
        .args(["read", "../../etc/passwd", "--from", "files"])
        .current_dir(dir.path())
        .assert()
        .failure();
}

#[test]
fn test_connect_nonexistent_directory() {
    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    bridge()
        .args([
            "connect",
            "file:///nonexistent/path/that/does/not/exist",
            "--as",
            "bad",
        ])
        .current_dir(dir.path())
        .assert()
        .success(); // connect succeeds (just saves config)

    // But ls should fail because the directory doesn't exist
    bridge()
        .args(["ls", "--from", "bad"])
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("provider_error"));
}

#[test]
fn test_ls_empty_directory() {
    let dir = TempDir::new().unwrap();
    bridge()
        .arg("init")
        .current_dir(dir.path())
        .assert()
        .success();

    let empty = dir.path().join("empty_dir");
    std::fs::create_dir(&empty).unwrap();

    let uri = format!("file://{}", empty.display());
    bridge()
        .args(["connect", &uri, "--as", "empty"])
        .current_dir(dir.path())
        .assert()
        .success();

    let output = bridge()
        .args(["ls", "--from", "empty"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let entries: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(entries.as_array().unwrap().len(), 0);
}

#[test]
fn test_status_shows_provider_health() {
    let dir = TempDir::new().unwrap();
    setup_with_fixtures(&dir);

    bridge()
        .args(["status"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"connected\": true"));
}

use base64::Engine;
