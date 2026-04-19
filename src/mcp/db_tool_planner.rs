// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Convert introspected database metadata into deterministic MCP tool
// definitions. The planner is pure: given identical metadata and the same
// connection name, it produces bit-identical output so `bridge generate`
// is diff-friendly across regenerations.

use crate::mcp::db_introspector::{
    ColumnCategory, ColumnMetadata, DbMetadata, TableKind, TableMetadata,
};
use crate::mcp::manifest::{
    Execute, LimitSpec, SqlColumnType, SqlSelectExecute, SqlSelectMode, Tool, ToolAnnotations,
};
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 200;

/// Columns we never expose as filters even when their type is otherwise
/// filterable — they tend to be freeform and lead to confusing tool shapes.
fn is_always_unfilterable(col: &ColumnMetadata) -> bool {
    matches!(
        col.category,
        ColumnCategory::Json | ColumnCategory::Unsupported
    )
}

/// Output of a planning pass. Diagnostics describe objects we intentionally
/// skipped so the CLI can surface them.
pub struct PlanOutput {
    pub tools: Vec<Tool>,
    pub diagnostics: Vec<String>,
}

pub fn plan(metadata: &DbMetadata, connection_ref: &str) -> PlanOutput {
    let mut tools = Vec::new();
    let mut diagnostics = Vec::new();
    let mut used_names: HashSet<String> = HashSet::new();

    for table in &metadata.tables {
        if table.columns.is_empty() {
            diagnostics.push(format!(
                "skipped {}.{}: no columns visible to the introspection query",
                metadata.schema, table.name
            ));
            continue;
        }

        let projected = projected_columns(table);
        if projected.selectable_columns.is_empty() {
            diagnostics.push(format!(
                "skipped {}.{}: no columns with supported output encodings",
                metadata.schema, table.name
            ));
            continue;
        }
        if !projected.omitted_columns.is_empty() {
            diagnostics.push(format!(
                "omitted unsupported output columns from {}.{}: {}",
                metadata.schema,
                table.name,
                projected.omitted_columns.join(", ")
            ));
        }

        // Always emit a list_* tool for tables and views.
        let list_tool =
            plan_list_tool(metadata, table, &projected, connection_ref, &mut used_names);
        tools.push(list_tool);

        // get_*_by_<pk> only when a deterministic single-column key is available.
        match choose_lookup_key(table) {
            Some(key_col) => {
                let get_tool = plan_get_tool(
                    metadata,
                    table,
                    &projected,
                    &key_col,
                    connection_ref,
                    &mut used_names,
                );
                tools.push(get_tool);
            }
            None => {
                if matches!(table.kind, TableKind::Table) {
                    if table.primary_key.len() > 1 {
                        diagnostics.push(format!(
                            "no get_* tool for {}.{}: composite primary key ({})",
                            metadata.schema,
                            table.name,
                            table.primary_key.join(", ")
                        ));
                    } else if table.primary_key.is_empty() {
                        diagnostics.push(format!(
                            "no get_* tool for {}.{}: no primary key or single-column unique key",
                            metadata.schema, table.name
                        ));
                    }
                }
            }
        }
    }

    PlanOutput { tools, diagnostics }
}

struct ProjectedColumns {
    selectable_columns: Vec<String>,
    column_types: IndexMap<String, SqlColumnType>,
    omitted_columns: Vec<String>,
}

fn projected_columns(table: &TableMetadata) -> ProjectedColumns {
    let mut selectable_columns = Vec::new();
    let mut column_types = IndexMap::new();
    let mut omitted_columns = Vec::new();

    for col in &table.columns {
        if let Some(column_type) = manifest_column_type(col.category) {
            selectable_columns.push(col.name.clone());
            column_types.insert(col.name.clone(), column_type);
        } else {
            omitted_columns.push(col.name.clone());
        }
    }

    ProjectedColumns {
        selectable_columns,
        column_types,
        omitted_columns,
    }
}

fn manifest_column_type(category: ColumnCategory) -> Option<SqlColumnType> {
    match category {
        ColumnCategory::Integer => Some(SqlColumnType::Integer),
        ColumnCategory::Float => Some(SqlColumnType::Float),
        ColumnCategory::Numeric => Some(SqlColumnType::Numeric),
        ColumnCategory::Boolean => Some(SqlColumnType::Boolean),
        ColumnCategory::Text => Some(SqlColumnType::Text),
        ColumnCategory::Timestamp => Some(SqlColumnType::Timestamp),
        ColumnCategory::Uuid => Some(SqlColumnType::Uuid),
        ColumnCategory::Json => Some(SqlColumnType::Json),
        ColumnCategory::Unsupported => None,
    }
}

fn choose_lookup_key(table: &TableMetadata) -> Option<String> {
    if table.primary_key.len() == 1 {
        let name = &table.primary_key[0];
        if table
            .columns
            .iter()
            .any(|c| &c.name == name && c.category.is_filterable())
        {
            return Some(name.clone());
        }
    }
    // Views never have a PK, but a single-column unique constraint is
    // equally deterministic.
    for uk in &table.unique_single_keys {
        if table
            .columns
            .iter()
            .any(|c| &c.name == uk && c.category.is_filterable())
        {
            return Some(uk.clone());
        }
    }
    None
}

fn plan_list_tool(
    meta: &DbMetadata,
    table: &TableMetadata,
    projected: &ProjectedColumns,
    connection_ref: &str,
    used: &mut HashSet<String>,
) -> Tool {
    let base_name = dedupe(format!("list_{}", table.name), used);
    let filterable: Vec<String> = table
        .columns
        .iter()
        .filter(|c| c.category.is_filterable() && !is_always_unfilterable(c))
        .map(|c| c.name.clone())
        .collect();
    let sortable: Vec<String> = table
        .columns
        .iter()
        .filter(|c| c.category.is_sortable() && !is_always_unfilterable(c))
        .map(|c| c.name.clone())
        .collect();

    let input_schema = build_list_input_schema(table, &filterable, &sortable);
    let output_schema = Some(build_list_output_schema(table, projected));

    let description = Some(table.comment.clone().unwrap_or_else(|| {
        format!(
            "List rows from {}.{} with optional filters, sorting, and pagination.",
            meta.schema, table.name
        )
    }));

    Tool {
        name: base_name,
        description,
        annotations: read_only_annotations(),
        input_schema,
        output_schema,
        execute: Execute::SqlSelect(SqlSelectExecute {
            connection_ref: connection_ref.to_string(),
            schema: meta.schema.clone(),
            table: table.name.clone(),
            mode: SqlSelectMode::List,
            selectable_columns: projected.selectable_columns.clone(),
            column_types: projected.column_types.clone(),
            filterable_columns: filterable,
            sortable_columns: sortable,
            key_column: None,
            limit: Some(LimitSpec {
                default: DEFAULT_LIMIT,
                max: MAX_LIMIT,
            }),
        }),
    }
}

fn plan_get_tool(
    meta: &DbMetadata,
    table: &TableMetadata,
    projected: &ProjectedColumns,
    key_col: &str,
    connection_ref: &str,
    used: &mut HashSet<String>,
) -> Tool {
    let singular = singularize(&table.name);
    let base_name = dedupe(format!("get_{}_by_{}", singular, key_col), used);
    let key_column = table
        .columns
        .iter()
        .find(|c| c.name == key_col)
        .expect("key column must exist in metadata");

    let mut properties = Map::new();
    properties.insert(key_col.to_string(), column_input_schema(key_column));
    let input_schema = json!({
        "type": "object",
        "properties": properties,
        "required": [key_col],
        "additionalProperties": false,
    });

    let output_schema = Some(build_get_output_schema(table, projected));

    let description = Some(
        table
            .comment
            .clone()
            .map(|c| format!("{c} Fetch a single row by {key_col}."))
            .unwrap_or_else(|| {
                format!(
                    "Fetch a single row from {}.{} by {}.",
                    meta.schema, table.name, key_col
                )
            }),
    );

    Tool {
        name: base_name,
        description,
        annotations: read_only_annotations(),
        input_schema,
        output_schema,
        execute: Execute::SqlSelect(SqlSelectExecute {
            connection_ref: connection_ref.to_string(),
            schema: meta.schema.clone(),
            table: table.name.clone(),
            mode: SqlSelectMode::GetByKey,
            selectable_columns: projected.selectable_columns.clone(),
            column_types: projected.column_types.clone(),
            filterable_columns: vec![key_col.to_string()],
            sortable_columns: Vec::new(),
            key_column: Some(key_col.to_string()),
            limit: None,
        }),
    }
}

fn read_only_annotations() -> ToolAnnotations {
    ToolAnnotations {
        read_only_hint: Some(true),
        destructive_hint: Some(false),
        idempotent_hint: Some(true),
        open_world_hint: Some(false),
        title: None,
    }
}

fn build_list_input_schema(
    table: &TableMetadata,
    filterable: &[String],
    sortable: &[String],
) -> Value {
    let mut properties = Map::new();
    for name in filterable {
        if let Some(col) = table.columns.iter().find(|c| &c.name == name) {
            properties.insert(name.clone(), column_input_schema(col));
        }
    }
    properties.insert(
        "limit".to_string(),
        json!({
            "type": "integer",
            "minimum": 1,
            "maximum": MAX_LIMIT,
            "description": format!(
                "Maximum rows to return (default {DEFAULT_LIMIT}, hard max {MAX_LIMIT})."
            ),
        }),
    );
    properties.insert(
        "offset".to_string(),
        json!({
            "type": "integer",
            "minimum": 0,
            "description": "Rows to skip before returning results.",
        }),
    );
    if !sortable.is_empty() {
        properties.insert(
            "order_by".to_string(),
            json!({
                "type": "string",
                "enum": sortable.iter().cloned().collect::<Vec<_>>(),
                "description": "Column to sort by. Restricted to an allowlist.",
            }),
        );
        properties.insert(
            "order_direction".to_string(),
            json!({
                "type": "string",
                "enum": ["asc", "desc"],
                "description": "Sort direction. Defaults to asc.",
            }),
        );
    }

    json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    })
}

fn build_list_output_schema(table: &TableMetadata, projected: &ProjectedColumns) -> Value {
    json!({
        "type": "object",
        "properties": {
            "ok": { "type": "boolean" },
            "count": { "type": "integer" },
            "rows": {
                "type": "array",
                "items": row_schema(table, projected),
            }
        }
    })
}

fn build_get_output_schema(table: &TableMetadata, projected: &ProjectedColumns) -> Value {
    json!({
        "type": "object",
        "properties": {
            "ok": { "type": "boolean" },
            "found": { "type": "boolean" },
            "row": row_schema(table, projected),
        }
    })
}

fn row_schema(table: &TableMetadata, projected: &ProjectedColumns) -> Value {
    let mut props = Map::new();
    for col_name in &projected.selectable_columns {
        let col = table
            .columns
            .iter()
            .find(|c| &c.name == col_name)
            .expect("projected column must exist in metadata");
        let mut prop = Map::new();
        if let Some(type_name) = output_schema_type(col.category) {
            prop.insert("type".into(), Value::String(type_name.to_string()));
            if col.is_nullable {
                // A nullable column can come back as either its real type or null;
                // JSON Schema draft-07 style via array-of-types is the
                // lowest-common-denominator way to express that in MCP clients.
                prop.insert("type".into(), json!([type_name, "null"]));
            }
        }
        if let Some(comment) = &col.comment {
            prop.insert("description".into(), Value::String(comment.clone()));
        }
        props.insert(col.name.clone(), Value::Object(prop));
    }
    json!({
        "type": "object",
        "properties": props,
    })
}

fn output_schema_type(category: ColumnCategory) -> Option<&'static str> {
    match category {
        ColumnCategory::Integer => Some("integer"),
        ColumnCategory::Float => Some("number"),
        ColumnCategory::Numeric => Some("string"),
        ColumnCategory::Boolean => Some("boolean"),
        ColumnCategory::Text | ColumnCategory::Timestamp | ColumnCategory::Uuid => Some("string"),
        ColumnCategory::Json | ColumnCategory::Unsupported => None,
    }
}

fn column_input_schema(col: &ColumnMetadata) -> Value {
    let mut obj = Map::new();
    obj.insert(
        "type".into(),
        Value::String(col.category.input_json_type().to_string()),
    );
    let desc = col
        .comment
        .clone()
        .unwrap_or_else(|| format!("Filter by {} (postgres type: {}).", col.name, col.udt_name));
    obj.insert("description".into(), Value::String(desc));
    Value::Object(obj)
}

fn dedupe(base: String, used: &mut HashSet<String>) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    for i in 2..u32::MAX {
        let candidate = format!("{base}_{i}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    base
}

/// Lightweight pluralization reversal. English-only, rule-based — intentionally
/// small so behaviour is predictable across schemas. Deterministic: same input
/// yields the same output, no word lists, no surprises.
pub fn singularize(name: &str) -> String {
    if name.len() <= 3 {
        return name.to_string();
    }
    if let Some(stem) = name.strip_suffix("ies") {
        if !stem.is_empty() {
            return format!("{stem}y");
        }
    }
    if let Some(stem) = name.strip_suffix("sses") {
        return format!("{stem}ss");
    }
    if let Some(stem) = name.strip_suffix("ches") {
        return format!("{stem}ch");
    }
    if let Some(stem) = name.strip_suffix("shes") {
        return format!("{stem}sh");
    }
    if let Some(stem) = name.strip_suffix("xes") {
        return format!("{stem}x");
    }
    if name.ends_with("ss") {
        return name.to_string();
    }
    if let Some(stem) = name.strip_suffix('s') {
        return stem.to_string();
    }
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singularize_common_patterns() {
        assert_eq!(singularize("customers"), "customer");
        assert_eq!(singularize("orders"), "order");
        assert_eq!(singularize("companies"), "company");
        assert_eq!(singularize("addresses"), "address");
        assert_eq!(singularize("invoice_items"), "invoice_item");
        assert_eq!(singularize("boxes"), "box");
        // Short words fall through unchanged — avoids "us" -> "u" style
        // mangling for words we can't meaningfully singularize.
        assert_eq!(singularize("data"), "data");
    }

    fn make_table(name: &str, cols: Vec<(&str, &str, bool)>, pk: Vec<&str>) -> TableMetadata {
        TableMetadata {
            name: name.to_string(),
            kind: TableKind::Table,
            columns: cols
                .into_iter()
                .map(|(n, udt, null)| ColumnMetadata {
                    name: n.to_string(),
                    data_type: udt.to_string(),
                    udt_name: udt.to_string(),
                    is_nullable: null,
                    comment: None,
                    category: crate::mcp::db_introspector::classify(udt),
                })
                .collect(),
            primary_key: pk.into_iter().map(String::from).collect(),
            unique_single_keys: vec![],
            comment: None,
        }
    }

    #[test]
    fn plan_generates_list_and_get_for_table_with_pk() {
        let md = DbMetadata {
            schema: "public".into(),
            tables: vec![make_table(
                "customers",
                vec![
                    ("id", "int4", false),
                    ("email", "text", false),
                    ("created_at", "timestamptz", false),
                ],
                vec!["id"],
            )],
        };
        let out = plan(&md, "analytics");
        let names: Vec<&str> = out.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["list_customers", "get_customer_by_id"]);
    }

    #[test]
    fn plan_skips_get_for_composite_pk() {
        let md = DbMetadata {
            schema: "public".into(),
            tables: vec![make_table(
                "line_items",
                vec![
                    ("a", "int4", false),
                    ("b", "int4", false),
                    ("qty", "int4", false),
                ],
                vec!["a", "b"],
            )],
        };
        let out = plan(&md, "db");
        assert!(out.tools.iter().any(|t| t.name == "list_line_items"));
        assert!(!out.tools.iter().any(|t| t.name.starts_with("get_")));
        assert!(out.diagnostics.iter().any(|d| d.contains("composite")));
    }

    #[test]
    fn plan_excludes_unsupported_types_from_filters() {
        let md = DbMetadata {
            schema: "public".into(),
            tables: vec![make_table(
                "events",
                vec![
                    ("id", "int4", false),
                    ("payload", "jsonb", false),
                    ("blob", "bytea", false),
                    ("name", "text", false),
                ],
                vec!["id"],
            )],
        };
        let out = plan(&md, "db");
        let list = out.tools.iter().find(|t| t.name == "list_events").unwrap();
        let Execute::SqlSelect(plan) = &list.execute else {
            panic!("expected SqlSelect")
        };
        assert!(plan.filterable_columns.contains(&"id".to_string()));
        assert!(plan.filterable_columns.contains(&"name".to_string()));
        assert!(!plan.filterable_columns.contains(&"payload".to_string()));
        assert!(!plan.filterable_columns.contains(&"blob".to_string()));
        // But the column is still projected so list_* returns it in rows.
        assert!(plan.selectable_columns.contains(&"payload".to_string()));
    }

    #[test]
    fn plan_is_deterministic_for_identical_metadata() {
        let md = DbMetadata {
            schema: "public".into(),
            tables: vec![make_table(
                "orders",
                vec![("id", "int4", false), ("status", "text", false)],
                vec!["id"],
            )],
        };
        let a = plan(&md, "db");
        let b = plan(&md, "db");
        let a_yaml = serde_yaml::to_string(&a.tools).unwrap();
        let b_yaml = serde_yaml::to_string(&b.tools).unwrap();
        assert_eq!(a_yaml, b_yaml);
    }
}
