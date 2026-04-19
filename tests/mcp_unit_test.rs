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

#[path = "../src/mcp/db_introspector.rs"]
pub mod mcp_db_introspector;
#[path = "../src/mcp/db_tool_planner.rs"]
pub mod mcp_db_tool_planner;
#[path = "../src/mcp/manifest.rs"]
pub mod mcp_manifest;
#[path = "../src/mcp/openapi.rs"]
pub mod mcp_openapi;
#[path = "../src/mcp/schema.rs"]
pub mod mcp_schema;
#[path = "../src/mcp/tool_mapper.rs"]
pub mod mcp_tool_mapper;

pub mod mcp {
    pub use crate::mcp_db_introspector as db_introspector;
    pub use crate::mcp_db_tool_planner as db_tool_planner;
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
            "petId": { "type": "string", "enum": ["7", "8"] },
            "limit": { "type": "integer", "minimum": 1, "maximum": 5 }
        },
        "required": ["petId"],
        "additionalProperties": false
    });

    schema::validate_input(
        "t",
        &schema_val,
        &serde_json::json!({"petId": "7", "limit": 3}),
    )
    .unwrap();

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

    let bad_enum = schema::validate_input("t", &schema_val, &serde_json::json!({"petId": "9"}));
    assert!(bad_enum.is_err());

    let bad_min = schema::validate_input(
        "t",
        &schema_val,
        &serde_json::json!({"petId": "7", "limit": 0}),
    );
    assert!(bad_min.is_err());

    let bad_max = schema::validate_input(
        "t",
        &schema_val,
        &serde_json::json!({"petId": "7", "limit": 9}),
    );
    assert!(bad_max.is_err());
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

// ─── Phase 2: DB → MCP manifest ──────────────────────────────────────────────

use mcp::db_introspector::{ColumnCategory, ColumnMetadata, DbMetadata, TableKind, TableMetadata};
use mcp::db_tool_planner;
use mcp::manifest::{Execute, SqlSelectMode};

fn col(name: &str, udt: &str, nullable: bool) -> ColumnMetadata {
    ColumnMetadata {
        name: name.into(),
        data_type: udt.into(),
        udt_name: udt.into(),
        is_nullable: nullable,
        comment: None,
        category: mcp::db_introspector::classify(udt),
    }
}

fn table(name: &str, cols: Vec<ColumnMetadata>, pk: Vec<&str>) -> TableMetadata {
    TableMetadata {
        name: name.into(),
        kind: TableKind::Table,
        columns: cols,
        primary_key: pk.into_iter().map(String::from).collect(),
        unique_single_keys: vec![],
        comment: None,
    }
}

#[test]
fn classify_recognizes_common_postgres_types() {
    assert_eq!(
        mcp::db_introspector::classify("int4"),
        ColumnCategory::Integer
    );
    assert_eq!(mcp::db_introspector::classify("text"), ColumnCategory::Text);
    assert_eq!(
        mcp::db_introspector::classify("timestamptz"),
        ColumnCategory::Timestamp
    );
    assert_eq!(
        mcp::db_introspector::classify("numeric"),
        ColumnCategory::Numeric
    );
    assert_eq!(
        mcp::db_introspector::classify("jsonb"),
        ColumnCategory::Json
    );
    assert_eq!(
        mcp::db_introspector::classify("bytea"),
        ColumnCategory::Unsupported
    );
    assert_eq!(mcp::db_introspector::classify("uuid"), ColumnCategory::Uuid);
}

#[test]
fn db_manifest_roundtrips_yaml_with_sql_select_plan() {
    let md = DbMetadata {
        schema: "public".into(),
        tables: vec![table(
            "customers",
            vec![
                col("id", "int4", false),
                col("email", "text", false),
                col("created_at", "timestamptz", false),
            ],
            vec!["id"],
        )],
    };
    let planned = db_tool_planner::plan(&md, "analytics");
    let mut m = mcp::manifest::Manifest::new(
        "analytics".into(),
        mcp::manifest::Source::Db {
            connection: "analytics".into(),
            dialect: "postgres".into(),
            schema: "public".into(),
        },
        mcp::manifest::Runtime {
            transport: mcp::manifest::Transport::Stdio,
            base_url_env: None,
            base_url: None,
            auth: None,
        },
    );
    m.tools = planned.tools;
    m.validate().unwrap();

    let yaml = m.to_yaml().unwrap();
    assert!(yaml.contains("type: db"));
    assert!(yaml.contains("dialect: postgres"));
    assert!(yaml.contains("type: sql_select"));
    assert!(yaml.contains("connection_ref: analytics"));

    let back: mcp::manifest::Manifest = serde_yaml::from_str(&yaml).unwrap();
    back.validate().unwrap();
    assert_eq!(back.tools.len(), m.tools.len());
    let list = back
        .tools
        .iter()
        .find(|t| t.name == "list_customers")
        .unwrap();
    let Execute::SqlSelect(plan) = &list.execute else {
        panic!("expected sql_select")
    };
    assert_eq!(plan.mode, SqlSelectMode::List);
    assert_eq!(plan.connection_ref, "analytics");
    assert_eq!(plan.table, "customers");
    assert_eq!(
        plan.column_types.get("created_at"),
        Some(&mcp::manifest::SqlColumnType::Timestamp)
    );
    assert!(plan.filterable_columns.contains(&"email".to_string()));
    assert!(plan.sortable_columns.contains(&"created_at".to_string()));
}

#[test]
fn list_tool_input_schema_enforces_sort_allowlist_and_limit_bounds() {
    let md = DbMetadata {
        schema: "public".into(),
        tables: vec![table(
            "orders",
            vec![col("id", "int4", false), col("status", "text", false)],
            vec!["id"],
        )],
    };
    let out = db_tool_planner::plan(&md, "db");
    let list = out.tools.iter().find(|t| t.name == "list_orders").unwrap();
    let props = &list.input_schema["properties"];
    let order_by = &props["order_by"];
    let allowed: Vec<&str> = order_by["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(allowed.contains(&"id"));
    assert!(allowed.contains(&"status"));

    assert_eq!(props["limit"]["maximum"], db_tool_planner::MAX_LIMIT);
    assert_eq!(props["limit"]["minimum"], 1);
    assert_eq!(props["offset"]["minimum"], 0);

    // additionalProperties is false so unknown filters are rejected before
    // reaching the SQL executor.
    assert_eq!(list.input_schema["additionalProperties"], false);
}

#[test]
fn plan_uses_single_column_unique_key_when_no_pk() {
    let mut t = table(
        "customers_v",
        vec![col("slug", "text", false), col("name", "text", false)],
        vec![],
    );
    t.kind = TableKind::View;
    t.unique_single_keys = vec!["slug".into()];
    let md = DbMetadata {
        schema: "public".into(),
        tables: vec![t],
    };
    let out = db_tool_planner::plan(&md, "db");
    assert!(out
        .tools
        .iter()
        .any(|t| t.name == "get_customers_v_by_slug"));
}

#[test]
fn unsupported_columns_are_omitted_from_output_schema_and_plan() {
    let md = DbMetadata {
        schema: "public".into(),
        tables: vec![table(
            "events",
            vec![
                col("id", "int4", false),
                col("payload", "jsonb", true),
                col("blob", "bytea", true),
            ],
            vec!["id"],
        )],
    };

    let out = db_tool_planner::plan(&md, "db");
    let list = out.tools.iter().find(|t| t.name == "list_events").unwrap();
    let Execute::SqlSelect(plan) = &list.execute else {
        panic!("expected SqlSelect")
    };

    assert!(plan.selectable_columns.contains(&"id".to_string()));
    assert!(plan.selectable_columns.contains(&"payload".to_string()));
    assert!(!plan.selectable_columns.contains(&"blob".to_string()));
    assert!(out
        .diagnostics
        .iter()
        .any(|d| d.contains("omitted unsupported output columns")));

    let row_props = list.output_schema.as_ref().unwrap()["properties"]["rows"]["items"]
        ["properties"]
        .as_object()
        .unwrap();
    assert!(row_props.contains_key("id"));
    assert!(row_props.contains_key("payload"));
    assert!(!row_props.contains_key("blob"));
}
