// Bridge CLI - One CLI. Any storage. Every agent.
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

use crate::config::ProviderConfig;
use crate::context::{ContextEntry, ContextValue};
use crate::error::{BridgeError, Result};
use async_trait::async_trait;
use serde::Serialize;

pub const SUPPORTED_PROVIDER_TYPES: &[&str] = &["filesystem", "postgres"];

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
        _ => Err(BridgeError::ProviderNotFound(
            type_name.to_string(),
            supported_provider_types().to_string(),
        )),
    }
}

pub fn is_supported_provider_type(type_name: &str) -> bool {
    SUPPORTED_PROVIDER_TYPES.contains(&type_name)
}

pub fn supported_provider_types() -> &'static str {
    "filesystem, postgres"
}
