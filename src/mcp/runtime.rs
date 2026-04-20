// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Stdio adapter + local wiring for the MCP service. This module is the "local"
// host: it resolves HTTP auth from process env and SQL connections from the
// Bridge config directory, assembles an `ExecutorBundle`, and drives the
// service over newline-delimited JSON-RPC 2.0 on stdin/stdout.
//
// A future remote host supplies its own `ExecutorBundle` (tenant secret scope,
// managed connection registry) and calls `McpService::new` directly — nothing
// in this file is on that path.

use crate::error::{BridgeError, Result};
use crate::mcp::executor::HttpExecutor;
use crate::mcp::manifest::{Execute, Manifest};
use crate::mcp::service::{ExecutorBundle, McpService};
use crate::mcp::sql_executor::SqlExecutor;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Build an `ExecutorBundle` from the local Bridge config layer: HTTP auth
/// comes from process env, SQL connections from `config_dir`. Only the arms
/// a manifest actually needs are constructed, so a pure-DB manifest doesn't
/// demand an HTTP base URL it will never call.
pub fn build_local_executors(
    manifest: &Manifest,
    config_dir: &Path,
    timeout_secs: u64,
) -> Result<ExecutorBundle> {
    let mut bundle = ExecutorBundle::new();

    if manifest
        .tools
        .iter()
        .any(|t| matches!(t.execute, Execute::Http(_)))
    {
        let http = HttpExecutor::from_manifest(manifest, timeout_secs)?;
        bundle = bundle.with_http(Arc::new(http));
    }

    if manifest
        .tools
        .iter()
        .any(|t| matches!(t.execute, Execute::SqlSelect(_)))
    {
        let sql = SqlExecutor::from_manifest(manifest, Some(config_dir), timeout_secs)?;
        bundle = bundle.with_sql(Arc::new(sql));
    }

    Ok(bundle)
}

pub async fn serve(manifest: Manifest, timeout_secs: u64, config_dir: &Path) -> Result<()> {
    let executors = build_local_executors(&manifest, config_dir, timeout_secs)?;
    let service = McpService::new(manifest, executors)?;

    eprintln!(
        "bridge mcp: serving '{}' with {} tool(s) over stdio",
        service.manifest().name,
        service.manifest().tools.len()
    );

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();

    while let Some(line) = reader
        .next_line()
        .await
        .map_err(|e| BridgeError::McpRuntime(format!("stdin read failed: {e}")))?
    {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Value>(trimmed) {
            Ok(req) => service.handle_jsonrpc(req).await,
            Err(e) => Some(json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": { "code": -32700, "message": format!("parse error: {e}") }
            })),
        };

        if let Some(resp) = response {
            let mut text = serde_json::to_string(&resp)
                .map_err(|e| BridgeError::McpRuntime(format!("encode response: {e}")))?;
            text.push('\n');
            stdout
                .write_all(text.as_bytes())
                .await
                .map_err(|e| BridgeError::McpRuntime(format!("stdout write failed: {e}")))?;
            stdout
                .flush()
                .await
                .map_err(|e| BridgeError::McpRuntime(format!("stdout flush failed: {e}")))?;
        }
    }

    Ok(())
}
