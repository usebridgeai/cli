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

use crate::config::{expand_env_vars, load_config, ProviderConfig};
use crate::error::{BridgeError, Result};
use crate::provider::{create_provider, ReadOptions};

pub async fn execute(path: String, from: String, limit: usize, timeout_secs: u64) -> Result<()> {
    let config = load_config()?;

    let provider_config = config.providers.get(&from).ok_or_else(|| {
        let available = config
            .providers
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        BridgeError::ProviderNotFound(from.clone(), available)
    })?;

    let expanded_uri = expand_env_vars(&provider_config.uri)?;
    let expanded_config = ProviderConfig {
        provider_type: provider_config.provider_type.clone(),
        uri: expanded_uri,
    };

    let mut provider = create_provider(&provider_config.provider_type)?;
    let timeout = tokio::time::Duration::from_secs(timeout_secs);

    tokio::time::timeout(timeout, provider.connect(&expanded_config))
        .await
        .map_err(|_| BridgeError::Timeout(timeout_secs))??;

    let read_options = ReadOptions { limit: Some(limit) };

    let context_value = tokio::time::timeout(timeout, provider.read(&path, read_options))
        .await
        .map_err(|_| BridgeError::Timeout(timeout_secs))??;

    let output = serde_json::to_string_pretty(&context_value)?;
    println!("{output}");
    Ok(())
}
