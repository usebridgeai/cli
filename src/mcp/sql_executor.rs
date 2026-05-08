// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Execute manifest-defined read-only SQL plans against Bridge database
// connections. Every column, filter, and sort key appearing in the emitted SQL
// is validated against allowlists built by the generator — values are always
// bound as parameters, never interpolated into the statement.

use crate::error::{BridgeError, Result};
use crate::mcp::manifest::{LimitSpec, Manifest, SqlColumnType, SqlSelectExecute, SqlSelectMode};
use crate::mcp::service::SqlExecuting;
use crate::provider::load_named_provider_config;
use crate::provider::postgres::PostgresProvider;
use crate::provider::sqlite::SqliteProvider;
use async_trait::async_trait;
use serde_json::Value;
use sqlx::postgres::PgPool;
use sqlx::sqlite::SqlitePool;
use sqlx::Row;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Hard ceiling enforced by the server regardless of per-tool `limit.max`.
/// Keeps a misconfigured manifest from pulling unbounded rows.
pub const HARD_ROW_CAP: u32 = 1000;

/// Applied via `SET LOCAL statement_timeout` before each query. 10s is more
/// than enough for the small, pk-indexed reads these tools generate.
const STATEMENT_TIMEOUT_MS: u64 = 10_000;

pub struct SqlExecutor {
    pools: Arc<Mutex<HashMap<String, SqlPool>>>,
    provider_types: HashMap<String, String>,
    config_dir: Option<PathBuf>,
    connect_timeout_secs: u64,
}

#[derive(Clone)]
enum SqlPool {
    Postgres(PgPool),
    Sqlite(SqlitePool),
}

impl SqlExecutor {
    /// Inspect the manifest, collect every `connection_ref` referenced by a
    /// SQL plan, resolve each one through Bridge's config layer, and fail fast
    /// if any connection is missing or unsupported.
    pub fn from_manifest(
        manifest: &Manifest,
        config_dir: Option<&Path>,
        connect_timeout_secs: u64,
    ) -> Result<Self> {
        use crate::mcp::manifest::Execute;
        let mut refs: Vec<String> = Vec::new();
        for tool in &manifest.tools {
            if let Execute::SqlSelect(plan) = &tool.execute {
                if !refs.iter().any(|r| r == &plan.connection_ref) {
                    refs.push(plan.connection_ref.clone());
                }
            }
        }
        if refs.is_empty() {
            return Ok(Self {
                pools: Arc::new(Mutex::new(HashMap::new())),
                provider_types: HashMap::new(),
                config_dir: config_dir.map(Path::to_path_buf),
                connect_timeout_secs,
            });
        }

        let mut provider_types = HashMap::new();
        for name in refs {
            let provider = load_named_provider_config(&name, config_dir)?;
            if !matches!(provider.provider_type.as_str(), "postgres" | "sqlite") {
                return Err(BridgeError::UnsupportedOperation(format!(
                    "MCP SQL tools require a postgres or sqlite connection; '{name}' is of type '{}'",
                    provider.provider_type
                )));
            }
            provider_types.insert(name, provider.provider_type);
        }
        Ok(Self {
            pools: Arc::new(Mutex::new(HashMap::new())),
            provider_types,
            config_dir: config_dir.map(Path::to_path_buf),
            connect_timeout_secs,
        })
    }

    async fn pool_for(&self, connection_ref: &str) -> Result<SqlPool> {
        {
            let pools = self.pools.lock().await;
            if let Some(p) = pools.get(connection_ref) {
                return Ok(p.clone());
            }
        }
        let provider_type = self.provider_types.get(connection_ref).ok_or_else(|| {
            BridgeError::ProviderError(format!(
                "SQL connection '{connection_ref}' was not resolved during startup"
            ))
        })?;
        let pool = match provider_type.as_str() {
            "postgres" => {
                let provider = PostgresProvider::connect_named(
                    connection_ref,
                    self.config_dir.as_deref(),
                    self.connect_timeout_secs,
                )
                .await?;
                SqlPool::Postgres(provider.pool_handle()?)
            }
            "sqlite" => {
                let provider = SqliteProvider::connect_named(
                    connection_ref,
                    self.config_dir.as_deref(),
                    self.connect_timeout_secs,
                )
                .await?;
                SqlPool::Sqlite(provider.pool_handle()?)
            }
            other => {
                return Err(BridgeError::UnsupportedOperation(format!(
                    "unsupported SQL provider type '{other}'"
                )));
            }
        };
        let mut pools = self.pools.lock().await;
        // Double-check after re-acquiring the lock: a concurrent caller may
        // have already inserted a pool while we were connecting. Discard ours
        // cleanly to avoid leaking connection slots.
        if let Some(existing) = pools.get(connection_ref) {
            drop(pool); // Drop without close — PgPool's Drop handles cleanup.
            return Ok(existing.clone());
        }
        pools.insert(connection_ref.to_string(), pool.clone());
        Ok(pool)
    }

    pub async fn call(&self, plan: &SqlSelectExecute, input: &Value) -> Result<Value> {
        let pool = self.pool_for(&plan.connection_ref).await?;
        match pool {
            SqlPool::Postgres(pool) => call_postgres(&pool, plan, input).await,
            SqlPool::Sqlite(pool) => call_sqlite(&pool, plan, input).await,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Dialect {
    Postgres,
    Sqlite,
}

impl Dialect {
    /// Positional placeholder: Postgres is `$N`, SQLite is `?N`.
    fn placeholder(self, n: usize) -> String {
        match self {
            Dialect::Postgres => format!("${n}"),
            Dialect::Sqlite => format!("?{n}"),
        }
    }

    fn select_expr(self, name: &str, ct: Option<&SqlColumnType>) -> String {
        match self {
            Dialect::Postgres => select_expression(name, ct),
            Dialect::Sqlite => sqlite_select_expression(name, ct),
        }
    }
}

/// Build the final SQL string and bind values for a plan. This is the single
/// source of truth for how plans become queries — both dialects call through
/// here so that any future change (new filter, new pagination mode, tighter
/// validation) automatically applies to Postgres and SQLite.
fn build_sql(
    plan: &SqlSelectExecute,
    input: &Value,
    dialect: Dialect,
) -> Result<(String, Vec<Value>)> {
    let obj = input.as_object();
    let mut where_clauses: Vec<String> = Vec::new();
    let mut bind_values: Vec<Value> = Vec::new();

    let filter_cols_lookup: std::collections::HashSet<&str> =
        plan.filterable_columns.iter().map(String::as_str).collect();

    if matches!(plan.mode, SqlSelectMode::GetByKey) {
        let key = plan
            .key_column
            .as_deref()
            .ok_or_else(|| BridgeError::Manifest("get_by_key plan missing `key_column`".into()))?;
        let v = obj
            .and_then(|o| o.get(key))
            .ok_or_else(|| BridgeError::Http(format!("missing required key '{key}'")))?;
        where_clauses.push(format!(
            "{} = {}",
            quote_ident(key),
            dialect.placeholder(bind_values.len() + 1)
        ));
        bind_values.push(v.clone());
    }

    if let Some(o) = obj {
        for (k, v) in o {
            if v.is_null() {
                continue;
            }
            // `limit`, `offset`, `order_by`, `order_direction` are handled
            // separately below.
            if matches!(
                k.as_str(),
                "limit" | "offset" | "order_by" | "order_direction"
            ) {
                continue;
            }
            if matches!(plan.mode, SqlSelectMode::GetByKey)
                && plan.key_column.as_deref() == Some(k.as_str())
            {
                continue;
            }
            if !filter_cols_lookup.contains(k.as_str()) {
                // Schema validator should already block these, but guard
                // defensively so we never emit SQL for an unlisted column.
                continue;
            }
            where_clauses.push(format!(
                "{} = {}",
                quote_ident(k),
                dialect.placeholder(bind_values.len() + 1)
            ));
            bind_values.push(v.clone());
        }
    }

    let select_list = if plan.selectable_columns.is_empty() {
        "*".to_string()
    } else {
        plan.selectable_columns
            .iter()
            .map(|c| dialect.select_expr(c, plan.column_types.get(c)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let from_clause = format!("{}.{}", quote_ident(&plan.schema), quote_ident(&plan.table));
    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_clauses.join(" AND "))
    };

    let mut order_sql = String::new();
    if let Some(obj) = obj {
        if let Some(col) = obj.get("order_by").and_then(|v| v.as_str()) {
            if plan.sortable_columns.iter().any(|c| c == col) {
                let dir = match obj.get("order_direction").and_then(|v| v.as_str()) {
                    Some("asc") | None => "ASC",
                    Some("desc") => "DESC",
                    Some(other) => {
                        return Err(BridgeError::ToolInputInvalid {
                            tool: plan.table.clone(),
                            reason: format!(
                                "order_direction '{other}' is invalid; allowed: [asc, desc]"
                            ),
                        });
                    }
                };
                order_sql = format!(" ORDER BY {} {}", quote_ident(col), dir);
            } else {
                return Err(BridgeError::ToolInputInvalid {
                    tool: plan.table.clone(),
                    reason: format!(
                        "order_by '{col}' is not an allowed sort column; allowed: [{}]",
                        plan.sortable_columns.join(", ")
                    ),
                });
            }
        }
    }

    let (effective_limit, effective_offset) = match plan.mode {
        SqlSelectMode::List => {
            let spec = plan.limit.as_ref().copied().unwrap_or(LimitSpec {
                default: 50,
                max: 200,
            });
            let requested = obj
                .and_then(|o| o.get("limit"))
                .and_then(|v| v.as_u64())
                .map(|n| n as u32)
                .unwrap_or(spec.default);
            let cap = std::cmp::min(spec.max, HARD_ROW_CAP);
            let limit = std::cmp::min(requested, cap);
            let offset = obj
                .and_then(|o| o.get("offset"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            (Some(limit), Some(offset))
        }
        SqlSelectMode::GetByKey => (Some(1), None),
    };

    let limit_sql = match (effective_limit, effective_offset) {
        (Some(l), Some(o)) if o > 0 => format!(" LIMIT {l} OFFSET {o}"),
        (Some(l), _) => format!(" LIMIT {l}"),
        _ => String::new(),
    };

    let sql = format!("SELECT {select_list} FROM {from_clause}{where_sql}{order_sql}{limit_sql}");
    Ok((sql, bind_values))
}

/// Package the decoded rows into the uniform `{ok, rows, count}` /
/// `{ok, found, row}` response shape that both dialects share.
fn finalize_rows(plan: &SqlSelectExecute, json_rows: Vec<Value>) -> Value {
    match plan.mode {
        SqlSelectMode::List => serde_json::json!({
            "ok": true,
            "rows": json_rows,
            "count": json_rows.len(),
        }),
        SqlSelectMode::GetByKey => {
            if let Some(row) = json_rows.into_iter().next() {
                serde_json::json!({ "ok": true, "found": true, "row": row })
            } else {
                serde_json::json!({ "ok": true, "found": false, "row": null })
            }
        }
    }
}

async fn call_postgres(pool: &PgPool, plan: &SqlSelectExecute, input: &Value) -> Result<Value> {
    let (sql, bind_values) = build_sql(plan, input, Dialect::Postgres)?;

    // Apply per-transaction statement timeout + read-only mode so a
    // misbehaving plan or slow table can never turn the server into a
    // resource sink. `SET LOCAL` keeps both settings scoped to this tx.
    let mut tx = pool.begin().await?;
    sqlx::query(&format!(
        "SET LOCAL statement_timeout = {STATEMENT_TIMEOUT_MS}"
    ))
    .execute(&mut *tx)
    .await?;
    sqlx::query("SET LOCAL transaction_read_only = on")
        .execute(&mut *tx)
        .await?;

    let mut query = sqlx::query(&sql);
    for v in &bind_values {
        query = bind_json(query, v);
    }
    let rows = query.fetch_all(&mut *tx).await?;
    // Rollback (not commit) is the correct terminal for a read-only
    // transaction — it signals no data was changed and releases locks
    // faster than commit on busy replicas.
    tx.rollback().await?;

    let json_rows: Vec<Value> = rows
        .iter()
        .map(|row| row_to_json(row, plan))
        .collect::<Result<_>>()?;

    Ok(finalize_rows(plan, json_rows))
}

async fn call_sqlite(pool: &SqlitePool, plan: &SqlSelectExecute, input: &Value) -> Result<Value> {
    let (sql, bind_values) = build_sql(plan, input, Dialect::Sqlite)?;

    // SQLite has no per-statement timeout or transaction read-only flag like
    // Postgres, so we acquire a dedicated connection, pin `query_only = ON` on
    // it for the duration of this call, and wrap the fetch in a wall-clock
    // timeout. `query_only` rejects any write (INSERT/UPDATE/DELETE/DDL) at the
    // SQL layer even if a malformed plan slipped past the manifest validator.
    let mut conn = pool.acquire().await?;
    sqlx::query("PRAGMA query_only = ON")
        .execute(&mut *conn)
        .await?;

    let mut query = sqlx::query(&sql);
    for v in &bind_values {
        query = bind_json_sqlite(query, v);
    }
    let rows = match tokio::time::timeout(
        std::time::Duration::from_millis(STATEMENT_TIMEOUT_MS),
        query.fetch_all(&mut *conn),
    )
    .await
    {
        Ok(res) => res?,
        Err(_) => return Err(BridgeError::Timeout(STATEMENT_TIMEOUT_MS / 1000)),
    };
    let json_rows: Vec<Value> = rows
        .iter()
        .map(|row| sqlite_row_to_json(row, plan))
        .collect::<Result<_>>()?;

    Ok(finalize_rows(plan, json_rows))
}

#[async_trait]
impl SqlExecuting for SqlExecutor {
    async fn call(&self, plan: &SqlSelectExecute, input: &Value) -> Result<Value> {
        SqlExecutor::call(self, plan, input).await
    }
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn select_expression(name: &str, column_type: Option<&SqlColumnType>) -> String {
    let ident = quote_ident(name);
    match column_type {
        Some(SqlColumnType::Integer) => format!("{ident}::bigint AS {ident}"),
        Some(SqlColumnType::Float) => format!("{ident}::double precision AS {ident}"),
        Some(SqlColumnType::Numeric)
        | Some(SqlColumnType::Timestamp)
        | Some(SqlColumnType::Uuid) => {
            format!("{ident}::text AS {ident}")
        }
        _ => ident,
    }
}

/// SQLite uses dynamic typing with affinity rules rather than explicit server-side
/// casts, so unlike Postgres we don't need per-column `::bigint` / `::text` wrappers
/// — `sqlite_column_value` handles conversion on the decode side. The
/// `_column_type` parameter is kept for signature symmetry with
/// `select_expression` so `build_sql` can treat both dialects uniformly.
fn sqlite_select_expression(name: &str, _column_type: Option<&SqlColumnType>) -> String {
    quote_ident(name)
}

/// Bind a JSON value into a sqlx::query as the most natural Postgres type.
/// We deliberately accept a narrow set: the schema validator already rejects
/// compound values for scalar filter slots, but we still re-check defensively
/// here and bind as text otherwise (which Postgres will coerce via the
/// column's implicit cast rules — safe because the column is whitelisted).
fn bind_json<'q>(
    query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    v: &'q Value,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    match v {
        Value::Null => query.bind(None::<String>),
        Value::Bool(b) => query.bind(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                query.bind(i)
            } else if let Some(f) = n.as_f64() {
                query.bind(f)
            } else {
                query.bind(n.to_string())
            }
        }
        Value::String(s) => query.bind(s.as_str()),
        // Arrays/objects shouldn't make it here for whitelisted scalar
        // filters, but fall back to a JSON string so we fail at the
        // database layer rather than panic.
        _ => query.bind(v.to_string()),
    }
}

fn bind_json_sqlite<'q>(
    query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    v: &'q Value,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    match v {
        Value::Null => query.bind(None::<String>),
        Value::Bool(b) => query.bind(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                query.bind(i)
            } else if let Some(f) = n.as_f64() {
                query.bind(f)
            } else {
                query.bind(n.to_string())
            }
        }
        Value::String(s) => query.bind(s.as_str()),
        _ => query.bind(v.to_string()),
    }
}

fn sqlite_row_to_json(row: &sqlx::sqlite::SqliteRow, plan: &SqlSelectExecute) -> Result<Value> {
    if plan.column_types.is_empty() {
        return fallback_sqlite_row_to_json(row);
    }

    let mut map = serde_json::Map::new();
    for name in &plan.selectable_columns {
        let column_type = plan.column_types.get(name).ok_or_else(|| {
            BridgeError::Manifest(format!(
                "sql plan missing column_types entry for selectable column '{name}'"
            ))
        })?;
        let value = sqlite_column_value(row, name, *column_type);
        map.insert(name.clone(), value);
    }
    Ok(Value::Object(map))
}

fn sqlite_column_value(
    row: &sqlx::sqlite::SqliteRow,
    name: &str,
    column_type: SqlColumnType,
) -> Value {
    // Emit a one-line warning when a typed column fails to decode — lets the
    // operator catch column_type/storage mismatches rather than silently
    // returning nulls forever.
    fn warn_decode(name: &str, expected: &str, err: &sqlx::Error) {
        eprintln!(
            "bridge mcp: sqlite column '{name}' expected {expected} but decode failed ({err}); returning null"
        );
    }

    match column_type {
        SqlColumnType::Integer => match row.try_get::<Option<i64>, _>(name) {
            Ok(Some(v)) => Value::Number(v.into()),
            Ok(None) => Value::Null,
            Err(e) => {
                warn_decode(name, "integer", &e);
                Value::Null
            }
        },
        SqlColumnType::Float => match row.try_get::<Option<f64>, _>(name) {
            Ok(Some(v)) => serde_json::Number::from_f64(v)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            Ok(None) => Value::Null,
            Err(e) => {
                warn_decode(name, "float", &e);
                Value::Null
            }
        },
        SqlColumnType::Boolean => match row.try_get::<Option<bool>, _>(name) {
            Ok(Some(v)) => Value::Bool(v),
            Ok(None) => Value::Null,
            // SQLite stores booleans as 0/1 integers — fall back before warning.
            Err(_) => match row.try_get::<Option<i64>, _>(name) {
                Ok(Some(v)) => Value::Bool(v != 0),
                Ok(None) => Value::Null,
                Err(e) => {
                    warn_decode(name, "boolean", &e);
                    Value::Null
                }
            },
        },
        SqlColumnType::Json => match row.try_get::<Option<String>, _>(name) {
            Ok(Some(s)) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
            Ok(None) => Value::Null,
            Err(e) => {
                warn_decode(name, "json (text)", &e);
                Value::Null
            }
        },
        SqlColumnType::Numeric | SqlColumnType::Timestamp | SqlColumnType::Uuid => {
            if let Ok(Some(s)) = row.try_get::<Option<String>, _>(name) {
                Value::String(s)
            } else if let Ok(Some(i)) = row.try_get::<Option<i64>, _>(name) {
                Value::String(i.to_string())
            } else if let Ok(Some(f)) = row.try_get::<Option<f64>, _>(name) {
                Value::String(f.to_string())
            } else {
                // Every decode attempt failed — emit one warning so the operator
                // can see the mismatch rather than silently returning null.
                if let Err(e) = row.try_get::<Option<String>, _>(name) {
                    warn_decode(name, "text/int/float", &e);
                }
                Value::Null
            }
        }
        SqlColumnType::Text => match row.try_get::<Option<String>, _>(name) {
            Ok(Some(v)) => Value::String(v),
            Ok(None) => Value::Null,
            Err(e) => {
                warn_decode(name, "text", &e);
                Value::Null
            }
        },
    }
}

fn fallback_sqlite_row_to_json(row: &sqlx::sqlite::SqliteRow) -> Result<Value> {
    use sqlx::{Column, Row, TypeInfo, ValueRef};

    let mut map = serde_json::Map::new();
    for col in row.columns() {
        let name = col.name().to_string();
        let ordinal = col.ordinal();
        let type_name = col.type_info().name().to_ascii_uppercase();
        let value = if row.try_get_raw(ordinal)?.is_null() {
            Value::Null
        } else if type_name.contains("INT") {
            row.try_get::<i64, _>(ordinal)
                .map(|v| Value::Number(v.into()))
                .unwrap_or(Value::Null)
        } else if type_name.contains("REAL")
            || type_name.contains("FLOA")
            || type_name.contains("DOUB")
        {
            row.try_get::<f64, _>(ordinal)
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number)
                .unwrap_or(Value::Null)
        } else {
            row.try_get::<String, _>(ordinal)
                .map(Value::String)
                .unwrap_or(Value::Null)
        };
        map.insert(name, value);
    }
    Ok(Value::Object(map))
}

fn row_to_json(row: &sqlx::postgres::PgRow, plan: &SqlSelectExecute) -> Result<Value> {
    if plan.column_types.is_empty() {
        return fallback_row_to_json(row);
    }

    let mut map = serde_json::Map::new();
    for name in &plan.selectable_columns {
        let column_type = plan.column_types.get(name).ok_or_else(|| {
            BridgeError::Manifest(format!(
                "sql plan missing column_types entry for selectable column '{name}'"
            ))
        })?;
        let value = match column_type {
            SqlColumnType::Integer => match row.try_get::<Option<i64>, _>(name.as_str()) {
                Ok(Some(v)) => Value::Number(v.into()),
                _ => Value::Null,
            },
            SqlColumnType::Float => match row.try_get::<Option<f64>, _>(name.as_str()) {
                Ok(Some(v)) => serde_json::Number::from_f64(v)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                _ => Value::Null,
            },
            SqlColumnType::Boolean => match row.try_get::<Option<bool>, _>(name.as_str()) {
                Ok(Some(v)) => Value::Bool(v),
                _ => Value::Null,
            },
            SqlColumnType::Json => match row.try_get::<Option<Value>, _>(name.as_str()) {
                Ok(Some(v)) => v,
                _ => Value::Null,
            },
            SqlColumnType::Numeric
            | SqlColumnType::Text
            | SqlColumnType::Timestamp
            | SqlColumnType::Uuid => match row.try_get::<Option<String>, _>(name.as_str()) {
                Ok(Some(v)) => Value::String(v),
                _ => Value::Null,
            },
        };
        map.insert(name.clone(), value);
    }
    Ok(Value::Object(map))
}

fn fallback_row_to_json(row: &sqlx::postgres::PgRow) -> Result<Value> {
    use sqlx::{Column, Row, TypeInfo};

    let columns = row.columns();
    let mut map = serde_json::Map::new();
    for col in columns {
        let name = col.name().to_string();
        let type_name = col.type_info().name();
        let value: Value = match type_name {
            "INT8" | "BIGSERIAL" => match row.try_get::<Option<i64>, _>(col.ordinal()) {
                Ok(Some(v)) => Value::Number(v.into()),
                _ => Value::Null,
            },
            "INT4" | "SERIAL" => match row.try_get::<Option<i32>, _>(col.ordinal()) {
                Ok(Some(v)) => Value::Number(v.into()),
                _ => Value::Null,
            },
            "INT2" | "SMALLSERIAL" => match row.try_get::<Option<i16>, _>(col.ordinal()) {
                Ok(Some(v)) => Value::Number(v.into()),
                _ => Value::Null,
            },
            "FLOAT4" | "FLOAT8" => match row.try_get::<Option<f64>, _>(col.ordinal()) {
                Ok(Some(v)) => serde_json::Number::from_f64(v)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                _ => Value::Null,
            },
            "BOOL" => match row.try_get::<Option<bool>, _>(col.ordinal()) {
                Ok(Some(v)) => Value::Bool(v),
                _ => Value::Null,
            },
            "JSON" | "JSONB" => match row.try_get::<Option<Value>, _>(col.ordinal()) {
                Ok(Some(v)) => v,
                _ => Value::Null,
            },
            _ => match row.try_get::<Option<String>, _>(col.ordinal()) {
                Ok(Some(v)) => Value::String(v),
                _ => Value::Null,
            },
        };
        map.insert(name, value);
    }
    Ok(Value::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use serde_json::json;

    fn list_plan() -> SqlSelectExecute {
        SqlSelectExecute {
            connection_ref: "test".into(),
            schema: "main".into(),
            table: "customers".into(),
            mode: SqlSelectMode::List,
            selectable_columns: vec!["id".into(), "email".into(), "status".into()],
            column_types: IndexMap::new(),
            filterable_columns: vec!["status".into()],
            sortable_columns: vec!["id".into()],
            key_column: None,
            limit: None,
        }
    }

    #[test]
    fn quote_ident_escapes_embedded_double_quote() {
        // Defense-in-depth check: the manifest validator rejects identifiers
        // containing quotes at load time, but if one ever slips through we
        // must double-up the quote so the SQL cannot be broken out of.
        assert_eq!(quote_ident(r#"weird"name"#), r#""weird""name""#);
        assert_eq!(quote_ident("normal"), r#""normal""#);
    }

    #[test]
    fn build_sql_drops_unauthorized_column_filters() {
        // An attacker-controlled input keys (like `email`) that are not in
        // filterable_columns must never reach the SQL or bind values.
        let plan = list_plan();
        let input = json!({
            "status": "active",
            "email": "x'; DROP TABLE users; --",
        });

        for dialect in [Dialect::Postgres, Dialect::Sqlite] {
            let (sql, binds) = build_sql(&plan, &input, dialect).unwrap();
            // `email` is a selectable column so it appears in the SELECT list
            // — what we care about is that it never lands in WHERE and no
            // bind value carries the injection payload.
            let where_clause = sql
                .split_once(" WHERE ")
                .map(|(_, rest)| rest)
                .unwrap_or("");
            assert!(
                !where_clause.contains("\"email\""),
                "{dialect:?}: email must not appear in WHERE: {sql}"
            );
            assert_eq!(binds.len(), 1, "{dialect:?}: only status should bind");
            assert_eq!(binds[0], json!("active"));
            assert!(!binds
                .iter()
                .any(|v| v.as_str().unwrap_or("").contains("DROP")));
        }
    }

    #[test]
    fn build_sql_uses_dialect_specific_placeholders() {
        let plan = list_plan();
        let input = json!({ "status": "active" });

        let (pg_sql, _) = build_sql(&plan, &input, Dialect::Postgres).unwrap();
        assert!(pg_sql.contains("$1"), "postgres sql: {pg_sql}");

        let (lite_sql, _) = build_sql(&plan, &input, Dialect::Sqlite).unwrap();
        assert!(lite_sql.contains("?1"), "sqlite sql: {lite_sql}");
    }

    #[test]
    fn build_sql_rejects_unknown_order_direction() {
        let plan = list_plan();
        let input = json!({ "order_by": "id", "order_direction": "random" });
        let err = build_sql(&plan, &input, Dialect::Sqlite).unwrap_err();
        assert!(
            matches!(err, BridgeError::ToolInputInvalid { .. }),
            "got {err:?}"
        );
    }
}
