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

use crate::config::{
    infer_provider_type, is_valid_env_var_name, load_config, save_config, ProviderConfig,
};
use crate::error::{BridgeError, Result};
use crate::provider::{is_supported_provider_type, supported_provider_types};
use serde_json::json;

pub async fn execute(target: String, provider_type: Option<String>, name: String) -> Result<()> {
    let mut config = load_config()?;
    let (provider_type, uri) = resolve_provider_target(&target, provider_type)?;

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

fn resolve_provider_target(
    target: &str,
    explicit_provider_type: Option<String>,
) -> Result<(String, String)> {
    if target.contains("://") {
        let inferred = infer_provider_type(target)?;

        if let Some(explicit) = explicit_provider_type {
            validate_provider_type(&explicit)?;
            if explicit != inferred {
                return Err(BridgeError::ProviderTypeConflict { explicit, inferred });
            }
            return Ok((explicit, target.to_string()));
        }

        return Ok((inferred, target.to_string()));
    }

    if target.contains("${") || target.contains('}') {
        return Err(BridgeError::InvalidConnectTarget(format!(
            "{target}. Pass the bare environment variable name instead, for example: bridge connect DATABASE_URL --type postgres --as db"
        )));
    }

    if is_valid_env_var_name(target) {
        let explicit = explicit_provider_type
            .ok_or_else(|| BridgeError::MissingProviderType(target.to_string()))?;
        validate_provider_type(&explicit)?;
        return Ok((explicit, format!("${{{target}}}")));
    }

    if looks_like_env_var_name(target) {
        return Err(BridgeError::InvalidEnvVarName(target.to_string()));
    }

    Err(BridgeError::InvalidConnectTarget(format!(
        "{target}. Pass a literal URI like postgres://localhost:5432/mydb or a bare environment variable name like DATABASE_URL."
    )))
}

fn validate_provider_type(provider_type: &str) -> Result<()> {
    if is_supported_provider_type(provider_type) {
        Ok(())
    } else {
        Err(BridgeError::InvalidProviderType(
            provider_type.to_string(),
            supported_provider_types(),
        ))
    }
}

fn looks_like_env_var_name(input: &str) -> bool {
    !input.is_empty()
        && input
            .chars()
            .all(|ch| ch == '_' || ch == '-' || ch.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_literal_uri_without_explicit_type() {
        let (provider_type, uri) =
            resolve_provider_target("postgres://localhost:5432/db", None).unwrap();
        assert_eq!(provider_type, "postgres");
        assert_eq!(uri, "postgres://localhost:5432/db");
    }

    #[test]
    fn test_resolve_literal_uri_with_matching_explicit_type() {
        let (provider_type, uri) =
            resolve_provider_target("file://./docs", Some("filesystem".to_string())).unwrap();
        assert_eq!(provider_type, "filesystem");
        assert_eq!(uri, "file://./docs");
    }

    #[test]
    fn test_resolve_env_var_target() {
        let (provider_type, uri) =
            resolve_provider_target("DATABASE_URL", Some("postgres".to_string())).unwrap();
        assert_eq!(provider_type, "postgres");
        assert_eq!(uri, "${DATABASE_URL}");
    }

    #[test]
    fn test_resolve_env_var_target_requires_type() {
        let error = resolve_provider_target("DATABASE_URL", None).unwrap_err();
        assert!(matches!(error, BridgeError::MissingProviderType(_)));
    }

    #[test]
    fn test_resolve_placeholder_target_is_invalid() {
        let error =
            resolve_provider_target("${DATABASE_URL}", Some("postgres".to_string())).unwrap_err();
        assert!(matches!(error, BridgeError::InvalidConnectTarget(_)));
    }
}
