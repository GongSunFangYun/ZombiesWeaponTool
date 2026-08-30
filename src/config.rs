//! 配置模型：`Config` 结构体、JSON 读写与校验。
//!
//! 字段名即配置文件里的 JSON 键（英文、无单位），由 `serde` 反序列化/序列化，
//! 带 `#[serde(default)]`，因此**缺字段会回落默认值**（旧配置向后兼容）。
//! 校验集中在 [`Config::validate`]：拦截 0 间隔（会让引擎忙等死循环）与非法
//! 执行顺序。加载路径 [`load_from_path`] 会先反序列化再校验，失败返回带原因的
//! `Err(String)`，供上层展示给用户。
//!
//! 注意：字段键名（如 `weapon_slot_1`）是**配置文件的标识符**，非 UI 文案，
//! 不参与国际化；只有校验/序列化的错误提示文案走 [`crate::lang`] 翻译。

use crate::lang::tr;
use crate::tfmt;
use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};

/// 执行方式：热键触发引擎运行/停止的模式。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExecutionMode {
    /// 长按：按住执行热键时运行，松开即停止。
    #[serde(rename = "hold")]
    Hold,
    /// 切换：点击执行热键开关运行状态。
    #[serde(rename = "toggle")]
    Toggle,
}

impl Default for ExecutionMode {
    /// 默认「切换」，与历史行为一致（旧配置文件缺该字段时自动回退到此值）。
    fn default() -> Self {
        ExecutionMode::Toggle
    }
}

/// 配置结构体，字段名即 JSON 中的键（英文，无单位）。
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    #[serde(rename = "weapon_slot_1")]
    pub weapon_slot1: String,
    #[serde(rename = "weapon_slot_2")]
    pub weapon_slot2: String,
    #[serde(rename = "weapon_slot_3")]
    pub weapon_slot3: String,
    #[serde(rename = "execution_hotkey")]
    pub execution_hotkey: String,
    #[serde(rename = "router_hotkey")]
    pub router_hotkey: String,
    #[serde(rename = "execution_order")]
    pub execution_order: String,
    #[serde(rename = "execution_mode")]
    pub execution_mode: ExecutionMode,
    #[serde(rename = "random_execution")]
    pub random_execution: bool,
    #[serde(rename = "switch_interval")]
    pub weapon_switch_interval_ms: u64,
    #[serde(rename = "switch_interval_offset")]
    pub weapon_switch_interval_offset_ms: u64,
    #[serde(rename = "shoot_interval")]
    pub shoot_interval_ms: u64,
    #[serde(rename = "shoot_interval_offset")]
    pub shoot_interval_offset_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            weapon_slot1: "2".to_string(),
            weapon_slot2: "3".to_string(),
            weapon_slot3: "4".to_string(),
            execution_hotkey: "LALT".to_string(),
            router_hotkey: "RALT".to_string(),
            execution_order: "ABC".to_string(),
            execution_mode: ExecutionMode::Toggle,
            random_execution: false,
            weapon_switch_interval_ms: 100,
            weapon_switch_interval_offset_ms: 20,
            shoot_interval_ms: 50,
            shoot_interval_offset_ms: 10,
        }
    }
}

impl Config {
    /// 校验配置合法性。
    ///
    /// 检查项：
    /// - `shoot_interval_ms` 不能为 0（会在 run_loop 中引发无意义的 200ms 忙等重试循环）；
    /// - `weapon_switch_interval_ms` 不能为 0（同上）；
    /// - `execution_order` 只能由 A/B/C 各最多一次组成，且长度 1~3
    ///   （非法字符或重复项在 `build_slots` 中会被静默忽略，但在加载时给出明确错误更友好）。
    ///
    /// 不对绑定字段做严格校验，因为空绑定是合法的（表示该槽未启用）。
    pub fn validate(&self) -> Result<(), String> {
        if self.shoot_interval_ms == 0 {
            return Err(tr(
                "shoot_interval 不能为 0，请设置一个正整数（单位 ms）",
                "shoot_interval cannot be 0; set a positive integer (ms)",
            )
            .into());
        }
        if self.weapon_switch_interval_ms == 0 {
            return Err(tr(
                "switch_interval 不能为 0，请设置一个正整数（单位 ms）",
                "switch_interval cannot be 0; set a positive integer (ms)",
            )
            .into());
        }
        // execution_order：长度 1~3，每个字符只能是 A/B/C，且不重复
        if self.execution_order.is_empty() || self.execution_order.len() > 3 {
            return Err(tfmt!(
                "execution_order 长度须在 1~3 之间，当前: {}",
                "execution_order length must be 1~3, current: {}",
                self.execution_order
            ));
        }
        let mut seen = [false; 3];
        for b in self.execution_order.bytes() {
            let idx = match b {
                b'A' => 0usize,
                b'B' => 1,
                b'C' => 2,
                other => {
                    return Err(tfmt!(
                        "execution_order 含非法字符 {}，只允许 A/B/C",
                        "execution_order contains invalid char {}; only A/B/C allowed",
                        other as char
                    ))
                }
            };
            if seen[idx] {
                return Err(tfmt!(
                    "execution_order 含重复字符 {}",
                    "execution_order contains duplicate char {}",
                    b as char
                ));
            }
            seen[idx] = true;
        }
        Ok(())
    }
}

pub fn save_to_path(cfg: &Config, path: &Path) -> io::Result<()> {
    let json = serde_json::to_string_pretty(cfg)
        .map_err(|e| io::Error::other(tfmt!("序列化失败: {}", "Serialization failed: {}", e)))?;
    fs::write(path, json)
}

/// 从路径加载配置，反序列化后立即执行 `validate()`。
///
/// # 校验修复
///
/// 原实现仅做 JSON 语法解析，不校验字段值的合法性：
/// - `shoot_interval_ms = 0` 会导致引擎线程进入 200ms 忙等重试死循环；
/// - `switch_interval_ms = 0` 同上；
/// - `execution_order` 含非法字符或重复项会被 `build_slots` 静默忽略，
///   但用户无法得到明确反馈。
///
/// 现在加载失败时返回包含具体原因的 `Err(String)`，调用方可展示给用户。
pub fn load_from_path(path: &Path) -> Result<Config, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let cfg: Config = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    cfg.validate()?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        assert!(Config::default().validate().is_ok());
    }

    /// execution_mode 序列化/反序列化往返一致，且缺字段时回退到 toggle。
    #[test]
    fn execution_mode_round_trips_and_defaults_to_toggle() {
        let json = serde_json::to_string(&Config::default()).unwrap();
        assert!(json.contains("\"execution_mode\":\"toggle\""), "got: {json}");
        assert!(json.contains("\"router_hotkey\":\"RALT\""), "got: {json}");

        // 显式 hold 可读回
        let c: Config = serde_json::from_str(
            "{\"execution_order\":\"AB\",\"execution_mode\":\"hold\"}",
        )
        .unwrap();
        assert_eq!(c.execution_mode, ExecutionMode::Hold);

        // 缺字段 → 回退 toggle（旧配置文件兼容）
        let c: Config = serde_json::from_str("{\"execution_order\":\"ABC\"}").unwrap();
        assert_eq!(c.execution_mode, ExecutionMode::Toggle);
        assert_eq!(c.execution_mode, ExecutionMode::default());
    }

    #[test]
    fn validate_rejects_zero_shoot_interval() {
        let mut c = Config::default();
        c.shoot_interval_ms = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_zero_switch_interval() {
        let mut c = Config::default();
        c.weapon_switch_interval_ms = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_empty_order() {
        let mut c = Config::default();
        c.execution_order = String::new();
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_order_too_long() {
        let mut c = Config::default();
        c.execution_order = "ABCA".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_invalid_order_char() {
        let mut c = Config::default();
        c.execution_order = "AXB".into();
        let err = c.validate().unwrap_err();
        assert!(err.contains('X'));
    }

    #[test]
    fn validate_rejects_duplicate_order_char() {
        let mut c = Config::default();
        c.execution_order = "AAB".into();
        let err = c.validate().unwrap_err();
        assert!(err.contains('A'));
    }

    #[test]
    fn validate_accepts_all_valid_orders() {
        for order in &["A", "B", "C", "AB", "AC", "BC", "ABC", "BAC", "CBA"] {
            let mut c = Config::default();
            c.execution_order = order.to_string();
            assert!(c.validate().is_ok(), "order {:?} should be valid", order);
        }
    }
}