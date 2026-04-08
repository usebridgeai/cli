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

use crate::config::{load_config, save_config};
use crate::error::{BridgeError, Result};
use serde_json::json;

pub async fn execute(name: String) -> Result<()> {
    let mut config = load_config()?;

    if config.providers.remove(&name).is_none() {
        let available = config
            .providers
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        return Err(BridgeError::ProviderNotFound(name, available));
    }

    save_config(&config)?;

    let output = json!({
        "status": "removed",
        "name": name
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
