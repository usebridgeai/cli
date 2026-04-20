// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only

use crate::error::{BridgeError, Result};
use crate::mcp::http::OriginPolicy;
use crate::mcp::manifest::Manifest;
use crate::mcp::{http, runtime};
use std::path::PathBuf;

pub async fn execute_serve(manifest_path: String, timeout_secs: u64) -> Result<()> {
    let (manifest, config_dir) = load_manifest(manifest_path)?;
    runtime::serve(manifest, timeout_secs, &config_dir).await
}

pub async fn execute_serve_http(
    manifest_path: String,
    bind: String,
    allow_origin: Vec<String>,
    timeout_secs: u64,
) -> Result<()> {
    let (manifest, config_dir) = load_manifest(manifest_path)?;
    let addr = bind
        .parse()
        .map_err(|e| BridgeError::McpRuntime(format!("invalid --bind '{bind}': {e}")))?;
    let origin_policy = OriginPolicy::new(allow_origin);
    http::serve(manifest, addr, timeout_secs, &config_dir, origin_policy).await
}

/// Resolve the manifest path and the directory Bridge should treat as the
/// config root. Hosted mode uses the manifest's parent directory so bridge.yaml
/// and other config files colocated with the manifest are picked up, matching
/// the local serve path.
fn load_manifest(manifest_path: String) -> Result<(Manifest, PathBuf)> {
    let path = PathBuf::from(manifest_path);
    let manifest = Manifest::load_from_path(&path)?;
    let config_dir = path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    Ok((manifest, config_dir))
}
