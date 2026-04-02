// Bridge CLI - One CLI. Any storage. Every agent.
// Copyright (c) 2026 Gabriel Beslic
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

use async_trait::async_trait;
use std::path::PathBuf;

use super::{Provider, ProviderCapabilities, ProviderStatus};
use crate::config::{parse_file_uri, ProviderConfig};
use crate::context::{ContextData, ContextEntry, ContextMetadata, ContextValue, EntryType};
use crate::error::{BridgeError, Result};

pub struct FilesystemProvider {
    root: Option<PathBuf>,
}

impl FilesystemProvider {
    pub fn new() -> Self {
        Self { root: None }
    }

    fn root(&self) -> Result<&PathBuf> {
        self.root
            .as_ref()
            .ok_or_else(|| BridgeError::ProviderError("Not connected".to_string()))
    }

    /// Resolve a path within the root, preventing path traversal.
    fn safe_resolve(&self, path: &str) -> Result<PathBuf> {
        let root = self
            .root()?
            .canonicalize()
            .map_err(|e| BridgeError::ProviderError(format!("Cannot canonicalize root: {e}")))?;
        let target = root.join(path);
        let canonical = target
            .canonicalize()
            .map_err(|_| BridgeError::ProviderError(format!("Path not found: {path}")))?;
        if !canonical.starts_with(&root) {
            return Err(BridgeError::PathTraversal(path.to_string()));
        }
        Ok(canonical)
    }
}

#[async_trait]
impl Provider for FilesystemProvider {
    fn name(&self) -> &str {
        "filesystem"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            read: true,
            list: true,
            write: false,
            delete: false,
            search: false,
        }
    }

    async fn connect(&mut self, config: &ProviderConfig) -> Result<()> {
        let path = parse_file_uri(&config.uri).ok_or_else(|| {
            BridgeError::InvalidUri(format!("Expected file:// URI, got: {}", config.uri))
        })?;

        let abs_path = if path.is_relative() {
            std::env::current_dir()?.join(&path)
        } else {
            path
        };

        if !abs_path.exists() {
            return Err(BridgeError::ProviderError(format!(
                "Directory does not exist: {}",
                abs_path.display()
            )));
        }
        if !abs_path.is_dir() {
            return Err(BridgeError::ProviderError(format!(
                "Path is not a directory: {}",
                abs_path.display()
            )));
        }
        self.root = Some(abs_path);
        Ok(())
    }

    async fn read(&self, path: &str) -> Result<ContextValue> {
        let file_path = self.safe_resolve(path)?;
        let metadata = tokio::fs::metadata(&file_path).await?;
        let size = metadata.len();
        let modified = metadata.modified().ok().map(chrono::DateTime::from);
        let created = metadata.created().ok().map(chrono::DateTime::from);

        let content_type = infer_content_type(path);
        let bytes = tokio::fs::read(&file_path).await?;

        let data = if content_type.as_deref() == Some("application/json") {
            match serde_json::from_slice(&bytes) {
                Ok(val) => ContextData::Json(val),
                Err(_) => ContextData::Text(String::from_utf8_lossy(&bytes).to_string()),
            }
        } else if let Ok(text) = String::from_utf8(bytes.clone()) {
            ContextData::Text(text)
        } else {
            ContextData::Binary(bytes)
        };

        Ok(ContextValue {
            data,
            metadata: ContextMetadata {
                source: "filesystem".to_string(),
                path: path.to_string(),
                content_type,
                size: Some(size),
                created_at: created,
                updated_at: modified,
            },
        })
    }

    async fn list(&self, prefix: Option<&str>) -> Result<Vec<ContextEntry>> {
        let root = self.root()?;
        let dir = match prefix {
            Some(p) => self.safe_resolve(p)?,
            None => root.canonicalize()?,
        };

        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let meta = entry.metadata().await?;
            let name = entry.file_name().to_string_lossy().to_string();
            let entry_path = match prefix {
                Some(p) => format!("{p}/{name}"),
                None => name.clone(),
            };
            entries.push(ContextEntry {
                name,
                path: entry_path,
                entry_type: if meta.is_dir() {
                    EntryType::Directory
                } else {
                    EntryType::File
                },
                size: if meta.is_file() {
                    Some(meta.len())
                } else {
                    None
                },
                updated_at: meta.modified().ok().map(chrono::DateTime::from),
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    async fn health(&self) -> Result<ProviderStatus> {
        let root = self.root()?;
        if root.exists() && root.is_dir() {
            Ok(ProviderStatus {
                connected: true,
                latency_ms: Some(0),
                message: Some(format!("Directory: {}", root.display())),
            })
        } else {
            Ok(ProviderStatus {
                connected: false,
                latency_ms: None,
                message: Some("Root directory does not exist".to_string()),
            })
        }
    }
}

fn infer_content_type(path: &str) -> Option<String> {
    let ext = path.rsplit('.').next()?;
    match ext.to_lowercase().as_str() {
        "json" => Some("application/json".to_string()),
        "md" | "markdown" => Some("text/markdown".to_string()),
        "txt" => Some("text/plain".to_string()),
        "yaml" | "yml" => Some("text/yaml".to_string()),
        "toml" => Some("text/toml".to_string()),
        "csv" => Some("text/csv".to_string()),
        "html" | "htm" => Some("text/html".to_string()),
        "xml" => Some("application/xml".to_string()),
        "pdf" => Some("application/pdf".to_string()),
        "png" => Some("image/png".to_string()),
        "jpg" | "jpeg" => Some("image/jpeg".to_string()),
        _ => Some("application/octet-stream".to_string()),
    }
}
