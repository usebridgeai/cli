// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only

use super::{ColumnCategory, ColumnMetadata, DbMetadata, TableKind, TableMetadata};
use crate::error::{BridgeError, Result};
use sqlx::sqlite::SqlitePool;
use sqlx::Row;

pub const DEFAULT_SCHEMA: &str = "main";

struct ColumnWithPk {
    metadata: ColumnMetadata,
    pk_position: i64,
}

pub async fn introspect(pool: &SqlitePool, schema: &str) -> Result<DbMetadata> {
    if schema != DEFAULT_SCHEMA {
        return Err(BridgeError::UnsupportedOperation(format!(
            "SQLite MCP generation only supports schema '{DEFAULT_SCHEMA}'"
        )));
    }

    let table_rows = sqlx::query(
        r#"
        SELECT name, type
        FROM sqlite_schema
        WHERE type IN ('table', 'view')
          AND name NOT LIKE 'sqlite_%'
        ORDER BY name
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut tables = Vec::with_capacity(table_rows.len());
    for row in table_rows {
        let name: String = row.get("name");
        let type_name: String = row.get("type");
        let kind = if type_name == "view" {
            TableKind::View
        } else {
            TableKind::Table
        };
        let columns = introspect_columns(pool, &name).await?;
        let primary_key = columns
            .iter()
            .filter(|c| c.pk_position > 0)
            .map(|c| c.metadata.name.clone())
            .collect();
        let unique_single_keys = introspect_unique_single_keys(pool, &name).await?;
        tables.push(TableMetadata {
            name,
            kind,
            columns: columns.into_iter().map(|c| c.metadata).collect(),
            primary_key,
            unique_single_keys,
            comment: None,
        });
    }

    Ok(DbMetadata {
        schema: schema.to_string(),
        tables,
    })
}

async fn introspect_columns(pool: &SqlitePool, table: &str) -> Result<Vec<ColumnWithPk>> {
    let pragma = format!("PRAGMA table_info({})", quote_ident(table));
    let rows = sqlx::query(&pragma).fetch_all(pool).await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let data_type: String = row.get("type");
        let is_nullable = row.get::<i64, _>("notnull") == 0;
        let pk = row.get::<i64, _>("pk");
        let category = classify_sqlite(&data_type);
        out.push(ColumnWithPk {
            metadata: ColumnMetadata {
                name: row.get("name"),
                data_type: data_type.clone(),
                udt_name: data_type,
                is_nullable,
                comment: None,
                category,
            },
            pk_position: pk,
        });
    }
    Ok(out)
}

async fn introspect_unique_single_keys(pool: &SqlitePool, table: &str) -> Result<Vec<String>> {
    let pragma = format!("PRAGMA index_list({})", quote_ident(table));
    let rows = sqlx::query(&pragma).fetch_all(pool).await?;
    let mut out = Vec::new();

    for row in rows {
        let unique = row.get::<i64, _>("unique") == 1;
        let partial = row.try_get::<i64, _>("partial").unwrap_or(0) == 1;
        if !unique || partial {
            continue;
        }

        let index_name: String = row.get("name");
        let index_pragma = format!("PRAGMA index_info({})", quote_ident(&index_name));
        let cols = sqlx::query(&index_pragma).fetch_all(pool).await?;
        if cols.len() == 1 {
            out.push(cols[0].get::<String, _>("name"));
        }
    }

    out.sort();
    out.dedup();
    Ok(out)
}

fn classify_sqlite(type_name: &str) -> ColumnCategory {
    let upper = type_name.trim().to_ascii_uppercase();
    if matches!(upper.as_str(), "BOOLEAN" | "BOOL") {
        ColumnCategory::Boolean
    } else if upper.contains("INT") {
        ColumnCategory::Integer
    } else if upper.contains("CHAR") || upper.contains("CLOB") || upper.contains("TEXT") {
        ColumnCategory::Text
    } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
        ColumnCategory::Float
    } else if upper.contains("JSON") {
        ColumnCategory::Json
    } else if upper.is_empty() || upper.contains("BLOB") {
        ColumnCategory::Unsupported
    } else {
        ColumnCategory::Numeric
    }
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh_pool() -> SqlitePool {
        SqlitePool::connect("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn unique_single_key_detection_skips_partial_indexes() {
        // A partial UNIQUE index doesn't guarantee uniqueness across the whole
        // table — row lookups based on it can return zero rows for values that
        // fail the WHERE clause. We must not offer it as a get-by-key column.
        let pool = fresh_pool().await;
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, token TEXT NOT NULL, active INTEGER)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE UNIQUE INDEX t_token_full ON t(token)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE UNIQUE INDEX t_token_partial ON t(token) WHERE active = 1")
            .execute(&pool)
            .await
            .unwrap();

        let keys = introspect_unique_single_keys(&pool, "t").await.unwrap();
        assert_eq!(keys, vec!["token".to_string()]);
    }
}
