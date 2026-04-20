// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// Minimal JSON Schema validation covering what derived-from-OpenAPI tool
// inputs actually need in MVP:
//   - root object type
//   - required keys
//   - basic scalar `type` checks for properties
//   - additionalProperties: false rejection
//
// This is intentionally not a full draft-07/2020 validator — full schema
// validation is a v2 concern. The goal is to catch real mistakes early
// before they hit the network.

use crate::error::{BridgeError, Result};
use serde_json::Value;

pub fn validate_input(tool_name: &str, schema: &Value, input: &Value) -> Result<()> {
    let ctx = ValidationCtx { tool_name };
    ctx.check(schema, input, "")
}

struct ValidationCtx<'a> {
    tool_name: &'a str,
}

impl<'a> ValidationCtx<'a> {
    fn err(&self, msg: String) -> BridgeError {
        BridgeError::ToolInputInvalid {
            tool: self.tool_name.to_string(),
            reason: msg,
        }
    }

    fn check(&self, schema: &Value, value: &Value, path: &str) -> Result<()> {
        let Some(schema) = schema.as_object() else {
            return Ok(());
        };

        if let Some(ty) = schema.get("type").and_then(|t| t.as_str()) {
            if !type_matches(ty, value) {
                return Err(self.err(format!(
                    "expected `{ty}` at `{}`, got `{}`",
                    display_path(path),
                    type_of(value),
                )));
            }
        }

        if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
            if let Some(obj) = value.as_object() {
                for (k, v) in obj {
                    if let Some(sub) = props.get(k) {
                        let child_path = if path.is_empty() {
                            k.clone()
                        } else {
                            format!("{path}.{k}")
                        };
                        self.check(sub, v, &child_path)?;
                    }
                }
            }
        }

        if let Some(req) = schema.get("required").and_then(|r| r.as_array()) {
            if let Some(obj) = value.as_object() {
                for field in req {
                    let Some(name) = field.as_str() else { continue };
                    if !obj.contains_key(name) {
                        return Err(self.err(format!(
                            "missing required field `{name}`{}",
                            if path.is_empty() {
                                String::new()
                            } else {
                                format!(" at `{path}`")
                            }
                        )));
                    }
                }
            }
        }

        if matches!(schema.get("additionalProperties"), Some(Value::Bool(false))) {
            if let Some(obj) = value.as_object() {
                if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
                    for key in obj.keys() {
                        if !props.contains_key(key) {
                            return Err(self.err(format!(
                                "unknown field `{key}`{}",
                                if path.is_empty() {
                                    String::new()
                                } else {
                                    format!(" at `{path}`")
                                }
                            )));
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

fn type_matches(ty: &str, value: &Value) -> bool {
    match ty {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn type_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn display_path(path: &str) -> &str {
    if path.is_empty() {
        "(root)"
    } else {
        path
    }
}
