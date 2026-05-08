// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Query SQL database catalogs and return a normalized metadata model. Purely
// schema-level — never touches user data rows.

#[path = "db_introspector/postgres.rs"]
pub mod postgres;
#[path = "db_introspector/sqlite.rs"]
pub mod sqlite;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DbMetadata {
    pub schema: String,
    pub tables: Vec<TableMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TableMetadata {
    pub name: String,
    pub kind: TableKind,
    pub columns: Vec<ColumnMetadata>,
    pub primary_key: Vec<String>,
    /// Single-column unique keys, useful when a table lacks a PK but can
    /// still be looked up deterministically.
    pub unique_single_keys: Vec<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TableKind {
    Table,
    View,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ColumnMetadata {
    pub name: String,
    /// Database-reported data type (e.g. "integer", "text", "timestamp with time zone").
    pub data_type: String,
    /// Stable type name used for classification and generated descriptions.
    pub udt_name: String,
    pub is_nullable: bool,
    pub comment: Option<String>,
    /// Classification derived from the database type.
    pub category: ColumnCategory,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ColumnCategory {
    Integer,
    Float,
    Numeric,
    Boolean,
    Text,
    Timestamp,
    Uuid,
    Json,
    /// Anything we can't prove is a safe filterable scalar — bytea, arrays,
    /// vectors, range types, etc.
    Unsupported,
}

impl ColumnCategory {
    pub fn is_filterable(self) -> bool {
        matches!(
            self,
            Self::Integer | Self::Float | Self::Boolean | Self::Text | Self::Uuid
        )
    }
    pub fn is_sortable(self) -> bool {
        matches!(
            self,
            Self::Integer
                | Self::Float
                | Self::Numeric
                | Self::Boolean
                | Self::Text
                | Self::Timestamp
                | Self::Uuid
        )
    }
    pub fn input_json_type(self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::Float => "number",
            Self::Boolean => "boolean",
            Self::Numeric => "string",
            _ => "string",
        }
    }
}

pub fn classify(udt_name: &str) -> ColumnCategory {
    match udt_name {
        "int2" | "int4" | "int8" | "smallserial" | "serial" | "bigserial" => {
            ColumnCategory::Integer
        }
        "float4" | "float8" => ColumnCategory::Float,
        "numeric" => ColumnCategory::Numeric,
        "bool" => ColumnCategory::Boolean,
        "text" | "varchar" | "bpchar" | "char" | "name" | "citext" => ColumnCategory::Text,
        "timestamp" | "timestamptz" | "date" | "time" | "timetz" => ColumnCategory::Timestamp,
        "uuid" => ColumnCategory::Uuid,
        "json" | "jsonb" => ColumnCategory::Json,
        _ => ColumnCategory::Unsupported,
    }
}
