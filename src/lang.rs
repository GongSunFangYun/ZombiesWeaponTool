//! Internationalization / localization: provides Simplified Chinese and English UI strings.
//!
//! ## Design
//!
//! A zero-dependency approach that matches this project's "hand-rolled lightweight
//! components" philosophy (see `router.rs`'s mini YAML parser and `session.rs`'s small
//! persistence layer):
//!
//! - A process-global current [`Lang`], **English by default**;
//! - A [`tr`]`(zh, en)` helper — both languages are written inline at the call site;
//! - Language switching goes through [`Lang::toggle`] and is persisted into the session
//!   file (`.zwt`, see `SessionState::language` in [`crate::session`]);
//! - [`init`] applies an **explicit runtime override** on startup (`--lang` / `ZWT_LANG`),
//!   returning `Some` when an override was applied, else `None` so the session/default wins.
//!   Precedence: `--lang` / `ZWT_LANG` > `.zwt` session record > English default.
//!
//! ## The language-switch button label (cross-language)
//!
//! The switch entry on the Operations row shows the language you'd switch **to**, so the
//! user always knows the target:
//! - current = Chinese → shows English text `Switch Language`;
//! - current = English → shows Chinese text `语言切换`.
//!
//! This falls out naturally from `tr("Switch Language", "语言切换")`, because `tr` selects
//! the branch matching the current language, which happens to be the opposite language.
//!
//! ## Why config keys / binding names are not translated
//!
//! Config fields (e.g. `weapon_slot_1`) and binding names (e.g. `LALT`, `MB2`) are
//! identifiers that get **serialized into the config file and matched against real input** —
//! they are not UI text, so they are deliberately excluded from translation (see the field
//! names in `keymap.rs` / `config.rs`). This module only covers strings shown to the user.

use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::sync::atomic::{AtomicU8, Ordering};

/// Supported languages. Derives serde so it can be stored in the `.zwt` session file.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    /// Simplified Chinese.
    Zh,
    /// English.
    En,
}

/// The current language (0 = Chinese, 1 = English).
/// Uses `AtomicU8` instead of a mutable global `static mut`, so reading from the listener /
/// engine threads is race-free; `Relaxed` is enough because the language is only written at
/// a few control points.
static LANG: AtomicU8 = AtomicU8::new(1);

// Test-scoped, thread-local language override.
//
// `Lang` is a process-global, but Rust unit tests run on multiple threads in parallel. If
// each test called `Lang::set` directly it would corrupt the others (one test switches to
// English while another is rendering Chinese and its assertion fails). So tests use
// `Lang::test_set` to set a **per-thread** override; `get`/`set` read/write the override
// when present and fall back to the global otherwise. Thus test threads never interfere.
thread_local! {
    static TEST_OVERRIDE: Cell<Option<Lang>> = const { Cell::new(None) };
}

impl Lang {
    /// Read the current language. Prefers the test thread's override when set; otherwise
    /// the global (default English, `LANG` initial value = 1).
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

    /// Set the current language. If the test thread has an override set, write into the
    /// override (isolated tests); otherwise write the global (production run).
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

    /// Test-only: set the language override for the *current thread*; `get`/`set` then
    /// use this thread-local value.
    #[cfg(test)]
    #[inline]
    pub fn test_set(self) {
        TEST_OVERRIDE.with(|t| t.set(Some(self)));
    }

    /// Test-only: clear the current thread's override, making `get` fall back to the
    /// global (i.e. the English default).
    #[cfg(test)]
    #[inline]
    pub fn test_clear() {
        TEST_OVERRIDE.with(|t| t.set(None));
    }

    /// The "opposite" language (i.e. the target the switch button points at).
    #[inline]
    pub fn other(self) -> Self {
        match self {
            Lang::Zh => Lang::En,
            Lang::En => Lang::Zh,
        }
    }

    /// Flip the current language, apply it, and return the new value.
    #[inline]
    pub fn toggle() -> Self {
        let new = Lang::get().other();
        new.set();
        new
    }
}

/// Return the template for `(zh, en)` matching the current language, as `&'static str`.
///
/// The template is usually fed to [`fmt`] / [`tfmt!`] to fill its `{}` placeholders. **Both
/// languages must use the same number and order of placeholders**, otherwise the runtime
/// output could be shifted. Inlining both languages at the call site keeps them easy to
/// compare and maintain.
///
/// The language-switch label uses `tr("Switch Language", "语言切换")`: current Chinese →
/// returns the English text, current English → returns the Chinese text — giving the
/// "show the target language" cross effect.
#[inline]
pub fn tr(zh: &'static str, en: &'static str) -> &'static str {
    match Lang::get() {
        Lang::Zh => zh,
        Lang::En => en,
    }
}

/// Translate into the current language and fill placeholders, returning a `String`.
///
/// Why not `format!(tr(...), …)`: under edition 2024 `format!`/`format_args!` require the
/// format string to be a **string literal** and reject a runtime string, whereas `tr` returns
/// a `&'static str` that is only resolved at runtime. So this implements a lightweight
/// formatter that only supports positional `{}` placeholders (this project's messages use
/// only `{}` — no named captures / `{:?}`).
///
/// `args` are pre-`.to_string()`-ed arguments, substituted in the order the `{}` appear.
/// If there are more args than placeholders the extras are ignored; if there are fewer,
/// the leftover `{}` are left as-is.
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

/// Convenience macro: `tfmt!(zh, en, arg1, arg2, …)`.
///
/// Expands to a [`lang::fmt`] call, `.to_string()`-ing each argument and filling them in
/// order. Used across modules for parameterized messages (see [`fmt`] for why `format!`
/// isn't usable).
#[macro_export]
macro_rules! tfmt {
    ($zh:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        $crate::lang::fmt($zh, $en, &[$($arg.to_string()),*])
    };
}

/// Apply an **explicit runtime override** on startup: `ZWT_LANG` env var → `--lang` flag.
///
/// Returns `Some(lang)` when an override was applied (session restore will no longer touch
/// the language), else `None` — in which case the language is decided by the `.zwt` session
/// record or the English default. Idempotent: calling it more than once has no side effect.
pub fn init() -> Option<Lang> {
    // 1. Environment variable ZWT_LANG
    if let Ok(v) = std::env::var("ZWT_LANG") {
        if let Some(l) = parse_lang(&v) {
            l.set();
            return Some(l);
        }
    }
    // 2. Command-line flag --lang
    if let Some(l) = parse_cli_lang() {
        l.set();
        return Some(l);
    }
    None
}

/// Parse a lenient language string: accepts `zh` / `zh-cn` / `zh_cn` / `chinese` and
/// `en` / `en-us` / `en_us` / `english` (case-insensitive).
fn parse_lang(v: &str) -> Option<Lang> {
    match v.trim().to_ascii_lowercase().as_str() {
        "zh" | "zh-cn" | "zh_cn" | "zh-sg" | "chinese" | "中文" => Some(Lang::Zh),
        "en" | "en-us" | "en_us" | "en-gb" | "english" | "英文" => Some(Lang::En),
        _ => None,
    }
}

/// Find `--lang <zh|en>` in the CLI args (also supports `--lang=zh`, `--lang:zh`).
/// Returns `None` when no explicit language is found, so the session record / default wins.
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

    /// The default language is English (no `init` / no session). Clear the thread override
    /// so `get` reads the global default.
    #[test]
    fn defaults_to_english() {
        Lang::test_clear();
        assert_eq!(Lang::get(), Lang::En);
    }

    /// After setting the language, `tr` returns the matching text.
    #[test]
    fn tr_switches_with_lang() {
        Lang::test_set(Lang::Zh);
        assert_eq!(tr("已导出", "Exported"), "已导出");
        Lang::test_set(Lang::En);
        assert_eq!(tr("已导出", "Exported"), "Exported");
        Lang::test_clear();
    }

    /// The language-switch button shows the opposite language: current Chinese → English
    /// text, current English → Chinese text.
    #[test]
    fn switch_button_shows_target_language() {
        Lang::test_set(Lang::Zh);
        assert_eq!(tr("Switch Language", "语言切换"), "Switch Language");
        Lang::test_set(Lang::En);
        assert_eq!(tr("Switch Language", "语言切换"), "语言切换");
        Lang::test_clear();
    }

    /// `toggle` flips the language and returns the new value.
    #[test]
    fn toggle_flips_language() {
        Lang::test_set(Lang::En);
        assert_eq!(Lang::toggle(), Lang::Zh);
        assert_eq!(Lang::get(), Lang::Zh);
        assert_eq!(Lang::toggle(), Lang::En);
        Lang::test_clear();
    }

    /// Lenient parsing of various language strings (case-insensitive).
    #[test]
    fn parse_lang_variants() {
        assert_eq!(parse_lang("zh"), Some(Lang::Zh));
        assert_eq!(parse_lang("ZH-CN"), Some(Lang::Zh));
        assert_eq!(parse_lang("en"), Some(Lang::En));
        assert_eq!(parse_lang("EN_us"), Some(Lang::En));
        assert_eq!(parse_lang("fr"), None);
    }

    /// `fmt` (via `tfmt!`) fills a message's `{}` placeholders; both languages share the
    /// same placeholder layout.
    #[test]
    fn formatted_tr_template() {
        Lang::test_set(Lang::En);
        let s = fmt("已导出到 {}", "Exported to {}", &["/tmp/x.json".to_string()]);
        assert_eq!(s, "Exported to /tmp/x.json");
        Lang::test_clear();
    }
}
