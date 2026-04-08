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

use crate::config::{config_exists, save_config, BridgeConfig};
use crate::error::Result;
use serde_json::json;

pub async fn execute() -> Result<()> {
    if config_exists() {
        let output = json!({
            "status": "already_exists",
            "path": "bridge.yaml"
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    let dir_name = std::env::current_dir()?
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "bridge-project".to_string());

    let config = BridgeConfig::default_with_name(&dir_name);
    save_config(&config)?;

    let output = json!({
        "status": "created",
        "path": "bridge.yaml",
        "name": dir_name
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
