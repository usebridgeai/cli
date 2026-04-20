// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Transport-agnostic, host-agnostic MCP service core. Holds the manifest and
// dispatches JSON-RPC requests against an injected executor bundle. The core
// makes no decisions about where connection credentials or HTTP base URLs
// come from — per ADR 0001, that belongs to the hosting/wiring layer.
//
// Callers are expected to build an `ExecutorBundle` from whatever trust and
// secret-resolution model they operate under (local env/config for stdio,
// tenant secret scope for a future remote host) and hand it to
// `McpService::new`.

use crate::error::{BridgeError, Result};
use crate::mcp::manifest::{Execute, HttpExecute, Manifest, SqlSelectExecute, Tool};
use crate::mcp::schema;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// MCP protocol version the server advertises. Clients negotiate down if needed.
const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "bridge";

/// Executes an HTTP-backed tool invocation. Implementations are free to source
/// base URL, auth material, and TLS policy from any secret/config backend.
#[async_trait]
pub trait HttpExecuting: Send + Sync {
    async fn call(&self, exec: &HttpExecute, input: &Value) -> Result<Value>;
}

/// Executes a SQL-backed tool invocation. Implementations own their own
/// connection registry — the manifest only references connections by name.
#[async_trait]
pub trait SqlExecuting: Send + Sync {
    async fn call(&self, plan: &SqlSelectExecute, input: &Value) -> Result<Value>;
}

/// Bundle of executors a service needs to serve its manifest. Either arm can
/// be absent: a pure-DB manifest has no HTTP executor, a pure-HTTP manifest
/// has no SQL executor. If a tool fires for a missing arm, the service surfaces
/// a tool-level error rather than panicking.
#[derive(Clone, Default)]
pub struct ExecutorBundle {
    pub http: Option<Arc<dyn HttpExecuting>>,
    pub sql: Option<Arc<dyn SqlExecuting>>,
}

impl ExecutorBundle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_http(mut self, http: Arc<dyn HttpExecuting>) -> Self {
        self.http = Some(http);
        self
    }

    pub fn with_sql(mut self, sql: Arc<dyn SqlExecuting>) -> Self {
        self.sql = Some(sql);
        self
    }
}

pub struct McpService {
    manifest: Manifest,
    tools_by_name: HashMap<String, Tool>,
    executors: ExecutorBundle,
}

impl McpService {
    /// Construct a service from a manifest and caller-owned executors. The
    /// manifest is validated up-front; no I/O, environment reads, or filesystem
    /// access happen here.
    pub fn new(manifest: Manifest, executors: ExecutorBundle) -> Result<Self> {
        manifest.validate()?;

        let tools_by_name: HashMap<String, Tool> = manifest
            .tools
            .iter()
            .cloned()
            .map(|t| (t.name.clone(), t))
            .collect();

        Ok(Self {
            manifest,
            tools_by_name,
            executors,
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Process a single JSON-RPC 2.0 request. Returns `None` for notifications
    /// (requests without an `id`) — the caller must not write anything back.
    pub async fn handle_jsonrpc(&self, request: Value) -> Option<Value> {
        let method = request
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("");
        let id = request.get("id").cloned();
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let is_notification = id.is_none();

        let result = self.dispatch(method, params).await;

        if is_notification {
            if let Err(e) = result {
                eprintln!("bridge mcp: notification '{method}' errored: {e}");
            }
            return None;
        }

        let id = id.unwrap_or(Value::Null);
        Some(match result {
            Ok(Some(value)) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": value,
            }),
            Ok(None) => rpc_error(id, -32601, &format!("method not found: {method}")),
            Err(e) => rpc_error(id, -32000, &e.to_string()),
        })
    }

    async fn dispatch(&self, method: &str, params: Value) -> Result<Option<Value>> {
        match method {
            "initialize" => Ok(Some(self.initialize_result())),
            "notifications/initialized" | "initialized" => Ok(Some(Value::Null)),
            "ping" => Ok(Some(json!({}))),
            "tools/list" => Ok(Some(self.tools_list_result())),
            "tools/call" => Ok(Some(self.tools_call_result(params).await?)),
            _ => Ok(None),
        }
    }

    fn initialize_result(&self) -> Value {
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {
                "tools": { "listChanged": false }
            },
            "serverInfo": {
                "name": format!("{SERVER_NAME}:{}", self.manifest.name),
                "version": env!("CARGO_PKG_VERSION"),
            }
        })
    }

    fn tools_list_result(&self) -> Value {
        let tools: Vec<Value> = self
            .manifest
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

    async fn tools_call_result(&self, params: Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| BridgeError::McpRuntime("tools/call missing `name`".into()))?;
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));

        let tool = self
            .tools_by_name
            .get(name)
            .ok_or_else(|| BridgeError::McpRuntime(format!("unknown tool '{name}'")))?;

        // Input validation before hitting the network or database. Validation
        // failures surface as a *tool-level* error (isError: true) rather than
        // a JSON-RPC error, so the model can react to them the same way it
        // reacts to any bad call.
        if let Err(e) = schema::validate_input(&tool.name, &tool.input_schema, &arguments) {
            return Ok(tool_error_content(&e.to_string()));
        }

        let exec_result = match &tool.execute {
            Execute::Http(http) => {
                let exec = self.executors.http.as_ref().ok_or_else(|| {
                    BridgeError::McpRuntime("HTTP executor unavailable for this manifest".into())
                })?;
                exec.call(http, &arguments).await
            }
            Execute::SqlSelect(plan) => {
                let exec = self.executors.sql.as_ref().ok_or_else(|| {
                    BridgeError::McpRuntime("SQL executor unavailable for this manifest".into())
                })?;
                exec.call(plan, &arguments).await
            }
        };
        let structured = match exec_result {
            Ok(v) => v,
            Err(e) => return Ok(tool_error_content(&e.to_string())),
        };

        let text =
            serde_json::to_string_pretty(&structured).unwrap_or_else(|_| structured.to_string());

        Ok(json!({
            "content": [{ "type": "text", "text": text }],
            "structuredContent": structured,
            "isError": false,
        }))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::manifest::{
        HttpParam, ParamLocation, Runtime, Source, ToolAnnotations, Transport,
    };
    use std::sync::Mutex;

    struct MockHttp {
        response: Value,
        last_call: Mutex<Option<(HttpExecute, Value)>>,
    }

    #[async_trait]
    impl HttpExecuting for MockHttp {
        async fn call(&self, exec: &HttpExecute, input: &Value) -> Result<Value> {
            *self.last_call.lock().unwrap() = Some((exec.clone(), input.clone()));
            Ok(self.response.clone())
        }
    }

    struct FailingHttp;

    #[async_trait]
    impl HttpExecuting for FailingHttp {
        async fn call(&self, _exec: &HttpExecute, _input: &Value) -> Result<Value> {
            Err(BridgeError::Http("upstream boom".into()))
        }
    }

    fn sample_manifest() -> Manifest {
        let tool = Tool {
            name: "getPetById".into(),
            description: Some("Fetch a pet".into()),
            annotations: ToolAnnotations {
                read_only_hint: Some(true),
                destructive_hint: Some(false),
                ..Default::default()
            },
            input_schema: json!({
                "type": "object",
                "properties": { "petId": { "type": "string" } },
                "required": ["petId"],
                "additionalProperties": false
            }),
            output_schema: None,
            execute: Execute::Http(HttpExecute {
                method: "GET".into(),
                path: "/pets/{petId}".into(),
                operation_id: Some("getPetById".into()),
                parameters: vec![HttpParam {
                    name: "petId".into(),
                    location: ParamLocation::Path,
                    required: true,
                }],
            }),
        };
        let mut m = Manifest::new(
            "petstore".into(),
            Source::Openapi {
                path: "./spec.yaml".into(),
            },
            Runtime {
                transport: Transport::Stdio,
                base_url_env: None,
                base_url: Some("http://unused.example".into()),
                auth: None,
            },
        );
        m.tools = vec![tool];
        m
    }

    fn service_with_http(http: Arc<dyn HttpExecuting>) -> McpService {
        McpService::new(sample_manifest(), ExecutorBundle::new().with_http(http))
            .expect("service builds")
    }

    #[tokio::test]
    async fn initialize_advertises_protocol_and_server_name() {
        let service = service_with_http(Arc::new(FailingHttp));
        let resp = service
            .handle_jsonrpc(json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}
            }))
            .await
            .expect("response");
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert!(resp["result"]["serverInfo"]["name"]
            .as_str()
            .unwrap()
            .contains("petstore"));
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn tools_list_returns_manifest_tools() {
        let service = service_with_http(Arc::new(FailingHttp));
        let resp = service
            .handle_jsonrpc(json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/list"
            }))
            .await
            .expect("response");
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "getPetById");
        assert!(tools[0]["inputSchema"].is_object());
        assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);
    }

    #[tokio::test]
    async fn tools_call_success_dispatches_to_injected_http_executor() {
        let mock = Arc::new(MockHttp {
            response: json!({ "ok": true, "status": 200, "body": { "name": "Fido" } }),
            last_call: Mutex::new(None),
        });
        let service = service_with_http(mock.clone());

        let resp = service
            .handle_jsonrpc(json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "getPetById",
                    "arguments": { "petId": "42" }
                }
            }))
            .await
            .expect("response");

        assert_eq!(resp["result"]["isError"], false);
        assert_eq!(resp["result"]["structuredContent"]["body"]["name"], "Fido");
        assert_eq!(resp["result"]["structuredContent"]["status"], 200);

        let captured = mock.last_call.lock().unwrap();
        let (exec, input) = captured.as_ref().expect("executor was called");
        assert_eq!(exec.operation_id.as_deref(), Some("getPetById"));
        assert_eq!(input["petId"], "42");
    }

    #[tokio::test]
    async fn tools_call_executor_error_becomes_tool_level_error() {
        let service = service_with_http(Arc::new(FailingHttp));
        let resp = service
            .handle_jsonrpc(json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "getPetById",
                    "arguments": { "petId": "42" }
                }
            }))
            .await
            .expect("response");
        assert_eq!(resp["result"]["isError"], true);
        assert!(resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("boom"));
    }

    #[tokio::test]
    async fn tools_call_missing_required_arg_returns_tool_level_error() {
        let service = service_with_http(Arc::new(FailingHttp));
        let resp = service
            .handle_jsonrpc(json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": { "name": "getPetById", "arguments": {} }
            }))
            .await
            .expect("response");
        assert_eq!(resp["result"]["isError"], true);
    }

    #[tokio::test]
    async fn tools_call_missing_http_executor_surfaces_runtime_error() {
        // Empty bundle — manifest has an HTTP tool but no HTTP executor wired.
        let service =
            McpService::new(sample_manifest(), ExecutorBundle::new()).expect("service builds");
        let resp = service
            .handle_jsonrpc(json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "tools/call",
                "params": { "name": "getPetById", "arguments": { "petId": "1" } }
            }))
            .await
            .expect("response");
        assert!(resp["error"].is_object());
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("HTTP executor unavailable"));
    }

    #[tokio::test]
    async fn tools_call_unknown_tool_returns_rpc_error() {
        let service = service_with_http(Arc::new(FailingHttp));
        let resp = service
            .handle_jsonrpc(json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": { "name": "does_not_exist", "arguments": {} }
            }))
            .await
            .expect("response");
        assert!(resp["error"].is_object());
        assert_eq!(resp["error"]["code"], -32000);
    }

    #[tokio::test]
    async fn notifications_produce_no_response() {
        let service = service_with_http(Arc::new(FailingHttp));
        let resp = service
            .handle_jsonrpc(json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }))
            .await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let service = service_with_http(Arc::new(FailingHttp));
        let resp = service
            .handle_jsonrpc(json!({
                "jsonrpc": "2.0", "id": 8, "method": "does/not/exist"
            }))
            .await
            .expect("response");
        assert_eq!(resp["error"]["code"], -32601);
    }
}
