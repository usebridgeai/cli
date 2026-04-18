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
    expand_env_vars, infer_provider_type, is_valid_env_var_name, load_config, save_config,
    ProviderConfig,
};
use crate::error::{redact_uri, BridgeError, Result};
use crate::provider::{is_supported_provider_type, probe_provider, supported_provider_types};
use serde_json::json;

/// The three outcomes of pre-save verification. Determines the final
/// `status` field in the JSON output and whether we save at all.
enum VerificationOutcome {
    /// Connection + health succeeded.
    Verified { latency_ms: Option<u64> },
    /// The URI references `${VAR}` and `VAR` is not set in this shell.
    /// We can't probe, but this is a legitimate case (config for another
    /// environment), so we save and warn.
    UnresolvedEnvVar(String),
    /// User passed `--no-verify`.
    Skipped,
}

pub async fn execute(
    target: String,
    provider_type: Option<String>,
    name: String,
    force: bool,
    no_verify: bool,
    timeout_secs: u64,
) -> Result<()> {
    let mut config = load_config()?;
    let (provider_type, uri) = resolve_provider_target(&target, provider_type)?;

    // Collision guard: refuse to silently overwrite an existing entry.
    if let Some(existing) = config.providers.get(&name) {
        if !force {
            return Err(BridgeError::ProviderAlreadyExists {
                name: name.clone(),
                existing: redact_uri(&existing.uri),
            });
        }
    }

    let new_entry = ProviderConfig {
        provider_type: provider_type.clone(),
        uri: uri.clone(),
    };

    let verification = if no_verify {
        VerificationOutcome::Skipped
    } else {
        match expand_env_vars(&new_entry.uri) {
            // Env-var target whose variable is unset in this shell. We
            // cannot verify locally, but this is the whole point of
            // storing `${VAR}` — it may be set later or in CI.
            Err(BridgeError::EnvVarNotSet(var)) => VerificationOutcome::UnresolvedEnvVar(var),
            // Any other expansion error is a config bug — surface it.
            Err(e) => return Err(e),
            Ok(_) => {
                let status = probe_provider(&new_entry, timeout_secs).await;
                if status.connected {
                    VerificationOutcome::Verified {
                        latency_ms: status.latency_ms,
                    }
                } else {
                    // Don't save on failed verification.
                    return Err(BridgeError::ConnectionVerificationFailed {
                        name,
                        reason: status
                            .message
                            .unwrap_or_else(|| "unknown error".to_string()),
                    });
                }
            }
        }
    };

    config.providers.insert(name.clone(), new_entry);
    save_config(&config)?;

    // Build the common identity fields once; per-outcome arms only set the
    // fields they're responsible for. Adding a new identity field later means
    // touching one place, not three.
    let mut output = json!({
        "name": name,
        "type": provider_type,
        "uri": uri,
    });
    match verification {
        VerificationOutcome::Verified { latency_ms } => {
            output["status"] = json!("connected");
            output["verified"] = json!(true);
            if let Some(ms) = latency_ms {
                output["latency_ms"] = json!(ms);
            }
        }
        VerificationOutcome::UnresolvedEnvVar(var) => {
            output["status"] = json!("saved_unverified");
            output["verified"] = json!(false);
            output["message"] = json!(format!(
                "Environment variable ${{{var}}} is not set in this shell; skipped verification. Run `bridge status` once it is available to verify the connection."
            ));
        }
        VerificationOutcome::Skipped => {
            output["status"] = json!("saved_unverified");
            output["verified"] = json!(false);
            output["message"] = json!("Verification skipped (--no-verify).");
        }
    }

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
