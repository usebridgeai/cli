// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use std::fs;
use tempfile::TempDir;

fn bridge() -> Command {
    Command::cargo_bin("bridge").expect("bridge binary built")
}

fn fixture_path(rel: &str) -> String {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    p.to_string_lossy().into_owned()
}

#[test]
fn generate_mcp_from_openapi_produces_tools() {
    let tmp = TempDir::new().unwrap();
    let out = tmp.path().join("petstore.mcp.yaml");

    bridge()
        .args([
            "generate",
            "mcp",
            "--from",
            "openapi",
            &fixture_path("fixtures/openapi/petstore.yaml"),
            "--name",
            "petstore",
            "--base-url-env",
            "PETSTORE_BASE_URL",
            "--bearer-env",
            "PETSTORE_TOKEN",
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(contains("\"status\": \"generated\""))
        .stdout(contains("listPets").or(contains("list_pets")))
        .stdout(contains("getPetById").or(contains("get_pet_by_id")));

    let body = fs::read_to_string(&out).unwrap();
    assert!(body.contains("kind: bridge.mcp/v1"));
    assert!(body.contains("name: petstore"));
    assert!(body.contains("transport: stdio"));
    assert!(body.contains("base_url_env: PETSTORE_BASE_URL"));
    assert!(body.contains("base_url: https://petstore.example.com"));
    assert!(body.contains("token_env: PETSTORE_TOKEN"));
    // POST should be skipped with a diagnostic, not crash generation.
    assert!(!body.contains("adoptPet"), "POST must be skipped in MVP");
}

#[test]
fn generate_mcp_uses_openapi_server_when_base_url_env_is_omitted() {
    let tmp = TempDir::new().unwrap();
    let out = tmp.path().join("petstore.mcp.yaml");

    bridge()
        .args([
            "generate",
            "mcp",
            "--from",
            "openapi",
            &fixture_path("fixtures/openapi/petstore.yaml"),
            "--name",
            "petstore",
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .success();

    let body = fs::read_to_string(&out).unwrap();
    assert!(body.contains("base_url: https://petstore.example.com"));
}

#[test]
fn generate_mcp_is_deterministic() {
    let tmp = TempDir::new().unwrap();
    let out_a = tmp.path().join("a.yaml");
    let out_b = tmp.path().join("b.yaml");
    for out in [&out_a, &out_b] {
        bridge()
            .args([
                "generate",
                "mcp",
                "--from",
                "openapi",
                &fixture_path("fixtures/openapi/petstore.yaml"),
                "--name",
                "petstore",
                "--base-url-env",
                "PETSTORE_BASE_URL",
                "--out",
                out.to_str().unwrap(),
            ])
            .assert()
            .success();
    }
    assert_eq!(
        fs::read_to_string(out_a).unwrap(),
        fs::read_to_string(out_b).unwrap()
    );
}

#[test]
fn generate_mcp_refuses_overwrite_without_force() {
    let tmp = TempDir::new().unwrap();
    let out = tmp.path().join("m.yaml");
    fs::write(&out, "existing").unwrap();
    bridge()
        .args([
            "generate",
            "mcp",
            "--from",
            "openapi",
            &fixture_path("fixtures/openapi/petstore.yaml"),
            "--name",
            "petstore",
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(contains("already exists"));
}

#[test]
fn generate_mcp_rejects_unsupported_source() {
    let tmp = TempDir::new().unwrap();
    let out = tmp.path().join("m.yaml");
    bridge()
        .args([
            "generate",
            "mcp",
            "--from",
            "graphql",
            "./schema.graphql",
            "--name",
            "x",
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(contains("graphql"));
}

#[test]
fn generate_mcp_requires_base_url_when_spec_has_no_servers() {
    let tmp = TempDir::new().unwrap();
    let spec = tmp.path().join("no-servers.yaml");
    let out = tmp.path().join("m.yaml");
    fs::write(
        &spec,
        r#"
openapi: 3.0.3
info: { title: no-servers, version: "1" }
paths:
  /pets:
    get:
      responses:
        "200": { description: ok }
"#,
    )
    .unwrap();

    bridge()
        .args([
            "generate",
            "mcp",
            "--from",
            "openapi",
            spec.to_str().unwrap(),
            "--name",
            "petstore",
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(contains("no base URL available"));
}

#[test]
fn generate_mcp_keeps_tool_when_response_schema_is_recursive() {
    let tmp = TempDir::new().unwrap();
    let spec = tmp.path().join("recursive.yaml");
    let out = tmp.path().join("recursive.mcp.yaml");
    fs::write(
        &spec,
        r##"
openapi: 3.0.3
info: { title: recursive, version: "1" }
servers:
  - url: https://api.example.com
paths:
  /tree:
    get:
      operationId: getTree
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Node"
components:
  schemas:
    Node:
      type: object
      properties:
        name:
          type: string
        children:
          type: array
          items:
            $ref: "#/components/schemas/Node"
"##,
    )
    .unwrap();

    bridge()
        .args([
            "generate",
            "mcp",
            "--from",
            "openapi",
            spec.to_str().unwrap(),
            "--name",
            "recursive",
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(contains("getTree"))
        .stdout(contains("output schema omitted"));

    let body = fs::read_to_string(&out).unwrap();
    assert!(body.contains("getTree"));
    assert!(!body.contains("output_schema:"));
}

#[test]
fn mcp_serve_errors_on_missing_env_var() {
    let tmp = TempDir::new().unwrap();
    let out = tmp.path().join("petstore.mcp.yaml");
    bridge()
        .args([
            "generate",
            "mcp",
            "--from",
            "openapi",
            &fixture_path("fixtures/openapi/petstore.yaml"),
            "--name",
            "petstore",
            "--base-url-env",
            "PETSTORE_BASE_URL_MISSING_XYZ",
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .success();

    bridge()
        .env_remove("PETSTORE_BASE_URL_MISSING_XYZ")
        .args(["mcp", "serve", out.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(contains("PETSTORE_BASE_URL_MISSING_XYZ"));
}
