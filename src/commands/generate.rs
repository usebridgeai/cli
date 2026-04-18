// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
// SPDX-License-Identifier: AGPL-3.0-only

use crate::error::{BridgeError, Result};
use crate::mcp::manifest::{Auth, Manifest, Runtime, Source, Transport};
use crate::mcp::{openapi, tool_mapper};
use serde_json::json;
use std::path::PathBuf;

pub async fn execute_mcp(
    from: Vec<String>,
    name: String,
    base_url_env: Option<String>,
    bearer_env: Option<String>,
    out: String,
    force: bool,
) -> Result<()> {
    // Expected form: `--from openapi <path>`. Clap collects both tokens into the
    // same Vec because `num_args = 2`, which keeps the UX in the ticket exactly.
    if from.len() != 2 {
        return Err(BridgeError::ProviderError(
            "expected `--from <kind> <path>` (e.g. `--from openapi ./openapi.yaml`)".into(),
        ));
    }
    let kind = from[0].to_lowercase();
    let source_path = PathBuf::from(&from[1]);

    if kind != "openapi" {
        return Err(BridgeError::UnsupportedOperation(format!(
            "generate source '{kind}' (only `openapi` is supported)"
        )));
    }
    if name.trim().is_empty() {
        return Err(BridgeError::ProviderError(
            "`--name` cannot be empty".into(),
        ));
    }
    let out_path = PathBuf::from(&out);
    if out_path.exists() && !force {
        return Err(BridgeError::ProviderError(format!(
            "{} already exists. Re-run with --force to overwrite.",
            out_path.display()
        )));
    }

    let parsed = openapi::parse(&source_path)?;
    let tools = tool_mapper::map_operations(&parsed.operations)?;
    if tools.is_empty() {
        return Err(BridgeError::OpenApi(
            "no supported operations found in the spec (only GET is supported in MVP)".into(),
        ));
    }

    let source = Source::Openapi {
        path: from[1].clone(),
    };
    let base_url = parsed.default_base_url.clone();
    if base_url_env.is_none() && base_url.is_none() {
        let reason = parsed.default_base_url_error.unwrap_or_else(|| {
            "no base URL available; pass `--base-url-env` or include a concrete OpenAPI `servers` entry"
                .into()
        });
        return Err(BridgeError::OpenApi(reason));
    }
    let runtime = Runtime {
        transport: Transport::Stdio,
        base_url_env: base_url_env.clone(),
        base_url,
        auth: bearer_env.clone().map(|e| Auth::Bearer { token_env: e }),
    };
    let mut manifest = Manifest::new(name.clone(), source, runtime);
    manifest.tools = tools;
    manifest.validate()?;

    let yaml = manifest.to_yaml()?;
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&out_path, yaml)?;

    let snippet = mcp_client_snippet(&name, &out_path);
    let output = json!({
        "status": "generated",
        "manifest": out_path.display().to_string(),
        "name": name,
        "tools": manifest.tools.iter().map(|t| &t.name).collect::<Vec<_>>(),
        "skipped": parsed.diagnostics,
        "client_snippet": snippet,
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn mcp_client_snippet(name: &str, manifest_path: &std::path::Path) -> serde_json::Value {
    // Emit the Claude Desktop / Cursor-compatible shape. Clients vary, but the
    // `mcpServers.<name> = { command, args }` pattern is the broadly accepted
    // lowest-common-denominator config.
    let bridge_cmd = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "bridge".to_string());
    let abs_manifest = std::fs::canonicalize(manifest_path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| manifest_path.display().to_string());
    json!({
        "mcpServers": {
            name: {
                "command": bridge_cmd,
                "args": ["mcp", "serve", abs_manifest],
            }
        }
    })
}
