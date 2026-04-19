// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only

use crate::error::{BridgeError, Result};
use crate::mcp::db_introspector;
use crate::mcp::db_tool_planner;
use crate::mcp::manifest::{Auth, Manifest, Runtime, Source, Transport};
use crate::mcp::{openapi, tool_mapper};
use crate::provider::postgres::PostgresProvider;
use serde_json::json;
use std::path::{Path, PathBuf};

#[allow(clippy::too_many_arguments)]
pub async fn execute_mcp(
    from: Vec<String>,
    connection: Option<String>,
    schema: Option<String>,
    name: String,
    base_url_env: Option<String>,
    bearer_env: Option<String>,
    out: String,
    force: bool,
    timeout_secs: u64,
) -> Result<()> {
    if from.is_empty() {
        return Err(BridgeError::ProviderError(
            "expected `--from <kind> [path]` (e.g. `--from openapi ./openapi.yaml` or `--from db`)"
                .into(),
        ));
    }
    let kind = from[0].to_lowercase();

    if name.trim().is_empty() {
        return Err(BridgeError::ProviderError(
            "`--name` cannot be empty".into(),
        ));
    }
    let out_path = PathBuf::from(&out);
    if out_path.exists() && !force {
        return Err(BridgeError::ProviderError(format!(
            "{} already exists. Re-run with --force to overwrite.",
            out_path.display()
        )));
    }

    match kind.as_str() {
        "openapi" => {
            if from.len() != 2 {
                return Err(BridgeError::ProviderError(
                    "expected `--from openapi <path>` (got a single argument)".into(),
                ));
            }
            execute_mcp_openapi(
                PathBuf::from(&from[1]),
                from[1].clone(),
                name,
                base_url_env,
                bearer_env,
                out_path,
            )
            .await
        }
        "db" => {
            let connection_name = connection.ok_or_else(|| {
                BridgeError::ProviderError(
                    "`--from db` requires `--connection <name>` (a provider from bridge.yaml)"
                        .into(),
                )
            })?;
            let schema = schema.unwrap_or_else(|| "public".to_string());
            execute_mcp_db(connection_name, schema, name, out_path, timeout_secs).await
        }
        other => Err(BridgeError::UnsupportedOperation(format!(
            "generate source '{other}' (supported: `openapi`, `db`)"
        ))),
    }
}

async fn execute_mcp_openapi(
    source_path: PathBuf,
    source_path_literal: String,
    name: String,
    base_url_env: Option<String>,
    bearer_env: Option<String>,
    out_path: PathBuf,
) -> Result<()> {
    let parsed = openapi::parse(&source_path)?;
    let tools = tool_mapper::map_operations(&parsed.operations)?;
    if tools.is_empty() {
        return Err(BridgeError::OpenApi(
            "no supported operations found in the spec (only GET is supported in MVP)".into(),
        ));
    }

    let source = Source::Openapi {
        path: source_path_literal,
    };
    let base_url = parsed.default_base_url.clone();
    if base_url_env.is_none() && base_url.is_none() {
        let reason = parsed.default_base_url_error.unwrap_or_else(|| {
            "no base URL available; pass `--base-url-env` or include a concrete OpenAPI `servers` entry"
                .into()
        });
        return Err(BridgeError::OpenApi(reason));
    }
    let runtime = Runtime {
        transport: Transport::Stdio,
        base_url_env,
        base_url,
        auth: bearer_env.map(|e| Auth::Bearer { token_env: e }),
    };
    let mut manifest = Manifest::new(name.clone(), source, runtime);
    manifest.tools = tools;
    manifest.validate()?;

    let yaml = manifest.to_yaml()?;
    write_manifest(&out_path, &yaml)?;

    let snippet = mcp_client_snippet(&name, &out_path);
    let output = json!({
        "status": "generated",
        "manifest": out_path.display().to_string(),
        "name": name,
        "tools": manifest.tools.iter().map(|t| &t.name).collect::<Vec<_>>(),
        "skipped": parsed.diagnostics,
        "client_snippet": snippet,
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

async fn execute_mcp_db(
    connection_name: String,
    schema: String,
    name: String,
    out_path: PathBuf,
    timeout_secs: u64,
) -> Result<()> {
    let timeout = std::time::Duration::from_secs(timeout_secs);
    let provider = PostgresProvider::connect_named(&connection_name, None, timeout_secs)
        .await
        .map_err(|e| match e {
            BridgeError::UnsupportedOperation(reason) => BridgeError::UnsupportedOperation(
                format!("`--from db` only supports postgres connections ({reason})"),
            ),
            other => other,
        })?;
    let pool = provider.pool_handle()?;

    let metadata = tokio::time::timeout(timeout, db_introspector::introspect(&pool, &schema))
        .await
        .map_err(|_| BridgeError::Timeout(timeout_secs))??;
    pool.close().await;

    if metadata.tables.is_empty() {
        return Err(BridgeError::ProviderError(format!(
            "schema '{schema}' contains no tables or views to expose. Connect a non-empty database and re-run."
        )));
    }

    let planned = db_tool_planner::plan(&metadata, &connection_name);
    if planned.tools.is_empty() {
        return Err(BridgeError::ProviderError(format!(
            "no tools could be generated from schema '{schema}' (every object was skipped). Diagnostics: {}",
            planned.diagnostics.join("; ")
        )));
    }

    let source = Source::Db {
        connection: connection_name.clone(),
        dialect: "postgres".into(),
        schema: schema.clone(),
    };
    let runtime = Runtime {
        transport: Transport::Stdio,
        base_url_env: None,
        base_url: None,
        auth: None,
    };
    let mut manifest = Manifest::new(name.clone(), source, runtime);
    manifest.tools = planned.tools;
    manifest.validate()?;

    let yaml = manifest.to_yaml()?;
    write_manifest(&out_path, &yaml)?;

    let snippet = mcp_client_snippet(&name, &out_path);
    let output = json!({
        "status": "generated",
        "manifest": out_path.display().to_string(),
        "name": name,
        "connection": connection_name,
        "schema": schema,
        "tools": manifest.tools.iter().map(|t| &t.name).collect::<Vec<_>>(),
        "skipped": planned.diagnostics,
        "client_snippet": snippet,
        "next_steps": [
            format!("Start the server: bridge mcp serve {}", out_path.display()),
            "Register the `client_snippet` with your MCP client (Claude Desktop, Cursor, etc.)",
        ],
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn write_manifest(out_path: &Path, yaml: &str) -> Result<()> {
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(out_path, yaml)?;
    Ok(())
}

fn mcp_client_snippet(name: &str, manifest_path: &std::path::Path) -> serde_json::Value {
    // Emit the Claude Desktop / Cursor-compatible shape. Clients vary, but the
    // `mcpServers.<name> = { command, args }` pattern is the broadly accepted
    // lowest-common-denominator config.
    let bridge_cmd = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "bridge".to_string());
    let abs_manifest = std::fs::canonicalize(manifest_path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| manifest_path.display().to_string());
    json!({
        "mcpServers": {
            name: {
                "command": bridge_cmd,
                "args": ["mcp", "serve", abs_manifest],
            }
        }
    })
}
