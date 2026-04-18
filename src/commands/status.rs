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

use crate::config::load_config;
use crate::error::Result;
use crate::provider::probe_provider;
use serde_json::json;
use std::collections::BTreeMap;

pub async fn execute(timeout_secs: u64) -> Result<()> {
    let config = load_config()?;
    let mut statuses: BTreeMap<String, serde_json::Value> = BTreeMap::new();

    for (name, provider_config) in &config.providers {
        let status = probe_provider(provider_config, timeout_secs).await;
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
