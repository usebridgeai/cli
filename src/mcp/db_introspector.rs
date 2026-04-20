// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Query Postgres's information_schema/pg_catalog and return a normalized
// metadata model. Purely schema-level — never touches user data rows.

use crate::error::{BridgeError, Result};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPool;
use sqlx::Row;

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
    /// Postgres data_type (e.g. "integer", "text", "timestamp with time zone").
    pub data_type: String,
    /// Postgres udt_name (e.g. "int4", "text", "timestamptz") — more reliable
    /// for classification than the human-readable data_type.
    pub udt_name: String,
    pub is_nullable: bool,
    pub comment: Option<String>,
    /// Classification derived from `udt_name`.
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

/// System schemas we never introspect — keeps noise out of generated manifests.
pub const SYSTEM_SCHEMAS: &[&str] = &["pg_catalog", "information_schema", "pg_toast"];

pub fn is_system_schema(name: &str) -> bool {
    SYSTEM_SCHEMAS.contains(&name) || name.starts_with("pg_")
}

pub async fn introspect(pool: &PgPool, schema: &str) -> Result<DbMetadata> {
    if is_system_schema(schema) {
        return Err(BridgeError::UnsupportedOperation(format!(
            "schema '{schema}' is a system schema and is excluded from introspection"
        )));
    }

    let schema_exists: bool = sqlx::query(
        "SELECT EXISTS(SELECT 1 FROM information_schema.schemata WHERE schema_name = $1) AS e",
    )
    .bind(schema)
    .fetch_one(pool)
    .await?
    .get("e");
    if !schema_exists {
        return Err(BridgeError::ProviderError(format!(
            "schema '{schema}' not found. Check the database and pass a different `--schema`."
        )));
    }

    // Tables and views in order by name — ordering feeds determinism.
    let table_rows = sqlx::query(
        r#"
        SELECT c.relname AS name,
               CASE c.relkind WHEN 'r' THEN 'table'
                              WHEN 'p' THEN 'table'
                              WHEN 'v' THEN 'view'
                              WHEN 'm' THEN 'view'
                END AS kind,
               obj_description(c.oid, 'pg_class') AS comment
        FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = $1
          AND c.relkind IN ('r', 'p', 'v', 'm')
        ORDER BY c.relname
        "#,
    )
    .bind(schema)
    .fetch_all(pool)
    .await?;

    let mut tables = Vec::with_capacity(table_rows.len());
    for r in table_rows {
        let name: String = r.get("name");
        let kind_str: String = r.get("kind");
        let kind = if kind_str == "view" {
            TableKind::View
        } else {
            TableKind::Table
        };
        let comment: Option<String> = r.try_get("comment").ok().flatten();
        let columns = introspect_columns(pool, schema, &name).await?;
        let primary_key = introspect_primary_key(pool, schema, &name).await?;
        let unique_single_keys = introspect_unique_single_keys(pool, schema, &name).await?;
        tables.push(TableMetadata {
            name,
            kind,
            columns,
            primary_key,
            unique_single_keys,
            comment,
        });
    }

    Ok(DbMetadata {
        schema: schema.to_string(),
        tables,
    })
}

async fn introspect_columns(
    pool: &PgPool,
    schema: &str,
    table: &str,
) -> Result<Vec<ColumnMetadata>> {
    let rows = sqlx::query(
        r#"
        SELECT c.column_name      AS name,
               c.data_type        AS data_type,
               c.udt_name         AS udt_name,
               c.is_nullable = 'YES' AS nullable,
               pgd.description    AS comment
        FROM information_schema.columns c
        LEFT JOIN pg_catalog.pg_class cl
            ON cl.relname = c.table_name
           AND cl.relnamespace = (
               SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = c.table_schema
           )
        LEFT JOIN pg_catalog.pg_description pgd
            ON pgd.objoid = cl.oid AND pgd.objsubid = c.ordinal_position
        WHERE c.table_schema = $1 AND c.table_name = $2
        ORDER BY c.ordinal_position
        "#,
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let udt_name: String = r.get("udt_name");
        let category = classify(&udt_name);
        out.push(ColumnMetadata {
            name: r.get("name"),
            data_type: r.get("data_type"),
            udt_name,
            is_nullable: r.get("nullable"),
            comment: r.try_get("comment").ok().flatten(),
            category,
        });
    }
    Ok(out)
}

async fn introspect_primary_key(pool: &PgPool, schema: &str, table: &str) -> Result<Vec<String>> {
    let rows = sqlx::query(
        r#"
        SELECT kcu.column_name AS name
        FROM information_schema.table_constraints tc
        JOIN information_schema.key_column_usage kcu
          ON tc.constraint_name = kcu.constraint_name
         AND tc.table_schema = kcu.table_schema
         AND tc.table_name = kcu.table_name
        WHERE tc.constraint_type = 'PRIMARY KEY'
          AND tc.table_schema = $1
          AND tc.table_name = $2
        ORDER BY kcu.ordinal_position
        "#,
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(|r| r.get::<String, _>("name")).collect())
}

async fn introspect_unique_single_keys(
    pool: &PgPool,
    schema: &str,
    table: &str,
) -> Result<Vec<String>> {
    // Constraint-level uniqueness with exactly one column. Ordered by column
    // name so regeneration is stable.
    let constraint_rows = sqlx::query(
        r#"
        SELECT kcu.column_name AS name
        FROM information_schema.table_constraints tc
        JOIN information_schema.key_column_usage kcu
          ON tc.constraint_name = kcu.constraint_name
         AND tc.table_schema = kcu.table_schema
         AND tc.table_name = kcu.table_name
        WHERE tc.constraint_type = 'UNIQUE'
          AND tc.table_schema = $1
          AND tc.table_name = $2
          AND 1 = (
            SELECT COUNT(*)
            FROM information_schema.key_column_usage k2
            WHERE k2.constraint_name = tc.constraint_name
              AND k2.table_schema = tc.table_schema
              AND k2.table_name = tc.table_name
          )
        ORDER BY kcu.column_name
        "#,
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;
    let index_rows = sqlx::query(
        r#"
        SELECT a.attname AS name
        FROM pg_index i
        JOIN pg_class t ON t.oid = i.indrelid
        JOIN pg_namespace n ON n.oid = t.relnamespace
        JOIN pg_attribute a
          ON a.attrelid = t.oid
         AND a.attnum = i.indkey[0]
        WHERE n.nspname = $1
          AND t.relname = $2
          AND i.indisunique
          AND NOT i.indisprimary
          AND i.indnkeyatts = 1
          AND i.indnatts = 1
          AND i.indexprs IS NULL
          AND i.indpred IS NULL
        ORDER BY a.attname
        "#,
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    let mut keys: Vec<String> = constraint_rows
        .iter()
        .chain(index_rows.iter())
        .map(|r| r.get::<String, _>("name"))
        .collect();
    keys.sort();
    keys.dedup();
    Ok(keys)
}
