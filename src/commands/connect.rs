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

use crate::config::{infer_provider_type, load_config, save_config, ProviderConfig};
use crate::error::Result;
use serde_json::json;

pub async fn execute(uri: String, name: String) -> Result<()> {
    let mut config = load_config()?;
    let provider_type = infer_provider_type(&uri)?;

    config.providers.insert(
        name.clone(),
        ProviderConfig {
            provider_type: provider_type.clone(),
            uri: uri.clone(),
        },
    );

    save_config(&config)?;

    let output = json!({
        "status": "connected",
        "name": name,
        "type": provider_type,
        "uri": uri
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
