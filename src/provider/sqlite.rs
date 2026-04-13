// Bridge CLI - One CLI. Any storage. Every agent.
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
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
use sqlx::sqlite::SqlitePool;
use sqlx::{Column, Row, TypeInfo, ValueRef};
use std::sync::LazyLock;
use std::time::Instant;

use super::{Provider, ProviderCapabilities, ProviderStatus, ReadOptions};
use crate::config::ProviderConfig;
use crate::context::{ContextData, ContextEntry, ContextMetadata, ContextValue, EntryType};
use crate::error::{redact_uri, BridgeError, Result};

static IDENTIFIER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z_][a-zA-Z0-9_]*$").unwrap());

#[derive(Copy, Clone)]
enum SqliteAffinity {
    Integer,
    Real,
    Text,
    Blob,
    Numeric,
    Boolean,
}

pub struct SqliteProvider {
    pool: Option<SqlitePool>,
    uri: String,
}

struct ParsedSqliteUri {
    connection_suffix: String,
    db_path: String,
    mode: Option<String>,
}

impl SqliteProvider {
    pub fn new() -> Self {
        Self {
            pool: None,
            uri: String::new(),
        }
    }

    fn pool(&self) -> Result<&SqlitePool> {
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

/// Detect the single primary key column for a table via PRAGMA table_info.
async fn detect_primary_key(pool: &SqlitePool, table: &str) -> Result<String> {
    validate_identifier(table)?;
    let pragma = format!("PRAGMA table_info({})", quote_ident(table));
    let rows = sqlx::query(&pragma).fetch_all(pool).await?;

    let pk_cols: Vec<String> = rows
        .iter()
        .filter(|row| {
            let pk: i32 = row.get("pk");
            pk > 0
        })
        .map(|row| row.get::<String, _>("name"))
        .collect();

    match pk_cols.len() {
        0 => Err(BridgeError::ProviderError(format!(
            "Table '{table}' has no primary key \u{2014} use `bridge read {table}` to list rows"
        ))),
        1 => Ok(pk_cols.into_iter().next().unwrap()),
        _ => Err(BridgeError::ProviderError(format!(
            "Table '{table}' has a composite primary key \u{2014} use `bridge read {table}` to list rows"
        ))),
    }
}

/// Check if a table exists in the database.
async fn table_exists(pool: &SqlitePool, table: &str) -> Result<bool> {
    let row =
        sqlx::query("SELECT COUNT(*) as cnt FROM sqlite_master WHERE type='table' AND name=?1")
            .bind(table)
            .fetch_one(pool)
            .await?;
    let count: i32 = row.get("cnt");
    Ok(count > 0)
}

fn sqlite_affinity(type_name: &str) -> SqliteAffinity {
    let upper = type_name.trim().to_ascii_uppercase();

    if matches!(upper.as_str(), "BOOLEAN" | "BOOL") {
        SqliteAffinity::Boolean
    } else if upper.contains("INT") {
        SqliteAffinity::Integer
    } else if upper.contains("CHAR") || upper.contains("CLOB") || upper.contains("TEXT") {
        SqliteAffinity::Text
    } else if upper.is_empty() || upper.contains("BLOB") {
        SqliteAffinity::Blob
    } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
        SqliteAffinity::Real
    } else {
        SqliteAffinity::Numeric
    }
}

fn json_integer(row: &sqlx::sqlite::SqliteRow, ordinal: usize) -> Option<serde_json::Value> {
    match row.try_get::<Option<i64>, _>(ordinal) {
        Ok(Some(value)) => Some(serde_json::Value::Number(value.into())),
        Ok(None) => Some(serde_json::Value::Null),
        Err(_) => None,
    }
}

fn json_float(row: &sqlx::sqlite::SqliteRow, ordinal: usize) -> Option<serde_json::Value> {
    match row.try_get::<Option<f64>, _>(ordinal) {
        Ok(Some(value)) => Some(
            serde_json::Number::from_f64(value)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
        ),
        Ok(None) => Some(serde_json::Value::Null),
        Err(_) => None,
    }
}

fn json_bool(row: &sqlx::sqlite::SqliteRow, ordinal: usize) -> Option<serde_json::Value> {
    match row.try_get::<Option<bool>, _>(ordinal) {
        Ok(Some(value)) => Some(serde_json::Value::Bool(value)),
        Ok(None) => Some(serde_json::Value::Null),
        Err(_) => None,
    }
}

fn json_text(row: &sqlx::sqlite::SqliteRow, ordinal: usize) -> Option<serde_json::Value> {
    match row.try_get::<Option<String>, _>(ordinal) {
        Ok(Some(value)) => Some(serde_json::Value::String(value)),
        Ok(None) => Some(serde_json::Value::Null),
        Err(_) => None,
    }
}

fn json_blob(row: &sqlx::sqlite::SqliteRow, ordinal: usize) -> Option<serde_json::Value> {
    match row.try_get::<Option<Vec<u8>>, _>(ordinal) {
        Ok(Some(value)) => {
            use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
            Some(serde_json::Value::String(BASE64.encode(value)))
        }
        Ok(None) => Some(serde_json::Value::Null),
        Err(_) => None,
    }
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|part| {
        let (name, value) = part.split_once('=').unwrap_or((part, ""));
        (name == key).then_some(value)
    })
}

/// Convert a sqlx SqliteRow to a serde_json::Value object.
///
/// SQLite uses dynamic typing, so we use Option<T> variants to correctly
/// detect SQL NULL values (which would otherwise decode as a default).
fn row_to_json(row: &sqlx::sqlite::SqliteRow) -> Result<serde_json::Value> {
    let columns = row.columns();
    let mut map = serde_json::Map::new();

    for col in columns {
        let name = col.name().to_string();
        let type_name = col.type_info().name().to_uppercase();
        let ordinal = col.ordinal();

        let value = if row.try_get_raw(ordinal)?.is_null() {
            serde_json::Value::Null
        } else {
            let decoders: &[fn(&sqlx::sqlite::SqliteRow, usize) -> Option<serde_json::Value>] =
                match sqlite_affinity(&type_name) {
                    SqliteAffinity::Integer => &[json_integer, json_float, json_text, json_blob],
                    SqliteAffinity::Real => &[json_float, json_integer, json_text, json_blob],
                    SqliteAffinity::Text => {
                        &[json_text, json_integer, json_float, json_bool, json_blob]
                    }
                    SqliteAffinity::Blob => &[json_blob, json_text],
                    SqliteAffinity::Numeric => {
                        &[json_integer, json_float, json_bool, json_text, json_blob]
                    }
                    SqliteAffinity::Boolean => {
                        &[json_bool, json_integer, json_float, json_text, json_blob]
                    }
                };

            decoders
                .iter()
                .find_map(|decode| decode(row, ordinal))
                .unwrap_or(serde_json::Value::Null)
        };

        map.insert(name, value);
    }

    Ok(serde_json::Value::Object(map))
}

/// Parse a sqlite:// URI to extract the file path.
/// Supports: sqlite://path/to/db.sqlite, sqlite://./relative, sqlite:///absolute,
/// and query parameters such as ?mode=ro.
fn parse_sqlite_uri(uri: &str) -> Result<ParsedSqliteUri> {
    let connection_suffix = uri.strip_prefix("sqlite://").ok_or_else(|| {
        BridgeError::InvalidUri(format!("SQLite URI must start with sqlite:// — got: {uri}"))
    })?;

    if connection_suffix.is_empty() {
        return Err(BridgeError::InvalidUri(
            "SQLite URI must include a file path after sqlite://".to_string(),
        ));
    }

    let (db_path, query) = connection_suffix
        .split_once('?')
        .unwrap_or((connection_suffix, ""));

    if db_path.is_empty() {
        return Err(BridgeError::InvalidUri(
            "SQLite URI must include a file path before query parameters".to_string(),
        ));
    }

    Ok(ParsedSqliteUri {
        connection_suffix: connection_suffix.to_string(),
        db_path: db_path.to_string(),
        mode: query_param(query, "mode").map(|mode| mode.to_ascii_lowercase()),
    })
}

#[async_trait]
impl Provider for SqliteProvider {
    fn name(&self) -> &str {
        "sqlite"
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
        let parsed = parse_sqlite_uri(&config.uri)?;

        let skip_exists_check = parsed.db_path == ":memory:"
            || matches!(parsed.mode.as_deref(), Some("memory" | "rwc"))
            // SQLite's file: URI form is valid but does not map 1:1 to a filesystem path.
            || parsed.db_path.starts_with("file:");

        if !skip_exists_check && !std::path::Path::new(&parsed.db_path).exists() {
            return Err(BridgeError::ProviderError(format!(
                "SQLite database file not found: {}",
                parsed.db_path
            )));
        }

        // sqlx expects sqlite: scheme (not sqlite://), but otherwise accepts the URI suffix as-is.
        let connect_url = format!("sqlite:{}", parsed.connection_suffix);

        let pool = SqlitePool::connect(&connect_url).await.map_err(|e| {
            BridgeError::ProviderError(format!("Failed to connect to SQLite database: {e}"))
        })?;

        self.uri = config.uri.clone();
        self.pool = Some(pool);
        Ok(())
    }

    async fn read(&self, path: &str, options: ReadOptions) -> Result<ContextValue> {
        let pool = self.pool()?;
        let limit = options.limit.unwrap_or(100);

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
                let query = format!(
                    "SELECT * FROM {table_q} WHERE {pk_q} = ?1 LIMIT 1",
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
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(pool)
        .await?;

        let entries = rows
            .iter()
            .map(|row| {
                let name: String = row.get("name");
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
