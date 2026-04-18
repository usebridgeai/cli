// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only

use crate::error::Result;
use crate::mcp::manifest::Manifest;
use crate::mcp::runtime;
use std::path::PathBuf;

pub async fn execute_serve(manifest_path: String, timeout_secs: u64) -> Result<()> {
    let path = PathBuf::from(manifest_path);
    let manifest = Manifest::load_from_path(&path)?;
    runtime::serve(manifest, timeout_secs).await
}
