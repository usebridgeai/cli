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
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

const CONFIG_FILENAME: &str = "bridge.yaml";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    pub version: String,
    pub name: String,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(rename = "type")]
    pub provider_type: String,
    pub uri: String,
}

impl BridgeConfig {
    pub fn default_with_name(name: &str) -> Self {
        Self {
            version: "1".to_string(),
            name: name.to_string(),
            providers: BTreeMap::new(),
        }
    }
}

/// Find the bridge.yaml config file, looking in the current directory.
pub fn config_path() -> PathBuf {
    PathBuf::from(CONFIG_FILENAME)
}

/// Check if bridge.yaml exists in the current directory.
pub fn config_exists() -> bool {
    config_path().exists()
}

/// Load bridge.yaml from the current directory.
pub fn load_config() -> Result<BridgeConfig> {
    let path = config_path();
    if !path.exists() {
        return Err(BridgeError::ConfigNotFound);
    }
    let content = std::fs::read_to_string(&path)?;
    let config: BridgeConfig =
        serde_yaml::from_str(&content).map_err(|e| BridgeError::ConfigParse(e.to_string()))?;
    Ok(config)
}

/// Save bridge.yaml to the current directory.
pub fn save_config(config: &BridgeConfig) -> Result<()> {
    let path = config_path();
    let content =
        serde_yaml::to_string(config).map_err(|e| BridgeError::ConfigParse(e.to_string()))?;
    std::fs::write(&path, content)?;
    Ok(())
}

/// Expand `${ENV_VAR}` references in a string.
/// Returns an error if any referenced variable is not set.
pub fn expand_env_vars(input: &str) -> Result<String> {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut var_name = String::new();
            loop {
                match chars.next() {
                    Some('}') => break,
                    Some(ch) => var_name.push(ch),
                    None => {
                        // Unterminated ${, just include literally
                        result.push_str("${");
                        result.push_str(&var_name);
                        return Ok(result);
                    }
                }
            }
            match std::env::var(&var_name) {
                Ok(val) => result.push_str(&val),
                Err(_) => return Err(BridgeError::EnvVarNotSet(var_name)),
            }
        } else {
            result.push(c);
        }
    }
    Ok(result)
}

/// Infer the provider type from a URI scheme.
pub fn infer_provider_type(uri: &str) -> Result<String> {
    if uri.starts_with("file://") {
        Ok("filesystem".to_string())
    } else if uri.starts_with("postgres://") || uri.starts_with("postgresql://") {
        Ok("postgres".to_string())
    } else if uri.starts_with("sqlite://") {
        Ok("sqlite".to_string())
    } else if uri.contains("://") {
        let scheme = uri.split("://").next().unwrap_or("");
        Err(BridgeError::InvalidUri(format!(
            "unsupported provider scheme: {scheme}://"
        )))
    } else {
        Err(BridgeError::InvalidUri(uri.to_string()))
    }
}

/// Check whether a string is a valid bare environment variable name.
pub fn is_valid_env_var_name(input: &str) -> bool {
    let mut chars = input.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {}
        _ => return false,
    }

    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

/// Extract the file path from a file:// URI.
pub fn parse_file_uri(uri: &str) -> Option<PathBuf> {
    uri.strip_prefix("file://").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_env_vars_with_set_var() {
        std::env::set_var("BRIDGE_TEST_VAR", "hello");
        assert_eq!(expand_env_vars("${BRIDGE_TEST_VAR}").unwrap(), "hello");
        std::env::remove_var("BRIDGE_TEST_VAR");
    }

    #[test]
    fn test_expand_env_vars_unset() {
        let result = expand_env_vars("${BRIDGE_DEFINITELY_NOT_SET_12345}");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), BridgeError::EnvVarNotSet(_)));
    }

    #[test]
    fn test_expand_env_vars_no_vars() {
        assert_eq!(
            expand_env_vars("postgres://localhost:5432/db").unwrap(),
            "postgres://localhost:5432/db"
        );
    }

    #[test]
    fn test_expand_env_vars_multiple() {
        std::env::set_var("BRIDGE_HOST", "localhost");
        std::env::set_var("BRIDGE_PORT", "5432");
        assert_eq!(
            expand_env_vars("postgres://${BRIDGE_HOST}:${BRIDGE_PORT}/db").unwrap(),
            "postgres://localhost:5432/db"
        );
        std::env::remove_var("BRIDGE_HOST");
        std::env::remove_var("BRIDGE_PORT");
    }

    #[test]
    fn test_infer_provider_type_file() {
        assert_eq!(infer_provider_type("file://./data").unwrap(), "filesystem");
    }

    #[test]
    fn test_infer_provider_type_postgres() {
        assert_eq!(
            infer_provider_type("postgres://localhost/db").unwrap(),
            "postgres"
        );
        assert_eq!(
            infer_provider_type("postgresql://localhost/db").unwrap(),
            "postgres"
        );
    }

    #[test]
    fn test_infer_provider_type_sqlite() {
        assert_eq!(infer_provider_type("sqlite://./data.db").unwrap(), "sqlite");
        assert_eq!(
            infer_provider_type("sqlite:///tmp/test.sqlite").unwrap(),
            "sqlite"
        );
    }

    #[test]
    fn test_infer_provider_type_unknown_scheme() {
        let result = infer_provider_type("redis://localhost");
        assert!(result.is_err());
    }

    #[test]
    fn test_infer_provider_type_no_scheme() {
        let result = infer_provider_type("localhost:5432/db");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), BridgeError::InvalidUri(_)));
    }

    #[test]
    fn test_parse_file_uri() {
        assert_eq!(
            parse_file_uri("file://./data"),
            Some(PathBuf::from("./data"))
        );
        assert_eq!(parse_file_uri("postgres://foo"), None);
    }

    #[test]
    fn test_valid_env_var_names() {
        assert!(is_valid_env_var_name("DATABASE_URL"));
        assert!(is_valid_env_var_name("PGHOST"));
        assert!(is_valid_env_var_name("_INTERNAL_DB"));
    }

    #[test]
    fn test_invalid_env_var_names() {
        assert!(!is_valid_env_var_name("db-url"));
        assert!(!is_valid_env_var_name("123DB"));
        assert!(!is_valid_env_var_name("${DATABASE_URL}"));
        assert!(!is_valid_env_var_name("postgres://localhost"));
    }
}
