// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// OpenAPI 3.0 -> canonical operation model. Only the subset the MVP
// needs is materialized here: GET operations with path/query parameters
// and a best-effort JSON success response schema. Unsupported pieces
// are skipped with a diagnostic, never panic.

use crate::error::{BridgeError, Result};
use openapiv3::{
    MediaType, OpenAPI, Operation, Parameter, ParameterSchemaOrContent, ReferenceOr, Response,
    Schema, Server, StatusCode,
};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::Path;
use url::Url;

/// A normalized, source-agnostic description of an API operation.
#[derive(Debug, Clone)]
pub struct CanonicalOp {
    pub operation_id: Option<String>,
    pub method: HttpMethod,
    pub path: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub parameters: Vec<CanonicalParam>,
    /// Best-effort JSON Schema for the success response (already ref-resolved).
    pub response_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
}

impl HttpMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CanonicalParam {
    pub name: String,
    pub location: CanonicalParamLocation,
    pub required: bool,
    pub description: Option<String>,
    pub schema: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanonicalParamLocation {
    Path,
    Query,
}

pub struct ParsedSpec {
    pub operations: Vec<CanonicalOp>,
    pub diagnostics: Vec<String>,
    pub default_base_url: Option<String>,
    pub default_base_url_error: Option<String>,
}

pub fn load_spec(path: &Path) -> Result<OpenAPI> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| BridgeError::OpenApi(format!("cannot read {}: {e}", path.display())))?;
    // Try YAML first (superset of JSON); fall back to JSON if the user handed
    // us a strict JSON spec that happened to include YAML-invalid bytes.
    let spec: OpenAPI = serde_yaml::from_str(&raw).or_else(|yaml_err| {
        serde_json::from_str(&raw).map_err(|json_err| {
            BridgeError::OpenApi(format!(
                "spec is neither valid YAML nor JSON (yaml: {yaml_err}; json: {json_err})"
            ))
        })
    })?;
    Ok(spec)
}

pub fn parse(path: &Path) -> Result<ParsedSpec> {
    let spec = load_spec(path)?;
    let mut operations = Vec::new();
    let mut diagnostics = Vec::new();
    let (default_base_url, default_base_url_error) = derive_default_base_url(&spec);

    for (route, path_item_ref) in &spec.paths.paths {
        let path_item = match path_item_ref {
            ReferenceOr::Item(item) => item,
            ReferenceOr::Reference { .. } => {
                diagnostics.push(format!("skipping $ref path item for '{route}'"));
                continue;
            }
        };

        // MVP: GET only. Other methods must be visible to the user as skipped,
        // not silently ignored.
        if let Some(op) = &path_item.get {
            match build_op(&spec, route, HttpMethod::Get, op, &path_item.parameters) {
                Ok((canonical, op_diagnostics)) => {
                    operations.push(canonical);
                    diagnostics.extend(op_diagnostics);
                }
                Err(e) => diagnostics.push(format!("skipping GET {route}: {e}")),
            }
        }
        for (method, present) in [
            ("POST", path_item.post.is_some()),
            ("PUT", path_item.put.is_some()),
            ("PATCH", path_item.patch.is_some()),
            ("DELETE", path_item.delete.is_some()),
        ] {
            if present {
                diagnostics.push(format!(
                    "skipping {method} {route} (only GET is supported in MVP)"
                ));
            }
        }
    }

    Ok(ParsedSpec {
        operations,
        diagnostics,
        default_base_url,
        default_base_url_error,
    })
}

fn build_op(
    spec: &OpenAPI,
    route: &str,
    method: HttpMethod,
    op: &Operation,
    path_level_params: &[ReferenceOr<Parameter>],
) -> Result<(CanonicalOp, Vec<String>)> {
    // Merge path-level and operation-level parameters. Operation-level wins on
    // (name, in) collisions per OpenAPI 3.0 spec.
    let mut params: Vec<CanonicalParam> = Vec::new();
    let mut seen: std::collections::HashSet<(String, CanonicalParamLocation)> =
        std::collections::HashSet::new();

    // Process operation-level first so they take precedence.
    for p in op.parameters.iter().chain(path_level_params.iter()) {
        let param = resolve_parameter(spec, p)?;
        let Some(canonical) = canonical_param(spec, param)? else {
            continue;
        };
        let key = (canonical.name.clone(), canonical.location);
        if seen.insert(key) {
            params.push(canonical);
        }
    }

    let mut diagnostics = Vec::new();
    let response_schema = match pick_success_response_schema(spec, op) {
        Ok(schema) => schema,
        Err(err) => {
            diagnostics.push(format!("GET {route}: output schema omitted: {err}"));
            None
        }
    };

    Ok((
        CanonicalOp {
            operation_id: op.operation_id.clone(),
            method,
            path: route.to_string(),
            summary: op.summary.clone(),
            description: op.description.clone(),
            parameters: params,
            response_schema,
        },
        diagnostics,
    ))
}

fn resolve_parameter<'a>(
    spec: &'a OpenAPI,
    p: &'a ReferenceOr<Parameter>,
) -> Result<&'a Parameter> {
    match p {
        ReferenceOr::Item(param) => Ok(param),
        ReferenceOr::Reference { reference } => {
            let name = strip_ref(reference, "#/components/parameters/")?;
            spec.components
                .as_ref()
                .and_then(|c| c.parameters.get(name))
                .and_then(|pr| match pr {
                    ReferenceOr::Item(p) => Some(p),
                    ReferenceOr::Reference { .. } => None,
                })
                .ok_or_else(|| {
                    BridgeError::OpenApi(format!("unresolved parameter $ref '{reference}'"))
                })
        }
    }
}

fn canonical_param(spec: &OpenAPI, param: &Parameter) -> Result<Option<CanonicalParam>> {
    let (name, data, location) = match param {
        Parameter::Path { parameter_data, .. } => (
            parameter_data.name.clone(),
            parameter_data,
            CanonicalParamLocation::Path,
        ),
        Parameter::Query { parameter_data, .. } => (
            parameter_data.name.clone(),
            parameter_data,
            CanonicalParamLocation::Query,
        ),
        // Header and cookie parameters are out of MVP scope: treated as skipped
        // rather than an error so generation still succeeds.
        Parameter::Header { .. } | Parameter::Cookie { .. } => return Ok(None),
    };

    let schema_json = match &data.format {
        ParameterSchemaOrContent::Schema(schema_ref) => schema_ref_to_json(spec, schema_ref)?,
        ParameterSchemaOrContent::Content(_) => serde_json::json!({ "type": "string" }),
    };

    Ok(Some(CanonicalParam {
        name,
        location,
        required: data.required,
        description: data.description.clone(),
        schema: schema_json,
    }))
}

fn pick_success_response_schema(
    spec: &OpenAPI,
    op: &Operation,
) -> Result<Option<serde_json::Value>> {
    // Prefer 200, then 2xx range, then default. First JSON-ish content type wins.
    let responses = &op.responses;

    let mut candidates: Vec<&ReferenceOr<Response>> = Vec::new();
    if let Some(r) = responses.responses.get(&StatusCode::Code(200)) {
        candidates.push(r);
    }
    for (code, resp) in &responses.responses {
        if let StatusCode::Code(n) = code {
            if (200..300).contains(n) && *n != 200 {
                candidates.push(resp);
            }
        }
    }
    for (code, resp) in &responses.responses {
        if matches!(code, StatusCode::Range(2)) {
            candidates.push(resp);
        }
    }
    if let Some(d) = &responses.default {
        candidates.push(d);
    }

    for resp_ref in candidates {
        let resp = match resp_ref {
            ReferenceOr::Item(r) => r,
            ReferenceOr::Reference { reference } => {
                let name = strip_ref(reference, "#/components/responses/")?;
                let Some(r) = spec
                    .components
                    .as_ref()
                    .and_then(|c| c.responses.get(name))
                    .and_then(|r| match r {
                        ReferenceOr::Item(r) => Some(r),
                        _ => None,
                    })
                else {
                    continue;
                };
                r
            }
        };
        if let Some(schema) = pick_json_media_schema(spec, &resp.content)? {
            return Ok(Some(schema));
        }
    }
    Ok(None)
}

fn pick_json_media_schema(
    spec: &OpenAPI,
    content: &indexmap::IndexMap<String, MediaType>,
) -> Result<Option<serde_json::Value>> {
    let order = ["application/json", "application/problem+json"];
    for ct in order {
        if let Some(mt) = content.get(ct) {
            if let Some(schema_ref) = &mt.schema {
                return Ok(Some(schema_ref_to_json(spec, schema_ref)?));
            }
        }
    }
    // Fallback: any JSON-ish media type.
    for (ct, mt) in content {
        if ct.contains("json") {
            if let Some(schema_ref) = &mt.schema {
                return Ok(Some(schema_ref_to_json(spec, schema_ref)?));
            }
        }
    }
    Ok(None)
}

fn schema_ref_to_json(spec: &OpenAPI, schema_ref: &ReferenceOr<Schema>) -> Result<Value> {
    let mut seen = HashSet::new();
    schema_ref_to_json_with_seen(spec, schema_ref, &mut seen)
}

fn schema_ref_to_json_with_seen(
    spec: &OpenAPI,
    schema_ref: &ReferenceOr<Schema>,
    seen: &mut HashSet<String>,
) -> Result<Value> {
    match schema_ref {
        ReferenceOr::Item(s) => inline_local_schema_refs(spec, schema_to_json(s)?, seen),
        ReferenceOr::Reference { reference } => resolve_schema_reference(spec, reference, seen),
    }
}

pub fn schema_to_json(schema: &Schema) -> Result<serde_json::Value> {
    // OpenAPI Schema serializes to a JSON Schema-compatible representation.
    // Good enough for embedding into the MCP manifest for MVP. We explicitly
    // round-trip through serde_json to get a stable `Value` rather than YAML.
    serde_json::to_value(schema)
        .map_err(|e| BridgeError::OpenApi(format!("failed to serialize schema: {e}")))
}

fn strip_ref<'a>(reference: &'a str, prefix: &str) -> Result<&'a str> {
    reference.strip_prefix(prefix).ok_or_else(|| {
        BridgeError::OpenApi(format!(
            "only local refs starting with '{prefix}' are supported; got '{reference}'"
        ))
    })
}

fn derive_default_base_url(spec: &OpenAPI) -> (Option<String>, Option<String>) {
    let Some(server) = spec.servers.first() else {
        return (None, None);
    };

    match expand_server_url(server).and_then(validate_base_url) {
        Ok(base_url) => (Some(base_url), None),
        Err(err) => (None, Some(err.to_string())),
    }
}

fn expand_server_url(server: &Server) -> Result<String> {
    let mut expanded = String::with_capacity(server.url.len());
    let mut chars = server.url.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '{' {
            expanded.push(ch);
            continue;
        }

        let mut name = String::new();
        loop {
            match chars.next() {
                Some('}') => break,
                Some(c) => name.push(c),
                None => {
                    return Err(BridgeError::OpenApi(format!(
                        "OpenAPI server URL '{}' has an unterminated variable placeholder",
                        server.url
                    )));
                }
            }
        }

        if name.is_empty() {
            return Err(BridgeError::OpenApi(format!(
                "OpenAPI server URL '{}' contains an empty variable placeholder",
                server.url
            )));
        }

        let default = server
            .variables
            .as_ref()
            .and_then(|vars| vars.get(&name))
            .map(|var| var.default.as_str())
            .ok_or_else(|| {
                BridgeError::OpenApi(format!(
                    "OpenAPI server URL '{}' references variable '{}' without a default",
                    server.url, name
                ))
            })?;
        expanded.push_str(default);
    }

    Ok(expanded)
}

fn validate_base_url(base_url: String) -> Result<String> {
    let parsed = Url::parse(&base_url).map_err(|err| {
        BridgeError::OpenApi(format!(
            "OpenAPI server URL '{}' is not a valid absolute URL: {}",
            base_url, err
        ))
    })?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(BridgeError::OpenApi(format!(
            "OpenAPI server URL '{}' must use http or https",
            base_url
        )));
    }

    if !parsed.has_host() {
        return Err(BridgeError::OpenApi(format!(
            "OpenAPI server URL '{}' must include a host",
            base_url
        )));
    }

    Ok(base_url)
}

fn resolve_schema_reference(
    spec: &OpenAPI,
    reference: &str,
    seen: &mut HashSet<String>,
) -> Result<Value> {
    if !seen.insert(reference.to_string()) {
        return Err(BridgeError::OpenApi(format!(
            "cyclic schema reference '{reference}'"
        )));
    }

    let resolved = (|| {
        let name = strip_ref(reference, "#/components/schemas/")?;
        let schema_ref = spec
            .components
            .as_ref()
            .and_then(|components| components.schemas.get(name))
            .ok_or_else(|| BridgeError::OpenApi(format!("unresolved schema $ref '{reference}'")))?;
        schema_ref_to_json_with_seen(spec, schema_ref, seen)
    })();

    seen.remove(reference);
    resolved
}

fn inline_local_schema_refs(
    spec: &OpenAPI,
    value: Value,
    seen: &mut HashSet<String>,
) -> Result<Value> {
    match value {
        Value::Array(items) => Ok(Value::Array(
            items
                .into_iter()
                .map(|item| inline_local_schema_refs(spec, item, seen))
                .collect::<Result<Vec<_>>>()?,
        )),
        Value::Object(map) => inline_local_schema_refs_in_object(spec, map, seen),
        other => Ok(other),
    }
}

fn inline_local_schema_refs_in_object(
    spec: &OpenAPI,
    map: Map<String, Value>,
    seen: &mut HashSet<String>,
) -> Result<Value> {
    if let Some(reference) = map.get("$ref").and_then(|value| value.as_str()) {
        let resolved = resolve_schema_reference(spec, reference, seen)?;
        if map.len() == 1 {
            return Ok(resolved);
        }

        let mut merged = match resolved {
            Value::Object(obj) => obj,
            other => {
                return Err(BridgeError::OpenApi(format!(
                    "cannot merge schema ref '{reference}' into non-object schema: {other}"
                )))
            }
        };

        for (key, value) in map {
            if key != "$ref" {
                merged.insert(key, inline_local_schema_refs(spec, value, seen)?);
            }
        }
        return Ok(Value::Object(merged));
    }

    let mut out = Map::with_capacity(map.len());
    for (key, value) in map {
        out.insert(key, inline_local_schema_refs(spec, value, seen)?);
    }
    Ok(Value::Object(out))
}
