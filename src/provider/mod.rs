// Bridge CLI - Any storage. Any agent. One CLI
// Copyright (c) 2026 Gabriel Beslic & Tomer Li Ran
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License version 3
// as published by the Free Software Foundation.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

pub mod filesystem;
pub mod postgres;
pub mod sqlite;

use crate::config::{expand_env_vars, load_config, load_config_from, BridgeConfig, ProviderConfig};
use crate::context::{ContextEntry, ContextValue};
use crate::error::{BridgeError, Result};
use async_trait::async_trait;
use serde::Serialize;
use std::path::Path;

pub const SUPPORTED_PROVIDER_TYPES: &[&str] = &["filesystem", "postgres", "sqlite"];

#[derive(Debug, Clone, Serialize)]
pub struct ProviderCapabilities {
    pub read: bool,
    pub list: bool,
    pub write: bool,
    pub delete: bool,
    pub search: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderStatus {
    pub connected: bool,
    pub latency_ms: Option<u64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ReadOptions {
    pub limit: Option<usize>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> ProviderCapabilities;
    async fn connect(&mut self, config: &ProviderConfig) -> Result<()>;
    async fn read(&self, path: &str, options: ReadOptions) -> Result<ContextValue>;
    async fn list(&self, prefix: Option<&str>) -> Result<Vec<ContextEntry>>;
    async fn health(&self) -> Result<ProviderStatus>;

    async fn write(&self, _key: &str, _value: &ContextValue) -> Result<()> {
        Err(BridgeError::UnsupportedOperation("write".to_string()))
    }

    async fn delete(&self, _key: &str) -> Result<()> {
        Err(BridgeError::UnsupportedOperation("delete".to_string()))
    }

    async fn search(&self, _query: &str) -> Result<Vec<ContextEntry>> {
        Err(BridgeError::UnsupportedOperation("search".to_string()))
    }
}

/// Create a provider instance by type name.
pub fn create_provider(type_name: &str) -> Result<Box<dyn Provider>> {
    match type_name {
        "filesystem" => Ok(Box::new(filesystem::FilesystemProvider::new())),
        "postgres" => Ok(Box::new(postgres::PostgresProvider::new())),
        "sqlite" => Ok(Box::new(sqlite::SqliteProvider::new())),
        _ => Err(BridgeError::ProviderNotFound(
            type_name.to_string(),
            supported_provider_types(),
        )),
    }
}

pub fn is_supported_provider_type(type_name: &str) -> bool {
    SUPPORTED_PROVIDER_TYPES.contains(&type_name)
}

pub fn supported_provider_types() -> String {
    SUPPORTED_PROVIDER_TYPES.join(", ")
}

pub fn named_provider_config(config: &BridgeConfig, name: &str) -> Result<ProviderConfig> {
    let provider_config = config.providers.get(name).ok_or_else(|| {
        BridgeError::ProviderNotFound(
            name.to_string(),
            config
                .providers
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        )
    })?;

    let expanded_uri = expand_env_vars(&provider_config.uri)?;
    Ok(ProviderConfig {
        provider_type: provider_config.provider_type.clone(),
        uri: expanded_uri,
    })
}

pub fn load_named_provider_config(name: &str, config_dir: Option<&Path>) -> Result<ProviderConfig> {
    let config = match config_dir {
        Some(dir) => load_config_from(dir)?,
        None => load_config()?,
    };
    named_provider_config(&config, name)
}

pub async fn connect_with_timeout(
    provider: &mut dyn Provider,
    config: &ProviderConfig,
    timeout_secs: u64,
) -> Result<()> {
    let timeout = tokio::time::Duration::from_secs(timeout_secs);
    tokio::time::timeout(timeout, provider.connect(config))
        .await
        .map_err(|_| BridgeError::Timeout(timeout_secs))?
}

/// Expand env vars in the URI, connect, and run a health check with a
/// per-stage timeout. Always returns a `ProviderStatus` — success or
/// structured failure — so callers can render it uniformly.
///
/// Used by both `bridge status` and `bridge connect` (for verification)
/// so the two commands can never disagree about what "healthy" means.
pub async fn probe_provider(config: &ProviderConfig, timeout_secs: u64) -> ProviderStatus {
    let expanded_config = match named_provider_config(
        &BridgeConfig {
            version: "1".into(),
            name: "probe".into(),
            providers: std::iter::once(("probe".to_string(), config.clone())).collect(),
        },
        "probe",
    ) {
        Ok(config) => config,
        Err(e) => {
            return ProviderStatus {
                connected: false,
                latency_ms: None,
                message: Some(format!("Config error: {e}")),
            };
        }
    };

    let mut provider = match create_provider(&config.provider_type) {
        Ok(p) => p,
        Err(e) => {
            return ProviderStatus {
                connected: false,
                latency_ms: None,
                message: Some(format!("Unknown provider type: {e}")),
            };
        }
    };

    match connect_with_timeout(&mut *provider, &expanded_config, timeout_secs).await {
        Ok(()) => {}
        Err(e) => {
            return ProviderStatus {
                connected: false,
                latency_ms: None,
                message: Some(format!("Connection failed: {e}")),
            };
        }
    }

    let timeout = tokio::time::Duration::from_secs(timeout_secs);
    match tokio::time::timeout(timeout, provider.health()).await {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => ProviderStatus {
            connected: false,
            latency_ms: None,
            message: Some(format!("Health check failed: {e}")),
        },
        Err(_) => ProviderStatus {
            connected: false,
            latency_ms: None,
            message: Some(format!("Health check timed out after {timeout_secs}s")),
        },
    }
}
