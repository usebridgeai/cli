// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only

use crate::error::Result;
use crate::mcp::http::{HostedHttpConfig, OriginPolicy};
use crate::mcp::manifest::Manifest;
use crate::mcp::{http, runtime};
use std::path::PathBuf;

pub async fn execute_serve(manifest_path: String, timeout_secs: u64) -> Result<()> {
    let (manifest, config_dir) = load_manifest(manifest_path)?;
    runtime::serve(manifest, timeout_secs, &config_dir).await
}

pub struct ServeHttpArgs {
    pub manifest_path: String,
    pub bind: String,
    pub public_url: Option<String>,
    pub max_header_bytes: usize,
    pub max_body_bytes: usize,
    pub read_timeout_secs: u64,
    pub request_timeout_secs: Option<u64>,
    pub shutdown_grace_secs: u64,
    pub allow_origin: Vec<String>,
    pub timeout_secs: u64,
}

pub async fn execute_serve_http(args: ServeHttpArgs) -> Result<()> {
    let (manifest, config_dir) = load_manifest(args.manifest_path)?;
    let config = HostedHttpConfig::new(
        args.bind,
        args.public_url,
        args.max_header_bytes,
        args.max_body_bytes,
        args.read_timeout_secs,
        args.request_timeout_secs.unwrap_or(args.timeout_secs),
        args.shutdown_grace_secs,
    )?;
    let origin_policy = OriginPolicy::new(args.allow_origin);
    http::serve(
        manifest,
        config,
        args.timeout_secs,
        &config_dir,
        origin_policy,
    )
    .await
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
