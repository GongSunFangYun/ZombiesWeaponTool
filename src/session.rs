//! Session-state persistence.
//!
//! Records the last-used config file, the last-loaded router table, and the current router
//! position into a small JSON dot-file (`~/.zwt`) in the user's home directory, so the app
//! can restore its state on the next launch.
//!
//! Robustness guarantees:
//! - Missing / corrupt / unreadable file → `load` returns `None` and the caller starts
//!   from a fresh default flow instead of crashing.
//! - Relative paths are stored verbatim. On startup they're resolved against the *current*
//!   working directory, so moving the whole tool keeps same-directory files findable
//!   (an absolute external path that no longer exists falls through to the caller's
//!   fallback chain).

use crate::lang::Lang;
use crate::tfmt;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Session state: the "last used" record restored on the next launch.
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub struct SessionState {
    /// Last-used config file path (stored verbatim; relative or absolute).
    #[serde(default)]
    pub last_config: Option<String>,
    /// Last-loaded router YAML path.
    #[serde(default)]
    pub last_router: Option<String>,
    /// Current router position (index into `router_files`).
    #[serde(default)]
    pub router_index: usize,
    /// User's last language choice (`zh` / `en`). When absent, the caller falls back to
    /// the default (English).
    #[serde(default)]
    pub language: Option<Lang>,
}

/// Full path of `~/.zwt`: prefer `USERPROFILE`, fall back to `HOME`, then the cwd.
pub fn state_path() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".zwt")
}

/// Load session state from a path (`state_path()` is the default file).
/// Any failure (I/O, JSON parse) yields `None` rather than an error, so the caller can
/// simply start fresh.
pub fn load_from(path: &Path) -> Option<SessionState> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write session state to a path (overwrites; ensures the parent directory exists first).
pub fn save_to(path: &Path, state: &SessionState) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| io::Error::other(tfmt!("序列化会话状态失败: {}", "Session serialization failed: {}", e)))?;
    fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_persists_all_fields() {
        let dir = std::env::temp_dir().join("zwt_session_test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".zwt");
        let st = SessionState {
            last_config: Some("D:\\game\\configs\\second.json".into()),
            last_router: Some("D:\\game\\zwtcfg_router.yaml".into()),
            router_index: 2,
            language: Some(Lang::En),
        };
        save_to(&path, &st).unwrap();
        assert_eq!(load_from(&path), Some(st));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_returns_none() {
        let path = std::env::temp_dir()
            .join("zwt_missing_test")
            .join(".zwt");
        assert_eq!(load_from(&path), None);
    }

    #[test]
    fn corrupt_file_returns_none() {
        let dir = std::env::temp_dir().join("zwt_corrupt_test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".zwt");
        fs::write(&path, "{ not valid json }").unwrap();
        assert_eq!(load_from(&path), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn defaults_are_empty_and_safe() {
        let st = SessionState::default();
        assert_eq!(st.last_config, None);
        assert_eq!(st.last_router, None);
        assert_eq!(st.router_index, 0);
    }
}
