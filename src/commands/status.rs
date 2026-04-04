// Bridge CLI - One CLI. Any storage. Every agent.
// Copyright (c) 2026 Gabriel Beslic & Tomer Liran
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

use crate::config::{expand_env_vars, load_config, ProviderConfig};
use crate::error::Result;
use crate::provider::{create_provider, ProviderStatus};
use serde_json::json;
use std::collections::BTreeMap;

pub async fn execute(timeout_secs: u64) -> Result<()> {
    let config = load_config()?;
    let mut statuses: BTreeMap<String, serde_json::Value> = BTreeMap::new();

    for (name, provider_config) in &config.providers {
        let status = check_provider(name, provider_config, timeout_secs).await;
        statuses.insert(
            name.clone(),
            serde_json::to_value(&status).unwrap_or(json!({
                "connected": false,
                "message": "Failed to serialize status"
            })),
        );
    }

    let output = json!({
        "providers": statuses
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

async fn check_provider(_name: &str, config: &ProviderConfig, timeout_secs: u64) -> ProviderStatus {
    let expanded_uri = match expand_env_vars(&config.uri) {
        Ok(uri) => uri,
        Err(e) => {
            return ProviderStatus {
                connected: false,
                latency_ms: None,
                message: Some(format!("Config error: {e}")),
            };
        }
    };

    let expanded_config = ProviderConfig {
        provider_type: config.provider_type.clone(),
        uri: expanded_uri,
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

    let timeout = tokio::time::Duration::from_secs(timeout_secs);

    match tokio::time::timeout(timeout, provider.connect(&expanded_config)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return ProviderStatus {
                connected: false,
                latency_ms: None,
                message: Some(format!("Connection failed: {e}")),
            };
        }
        Err(_) => {
            return ProviderStatus {
                connected: false,
                latency_ms: None,
                message: Some(format!("Connection timed out after {timeout_secs}s")),
            };
        }
    }

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
