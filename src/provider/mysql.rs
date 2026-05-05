// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only

use async_trait::async_trait;
use regex::Regex;
use sqlx::mysql::MySqlPool;
use sqlx::Row;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Instant;

use super::{
    connect_with_timeout, load_named_provider_config, Provider, ProviderCapabilities,
    ProviderStatus, ReadOptions,
};
use crate::config::ProviderConfig;
use crate::context::{ContextData, ContextEntry, ContextMetadata, ContextValue, EntryType};
use crate::error::{redact_uri, BridgeError, Result};

static IDENTIFIER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z_][a-zA-Z0-9_]*$").unwrap());

pub struct MySqlProvider {
    pool: Option<MySqlPool>,
    uri: String,
}

impl MySqlProvider {
    pub fn new() -> Self {
        Self {
            pool: None,
            uri: String::new(),
        }
    }

    fn pool(&self) -> Result<&MySqlPool> {
        self.pool
            .as_ref()
            .ok_or_else(|| BridgeError::ProviderError("Not connected".to_string()))
    }

    pub fn pool_handle(&self) -> Result<MySqlPool> {
        Ok(self.pool()?.clone())
    }

    pub async fn connect_named(
        connection_name: &str,
        config_dir: Option<&Path>,
        timeout_secs: u64,
    ) -> Result<Self> {
        let config = load_named_provider_config(connection_name, config_dir)?;
        if config.provider_type != "mysql" {
            return Err(BridgeError::UnsupportedOperation(format!(
                "mysql connection '{connection_name}' is of type '{}'",
                config.provider_type
            )));
        }

        let mut provider = Self::new();
        connect_with_timeout(&mut provider, &config, timeout_secs).await?;
        Ok(provider)
    }
}

fn validate_identifier(name: &str) -> Result<()> {
    if !IDENTIFIER_RE.is_match(name) {
        return Err(BridgeError::InvalidIdentifier(name.to_string()));
    }
    Ok(())
}

// MySQL uses backtick quoting for identifiers.
fn quote_ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

async fn detect_primary_key(pool: &MySqlPool, db: &str, table: &str) -> Result<String> {
    let rows = sqlx::query(
        r#"
        SELECT COLUMN_NAME as column_name
        FROM information_schema.KEY_COLUMN_USAGE
        WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND CONSTRAINT_NAME = 'PRIMARY'
        ORDER BY ORDINAL_POSITION
        "#,
    )
    .bind(db)
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

async fn current_database(pool: &MySqlPool) -> Result<String> {
    let row = sqlx::query("SELECT DATABASE() as db")
        .fetch_one(pool)
        .await?;
    let db: Option<String> = row.try_get("db").ok().flatten();
    db.ok_or_else(|| {
        BridgeError::ProviderError(
            "Could not determine current MySQL database from connection URI".to_string(),
        )
    })
}

async fn table_exists(pool: &MySqlPool, db: &str, table: &str) -> Result<bool> {
    let row = sqlx::query(
        "SELECT COUNT(*) as cnt FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?"
    )
    .bind(db)
    .bind(table)
    .fetch_one(pool)
    .await?;
    let count: i64 = row.get("cnt");
    Ok(count > 0)
}

#[async_trait]
impl Provider for MySqlProvider {
    fn name(&self) -> &str {
        "mysql"
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
        let pool = MySqlPool::connect(&config.uri).await?;
        self.uri = config.uri.clone();
        self.pool = Some(pool);
        Ok(())
    }

    async fn read(&self, path: &str, options: ReadOptions) -> Result<ContextValue> {
        let pool = self.pool()?;
        let limit = options.limit.unwrap_or(100);
        let db = current_database(pool).await?;

        // Parse path: "table" or "table/pk_value"
        let (table, pk_value) = if let Some(slash_pos) = path.find('/') {
            let t = &path[..slash_pos];
            let pk = &path[slash_pos + 1..];
            (t, Some(pk))
        } else {
            (path, None)
        };

        validate_identifier(table)?;

        if !table_exists(pool, &db, table).await? {
            return Err(BridgeError::ProviderError(format!(
                "Table '{table}' not found in database. Run `bridge ls --from <provider>` to see available tables."
            )));
        }

        let pk_col = detect_primary_key(pool, &db, table).await?;

        match pk_value {
            Some(pk) => {
                let query = format!(
                    "SELECT * FROM {table_q} WHERE {pk_q} = ? LIMIT 1",
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
                let query = format!(
                    "SELECT * FROM {table_q} ORDER BY {pk_q} LIMIT {limit}",
                    table_q = quote_ident(table),
                    pk_q = quote_ident(&pk_col),
                    limit = limit,
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
        let db = current_database(pool).await?;
        let rows = sqlx::query(
            "SELECT TABLE_NAME as table_name FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME"
        )
        .bind(&db)
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

fn row_to_json(row: &sqlx::mysql::MySqlRow) -> Result<serde_json::Value> {
    use sqlx::{Column, TypeInfo};

    let columns = row.columns();
    let mut map = serde_json::Map::new();

    for col in columns {
        let name = col.name().to_string();
        let type_name = col.type_info().name().to_uppercase();

        let value: serde_json::Value = if type_name.contains("INT") {
            match row.try_get::<Option<i64>, _>(col.ordinal()) {
                Ok(Some(v)) => serde_json::Value::Number(v.into()),
                Ok(None) => serde_json::Value::Null,
                Err(_) => serde_json::Value::Null,
            }
        } else if type_name == "FLOAT" || type_name.contains("DOUBLE") {
            match row.try_get::<Option<f64>, _>(col.ordinal()) {
                Ok(Some(v)) => serde_json::Number::from_f64(v)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null),
                Ok(None) => serde_json::Value::Null,
                Err(_) => serde_json::Value::Null,
            }
        } else if type_name == "BOOLEAN" {
            match row.try_get::<Option<bool>, _>(col.ordinal()) {
                Ok(Some(v)) => serde_json::Value::Bool(v),
                Ok(None) => serde_json::Value::Null,
                Err(_) => serde_json::Value::Null,
            }
        } else if type_name == "JSON" {
            // MySQL returns JSON columns as text over the wire.
            match row.try_get::<Option<String>, _>(col.ordinal()) {
                Ok(Some(s)) => serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s)),
                Ok(None) => serde_json::Value::Null,
                Err(_) => serde_json::Value::Null,
            }
        } else {
            // VARCHAR, TEXT, DECIMAL, DATETIME, TIMESTAMP, DATE, TIME, CHAR, ENUM, etc.
            match row.try_get::<Option<String>, _>(col.ordinal()) {
                Ok(Some(v)) => serde_json::Value::String(v),
                Ok(None) => serde_json::Value::Null,
                Err(_) => serde_json::Value::Null,
            }
        };

        map.insert(name, value);
    }

    Ok(serde_json::Value::Object(map))
}
