//! 绑定名（crossterm 捕获产生的可读名称）↔ rdev 全局输入 的映射与模拟。

use rdev::{Button, EventType, Key};
use std::thread;
use std::time::Duration;

/// 模拟敲击按键/鼠标按钮时，按下与释放之间的间隔。
/// 足够让 SendInput 注册一次点击，又不显著拖慢间隔节奏（40ms 会叠加进射击/切枪周期）。
const TAP_DELAY: Duration = Duration::from_millis(8);

const NUM_KEYS: [Key; 10] = [
    Key::Num0,
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
    Key::Num8,
    Key::Num9,
];

const LETTER_KEYS: [Key; 26] = [
    Key::KeyA,
    Key::KeyB,
    Key::KeyC,
    Key::KeyD,
    Key::KeyE,
    Key::KeyF,
    Key::KeyG,
    Key::KeyH,
    Key::KeyI,
    Key::KeyJ,
    Key::KeyK,
    Key::KeyL,
    Key::KeyM,
    Key::KeyN,
    Key::KeyO,
    Key::KeyP,
    Key::KeyQ,
    Key::KeyR,
    Key::KeyS,
    Key::KeyT,
    Key::KeyU,
    Key::KeyV,
    Key::KeyW,
    Key::KeyX,
    Key::KeyY,
    Key::KeyZ,
];

const F_KEYS: [Key; 12] = [
    Key::F1,
    Key::F2,
    Key::F3,
    Key::F4,
    Key::F5,
    Key::F6,
    Key::F7,
    Key::F8,
    Key::F9,
    Key::F10,
    Key::F11,
    Key::F12,
];

/// 把单个键名解析为 rdev::Key。
fn main_key(name: &str) -> Option<Key> {
    let n = name.to_ascii_uppercase();
    if n.len() == 1 {
        let b = n.as_bytes()[0];
        match b {
            b'0'..=b'9' => return Some(NUM_KEYS[(b - b'0') as usize]),
            b'A'..=b'Z' => return Some(LETTER_KEYS[(b - b'A') as usize]),
            _ => {}
        }
    }
    if let Some(f) = n.strip_prefix('F').and_then(|s| s.parse::<usize>().ok()) {
        if (1..=12).contains(&f) {
            return Some(F_KEYS[f - 1]);
        }
    }
    match n.as_str() {
        "LALT" | "ALT" => Some(Key::Alt),
        "RALT" | "ALTGR" => Some(Key::AltGr),
        "TAB" => Some(Key::Tab),
        "SPACE" => Some(Key::Space),
        "ENTER" => Some(Key::Return),
        "BACKSPACE" => Some(Key::Backspace),
        "DEL" | "DELETE" => Some(Key::Delete),
        "INSERT" => Some(Key::Insert),
        "HOME" => Some(Key::Home),
        "END" => Some(Key::End),
        "PAGEUP" => Some(Key::PageUp),
        "PAGEDOWN" => Some(Key::PageDown),
        "ARROW_LEFT" => Some(Key::LeftArrow),
        "ARROW_RIGHT" => Some(Key::RightArrow),
        "ARROW_UP" => Some(Key::UpArrow),
        "ARROW_DOWN" => Some(Key::DownArrow),
        "CAPSLOCK" => Some(Key::CapsLock),
        "SCROLLLOCK" => Some(Key::ScrollLock),
        "NUMLOCK" => Some(Key::NumLock),
        "PRINTSCREEN" => Some(Key::PrintScreen),
        "PAUSE" => Some(Key::Pause),
        "MENU" => Some(Key::AltGr),
        "PERIOD" => Some(Key::Dot),
        "COMMA" => Some(Key::Comma),
        "SLASH" => Some(Key::Slash),
        "BACKSLASH" => Some(Key::BackSlash),
        "MINUS" => Some(Key::Minus),
        "EQUALS" => Some(Key::Equal),
        "LBRACKET" => Some(Key::LeftBracket),
        "RBRACKET" => Some(Key::RightBracket),
        "SEMICOLON" => Some(Key::SemiColon),
        "APOSTROPHE" => Some(Key::Quote),
        "GRAVE" => Some(Key::BackQuote),
        _ => None,
    }
}

/// 绑定名的「主键」，用于全局监听时匹配按键事件（忽略 CTRL/ALT/WIN 组合前缀）。
pub fn binding_main_key(name: &str) -> Option<Key> {
    name.split('+')
        .last()
        .and_then(main_key)
}

/// rdev 按键 → 绑定名（捕获模式把全局按下的键转成可存配置的名称）。
/// Esc 返回 None（由上层处理为取消捕获）。
pub fn rdev_key_name(k: Key) -> Option<String> {
    use rdev::Key as K;
    let s = match k {
        K::Num0 => "0",
        K::Num1 => "1",
        K::Num2 => "2",
        K::Num3 => "3",
        K::Num4 => "4",
        K::Num5 => "5",
        K::Num6 => "6",
        K::Num7 => "7",
        K::Num8 => "8",
        K::Num9 => "9",
        K::KeyA => "A",
        K::KeyB => "B",
        K::KeyC => "C",
        K::KeyD => "D",
        K::KeyE => "E",
        K::KeyF => "F",
        K::KeyG => "G",
        K::KeyH => "H",
        K::KeyI => "I",
        K::KeyJ => "J",
        K::KeyK => "K",
        K::KeyL => "L",
        K::KeyM => "M",
        K::KeyN => "N",
        K::KeyO => "O",
        K::KeyP => "P",
        K::KeyQ => "Q",
        K::KeyR => "R",
        K::KeyS => "S",
        K::KeyT => "T",
        K::KeyU => "U",
        K::KeyV => "V",
        K::KeyW => "W",
        K::KeyX => "X",
        K::KeyY => "Y",
        K::KeyZ => "Z",
        K::F1 => "F1",
        K::F2 => "F2",
        K::F3 => "F3",
        K::F4 => "F4",
        K::F5 => "F5",
        K::F6 => "F6",
        K::F7 => "F7",
        K::F8 => "F8",
        K::F9 => "F9",
        K::F10 => "F10",
        K::F11 => "F11",
        K::F12 => "F12",
        K::Alt => "LALT",
        K::AltGr => "RALT",
        K::Tab => "TAB",
        K::Space => "SPACE",
        K::Return => "ENTER",
        K::Backspace => "BACKSPACE",
        K::Delete => "DEL",
        K::Insert => "INSERT",
        K::Home => "HOME",
        K::End => "END",
        K::PageUp => "PAGEUP",
        K::PageDown => "PAGEDOWN",
        K::LeftArrow => "ARROW_LEFT",
        K::RightArrow => "ARROW_RIGHT",
        K::UpArrow => "ARROW_UP",
        K::DownArrow => "ARROW_DOWN",
        K::CapsLock => "CAPSLOCK",
        K::ScrollLock => "SCROLLLOCK",
        K::NumLock => "NUMLOCK",
        K::PrintScreen => "PRINTSCREEN",
        K::Pause => "PAUSE",
        K::Dot => "PERIOD",
        K::Comma => "COMMA",
        K::Slash => "SLASH",
        K::BackSlash => "BACKSLASH",
        K::Minus => "MINUS",
        K::Equal => "EQUALS",
        K::LeftBracket => "LBRACKET",
        K::RightBracket => "RBRACKET",
        K::SemiColon => "SEMICOLON",
        K::Quote => "APOSTROPHE",
        K::BackQuote => "GRAVE",
        _ => return None,
    };
    Some(s.to_string())
}

/// 解析绑定名成 (修饰键列表, 主键)。
fn parse_chord(name: &str) -> Option<(Vec<Key>, Key)> {
    let mut mods = Vec::new();
    let mut main = None;
    for part in name.split('+') {
        match part {
            "CTRL" => mods.push(Key::ControlLeft),
            "ALT" => mods.push(Key::Alt),
            "WIN" => mods.push(Key::MetaLeft),
            other => main = Some(main_key(other)?),
        }
    }
    Some((mods, main?))
}

/// 把绑定名解析为鼠标按钮。
pub fn binding_to_button(name: &str) -> Option<Button> {
    match name {
        "MB1" => Some(Button::Left),
        "MB2" => Some(Button::Right),
        "MB3" => Some(Button::Middle),
        _ => None,
    }
}

// FIX: 原先忽略返回值；现在在 debug 模式下打印错误，方便排查 rdev 驱动问题。
fn simulate_press(et: &EventType) -> bool {
    match rdev::simulate(et) {
        Ok(()) => true,
        Err(_e) => {
            #[cfg(debug_assertions)]
            eprintln!("[keymap] simulate failed: {:?} — {:?}", et, _e);
            false
        }
    }
}

/// 模拟敲击一次绑定（鼠标按钮或按键，支持 CTRL+/ALT+/WIN+ 组合）。
///
/// # 修饰键泄漏修复
///
/// 原实现在以下情况下会导致修饰键（CTRL/ALT/WIN）永久卡住：
/// - `simulate_press` 失败后提前 return，已按下的修饰键未被释放。
///
/// 新实现采用"逐个按下、失败即回滚"的策略：
/// 1. 每按下一个修饰键，立即记录到 `pressed_mods`；
/// 2. 任意步骤失败时，先把 `pressed_mods` 里已按下的键全部**逆序释放**，再 return false；
/// 3. 主键按下后，无论成功与否，修饰键都保证被释放。
///
/// 这样即使 rdev::simulate 中途出错，系统键盘状态仍能恢复干净，
/// 不会干扰玩家在游戏窗口的后续原生输入。
pub fn tap_binding(name: &str) -> bool {
    // --- 鼠标按钮路径（无修饰键，逻辑不变）---
    if let Some(btn) = binding_to_button(name) {
        let ok1 = simulate_press(&EventType::ButtonPress(btn));
        thread::sleep(TAP_DELAY);
        let ok2 = simulate_press(&EventType::ButtonRelease(btn));
        return ok1 && ok2;
    }

    // --- 键盘路径 ---
    let Some((mods, main)) = parse_chord(name) else {
        return false;
    };

    // 逐个按下修饰键；任意失败时回滚已按下的修饰键并中止。
    let mut pressed_mods: Vec<Key> = Vec::with_capacity(mods.len());
    for m in &mods {
        if simulate_press(&EventType::KeyPress(*m)) {
            pressed_mods.push(*m);
        } else {
            // 回滚：逆序释放已按下的修饰键，保证系统键盘状态干净。
            for pm in pressed_mods.iter().rev() {
                simulate_press(&EventType::KeyRelease(*pm));
            }
            return false;
        }
    }

    // 主键按下 → 等待 TAP_DELAY → 主键释放。
    // 无论 main_ok 结果如何，修饰键都必须在下方释放。
    let main_ok = simulate_press(&EventType::KeyPress(main));
    thread::sleep(TAP_DELAY);
    simulate_press(&EventType::KeyRelease(main));

    // 修饰键逆序释放（保证与按下顺序镜像对称，符合 OS 期望的嵌套 press/release）。
    for m in pressed_mods.iter().rev() {
        simulate_press(&EventType::KeyRelease(*m));
    }

    main_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_key_parses_digits_letters_fkeys() {
        assert_eq!(main_key("2"), Some(Key::Num2));
        assert_eq!(main_key("3"), Some(Key::Num3));
        assert_eq!(main_key("A"), Some(Key::KeyA));
        assert_eq!(main_key("Z"), Some(Key::KeyZ));
        assert_eq!(main_key("F5"), Some(Key::F5));
        assert_eq!(main_key("F12"), Some(Key::F12));
    }

    #[test]
    fn main_key_parses_named_keys() {
        assert_eq!(main_key("LALT"), Some(Key::Alt));
        assert_eq!(main_key("RALT"), Some(Key::AltGr));
        assert_eq!(main_key("TAB"), Some(Key::Tab));
        assert_eq!(main_key("ENTER"), Some(Key::Return));
        assert_eq!(main_key("ARROW_LEFT"), Some(Key::LeftArrow));
        assert_eq!(main_key("SPACE"), Some(Key::Space));
    }

    #[test]
    fn binding_main_key_strips_combo_prefix() {
        assert_eq!(binding_main_key("LALT"), Some(Key::Alt));
        assert_eq!(binding_main_key("CTRL+1"), Some(Key::Num1));
        assert_eq!(binding_main_key("ALT+F5"), Some(Key::F5));
    }

    #[test]
    fn mouse_buttons() {
        assert_eq!(binding_to_button("MB1"), Some(Button::Left));
        assert_eq!(binding_to_button("MB2"), Some(Button::Right));
        assert_eq!(binding_to_button("MB3"), Some(Button::Middle));
        assert_eq!(binding_to_button("2"), None);
    }

    #[test]
    fn chord_parses() {
        let (mods, main) = parse_chord("CTRL+1").unwrap();
        assert_eq!(mods, vec![Key::ControlLeft]);
        assert_eq!(main, Key::Num1);
    }

    #[test]
    fn rdev_key_name_maps_back() {
        assert_eq!(rdev_key_name(Key::Num2).as_deref(), Some("2"));
        assert_eq!(rdev_key_name(Key::KeyA).as_deref(), Some("A"));
        assert_eq!(rdev_key_name(Key::F5).as_deref(), Some("F5"));
        assert_eq!(rdev_key_name(Key::Alt).as_deref(), Some("LALT"));
        assert_eq!(rdev_key_name(Key::AltGr).as_deref(), Some("RALT"));
        assert_eq!(rdev_key_name(Key::Escape), None);
    }

    /// 验证无效绑定名不会触发任何模拟（不 panic，返回 false）。
    #[test]
    fn tap_binding_invalid_name_returns_false() {
        // 不会真正调用 rdev::simulate（单元测试无系统钩子），
        // 但至少不应 panic，且返回 false。
        let result = tap_binding("INVALID_KEY_NAME_XYZ");
        assert!(!result);
    }

    /// 验证空绑定名安全处理。
    #[test]
    fn tap_binding_empty_returns_false() {
        let result = tap_binding("");
        assert!(!result);
    }
}