//! 会话状态持久化：记录上次使用的配置文件、上次载入的路由表与当前路由位置，
//! 存于用户目录点文件 `~/.zwt`（内容为 JSON），供启动时自动恢复。
//!
//! - 缺失 / 损坏 / 不可读 → `load` 返回 None，调用方走全新默认流程，不崩溃；
//! - 相对路径按原样存储：启动时相对路径按当前工作目录解析，工具整体移动后
//!   同目录文件仍可命中（绝对的外部路径失效则走调用方的回退链）。

use crate::lang::Lang;
use crate::tfmt;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// 会话状态：下次启动恢复「上次使用」的依据。
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub struct SessionState {
    /// 上次使用的配置文件路径（按原样存储，相对/绝对均可）
    #[serde(default)]
    pub last_config: Option<String>,
    /// 上次载入的路由 yaml 路径
    #[serde(default)]
    pub last_router: Option<String>,
    /// 当前路由位置（router_files 下标）
    #[serde(default)]
    pub router_index: usize,
    /// 用户上次选择的语言（`zh` / `en`）。缺失 → 由调用方回退到默认英文。
    #[serde(default)]
    pub language: Option<Lang>,
}

/// `~/.zwt` 的完整路径：优先 USERPROFILE，回退 HOME，再回退当前目录。
pub fn state_path() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".zwt")
}

/// 从指定路径读取会话状态（`state_path()` 即默认状态文件）；任何失败返回 None。
pub fn load_from(path: &Path) -> Option<SessionState> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 写入会话状态到指定路径（覆盖写，先确保父目录存在）。
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
