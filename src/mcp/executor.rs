// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Translate a tool invocation into a real HTTP call based on the manifest's
// execution mapping. Auth and base URL are resolved from environment
// variables at call time (not at load time) so secrets are never cached or
// serialized.

use crate::error::{BridgeError, Result};
use crate::mcp::manifest::{Auth, HttpExecute, HttpParam, Manifest, ParamLocation};
use reqwest::{Client, Method};
use serde_json::Value;
use std::time::Duration;

pub struct HttpExecutor {
    client: Client,
    base_url: String,
    auth_header: Option<String>,
}

impl HttpExecutor {
    pub fn from_manifest(manifest: &Manifest, timeout_secs: u64) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| BridgeError::Http(format!("failed to build HTTP client: {e}")))?;

        let base_url = resolve_base_url(manifest)?;
        let auth_header = match &manifest.runtime.auth {
            Some(Auth::Bearer { token_env }) => {
                let tok = std::env::var(token_env)
                    .map_err(|_| BridgeError::EnvVarNotSet(token_env.clone()))?;
                Some(format!("Bearer {tok}"))
            }
            None => None,
        };

        Ok(Self {
            client,
            base_url,
            auth_header,
        })
    }

    pub async fn call(&self, exec: &HttpExecute, input: &Value) -> Result<Value> {
        let method = Method::from_bytes(exec.method.as_bytes())
            .map_err(|_| BridgeError::Http(format!("invalid HTTP method '{}'", exec.method)))?;

        let (path, query) = render_path_and_query(&exec.path, &exec.parameters, input)?;
        let url = join_url(&self.base_url, &path);

        let mut req = self.client.request(method, &url);
        if !query.is_empty() {
            req = req.query(&query);
        }
        if let Some(h) = &self.auth_header {
            req = req.header("authorization", h);
        }
        req = req.header("accept", "application/json");

        let resp = req
            .send()
            .await
            .map_err(|e| BridgeError::Http(format!("request to {url} failed: {e}")))?;

        let status = resp.status();
        let ct = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = resp
            .text()
            .await
            .map_err(|e| BridgeError::Http(format!("reading response body failed: {e}")))?;

        let body_json: Value = if ct.contains("json") && !text.is_empty() {
            serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.clone()))
        } else if text.is_empty() {
            Value::Null
        } else {
            Value::String(text.clone())
        };

        // Uniform envelope — keeps the tool output predictable even when the
        // upstream returns a non-JSON body or a non-2xx status. The MCP client
        // can still tell success from failure via `ok`.
        Ok(serde_json::json!({
            "ok": status.is_success(),
            "status": status.as_u16(),
            "url": url,
            "body": body_json,
        }))
    }
}

fn resolve_base_url(manifest: &Manifest) -> Result<String> {
    if let Some(env_name) = &manifest.runtime.base_url_env {
        return std::env::var(env_name).map_err(|_| BridgeError::EnvVarNotSet(env_name.clone()));
    }
    if let Some(lit) = &manifest.runtime.base_url {
        return Ok(lit.clone());
    }
    Err(BridgeError::Manifest(
        "manifest has neither `runtime.base_url_env` nor `runtime.base_url`".into(),
    ))
}

fn render_path_and_query(
    path_template: &str,
    params: &[HttpParam],
    input: &Value,
) -> Result<(String, Vec<(String, String)>)> {
    let obj = input.as_object();
    let mut path = path_template.to_string();
    let mut query: Vec<(String, String)> = Vec::new();

    for p in params {
        let val = obj.and_then(|o| o.get(&p.name));
        match p.location {
            ParamLocation::Path => {
                let placeholder = format!("{{{}}}", p.name);
                match val {
                    Some(v) => {
                        let rendered = scalar_to_string(v)?;
                        path = path.replace(&placeholder, &percent_encode_path_segment(&rendered));
                    }
                    None => {
                        if p.required {
                            return Err(BridgeError::Http(format!(
                                "missing required path parameter '{}'",
                                p.name
                            )));
                        }
                    }
                }
            }
            ParamLocation::Query => {
                if let Some(v) = val {
                    for item in scalar_to_query_values(v)? {
                        query.push((p.name.clone(), item));
                    }
                } else if p.required {
                    return Err(BridgeError::Http(format!(
                        "missing required query parameter '{}'",
                        p.name
                    )));
                }
            }
        }
    }

    Ok((path, query))
}

fn scalar_to_string(v: &Value) -> Result<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        _ => Err(BridgeError::Http(format!(
            "expected scalar for path parameter, got {v}"
        ))),
    }
}

fn scalar_to_query_values(v: &Value) -> Result<Vec<String>> {
    match v {
        Value::Array(items) => items.iter().map(scalar_to_string).collect(),
        Value::Null => Ok(Vec::new()),
        _ => scalar_to_string(v).map(|s| vec![s]),
    }
}

fn percent_encode_path_segment(input: &str) -> String {
    // Encode everything that isn't an unreserved character per RFC 3986.
    // Small hand-rolled encoder avoids pulling in `percent-encoding`.
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        let is_unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~');
        if is_unreserved {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn join_url(base: &str, path: &str) -> String {
    let base_trimmed = base.trim_end_matches('/');
    if path.starts_with('/') {
        format!("{base_trimmed}{path}")
    } else {
        format!("{base_trimmed}/{path}")
    }
}
