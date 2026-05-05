// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only

use super::{ColumnCategory, ColumnMetadata, DbMetadata, TableKind, TableMetadata};
use crate::error::{BridgeError, Result};
use sqlx::mysql::MySqlPool;
use sqlx::Row;

/// Empty string means "use DATABASE() — the currently connected database."
pub const DEFAULT_SCHEMA: &str = "";

pub async fn introspect(pool: &MySqlPool, schema: &str) -> Result<DbMetadata> {
    // Resolve the effective database name. MySQL's "schema" IS the database.
    let effective_schema = if schema.is_empty() {
        let row = sqlx::query("SELECT DATABASE() as db")
            .fetch_one(pool)
            .await?;
        let db: Option<String> = row.try_get("db").ok().flatten();
        db.ok_or_else(|| {
            BridgeError::ProviderError(
                "Could not determine current MySQL database. Specify --schema <database_name>."
                    .to_string(),
            )
        })?
    } else {
        schema.to_string()
    };

    // Verify the database exists and is accessible.
    let exists_row = sqlx::query(
        "SELECT COUNT(*) as cnt FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?",
    )
    .bind(&effective_schema)
    .fetch_one(pool)
    .await?;
    let exists: i64 = exists_row.get("cnt");
    if exists == 0 {
        return Err(BridgeError::ProviderError(format!(
            "MySQL database '{effective_schema}' not found. Check the connection and pass a different `--schema`."
        )));
    }

    let table_rows = sqlx::query(
        r#"
        SELECT TABLE_NAME as name, TABLE_TYPE as table_type
        FROM information_schema.TABLES
        WHERE TABLE_SCHEMA = ?
        ORDER BY TABLE_NAME
        "#,
    )
    .bind(&effective_schema)
    .fetch_all(pool)
    .await?;

    let mut tables = Vec::with_capacity(table_rows.len());
    for r in table_rows {
        let name: String = r.get("name");
        let table_type: String = r.get("table_type");
        let kind = if table_type.contains("VIEW") {
            TableKind::View
        } else {
            TableKind::Table
        };
        let columns = introspect_columns(pool, &effective_schema, &name).await?;
        let primary_key = introspect_primary_key(pool, &effective_schema, &name).await?;
        let unique_single_keys =
            introspect_unique_single_keys(pool, &effective_schema, &name).await?;
        tables.push(TableMetadata {
            name,
            kind,
            columns,
            primary_key,
            unique_single_keys,
            comment: None,
        });
    }

    Ok(DbMetadata {
        schema: effective_schema,
        tables,
    })
}

async fn introspect_columns(
    pool: &MySqlPool,
    db: &str,
    table: &str,
) -> Result<Vec<ColumnMetadata>> {
    let rows = sqlx::query(
        r#"
        SELECT COLUMN_NAME    AS name,
               DATA_TYPE      AS data_type,
               COLUMN_TYPE    AS udt_name,
               IS_NULLABLE    AS is_nullable,
               COLUMN_COMMENT AS comment
        FROM information_schema.COLUMNS
        WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
        ORDER BY ORDINAL_POSITION
        "#,
    )
    .bind(db)
    .bind(table)
    .fetch_all(pool)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let data_type: String = r.get("data_type");
        let udt_name: String = r.get("udt_name");
        let nullable_str: String = r.get("is_nullable");
        let is_nullable = nullable_str.eq_ignore_ascii_case("YES");
        let comment: String = r.get("comment");
        let category = classify_mysql(&data_type);
        out.push(ColumnMetadata {
            name: r.get("name"),
            data_type,
            udt_name,
            is_nullable,
            comment: if comment.is_empty() {
                None
            } else {
                Some(comment)
            },
            category,
        });
    }
    Ok(out)
}

async fn introspect_primary_key(pool: &MySqlPool, db: &str, table: &str) -> Result<Vec<String>> {
    let rows = sqlx::query(
        r#"
        SELECT COLUMN_NAME as name
        FROM information_schema.KEY_COLUMN_USAGE
        WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND CONSTRAINT_NAME = 'PRIMARY'
        ORDER BY ORDINAL_POSITION
        "#,
    )
    .bind(db)
    .bind(table)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(|r| r.get::<String, _>("name")).collect())
}

async fn introspect_unique_single_keys(
    pool: &MySqlPool,
    db: &str,
    table: &str,
) -> Result<Vec<String>> {
    // Find UNIQUE constraints with exactly one column.
    let rows = sqlx::query(
        r#"
        SELECT kcu.COLUMN_NAME as name
        FROM information_schema.TABLE_CONSTRAINTS tc
        JOIN information_schema.KEY_COLUMN_USAGE kcu
          ON tc.CONSTRAINT_NAME = kcu.CONSTRAINT_NAME
         AND tc.TABLE_SCHEMA = kcu.TABLE_SCHEMA
         AND tc.TABLE_NAME = kcu.TABLE_NAME
        WHERE tc.CONSTRAINT_TYPE = 'UNIQUE'
          AND tc.TABLE_SCHEMA = ?
          AND tc.TABLE_NAME = ?
          AND 1 = (
            SELECT COUNT(*)
            FROM information_schema.KEY_COLUMN_USAGE k2
            WHERE k2.CONSTRAINT_NAME = tc.CONSTRAINT_NAME
              AND k2.TABLE_SCHEMA = tc.TABLE_SCHEMA
              AND k2.TABLE_NAME = tc.TABLE_NAME
          )
        ORDER BY kcu.COLUMN_NAME
        "#,
    )
    .bind(db)
    .bind(table)
    .fetch_all(pool)
    .await?;

    let mut keys: Vec<String> = rows.iter().map(|r| r.get::<String, _>("name")).collect();
    keys.sort();
    keys.dedup();
    Ok(keys)
}

fn classify_mysql(data_type: &str) -> ColumnCategory {
    let lower = data_type.trim().to_ascii_lowercase();
    match lower.as_str() {
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" => {
            ColumnCategory::Integer
        }
        "float" | "double" | "real" | "double precision" => ColumnCategory::Float,
        "decimal" | "numeric" | "dec" | "fixed" => ColumnCategory::Numeric,
        "bool" | "boolean" => ColumnCategory::Boolean,
        "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" | "enum" | "set" => {
            ColumnCategory::Text
        }
        "date" | "datetime" | "timestamp" | "time" | "year" => ColumnCategory::Timestamp,
        "json" => ColumnCategory::Json,
        // BLOB, BINARY, VARBINARY, BIT, GEOMETRY, etc.
        _ => ColumnCategory::Unsupported,
    }
}
