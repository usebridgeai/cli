// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// The `bridge.mcp/v1` manifest: the single source of truth that both
// generation and runtime operate on. Designed so Bridge Cloud can
// consume the exact same artifact without format changes.

use crate::error::{BridgeError, Result};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const MANIFEST_KIND: &str = "bridge.mcp/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub kind: String,
    pub name: String,
    pub source: Source,
    pub runtime: Runtime,
    #[serde(default)]
    pub tools: Vec<Tool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    Openapi {
        path: String,
    },
    Db {
        /// Name of the Bridge connection (in bridge.yaml) to resolve at runtime.
        connection: String,
        /// SQL dialect. `postgres` is the only supported value in this phase.
        dialect: String,
        /// Schema the manifest was generated from. Purely informational at
        /// runtime — tool execution plans carry their own `schema` field.
        schema: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Runtime {
    /// Always `stdio` in MVP. Kept explicit so future transports can be added
    /// without breaking manifest v1.
    pub transport: Transport,

    /// Environment variable holding the API base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url_env: Option<String>,

    /// Literal base URL, used only when no env var is set (e.g. for examples).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Stdio,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Auth {
    Bearer { token_env: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "ToolAnnotations::is_empty")]
    pub annotations: ToolAnnotations,
    pub input_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    pub execute: Execute,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolAnnotations {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "readOnlyHint"
    )]
    pub read_only_hint: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "destructiveHint"
    )]
    pub destructive_hint: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "idempotentHint"
    )]
    pub idempotent_hint: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "openWorldHint"
    )]
    pub open_world_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "title")]
    pub title: Option<String>,
}

impl ToolAnnotations {
    pub fn is_empty(&self) -> bool {
        self.read_only_hint.is_none()
            && self.destructive_hint.is_none()
            && self.idempotent_hint.is_none()
            && self.open_world_hint.is_none()
            && self.title.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Execute {
    Http(HttpExecute),
    SqlSelect(SqlSelectExecute),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SqlSelectExecute {
    /// Name of the Bridge connection to resolve at runtime.
    pub connection_ref: String,
    pub schema: String,
    pub table: String,
    /// Whether this plan returns many rows (list_*) or a single row by key (get_*_by_*).
    pub mode: SqlSelectMode,
    /// Columns projected in the SELECT clause (whitelisted at generation time).
    pub selectable_columns: Vec<String>,
    /// Stable per-column output types used by the runtime to choose safe SQL
    /// casts and JSON decoding. Keys must exactly match `selectable_columns`.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub column_types: IndexMap<String, SqlColumnType>,
    /// Columns allowed as equality filters. Must be a subset of selectable_columns.
    #[serde(default)]
    pub filterable_columns: Vec<String>,
    /// Columns allowed in ORDER BY. Must be a subset of selectable_columns.
    #[serde(default)]
    pub sortable_columns: Vec<String>,
    /// Column used by `get_*_by_*` plans. Must also be in filterable_columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_column: Option<String>,
    /// Pagination bounds. Only applies to `mode: list`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<LimitSpec>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SqlSelectMode {
    List,
    GetByKey,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SqlColumnType {
    Integer,
    Float,
    Numeric,
    Boolean,
    Text,
    Timestamp,
    Uuid,
    Json,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct LimitSpec {
    pub default: u32,
    pub max: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HttpExecute {
    pub method: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// Ordered list of parameters, used to map tool input fields to path/query slots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<HttpParam>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HttpParam {
    pub name: String,
    pub location: ParamLocation,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ParamLocation {
    Path,
    Query,
}

impl Manifest {
    pub fn new(name: String, source: Source, runtime: Runtime) -> Self {
        Self {
            kind: MANIFEST_KIND.to_string(),
            name,
            source,
            runtime,
            tools: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.kind != MANIFEST_KIND {
            return Err(BridgeError::Manifest(format!(
                "unsupported manifest kind '{}', expected '{}'",
                self.kind, MANIFEST_KIND
            )));
        }
        if self.name.trim().is_empty() {
            return Err(BridgeError::Manifest("manifest `name` is empty".into()));
        }
        let mut seen: IndexMap<&str, ()> = IndexMap::new();
        for tool in &self.tools {
            if tool.name.trim().is_empty() {
                return Err(BridgeError::Manifest("tool name is empty".into()));
            }
            if seen.insert(tool.name.as_str(), ()).is_some() {
                return Err(BridgeError::Manifest(format!(
                    "duplicate tool name '{}'",
                    tool.name
                )));
            }
            if !tool.input_schema.is_object() {
                return Err(BridgeError::Manifest(format!(
                    "tool '{}' input_schema must be a JSON object",
                    tool.name
                )));
            }
            if let Execute::SqlSelect(plan) = &tool.execute {
                validate_sql_plan(plan, &tool.name)?;
            }
        }
        Ok(())
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            BridgeError::Manifest(format!("cannot read manifest at {}: {}", path.display(), e))
        })?;
        let manifest: Manifest = serde_yaml::from_str(&raw)
            .map_err(|e| BridgeError::Manifest(format!("invalid manifest YAML: {e}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn to_yaml(&self) -> Result<String> {
        // Emit a stable header comment so the file is self-identifying, followed by
        // serde_yaml's deterministic output.
        let body = serde_yaml::to_string(self)?;
        Ok(format!(
            "# Generated by bridge. Do not edit by hand unless you know what you are doing.\n# Manifest kind: {MANIFEST_KIND}\n{body}"
        ))
    }
}

/// Validate a `SqlSelectExecute` plan loaded from a manifest. All identifier
/// fields come from a YAML file that a user could hand-edit, so we reject
/// anything that isn't a safe Postgres identifier before it reaches `quote_ident`.
///
/// The `BridgeError::InvalidIdentifier` variant already exists for this purpose.
fn validate_sql_plan(plan: &SqlSelectExecute, tool_name: &str) -> Result<()> {
    let check = |name: &str, ctx: &str| {
        if !is_safe_identifier(name) {
            Err(BridgeError::Manifest(format!(
                "tool '{tool_name}': {ctx} '{name}' is not a valid identifier \
                 (only [a-zA-Z0-9_] allowed)"
            )))
        } else {
            Ok(())
        }
    };

    check(&plan.schema, "schema")?;
    check(&plan.table, "table")?;
    if plan.connection_ref.trim().is_empty() {
        return Err(BridgeError::Manifest(format!(
            "tool '{tool_name}': `connection_ref` is empty"
        )));
    }
    for col in &plan.selectable_columns {
        check(col, "selectable_columns entry")?;
    }
    for col in plan.column_types.keys() {
        check(col, "column_types key")?;
    }
    for col in &plan.filterable_columns {
        check(col, "filterable_columns entry")?;
    }
    for col in &plan.sortable_columns {
        check(col, "sortable_columns entry")?;
    }
    if let Some(key) = &plan.key_column {
        check(key, "key_column")?;
    }
    for col in &plan.filterable_columns {
        if !plan
            .selectable_columns
            .iter()
            .any(|selected| selected == col)
        {
            return Err(BridgeError::Manifest(format!(
                "tool '{tool_name}': filterable column '{col}' is not selectable"
            )));
        }
    }
    for col in &plan.sortable_columns {
        if !plan
            .selectable_columns
            .iter()
            .any(|selected| selected == col)
        {
            return Err(BridgeError::Manifest(format!(
                "tool '{tool_name}': sortable column '{col}' is not selectable"
            )));
        }
    }
    if let Some(key) = &plan.key_column {
        if !plan
            .filterable_columns
            .iter()
            .any(|filterable| filterable == key)
        {
            return Err(BridgeError::Manifest(format!(
                "tool '{tool_name}': key_column '{key}' must also be filterable"
            )));
        }
    }
    if !plan.column_types.is_empty() {
        for col in &plan.selectable_columns {
            if !plan.column_types.contains_key(col) {
                return Err(BridgeError::Manifest(format!(
                    "tool '{tool_name}': selectable column '{col}' is missing from column_types"
                )));
            }
        }
        for col in plan.column_types.keys() {
            if !plan
                .selectable_columns
                .iter()
                .any(|selected| selected == col)
            {
                return Err(BridgeError::Manifest(format!(
                    "tool '{tool_name}': column_types entry '{col}' is not selectable"
                )));
            }
        }
    }
    if let Some(spec) = plan.limit {
        // The hard row cap is enforced at runtime, but surface the mismatch
        // at load time so misconfigured manifests fail loudly before any query
        // is executed.
        const HARD_ROW_CAP: u32 = 1000;
        if spec.max > HARD_ROW_CAP {
            return Err(BridgeError::Manifest(format!(
                "tool '{tool_name}': limit.max ({}) exceeds the server hard cap of {HARD_ROW_CAP}; \
                 lower it in the manifest",
                spec.max
            )));
        }
        if spec.default > spec.max {
            return Err(BridgeError::Manifest(format!(
                "tool '{tool_name}': limit.default ({}) exceeds limit.max ({})",
                spec.default, spec.max
            )));
        }
    }
    Ok(())
}

/// Returns true if `name` contains only characters safe for use as a
/// Postgres identifier without escaping concerns. This is stricter than
/// Postgres actually requires, but column and table names generated by
/// `db_tool_planner` will always match this pattern; hand-edited manifests
/// that don't are rejected at load time rather than at query time.
fn is_safe_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
