//! Config model: the [`Config`] struct plus JSON (de)serialization and validation.
//!
//! Field names are the JSON keys used in the config file (English, no units). They are
//! (de)serialized by `serde`, and the struct carries `#[serde(default)]`, so **missing
//! fields fall back to sensible defaults** — this keeps older config files compatible.
//! Validation is centralized in [`Config::validate`], which rejects two classes of bad
//! input: a zero interval (which would spin the engine into a busy-loop) and an invalid
//! `execution_order`. The [`load_from_path`] entry point deserializes then validates,
//! returning an `Err(String)` with a human-readable reason that callers can surface.
//!
//! Note: the JSON **keys** (e.g. `weapon_slot_1`) are config-file identifiers, not UI text,
//! so they are deliberately **not** localized; only the validation / serialization error
//! messages are translated via [`crate::lang`].

use crate::lang::tr;
use crate::tfmt;
use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};

/// How the execution hotkey toggles the engine.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExecutionMode {
    /// Hold: the engine runs for as long as the hotkey is physically held, stopping on release.
    #[serde(rename = "hold")]
    Hold,
    /// Toggle: each press flips the engine's running state.
    #[serde(rename = "toggle")]
    Toggle,
}

impl Default for ExecutionMode {
    /// Default to `Toggle` — preserves the historical behavior, and is the value an
    /// older config file falls back to when the field is absent.
    fn default() -> Self {
        ExecutionMode::Toggle
    }
}

/// The full configuration. Each field is exposed as-is in JSON (key = field's `#[serde(rename)]`).
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
    /// Validate the config for internal consistency.
    ///
    /// Checks (rejected with a descriptive `Err`):
    /// - `shoot_interval_ms != 0` — a zero value would loop the engine's `run_loop` into a
    ///   meaningless 200 ms busy-retry spin (see `engine.rs`).
    /// - `weapon_switch_interval_ms != 0` — same busy-loop hazard as above.
    /// - `execution_order` is 1–3 chars, each one of `A`/`B`/`C`, with no repeats. Invalid
    ///   chars or duplicates are silently ignored later inside `build_slots`, but reporting
    ///   them at load time is much friendlier than a silent misconfiguration.
    ///
    /// Binding fields are deliberately left unvalidated, because an *empty* binding is legal
    /// (it simply means that weapon slot is not used).
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
        // execution_order: length 1–3, each char one of A/B/C, no duplicates.
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

/// Load a config from a path, deserialize it, then immediately run [`Config::validate`].
///
/// Why validation matters (bug-fix rationale): the original implementation only parsed the
/// JSON and never checked the field values, which let three failure modes slip through:
/// - `shoot_interval_ms = 0` sent the engine thread into a 200 ms busy-retry spin;
/// - `switch_interval_ms = 0` did the same;
/// - an `execution_order` with invalid chars or duplicates was silently dropped inside
///   `build_slots`, so the user got no feedback.
///
/// The current implementation returns an `Err(String)` carrying the specific reason so the
/// caller can show it to the user instead of silently misbehaving.
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

    /// `execution_mode` round-trips through JSON unchanged, and an absent field falls
    /// back to `toggle` (so older config files stay compatible).
    #[test]
    fn execution_mode_round_trips_and_defaults_to_toggle() {
        let json = serde_json::to_string(&Config::default()).unwrap();
        assert!(json.contains("\"execution_mode\":\"toggle\""), "got: {json}");
        assert!(json.contains("\"router_hotkey\":\"RALT\""), "got: {json}");

        // An explicit `hold` value reads back as `Hold`.
        let c: Config = serde_json::from_str(
            "{\"execution_order\":\"AB\",\"execution_mode\":\"hold\"}",
        )
        .unwrap();
        assert_eq!(c.execution_mode, ExecutionMode::Hold);

        // A missing field falls back to `toggle` (legacy config compatibility).
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