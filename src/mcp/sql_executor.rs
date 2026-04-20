// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Execute manifest-defined read-only SQL plans against a Bridge Postgres
// connection. Every column, filter, and sort key appearing in the emitted
// SQL is validated against allowlists built by the generator — values are
// always bound as parameters, never interpolated into the statement.

use crate::error::{BridgeError, Result};
use crate::mcp::manifest::{LimitSpec, Manifest, SqlColumnType, SqlSelectExecute, SqlSelectMode};
use crate::mcp::service::SqlExecuting;
use crate::provider::load_named_provider_config;
use crate::provider::postgres::PostgresProvider;
use async_trait::async_trait;
use serde_json::Value;
use sqlx::postgres::PgPool;
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
    pools: Arc<Mutex<HashMap<String, PgPool>>>,
    config_dir: Option<PathBuf>,
    connect_timeout_secs: u64,
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
                config_dir: config_dir.map(Path::to_path_buf),
                connect_timeout_secs,
            });
        }

        for name in refs {
            let provider = load_named_provider_config(&name, config_dir)?;
            if provider.provider_type != "postgres" {
                return Err(BridgeError::UnsupportedOperation(format!(
                    "MCP SQL tools require a postgres connection; '{name}' is of type '{}'",
                    provider.provider_type
                )));
            }
        }
        Ok(Self {
            pools: Arc::new(Mutex::new(HashMap::new())),
            config_dir: config_dir.map(Path::to_path_buf),
            connect_timeout_secs,
        })
    }

    async fn pool_for(&self, connection_ref: &str) -> Result<PgPool> {
        {
            let pools = self.pools.lock().await;
            if let Some(p) = pools.get(connection_ref) {
                return Ok(p.clone());
            }
        }
        let provider = PostgresProvider::connect_named(
            connection_ref,
            self.config_dir.as_deref(),
            self.connect_timeout_secs,
        )
        .await?;
        let pool = provider.pool_handle()?;
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

        let obj = input.as_object();
        let mut where_clauses: Vec<String> = Vec::new();
        let mut bind_values: Vec<Value> = Vec::new();

        let filter_cols_lookup: std::collections::HashSet<&str> =
            plan.filterable_columns.iter().map(String::as_str).collect();

        if matches!(plan.mode, SqlSelectMode::GetByKey) {
            let key = plan.key_column.as_deref().ok_or_else(|| {
                BridgeError::Manifest("get_by_key plan missing `key_column`".into())
            })?;
            let v = obj
                .and_then(|o| o.get(key))
                .ok_or_else(|| BridgeError::Http(format!("missing required key '{key}'")))?;
            where_clauses.push(format!("{} = ${}", quote_ident(key), bind_values.len() + 1));
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
                where_clauses.push(format!("{} = ${}", quote_ident(k), bind_values.len() + 1));
                bind_values.push(v.clone());
            }
        }

        let select_list = if plan.selectable_columns.is_empty() {
            "*".to_string()
        } else {
            plan.selectable_columns
                .iter()
                .map(|c| select_expression(c, plan.column_types.get(c)))
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

        let sql =
            format!("SELECT {select_list} FROM {from_clause}{where_sql}{order_sql}{limit_sql}");

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

        match plan.mode {
            SqlSelectMode::List => Ok(serde_json::json!({
                "ok": true,
                "rows": json_rows,
                "count": json_rows.len(),
            })),
            SqlSelectMode::GetByKey => {
                if let Some(row) = json_rows.into_iter().next() {
                    Ok(serde_json::json!({ "ok": true, "found": true, "row": row }))
                } else {
                    Ok(serde_json::json!({ "ok": true, "found": false, "row": null }))
                }
            }
        }
    }
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
