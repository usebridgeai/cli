// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Unit-ish coverage of the MCP subsystem. We re-include the source files via
// `#[path]` because `bridge-cli` is a binary crate with no library target.
// The module layout is chosen so that `crate::error` and `crate::mcp::*`
// inside the included files resolve to real modules in this test crate.

// Included sources contain items not exercised by these tests; suppress the
// resulting dead-code noise since the items exist for the production binary.
#![allow(dead_code)]

#[path = "../src/error.rs"]
mod error;

#[path = "../src/mcp/manifest.rs"]
pub mod mcp_manifest;
#[path = "../src/mcp/openapi.rs"]
pub mod mcp_openapi;
#[path = "../src/mcp/schema.rs"]
pub mod mcp_schema;
#[path = "../src/mcp/tool_mapper.rs"]
pub mod mcp_tool_mapper;

pub mod mcp {
    pub use crate::mcp_manifest as manifest;
    pub use crate::mcp_openapi as openapi;
    pub use crate::mcp_schema as schema;
    pub use crate::mcp_tool_mapper as tool_mapper;
}

use mcp::{manifest, openapi, schema, tool_mapper};
use std::path::PathBuf;

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

#[test]
fn openapi_parser_extracts_get_and_skips_post() {
    let parsed = openapi::parse(&fixture("fixtures/openapi/petstore.yaml")).unwrap();
    let methods: Vec<_> = parsed
        .operations
        .iter()
        .map(|op| (op.method.as_str(), op.path.as_str()))
        .collect();
    assert!(methods.contains(&("GET", "/pets")));
    assert!(methods.contains(&("GET", "/pets/{petId}")));
    assert!(methods.iter().all(|(m, _)| *m == "GET"));
    assert!(parsed.diagnostics.iter().any(|d| d.contains("POST")));
    assert_eq!(
        parsed.default_base_url.as_deref(),
        Some("https://petstore.example.com")
    );
}

#[test]
fn tool_mapper_uses_operation_id_and_annotates_readonly() {
    let parsed = openapi::parse(&fixture("fixtures/openapi/petstore.yaml")).unwrap();
    let tools = tool_mapper::map_operations(&parsed.operations).unwrap();
    let get_pet = tools
        .iter()
        .find(|t| t.name == "getPetById")
        .expect("getPetById tool");
    assert_eq!(get_pet.annotations.read_only_hint, Some(true));
    assert_eq!(get_pet.annotations.destructive_hint, Some(false));
    let required = get_pet
        .input_schema
        .get("required")
        .unwrap()
        .as_array()
        .unwrap();
    assert!(required.iter().any(|v| v == "petId"));
}

#[test]
fn manifest_roundtrips_yaml() {
    let parsed = openapi::parse(&fixture("fixtures/openapi/petstore.yaml")).unwrap();
    let tools = tool_mapper::map_operations(&parsed.operations).unwrap();
    let mut m = manifest::Manifest::new(
        "petstore".into(),
        manifest::Source::Openapi {
            path: "./spec.yaml".into(),
        },
        manifest::Runtime {
            transport: manifest::Transport::Stdio,
            base_url_env: Some("X".into()),
            base_url: None,
            auth: Some(manifest::Auth::Bearer {
                token_env: "Y".into(),
            }),
        },
    );
    m.tools = tools;
    let yaml = m.to_yaml().unwrap();
    let back: manifest::Manifest = serde_yaml::from_str(&yaml).unwrap();
    back.validate().unwrap();
    assert_eq!(back.name, m.name);
    assert_eq!(back.tools.len(), m.tools.len());
}

#[test]
fn schema_validator_rejects_unknown_and_missing_fields() {
    let schema_val = serde_json::json!({
        "type": "object",
        "properties": {
            "petId": { "type": "string" }
        },
        "required": ["petId"],
        "additionalProperties": false
    });

    schema::validate_input("t", &schema_val, &serde_json::json!({"petId": "7"})).unwrap();

    let missing = schema::validate_input("t", &schema_val, &serde_json::json!({}));
    assert!(missing.is_err());

    let unknown = schema::validate_input(
        "t",
        &schema_val,
        &serde_json::json!({"petId": "7", "bogus": true}),
    );
    assert!(unknown.is_err());

    let wrong_type = schema::validate_input("t", &schema_val, &serde_json::json!({"petId": 7}));
    assert!(wrong_type.is_err());
}

#[test]
fn name_fallback_is_deterministic_when_no_operation_id() {
    let spec = r#"
openapi: 3.0.3
info: { title: t, version: "1" }
paths:
  /users/{id}/items:
    get:
      parameters:
        - name: id
          in: path
          required: true
          schema: { type: string }
      responses:
        "200": { description: ok }
"#;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), spec).unwrap();
    let parsed = openapi::parse(tmp.path()).unwrap();
    let tools = tool_mapper::map_operations(&parsed.operations).unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "get_users_by_id_items");
}

#[test]
fn parser_expands_server_variables_with_defaults() {
    let spec = r#"
openapi: 3.0.3
info: { title: templated, version: "1" }
servers:
  - url: https://{region}.example.com/{stage}
    variables:
      region:
        default: eu
      stage:
        default: api
paths:
  /users:
    get:
      responses:
        "200": { description: ok }
"#;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), spec).unwrap();

    let parsed = openapi::parse(tmp.path()).unwrap();
    assert_eq!(
        parsed.default_base_url.as_deref(),
        Some("https://eu.example.com/api")
    );
}

#[test]
fn parameter_schema_refs_are_inlined_into_tool_input_schema() {
    let spec = r##"
openapi: 3.0.3
info: { title: refs, version: "1" }
servers:
  - url: https://api.example.com
paths:
  /pets:
    get:
      parameters:
        - name: petId
          in: query
          required: true
          schema:
            $ref: "#/components/schemas/PetId"
      responses:
        "200": { description: ok }
components:
  schemas:
    PetId:
      type: string
      pattern: "^[0-9]+$"
"##;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), spec).unwrap();

    let parsed = openapi::parse(tmp.path()).unwrap();
    let tools = tool_mapper::map_operations(&parsed.operations).unwrap();
    let tool = &tools[0];
    let pet_id = &tool.input_schema["properties"]["petId"];

    assert_eq!(pet_id["type"], "string");
    assert_eq!(pet_id["pattern"], "^[0-9]+$");
    assert!(pet_id.get("$ref").is_none());
}

#[test]
fn recursive_response_schema_omits_output_schema_but_keeps_operation() {
    let spec = r##"
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
"##;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), spec).unwrap();

    let parsed = openapi::parse(tmp.path()).unwrap();
    assert_eq!(parsed.operations.len(), 1);
    assert_eq!(
        parsed.operations[0].operation_id.as_deref(),
        Some("getTree")
    );
    assert!(parsed.operations[0].response_schema.is_none());
    assert!(parsed
        .diagnostics
        .iter()
        .any(|d| d.contains("output schema omitted")));
}
