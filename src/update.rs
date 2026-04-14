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

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CACHE_TTL_SECS: u64 = 86_400; // 24 hours
const GITHUB_RELEASES_URL: &str = "https://api.github.com/repos/usebridgeai/cli/releases/latest";

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateCache {
    pub checked_at: u64,
    pub latest_version: String,
}

/// How bridge was installed — determines the correct update mechanism.
///
///   Binary path                              → Detected method
///   /opt/homebrew/Cellar/bridge/…            → Homebrew
///   /usr/local/Cellar/bridge/…               → Homebrew
///   /home/linuxbrew/.linuxbrew/Cellar/…      → Homebrew
///   HOMEBREW_PREFIX/…                        → Homebrew
///   ~/.bridge/bin/bridge (macOS/Linux)       → Script
///   %USERPROFILE%\.bridge\bin\bridge.exe     → Windows
///   anything else                            → Unknown (manual update)
#[derive(Debug, PartialEq)]
pub enum InstallMethod {
    Homebrew,
    Script, // curl | sh  (macOS / Linux)
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    Windows, // iwr | iex  (Windows PowerShell)
    Unknown, // cargo install, nix, manual, etc.
}

/// Compare two semver-like strings of the form [v]X.Y.Z[-suffix].
/// Returns true if `latest` is strictly greater than `current`.
///
///   "1.1.0" > "1.0.0" → true
///   "1.0.0" = "1.0.0" → false
///   "0.9.9" < "1.0.0" → false
pub fn is_newer(current: &str, latest: &str) -> bool {
    fn parse(s: &str) -> [u32; 3] {
        let s = s.trim_start_matches('v');
        let mut it = s.splitn(3, '.');
        std::array::from_fn(|_| {
            it.next()
                .and_then(|p| p.split('-').next()) // strip pre-release label
                .and_then(|p| p.parse().ok())
                .unwrap_or(0)
        })
    }
    parse(latest) > parse(current)
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

fn default_cache_path() -> Option<PathBuf> {
    Some(home_dir()?.join(".bridge").join(".update_check"))
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn read_cache() -> Option<UpdateCache> {
    read_cache_from(&default_cache_path()?)
}

pub fn read_cache_from(path: &Path) -> Option<UpdateCache> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn write_cache(latest_version: &str) {
    if let Some(path) = default_cache_path() {
        write_cache_to(latest_version, &path);
    }
}

pub fn write_cache_to(latest_version: &str, path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let cache = UpdateCache {
        checked_at: now_secs(),
        latest_version: latest_version.to_string(),
    };
    if let Ok(json) = serde_json::to_string(&cache) {
        // Atomic write: temp file + rename to avoid partial/corrupt cache
        // from crashes or concurrent bridge invocations.
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// Result of the passive update check.
pub struct UpdateNotice {
    /// Version string to show the user, if an update is available from cache.
    pub version: Option<String>,
    /// Background refresh handle — must be awaited (with timeout) before exit
    /// to ensure the cache is populated for the next invocation.
    pub refresh: Option<tokio::task::JoinHandle<()>>,
}

/// Checks for available updates from the local cache (no blocking network I/O).
///
/// If the cache is stale or missing, spawns a background task to fetch the
/// latest version from GitHub. The caller **must** await `refresh` before the
/// process exits (see `wait_for_refresh`), otherwise the tokio runtime will
/// drop the task and the cache will never populate.
///
/// ```text
/// check_update_notice()
///      │
///      ├─ BRIDGE_NO_UPDATE_CHECK set? → no version, no refresh
///      │
///      ├─ read cache file (sync, < 1 ms)
///      │       ├─ fresh (< 24 h): compare versions → Some(version), no refresh
///      │       └─ stale / missing: spawn background HTTP refresh → no version
///      │                 └─ [background] fetch_latest_version()
///      │                            └─ write_cache() for next invocation
///      │
///      └─ caller prints notice only if version.is_some() AND stderr is_terminal()
/// ```
pub fn check_update_notice() -> UpdateNotice {
    if std::env::var("BRIDGE_NO_UPDATE_CHECK").is_ok() {
        return UpdateNotice {
            version: None,
            refresh: None,
        };
    }

    let current = env!("CARGO_PKG_VERSION");
    let cache = read_cache();

    match cache {
        Some(c) if now_secs().saturating_sub(c.checked_at) < CACHE_TTL_SECS => {
            // Cache is fresh — use cached value, no network call
            let version = if is_newer(current, &c.latest_version) {
                Some(c.latest_version)
            } else {
                None
            };
            UpdateNotice {
                version,
                refresh: None,
            }
        }
        _ => {
            // Cache is stale or missing — refresh in background for next invocation
            let handle = tokio::spawn(async move {
                if let Some(latest) = fetch_latest_version().await {
                    write_cache(&latest);
                }
            });
            UpdateNotice {
                version: None,
                refresh: Some(handle),
            }
        }
    }
}

/// Awaits the background cache refresh (if any) with a short timeout.
/// Call this after the main command has finished and output has been printed,
/// so the user perceives no delay for fast commands.
pub async fn wait_for_refresh(notice: &mut UpdateNotice) {
    if let Some(handle) = notice.refresh.take() {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), handle).await;
    }
}

/// Fetches the latest release tag from GitHub Releases.
/// Returns None on any error (network failure, timeout, unexpected response shape).
pub async fn fetch_latest_version() -> Option<String> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("bridge-cli/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;

    let response: serde_json::Value = client
        .get(GITHUB_RELEASES_URL)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    response["tag_name"]
        .as_str()
        .map(|s| s.trim_start_matches('v').to_string())
}

/// Detects how bridge was installed from the current executable path and OS.
///
/// Resolution order (first match wins):
/// 1. Windows target → Windows
/// 2. Resolve symlinks, then check if the real path lives under a Homebrew prefix
///    (Cellar dir, /homebrew/ component, or HOMEBREW_PREFIX env)
/// 3. Path under ~/.bridge/bin → Script (curl | sh install)
/// 4. Anything else → Unknown (user must update manually)
pub fn detect_install_method() -> InstallMethod {
    detect_install_method_inner(
        std::env::current_exe().ok().as_deref(),
        std::env::var("HOMEBREW_PREFIX").ok().as_deref(),
    )
}

fn detect_install_method_inner(exe: Option<&Path>, homebrew_prefix: Option<&str>) -> InstallMethod {
    #[cfg(target_os = "windows")]
    {
        let _ = (exe, homebrew_prefix);
        return InstallMethod::Windows;
    }

    #[cfg(not(target_os = "windows"))]
    {
        // Resolve symlinks so /opt/homebrew/bin/bridge → /opt/homebrew/Cellar/…/bridge
        let resolved = exe.and_then(|p| std::fs::canonicalize(p).ok());
        let path_str = resolved
            .as_deref()
            .or(exe)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        if is_homebrew_path(&path_str, homebrew_prefix) {
            return InstallMethod::Homebrew;
        }

        // ~/.bridge/bin/bridge is the standard script install location
        if let Some(home) = home_dir() {
            let script_dir = home.join(".bridge").join("bin");
            if let Some(exe_path) = resolved.as_deref().or(exe) {
                if exe_path.starts_with(&script_dir) {
                    return InstallMethod::Script;
                }
            }
        }

        InstallMethod::Unknown
    }
}

#[cfg(not(target_os = "windows"))]
fn is_homebrew_path(path: &str, homebrew_prefix: Option<&str>) -> bool {
    // Standard Homebrew Cellar paths (macOS Intel, Apple Silicon, Linuxbrew)
    if path.contains("/Cellar/") {
        return true;
    }
    // Covers /opt/homebrew/..., /home/linuxbrew/.linuxbrew/...
    if path.contains("/homebrew/") || path.contains("/linuxbrew/") {
        return true;
    }
    // Custom HOMEBREW_PREFIX (e.g. user-specified install location)
    if let Some(prefix) = homebrew_prefix {
        if !prefix.is_empty() && path.starts_with(prefix) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ─── is_newer ────────────────────────────────────────────────────────────

    #[test]
    fn test_is_newer_returns_true_when_update_available() {
        assert!(is_newer("1.0.0", "1.1.0"));
        assert!(is_newer("1.0.0", "2.0.0"));
        assert!(is_newer("1.0.9", "1.1.0"));
    }

    #[test]
    fn test_is_newer_returns_false_when_same_version() {
        assert!(!is_newer("1.0.0", "1.0.0"));
    }

    #[test]
    fn test_is_newer_returns_false_when_already_ahead() {
        // Dev builds or canaries may be ahead of the published release
        assert!(!is_newer("1.1.0", "1.0.0"));
        assert!(!is_newer("2.0.0", "1.9.9"));
    }

    #[test]
    fn test_is_newer_handles_v_prefix() {
        assert!(is_newer("1.0.0", "v1.1.0"));
        assert!(!is_newer("1.0.0", "v1.0.0"));
    }

    #[test]
    fn test_is_newer_handles_pre_release_suffix() {
        assert!(is_newer("1.0.0", "1.1.0-rc1"));
        assert!(!is_newer("1.0.0", "1.0.0-rc1")); // same base version
    }

    #[test]
    fn test_is_newer_handles_malformed_strings_without_panic() {
        assert!(!is_newer("1.0.0", ""));
        assert!(!is_newer("not-a-version", "also-not"));
        assert!(!is_newer("1.0.0", "garbage"));
    }

    // ─── cache ───────────────────────────────────────────────────────────────

    fn temp_cache_path(dir: &TempDir) -> PathBuf {
        dir.path().join(".bridge").join(".update_check")
    }

    #[test]
    fn test_cache_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = temp_cache_path(&dir);

        write_cache_to("1.2.3", &path);

        let cache = read_cache_from(&path).expect("cache should be readable after write");
        assert_eq!(cache.latest_version, "1.2.3");
        assert!(cache.checked_at > 0);
    }

    #[test]
    fn test_cache_is_stale_after_ttl() {
        let dir = TempDir::new().unwrap();
        let path = temp_cache_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        let old = serde_json::json!({
            "checked_at": now_secs() - CACHE_TTL_SECS - 1,
            "latest_version": "2.0.0"
        });
        fs::write(&path, old.to_string()).unwrap();

        let cache = read_cache_from(&path).unwrap();
        let age = now_secs().saturating_sub(cache.checked_at);
        assert!(age >= CACHE_TTL_SECS, "cache should be considered stale");
    }

    #[test]
    fn test_cache_is_fresh_within_ttl() {
        let dir = TempDir::new().unwrap();
        let path = temp_cache_path(&dir);

        write_cache_to("1.5.0", &path);

        let cache = read_cache_from(&path).unwrap();
        let age = now_secs().saturating_sub(cache.checked_at);
        assert!(
            age < CACHE_TTL_SECS,
            "freshly written cache should not be stale"
        );
    }

    #[test]
    fn test_cache_missing_returns_none() {
        let dir = TempDir::new().unwrap();
        let path = temp_cache_path(&dir);
        // Nothing written — should return None gracefully
        assert!(read_cache_from(&path).is_none());
    }

    #[test]
    fn test_write_cache_creates_parent_dir_if_missing() {
        let dir = TempDir::new().unwrap();
        let path = temp_cache_path(&dir);

        assert!(!path.parent().unwrap().exists());
        write_cache_to("1.0.1", &path);
        assert!(path.exists());
    }

    #[test]
    fn test_cache_with_garbage_content_returns_none() {
        let dir = TempDir::new().unwrap();
        let path = temp_cache_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not valid json at all!!!").unwrap();

        assert!(read_cache_from(&path).is_none());
    }

    #[test]
    fn test_write_cache_leaves_no_temp_file() {
        let dir = TempDir::new().unwrap();
        let path = temp_cache_path(&dir);

        write_cache_to("1.0.0", &path);

        let tmp = path.with_extension("tmp");
        assert!(
            !tmp.exists(),
            "temp file should be renamed away after write"
        );
        assert!(path.exists(), "final cache file should exist");
    }

    #[test]
    fn test_write_cache_is_atomic_overwrites_cleanly() {
        let dir = TempDir::new().unwrap();
        let path = temp_cache_path(&dir);

        write_cache_to("1.0.0", &path);
        write_cache_to("2.0.0", &path);

        let cache = read_cache_from(&path).expect("cache should be readable after overwrite");
        assert_eq!(cache.latest_version, "2.0.0");
        // No leftover temp file
        assert!(!path.with_extension("tmp").exists());
    }

    // ─── detect_install_method ───────────────────────────────────────────

    #[cfg(not(target_os = "windows"))]
    mod detect {
        use super::*;

        #[test]
        fn test_homebrew_cellar_macos_apple_silicon() {
            let exe = Path::new("/opt/homebrew/Cellar/bridge/1.0.3/bin/bridge");
            assert_eq!(
                detect_install_method_inner(Some(exe), None),
                InstallMethod::Homebrew
            );
        }

        #[test]
        fn test_homebrew_cellar_macos_intel() {
            let exe = Path::new("/usr/local/Cellar/bridge/1.0.3/bin/bridge");
            assert_eq!(
                detect_install_method_inner(Some(exe), None),
                InstallMethod::Homebrew
            );
        }

        #[test]
        fn test_linuxbrew_cellar() {
            let exe = Path::new("/home/linuxbrew/.linuxbrew/Cellar/bridge/1.0.3/bin/bridge");
            assert_eq!(
                detect_install_method_inner(Some(exe), None),
                InstallMethod::Homebrew
            );
        }

        #[test]
        fn test_homebrew_prefix_env_custom_location() {
            let exe = Path::new("/my/custom/brew/bin/bridge");
            assert_eq!(
                detect_install_method_inner(Some(exe), Some("/my/custom/brew")),
                InstallMethod::Homebrew,
            );
        }

        #[test]
        fn test_homebrew_prefix_env_empty_is_ignored() {
            let exe = Path::new("/some/random/path/bridge");
            assert_ne!(
                detect_install_method_inner(Some(exe), Some("")),
                InstallMethod::Homebrew,
            );
        }

        #[test]
        fn test_script_install_detected() {
            let home = home_dir().unwrap();
            let exe = home.join(".bridge").join("bin").join("bridge");
            assert_eq!(
                detect_install_method_inner(Some(&exe), None),
                InstallMethod::Script
            );
        }

        #[test]
        fn test_cargo_install_returns_unknown() {
            let exe = Path::new("/home/user/.cargo/bin/bridge");
            assert_eq!(
                detect_install_method_inner(Some(exe), None),
                InstallMethod::Unknown
            );
        }

        #[test]
        fn test_nix_install_returns_unknown() {
            let exe = Path::new("/nix/store/abc123-bridge-1.0.3/bin/bridge");
            assert_eq!(
                detect_install_method_inner(Some(exe), None),
                InstallMethod::Unknown
            );
        }

        #[test]
        fn test_no_exe_path_returns_unknown() {
            assert_eq!(
                detect_install_method_inner(None, None),
                InstallMethod::Unknown
            );
        }
    }
}
