// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Convert canonical operations into bridge.mcp/v1 tool definitions.
// Naming is deterministic so the same spec regenerates bit-identical
// manifests (modulo serde_yaml ordering, which is stable too).

use crate::error::{BridgeError, Result};
use crate::mcp::manifest::{Execute, HttpExecute, HttpParam, ParamLocation, Tool, ToolAnnotations};
use crate::mcp::openapi::{CanonicalOp, CanonicalParam, CanonicalParamLocation, HttpMethod};
use std::collections::HashSet;

pub fn map_operations(ops: &[CanonicalOp]) -> Result<Vec<Tool>> {
    let mut out = Vec::with_capacity(ops.len());
    let mut used_names: HashSet<String> = HashSet::new();

    for op in ops {
        let base = desired_name(op);
        let name = dedupe_name(base, &mut used_names);
        let description = build_description(op);
        let annotations = annotations_for(op);
        let input_schema = build_input_schema(&op.parameters);
        let output_schema = op.response_schema.clone();
        let execute = Execute::Http(HttpExecute {
            method: op.method.as_str().to_string(),
            path: op.path.clone(),
            operation_id: op.operation_id.clone(),
            parameters: op
                .parameters
                .iter()
                .map(|p| HttpParam {
                    name: p.name.clone(),
                    location: match p.location {
                        CanonicalParamLocation::Path => ParamLocation::Path,
                        CanonicalParamLocation::Query => ParamLocation::Query,
                    },
                    required: p.required,
                })
                .collect(),
        });
        out.push(Tool {
            name,
            description,
            annotations,
            input_schema,
            output_schema,
            execute,
        });
    }

    Ok(out)
}

fn desired_name(op: &CanonicalOp) -> String {
    if let Some(id) = &op.operation_id {
        // MCP tool names must be stable identifiers; an operationId that already
        // looks like one is passed through verbatim so `getPetById` stays
        // `getPetById`, matching user expectations. Only fall back to
        // sanitisation when the operationId contains problematic characters.
        if !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return id.clone();
        }
        let s = sanitize_name(id);
        if !s.is_empty() {
            return s;
        }
    }
    // Deterministic fallback: "<method>_<path-sanitized>".
    let mut s = String::new();
    s.push_str(&op.method.as_str().to_lowercase());
    for seg in op.path.split('/') {
        if seg.is_empty() {
            continue;
        }
        s.push('_');
        // Path parameters in braces become `by_<name>` so two similar routes
        // stay distinguishable.
        if let Some(inner) = seg.strip_prefix('{').and_then(|x| x.strip_suffix('}')) {
            s.push_str("by_");
            s.push_str(&sanitize_name(inner));
        } else {
            s.push_str(&sanitize_name(seg));
        }
    }
    if s.is_empty() {
        "operation".to_string()
    } else {
        s
    }
}

fn sanitize_name(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_underscore = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore && !out.is_empty() {
            out.push('_');
            prev_underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

fn dedupe_name(base: String, used: &mut HashSet<String>) -> String {
    if used.insert(base.clone()) {
        return base;
    }
    for i in 2..u32::MAX {
        let candidate = format!("{base}_{i}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    // Practically unreachable.
    base
}

fn build_description(op: &CanonicalOp) -> Option<String> {
    match (&op.summary, &op.description) {
        (Some(s), Some(d)) if s != d => Some(format!("{s}\n\n{d}")),
        (Some(s), _) => Some(s.clone()),
        (None, Some(d)) => Some(d.clone()),
        (None, None) => Some(format!("{} {}", op.method.as_str(), op.path)),
    }
}

fn annotations_for(op: &CanonicalOp) -> ToolAnnotations {
    match op.method {
        HttpMethod::Get => ToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
            title: None,
        },
    }
}

fn build_input_schema(params: &[CanonicalParam]) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for p in params {
        let mut prop = match &p.schema {
            serde_json::Value::Object(m) => serde_json::Value::Object(m.clone()),
            other => other.clone(),
        };
        if let (Some(desc), Some(obj)) = (p.description.as_ref(), prop.as_object_mut()) {
            obj.entry("description".to_string())
                .or_insert_with(|| serde_json::Value::String(desc.clone()));
        }
        properties.insert(p.name.clone(), prop);
        if p.required {
            required.push(serde_json::Value::String(p.name.clone()));
        }
    }
    let mut schema = serde_json::Map::new();
    schema.insert("type".into(), serde_json::Value::String("object".into()));
    schema.insert("properties".into(), serde_json::Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".into(), serde_json::Value::Array(required));
    }
    schema.insert(
        "additionalProperties".into(),
        serde_json::Value::Bool(false),
    );
    serde_json::Value::Object(schema)
}

#[allow(dead_code)]
pub(crate) fn ensure_unique(tools: &[Tool]) -> Result<()> {
    let mut seen = HashSet::new();
    for t in tools {
        if !seen.insert(&t.name) {
            return Err(BridgeError::Manifest(format!(
                "duplicate tool name '{}' after mapping",
                t.name
            )));
        }
    }
    Ok(())
}
