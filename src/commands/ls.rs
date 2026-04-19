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

use crate::error::{BridgeError, Result};
use crate::provider::{connect_with_timeout, create_provider, load_named_provider_config};
use serde_json::json;

pub async fn execute(from: Option<String>, timeout_secs: u64) -> Result<()> {
    let from = from.ok_or_else(|| {
        BridgeError::ProviderError("Please specify a provider with --from <name>".to_string())
    })?;
    let provider_config = load_named_provider_config(&from, None)?;
    let mut provider = create_provider(&provider_config.provider_type)?;
    let timeout = tokio::time::Duration::from_secs(timeout_secs);

    connect_with_timeout(&mut *provider, &provider_config, timeout_secs).await?;

    let entries = tokio::time::timeout(timeout, provider.list(None))
        .await
        .map_err(|_| BridgeError::Timeout(timeout_secs))??;

    let output = json!(entries);
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
