// Bridge CLI - One CLI. Any storage. Every agent.
// Copyright (c) 2026 Gabriel Beslic
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

use async_trait::async_trait;
use regex::Regex;
use sqlx::postgres::PgPool;
use sqlx::Row;
use std::sync::LazyLock;
use std::time::Instant;

use super::{Provider, ProviderCapabilities, ProviderStatus};
use crate::config::ProviderConfig;
use crate::context::{ContextData, ContextEntry, ContextMetadata, ContextValue, EntryType};
use crate::error::{redact_uri, BridgeError, Result};

static IDENTIFIER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z_][a-zA-Z0-9_]*$").unwrap());

pub struct PostgresProvider {
    pool: Option<PgPool>,
    uri: String,
}

impl PostgresProvider {
    pub fn new() -> Self {
        Self {
            pool: None,
            uri: String::new(),
        }
    }

    fn pool(&self) -> Result<&PgPool> {
        self.pool
            .as_ref()
            .ok_or_else(|| BridgeError::ProviderError("Not connected".to_string()))
    }
}

/// Validate that an identifier is safe for use in SQL.
fn validate_identifier(name: &str) -> Result<()> {
    if !IDENTIFIER_RE.is_match(name) {
        return Err(BridgeError::InvalidIdentifier(name.to_string()));
    }
    Ok(())
}

/// Quote an identifier for safe use in SQL.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Detect the single primary key column for a table.
async fn detect_primary_key(pool: &PgPool, table: &str) -> Result<String> {
    let rows = sqlx::query(
        r#"
        SELECT kcu.column_name
        FROM information_schema.table_constraints tc
        JOIN information_schema.key_column_usage kcu
            ON tc.constraint_name = kcu.constraint_name
            AND tc.table_schema = kcu.table_schema
        WHERE tc.constraint_type = 'PRIMARY KEY'
            AND tc.table_schema = 'public'
            AND tc.table_name = $1
        ORDER BY kcu.ordinal_position
        "#,
    )
    .bind(table)
    .fetch_all(pool)
    .await?;

    match rows.len() {
        0 => Err(BridgeError::ProviderError(format!(
            "Table '{table}' has no primary key — use `bridge read {table}` to list rows"
        ))),
        1 => {
            let col: String = rows[0].get("column_name");
            Ok(col)
        }
        _ => Err(BridgeError::ProviderError(format!(
            "Table '{table}' has a composite primary key — use `bridge read {table}` to list rows"
        ))),
    }
}

/// Detect the data type of a column.
async fn detect_column_type(pool: &PgPool, table: &str, column: &str) -> Result<String> {
    let row = sqlx::query(
        "SELECT data_type FROM information_schema.columns WHERE table_schema = 'public' AND table_name = $1 AND column_name = $2",
    )
    .bind(table)
    .bind(column)
    .fetch_one(pool)
    .await?;

    let data_type: String = row.get("data_type");
    Ok(data_type)
}

/// Check if a table exists in the public schema.
async fn table_exists(pool: &PgPool, table: &str) -> Result<bool> {
    let row = sqlx::query(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_schema = 'public' AND table_name = $1) as exists"
    )
    .bind(table)
    .fetch_one(pool)
    .await?;
    Ok(row.get::<bool, _>("exists"))
}

#[async_trait]
impl Provider for PostgresProvider {
    fn name(&self) -> &str {
        "postgres"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            read: true,
            list: true,
            write: false,
            delete: false,
            search: false,
        }
    }

    async fn connect(&mut self, config: &ProviderConfig) -> Result<()> {
        let pool = PgPool::connect(&config.uri).await?;
        self.uri = config.uri.clone();
        self.pool = Some(pool);
        Ok(())
    }

    async fn read(&self, path: &str) -> Result<ContextValue> {
        let pool = self.pool()?;

        // Parse path: "table" or "table/pk_value"
        let (table, pk_value) = if let Some(slash_pos) = path.find('/') {
            let t = &path[..slash_pos];
            let pk = &path[slash_pos + 1..];
            (t, Some(pk))
        } else {
            (path, None)
        };

        validate_identifier(table)?;

        if !table_exists(pool, table).await? {
            return Err(BridgeError::ProviderError(format!(
                "Table '{table}' not found in database. Run `bridge ls --from <provider>` to see available tables."
            )));
        }

        let pk_col = detect_primary_key(pool, table).await?;

        match pk_value {
            Some(pk) => {
                // Single row read — cast $1 to the PK column type to avoid type mismatch
                let pk_type = detect_column_type(pool, table, &pk_col).await?;
                let cast = format!("$1::{}", pk_type);
                let query = format!(
                    "SELECT * FROM {table_q} WHERE {pk_q} = {cast} LIMIT 1",
                    table_q = quote_ident(table),
                    pk_q = quote_ident(&pk_col),
                );
                let row = sqlx::query(&query).bind(pk).fetch_optional(pool).await?;

                match row {
                    Some(row) => {
                        let json = row_to_json(&row)?;
                        Ok(ContextValue {
                            data: ContextData::Json(json),
                            metadata: ContextMetadata {
                                source: redact_uri(&self.uri),
                                path: path.to_string(),
                                content_type: Some("application/json".to_string()),
                                size: None,
                                created_at: None,
                                updated_at: None,
                            },
                        })
                    }
                    None => Err(BridgeError::ProviderError(format!(
                        "Row not found: {table}/{pk}"
                    ))),
                }
            }
            None => {
                // Table read with ORDER BY and LIMIT
                let query = format!(
                    "SELECT * FROM {table_q} ORDER BY {pk_q} LIMIT 100",
                    table_q = quote_ident(table),
                    pk_q = quote_ident(&pk_col),
                );
                let rows = sqlx::query(&query).fetch_all(pool).await?;
                let json_rows: Vec<serde_json::Value> =
                    rows.iter().map(row_to_json).collect::<Result<_>>()?;

                Ok(ContextValue {
                    data: ContextData::Json(serde_json::Value::Array(json_rows)),
                    metadata: ContextMetadata {
                        source: redact_uri(&self.uri),
                        path: table.to_string(),
                        content_type: Some("application/json".to_string()),
                        size: None,
                        created_at: None,
                        updated_at: None,
                    },
                })
            }
        }
    }

    async fn list(&self, _prefix: Option<&str>) -> Result<Vec<ContextEntry>> {
        let pool = self.pool()?;
        let rows = sqlx::query(
            "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' ORDER BY table_name"
        )
        .fetch_all(pool)
        .await?;

        let entries = rows
            .iter()
            .map(|row| {
                let name: String = row.get("table_name");
                ContextEntry {
                    path: name.clone(),
                    name,
                    entry_type: EntryType::Table,
                    size: None,
                    updated_at: None,
                }
            })
            .collect();

        Ok(entries)
    }

    async fn health(&self) -> Result<ProviderStatus> {
        let pool = self.pool()?;
        let start = Instant::now();
        match sqlx::query("SELECT 1 as health").fetch_one(pool).await {
            Ok(_) => Ok(ProviderStatus {
                connected: true,
                latency_ms: Some(start.elapsed().as_millis() as u64),
                message: Some(format!("Connected to {}", redact_uri(&self.uri))),
            }),
            Err(e) => Ok(ProviderStatus {
                connected: false,
                latency_ms: None,
                message: Some(format!("Connection failed: {e}")),
            }),
        }
    }
}

/// Convert a sqlx Row to a serde_json::Value object.
fn row_to_json(row: &sqlx::postgres::PgRow) -> Result<serde_json::Value> {
    use sqlx::Column;
    use sqlx::TypeInfo;

    let columns = row.columns();
    let mut map = serde_json::Map::new();

    for col in columns {
        let name = col.name().to_string();
        let type_name = col.type_info().name();
        let value: serde_json::Value = match type_name {
            "INT8" | "BIGSERIAL" => match row.try_get::<i64, _>(col.ordinal()) {
                Ok(v) => serde_json::Value::Number(v.into()),
                Err(_) => serde_json::Value::Null,
            },
            "INT4" | "SERIAL" => match row.try_get::<i32, _>(col.ordinal()) {
                Ok(v) => serde_json::Value::Number(v.into()),
                Err(_) => serde_json::Value::Null,
            },
            "INT2" | "SMALLSERIAL" => match row.try_get::<i16, _>(col.ordinal()) {
                Ok(v) => serde_json::Value::Number(v.into()),
                Err(_) => serde_json::Value::Null,
            },
            "FLOAT4" | "FLOAT8" | "NUMERIC" => match row.try_get::<f64, _>(col.ordinal()) {
                Ok(v) => serde_json::Number::from_f64(v)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null),
                Err(_) => serde_json::Value::Null,
            },
            "BOOL" => match row.try_get::<bool, _>(col.ordinal()) {
                Ok(v) => serde_json::Value::Bool(v),
                Err(_) => serde_json::Value::Null,
            },
            "JSON" | "JSONB" => match row.try_get::<serde_json::Value, _>(col.ordinal()) {
                Ok(v) => v,
                Err(_) => serde_json::Value::Null,
            },
            _ => {
                // Default: try as string
                match row.try_get::<String, _>(col.ordinal()) {
                    Ok(v) => serde_json::Value::String(v),
                    Err(_) => serde_json::Value::Null,
                }
            }
        };
        map.insert(name, value);
    }

    Ok(serde_json::Value::Object(map))
}
