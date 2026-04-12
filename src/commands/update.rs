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

use crate::error::{BridgeError, Result};
use crate::update::{detect_install_method, fetch_latest_version, is_newer, write_cache, InstallMethod};
use serde_json::json;

pub async fn execute(check_only: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");

    if check_only {
        // Always do a fresh network check for --check
        let latest = fetch_latest_version()
            .await
            .ok_or_else(|| BridgeError::UpdateFailed(
                "Could not fetch latest version from GitHub. Check your internet connection.".to_string(),
            ))?;

        // Cache the result so the passive check is up to date
        write_cache(&latest);

        let update_available = is_newer(current, &latest);
        let output = json!({
            "current_version": current,
            "latest_version": latest,
            "update_available": update_available
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    // Install path — fetch latest first
    let latest = fetch_latest_version()
        .await
        .ok_or_else(|| BridgeError::UpdateFailed(
            "Could not fetch latest version from GitHub. Check your internet connection.".to_string(),
        ))?;

    if !is_newer(current, &latest) {
        let output = json!({
            "status": "up_to_date",
            "version": current
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    // An update is available — install based on detected method
    match detect_install_method() {
        InstallMethod::Homebrew => {
            let output = json!({
                "status": "manual_required",
                "latest_version": latest,
                "instruction": "brew upgrade usebridgeai/tap/bridge"
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }

        InstallMethod::Script => {
            let status = std::process::Command::new("sh")
                .args(["-c", "curl -fsSL https://bridge.ls/install | sh"])
                .status()?;

            if !status.success() {
                return Err(BridgeError::UpdateFailed(
                    "Installation script failed. Try running manually: curl -fsSL https://bridge.ls/install | sh".to_string(),
                ));
            }

            write_cache(&latest);
            let output = json!({
                "status": "updated",
                "version": latest
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }

        InstallMethod::Windows => {
            let status = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", "irm https://bridge.ls/install | iex"])
                .status()?;

            if !status.success() {
                return Err(BridgeError::UpdateFailed(
                    "Installation script failed. Try running manually: irm https://bridge.ls/install | iex".to_string(),
                ));
            }

            write_cache(&latest);
            let output = json!({
                "status": "updated",
                "version": latest
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
    }

    Ok(())
}
