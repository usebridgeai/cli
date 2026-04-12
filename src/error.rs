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

use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("bridge.yaml not found. Run `bridge init` to create one.")]
    ConfigNotFound,

    #[error("Failed to parse bridge.yaml: {0}")]
    ConfigParse(String),

    #[error("Provider '{0}' not found. Available providers: {1}")]
    ProviderNotFound(String, String),

    #[error("Provider error: {0}")]
    ProviderError(String),

    #[error("Operation '{0}' is not supported by this provider")]
    UnsupportedOperation(String),

    #[error("Environment variable '{0}' is not set (referenced in bridge.yaml)")]
    EnvVarNotSet(String),

    #[error(
        "Invalid URI: {0}. Expected a scheme (e.g., postgres://..., file://..., sqlite://...)"
    )]
    InvalidUri(String),

    #[error("Invalid connect target: {0}")]
    InvalidConnectTarget(String),

    #[error("Provider type is required when target '{0}' is an environment variable name. Pass --type <provider>.")]
    MissingProviderType(String),

    #[error("Invalid provider type '{0}'. Supported provider types: {1}")]
    InvalidProviderType(String, String),

    #[error(
        "Provider type conflict: target implies '{inferred}', but --type specified '{explicit}'"
    )]
    ProviderTypeConflict { explicit: String, inferred: String },

    #[error("Invalid environment variable name '{0}'. Use a bare name like DATABASE_URL.")]
    InvalidEnvVarName(String),

    #[error("Path traversal denied: '{0}' escapes the provider root directory")]
    PathTraversal(String),

    #[error("Operation timed out after {0} seconds")]
    Timeout(u64),

    #[error("Update failed: {0}")]
    UpdateFailed(String),

    #[error("Invalid identifier: '{0}'. Table and column names must match [a-zA-Z_][a-zA-Z0-9_]*")]
    InvalidIdentifier(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),

    #[error(transparent)]
    SerdeJson(#[from] serde_json::Error),

    #[error(transparent)]
    SerdeYaml(#[from] serde_yaml::Error),
}

impl BridgeError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::ConfigNotFound => "config_not_found",
            Self::ConfigParse(_) => "config_parse_error",
            Self::ProviderNotFound(_, _) => "provider_not_found",
            Self::ProviderError(_) => "provider_error",
            Self::UnsupportedOperation(_) => "unsupported_operation",
            Self::EnvVarNotSet(_) => "env_var_not_set",
            Self::InvalidUri(_) => "invalid_uri",
            Self::InvalidConnectTarget(_) => "invalid_connect_target",
            Self::MissingProviderType(_) => "missing_provider_type",
            Self::InvalidProviderType(_, _) => "invalid_provider_type",
            Self::ProviderTypeConflict { .. } => "provider_type_conflict",
            Self::InvalidEnvVarName(_) => "invalid_env_var_name",
            Self::PathTraversal(_) => "path_traversal",
            Self::Timeout(_) => "timeout",
            Self::UpdateFailed(_) => "update_failed",
            Self::InvalidIdentifier(_) => "invalid_identifier",
            Self::Io(_) => "io_error",
            Self::Sqlx(_) => "database_error",
            Self::SerdeJson(_) => "json_error",
            Self::SerdeYaml(_) => "yaml_error",
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "error": {
                "code": self.error_code(),
                "message": self.to_string()
            }
        })
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Io(_) | Self::Sqlx(_) | Self::Timeout(_) => 2,
            _ => 1,
        }
    }
}

pub type Result<T> = std::result::Result<T, BridgeError>;

/// Redact password from a URI string for safe display.
pub fn redact_uri(uri: &str) -> String {
    if let Some(at_pos) = uri.find('@') {
        if let Some(scheme_end) = uri.find("://") {
            let after_scheme = scheme_end + 3;
            if let Some(colon_pos) = uri[after_scheme..at_pos].find(':') {
                let password_start = after_scheme + colon_pos + 1;
                return format!("{}***{}", &uri[..password_start], &uri[at_pos..]);
            }
        }
    }
    uri.to_string()
}
