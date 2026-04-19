// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Minimal MCP stdio server: newline-delimited JSON-RPC 2.0 on stdin/stdout.
// Implements the subset required for a client (Claude Desktop, Cursor, etc.)
// to discover and call generated tools. All logs go to stderr to keep the
// stdio transport clean — a single stray write to stdout would desync the
// client.

use crate::error::{BridgeError, Result};
use crate::mcp::executor::HttpExecutor;
use crate::mcp::manifest::{Execute, Manifest, Tool};
use crate::mcp::schema;
use crate::mcp::sql_executor::SqlExecutor;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// MCP protocol version the server advertises. Clients negotiate down if needed.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "bridge";

pub async fn serve(manifest: Manifest, timeout_secs: u64, config_dir: &Path) -> Result<()> {
    manifest.validate()?;

    // Build the executors we actually need. HTTP is only constructed when at
    // least one tool uses it, so a pure-DB manifest doesn't demand a base URL
    // or bearer token it will never call. Both fail fast on missing config —
    // exactly what the ticket's acceptance criteria asks for.
    let has_http = manifest
        .tools
        .iter()
        .any(|t| matches!(t.execute, Execute::Http(_)));
    let http_executor = if has_http {
        Some(HttpExecutor::from_manifest(&manifest, timeout_secs)?)
    } else {
        None
    };
    let sql_executor = SqlExecutor::from_manifest(&manifest, Some(config_dir), timeout_secs)?;

    let tools_by_name: HashMap<String, Tool> = manifest
        .tools
        .iter()
        .cloned()
        .map(|t| (t.name.clone(), t))
        .collect();

    eprintln!(
        "bridge mcp: serving '{}' with {} tool(s) over stdio",
        manifest.name,
        manifest.tools.len()
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

        let response = handle_line(
            trimmed,
            &manifest,
            &tools_by_name,
            http_executor.as_ref(),
            &sql_executor,
        )
        .await;

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

async fn handle_line(
    line: &str,
    manifest: &Manifest,
    tools: &HashMap<String, Tool>,
    http_executor: Option<&HttpExecutor>,
    sql_executor: &SqlExecutor,
) -> Option<Value> {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(rpc_error(Value::Null, -32700, &format!("parse error: {e}")));
        }
    };

    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = req.get("id").cloned();
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let is_notification = id.is_none();

    let result = dispatch(method, params, manifest, tools, http_executor, sql_executor).await;

    if is_notification {
        // Notifications never get a response — even on error.
        if let Err(e) = result {
            eprintln!("bridge mcp: notification '{method}' errored: {e}");
        }
        return None;
    }

    let id = id.unwrap_or(Value::Null);
    match result {
        Ok(Some(value)) => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": value,
        })),
        Ok(None) => Some(rpc_error(
            id,
            -32601,
            &format!("method not found: {method}"),
        )),
        Err(e) => Some(rpc_error(id, -32000, &e.to_string())),
    }
}

async fn dispatch(
    method: &str,
    params: Value,
    manifest: &Manifest,
    tools: &HashMap<String, Tool>,
    http_executor: Option<&HttpExecutor>,
    sql_executor: &SqlExecutor,
) -> Result<Option<Value>> {
    match method {
        "initialize" => Ok(Some(initialize_result(manifest))),
        "notifications/initialized" | "initialized" => Ok(Some(Value::Null)),
        "ping" => Ok(Some(json!({}))),
        "tools/list" => Ok(Some(tools_list_result(manifest))),
        "tools/call" => Ok(Some(
            tools_call_result(params, tools, http_executor, sql_executor).await?,
        )),
        _ => Ok(None),
    }
}

fn initialize_result(manifest: &Manifest) -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": format!("{SERVER_NAME}:{}", manifest.name),
            "version": env!("CARGO_PKG_VERSION"),
        }
    })
}

fn tools_list_result(manifest: &Manifest) -> Value {
    let tools: Vec<Value> = manifest
        .tools
        .iter()
        .map(|t| {
            let mut obj = serde_json::Map::new();
            obj.insert("name".into(), Value::String(t.name.clone()));
            if let Some(d) = &t.description {
                obj.insert("description".into(), Value::String(d.clone()));
            }
            obj.insert("inputSchema".into(), t.input_schema.clone());
            if let Some(out) = &t.output_schema {
                obj.insert("outputSchema".into(), out.clone());
            }
            if !t.annotations.is_empty() {
                if let Ok(ann) = serde_json::to_value(&t.annotations) {
                    obj.insert("annotations".into(), ann);
                }
            }
            Value::Object(obj)
        })
        .collect();
    json!({ "tools": tools })
}

async fn tools_call_result(
    params: Value,
    tools: &HashMap<String, Tool>,
    http_executor: Option<&HttpExecutor>,
    sql_executor: &SqlExecutor,
) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| BridgeError::McpRuntime("tools/call missing `name`".into()))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));

    let tool = tools
        .get(name)
        .ok_or_else(|| BridgeError::McpRuntime(format!("unknown tool '{name}'")))?;

    // Input validation before hitting the network or database. Validation
    // failures surface as a *tool-level* error (isError: true) rather than a
    // JSON-RPC error, so the model can react to them the same way it reacts
    // to any bad call.
    if let Err(e) = schema::validate_input(&tool.name, &tool.input_schema, &arguments) {
        return Ok(tool_error_content(&e.to_string()));
    }

    let structured = match &tool.execute {
        Execute::Http(http) => {
            let exec = http_executor.ok_or_else(|| {
                BridgeError::McpRuntime("HTTP executor unavailable for this manifest".into())
            })?;
            match exec.call(http, &arguments).await {
                Ok(v) => v,
                Err(e) => return Ok(tool_error_content(&e.to_string())),
            }
        }
        Execute::SqlSelect(plan) => match sql_executor.call(plan, &arguments).await {
            Ok(v) => v,
            Err(e) => return Ok(tool_error_content(&e.to_string())),
        },
    };

    let text = serde_json::to_string_pretty(&structured).unwrap_or_else(|_| structured.to_string());

    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured,
        "isError": false,
    }))
}

fn tool_error_content(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}
