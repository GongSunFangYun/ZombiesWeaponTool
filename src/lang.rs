//! 国际化 / 本地化：为 UI 提供简体中文与英文两套文案。
//!
//! ## 设计
//!
//! 采用零第三方依赖的轻量方案（沿用本项目「自写轻量组件」的风格，参见
//! `router.rs` 的轻量 YAML 解析、`session.rs` 的轻量状态持久化）：
//!
//! - 全局记录当前语言（`Lang`），**默认英文**；
//! - 对外暴露 `tr(zh, en)`：调用点内联中英文，`format!` 直接填充占位符；
//! - 语言切换经由 [`Lang::toggle`] 翻转，并持久化到会话文件 `.zwt`
//!   （见 [`crate::session`] 的 `SessionState::language`）；
//! - [`init`] 在启动时应用**显式运行时覆盖**（`--lang` 参数 / `ZWT_LANG` 环境变量），
//!   返回 `Some` 表示已有覆盖；否则返回 `None`，交由会话恢复或默认英文。
//!   优先级：`--lang` / `ZWT_LANG` > 会话记录 `.zwt` > 默认英文。
//!
//! ## 语言切换按钮文案（交叉语言）
//!
//! 「操作」行的语言切换项**显示将要切换到的语言**，便于用户识别目标语言：
//! - 当前中文 → 显示英文 `Switch Language`；
//! - 当前英文 → 显示中文 `语言切换`。
//!
//! 由 `tr("Switch Language", "语言切换")` 天然实现 —— `tr` 依据当前语言选择分支，
//! 恰好产生「显示对侧语言」的效果。
//!
//! ## 为什么不翻译配置/键名
//!
//! 配置项（如 `weapon_slot_1`）、绑定键名（如 `LALT`、`MB2`）是**序列化到配置
//! 文件并用于匹配真实输入**的标识符，不属于 UI 文案，因此**不参与翻译**（见
//! `keymap.rs`、`config.rs` 的字段名）。本模块只覆盖展示给用户看的字符串。

use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::sync::atomic::{AtomicU8, Ordering};

/// 支持的语言。派生 serde，以便存进会话文件 `.zwt` 持久化。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    /// 简体中文
    Zh,
    /// English
    En,
}

/// 当前语言（0 = 中文，1 = 英文）。
/// 用 `AtomicU8` 而非全局可变 `static mut`，保证多线程（监听线程 / 引擎线程）
/// 读取时无数据竞争；`Relaxed` 足够，因为语言只在少数控制点写入。
static LANG: AtomicU8 = AtomicU8::new(1);

// 测试作用域的线程本地语言覆盖。
//
// `Lang` 是进程级全局，而 Rust 单元测试默认多线程并行执行，若各测试直接
// 用 `Lang::set` 切换全局语言，会互相干扰（一个测试切到英文，另一个正在渲染
// 中文断言就会失败）。因此测试通过 `Lang::test_set` 设置**本线程**的覆盖值；
// `get`/`set` 在覆盖存在时就地读写，无覆盖时回落全局。这样各测试线程互不作用。
thread_local! {
    static TEST_OVERRIDE: Cell<Option<Lang>> = const { Cell::new(None) };
}

impl Lang {
    /// 读取当前语言。优先使用测试线程的覆盖值（若设置）；否则取全局。
    /// 默认英文（`LANG` 初值 = 1）。
    #[inline]
    pub fn get() -> Self {
        TEST_OVERRIDE.with(|t| match t.get() {
            Some(l) => l,
            None => {
                if LANG.load(Ordering::Relaxed) == 0 {
                    Lang::Zh
                } else {
                    Lang::En
                }
            }
        })
    }

    /// 设置当前语言。若本线程已设测试覆盖，则写入覆盖（隔离测试）；
    /// 否则写入全局（生产运行）。
    #[inline]
    pub fn set(self) {
        let v = if self == Lang::Zh { 0 } else { 1 };
        TEST_OVERRIDE.with(|t| {
            if t.get().is_some() {
                t.set(Some(self));
            } else {
                LANG.store(v, Ordering::Relaxed);
            }
        });
    }

    /// 测试专用：为当前线程设置语言覆盖；`Lang::get`/`set` 都改走该线程本地值。
    #[cfg(test)]
    #[inline]
    pub fn test_set(self) {
        TEST_OVERRIDE.with(|t| t.set(Some(self)));
    }

    /// 测试专用：清除当前线程的语言覆盖，使 `get` 回落全局（即默认英文）。
    #[cfg(test)]
    #[inline]
    pub fn test_clear() {
        TEST_OVERRIDE.with(|t| t.set(None));
    }

    /// 当前语言的「对侧」语言（即切换按钮指向的目标语言）。
    #[inline]
    pub fn other(self) -> Self {
        match self {
            Lang::Zh => Lang::En,
            Lang::En => Lang::Zh,
        }
    }

    /// 翻转当前语言并设置，返回切换后的新语言。
    #[inline]
    pub fn toggle() -> Self {
        let new = Lang::get().other();
        new.set();
        new
    }
}

/// 按当前语言返回 `(zh, en)` 对应的文案模板，返回 `&'static str`。
///
/// 模板通常交给 [`fmt`] / [`tfmt!`] 填充 `{}` 占位符；**两种语言的占位符数量与
/// 顺序必须一致**，否则运行时可能输出错位。调用点内联中英文，便于对照与维护。
///
/// 语言切换按钮文案即用 `tr("Switch Language", "语言切换")`：当前中文 → 返回
/// 「英文」文本，当前英文 → 返回「中文」文本，实现「显示目标语言」的交叉效果。
#[inline]
pub fn tr(zh: &'static str, en: &'static str) -> &'static str {
    match Lang::get() {
        Lang::Zh => zh,
        Lang::En => en,
    }
}

/// 依据当前语言翻译并填充占位符，返回 `String`。
///
/// 之所以用这套自带的占位符替换而**不用** `format!(tr(...), …)`：在 edition 2024
/// 下 `format!`/`format_args!` 要求格式串必须是**字符串字面量**，不接受运行时字符串，
/// 而 `tr` 返回的 `&'static str` 是运行期才确定的当前语言。因此这里实现一个
/// 仅支持位置占位符 `{}` 的轻量格式化器（本项目文案只用 `{}`，无命名捕获/`{:?}`）。
///
/// `args` 为预先 `.to_string()` 后的参数列表，按 `{}` 出现的顺序依次替换；
/// 参数多于占位符时不报错（忽略多余），少于占位符时保留 `{}`。
pub fn fmt(zh: &'static str, en: &'static str, args: &[String]) -> String {
    let s = tr(zh, en);
    if args.is_empty() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + args.iter().map(String::len).sum::<usize>() + 8);
    let mut rest = s;
    let mut idx = 0;
    while let Some(pos) = rest.find("{}") {
        out.push_str(&rest[..pos]);
        if let Some(a) = args.get(idx) {
            out.push_str(a);
        }
        idx += 1;
        rest = &rest[pos + 2..];
    }
    out.push_str(rest);
    out
}

/// 翻译 + 格式化 的便捷宏：`tfmt!(zh, en, arg1, arg2, …)`。
///
/// 展开为 [`lang::fmt`] 调用，把每个参数 `.to_string()` 后按序填充 `{}`。
/// 供各模块在展示带参数的文案时使用（见 [`fmt`] 关于为何不用 `format!` 的说明）。
#[macro_export]
macro_rules! tfmt {
    ($zh:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        $crate::lang::fmt($zh, $en, &[$($arg.to_string()),*])
    };
}

/// 启动时应用**显式运行时覆盖**：`ZWT_LANG` 环境变量 → `--lang` 参数。
///
/// 返回 `Some(lang)` 表示已应用覆盖（此后会话恢复不再改动语言）；否则返回
/// `None`，此时语言交给「会话记录 `.zwt`」或「默认英文」决定。幂等：重复
/// 调用无副作用。
pub fn init() -> Option<Lang> {
    // 1. 环境变量 ZWT_LANG
    if let Ok(v) = std::env::var("ZWT_LANG") {
        if let Some(l) = parse_lang(&v) {
            l.set();
            return Some(l);
        }
    }
    // 2. 命令行参数 --lang
    if let Some(l) = parse_cli_lang() {
        l.set();
        return Some(l);
    }
    None
}

/// 解析宽松的语言字符串：接受 `zh` / `zh-cn` / `zh_cn` / `chinese` 与
/// `en` / `en-us` / `en_us` / `english`（大小写不敏感）。
fn parse_lang(v: &str) -> Option<Lang> {
    match v.trim().to_ascii_lowercase().as_str() {
        "zh" | "zh-cn" | "zh_cn" | "zh-sg" | "chinese" | "中文" => Some(Lang::Zh),
        "en" | "en-us" | "en_us" | "en-gb" | "english" | "英文" => Some(Lang::En),
        _ => None,
    }
}

/// 从命令行参数里找 `--lang <zh|en>`（也支持 `--lang=zh`、`--lang:zh`）。
/// 找不到明确的语言则返回 None，交给会话记录/默认语言。
fn parse_cli_lang() -> Option<Lang> {
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == "--lang" || a == "-lang" {
            if let Some(v) = it.next() {
                if let Some(l) = parse_lang(&v) {
                    return Some(l);
                }
            }
        } else if let Some(v) = a.strip_prefix("--lang=") {
            return parse_lang(v);
        } else if let Some(v) = a.strip_prefix("--lang:") {
            return parse_lang(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认语言为英文（未调用 init / 无会话时）。清除线程覆盖以读取全局默认。
    #[test]
    fn defaults_to_english() {
        Lang::test_clear();
        assert_eq!(Lang::get(), Lang::En);
    }

    /// 设置后 `tr` 返回对应语言文案。
    #[test]
    fn tr_switches_with_lang() {
        Lang::test_set(Lang::Zh);
        assert_eq!(tr("已导出", "Exported"), "已导出");
        Lang::test_set(Lang::En);
        assert_eq!(tr("已导出", "Exported"), "Exported");
        Lang::test_clear();
    }

    /// 语言切换按钮显示「对侧」语言：当前中文 → 英文文本，当前英文 → 中文文本。
    #[test]
    fn switch_button_shows_target_language() {
        Lang::test_set(Lang::Zh);
        assert_eq!(tr("Switch Language", "语言切换"), "Switch Language");
        Lang::test_set(Lang::En);
        assert_eq!(tr("Switch Language", "语言切换"), "语言切换");
        Lang::test_clear();
    }

    /// `toggle` 翻转并返回新语言。
    #[test]
    fn toggle_flips_language() {
        Lang::test_set(Lang::En);
        assert_eq!(Lang::toggle(), Lang::Zh);
        assert_eq!(Lang::get(), Lang::Zh);
        assert_eq!(Lang::toggle(), Lang::En);
        Lang::test_clear();
    }

    /// 宽松解析各种语言字符串（大小写不敏感）。
    #[test]
    fn parse_lang_variants() {
        assert_eq!(parse_lang("zh"), Some(Lang::Zh));
        assert_eq!(parse_lang("ZH-CN"), Some(Lang::Zh));
        assert_eq!(parse_lang("en"), Some(Lang::En));
        assert_eq!(parse_lang("EN_us"), Some(Lang::En));
        assert_eq!(parse_lang("fr"), None);
    }

    /// `fmt`（配合 `tfmt!`）可把当前语言文案的 `{}` 用参数填充，两语言占位符一致。
    #[test]
    fn formatted_tr_template() {
        Lang::test_set(Lang::En);
        let s = fmt("已导出到 {}", "Exported to {}", &["/tmp/x.json".to_string()]);
        assert_eq!(s, "Exported to /tmp/x.json");
        Lang::test_clear();
    }
}
