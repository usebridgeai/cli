// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only
//
// The `bridge.mcp/v1` manifest: the single source of truth that both
// generation and runtime operate on. Designed so Bridge Cloud can
// consume the exact same artifact without format changes.

use crate::error::{BridgeError, Result};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const MANIFEST_KIND: &str = "bridge.mcp/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub kind: String,
    pub name: String,
    pub source: Source,
    pub runtime: Runtime,
    #[serde(default)]
    pub tools: Vec<Tool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    Openapi { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Runtime {
    /// Always `stdio` in MVP. Kept explicit so future transports can be added
    /// without breaking manifest v1.
    pub transport: Transport,

    /// Environment variable holding the API base URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url_env: Option<String>,

    /// Literal base URL, used only when no env var is set (e.g. for examples).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Stdio,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Auth {
    Bearer { token_env: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "ToolAnnotations::is_empty")]
    pub annotations: ToolAnnotations,
    pub input_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    pub execute: Execute,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolAnnotations {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "readOnlyHint"
    )]
    pub read_only_hint: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "destructiveHint"
    )]
    pub destructive_hint: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "idempotentHint"
    )]
    pub idempotent_hint: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "openWorldHint"
    )]
    pub open_world_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "title")]
    pub title: Option<String>,
}

impl ToolAnnotations {
    pub fn is_empty(&self) -> bool {
        self.read_only_hint.is_none()
            && self.destructive_hint.is_none()
            && self.idempotent_hint.is_none()
            && self.open_world_hint.is_none()
            && self.title.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Execute {
    Http(HttpExecute),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HttpExecute {
    pub method: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// Ordered list of parameters, used to map tool input fields to path/query slots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<HttpParam>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HttpParam {
    pub name: String,
    pub location: ParamLocation,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ParamLocation {
    Path,
    Query,
}

impl Manifest {
    pub fn new(name: String, source: Source, runtime: Runtime) -> Self {
        Self {
            kind: MANIFEST_KIND.to_string(),
            name,
            source,
            runtime,
            tools: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.kind != MANIFEST_KIND {
            return Err(BridgeError::Manifest(format!(
                "unsupported manifest kind '{}', expected '{}'",
                self.kind, MANIFEST_KIND
            )));
        }
        if self.name.trim().is_empty() {
            return Err(BridgeError::Manifest("manifest `name` is empty".into()));
        }
        let mut seen: IndexMap<&str, ()> = IndexMap::new();
        for tool in &self.tools {
            if tool.name.trim().is_empty() {
                return Err(BridgeError::Manifest("tool name is empty".into()));
            }
            if seen.insert(tool.name.as_str(), ()).is_some() {
                return Err(BridgeError::Manifest(format!(
                    "duplicate tool name '{}'",
                    tool.name
                )));
            }
            if !tool.input_schema.is_object() {
                return Err(BridgeError::Manifest(format!(
                    "tool '{}' input_schema must be a JSON object",
                    tool.name
                )));
            }
        }
        Ok(())
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            BridgeError::Manifest(format!("cannot read manifest at {}: {}", path.display(), e))
        })?;
        let manifest: Manifest = serde_yaml::from_str(&raw)
            .map_err(|e| BridgeError::Manifest(format!("invalid manifest YAML: {e}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn to_yaml(&self) -> Result<String> {
        // Emit a stable header comment so the file is self-identifying, followed by
        // serde_yaml's deterministic output.
        let body = serde_yaml::to_string(self)?;
        Ok(format!(
            "# Generated by bridge. Do not edit by hand unless you know what you are doing.\n# Manifest kind: {MANIFEST_KIND}\n{body}"
        ))
    }
}
