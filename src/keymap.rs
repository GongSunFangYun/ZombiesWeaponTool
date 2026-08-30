//! Binding-name ↔ rdev mapping and simulation.
//!
//! Translates between the human-readable binding names produced by crossterm capture
//! (e.g. `2`, `LALT`, `MB2`) and `rdev`'s global input types, and simulates a tap of either.
//!
//! These names are stored in the config file and matched against real input, so they are
//! intentionally **not** localized (see `lang.rs`).

use rdev::{Button, EventType, Key};
use std::thread;
use std::time::Duration;

/// Interval between a simulated key press and its release.
/// Long enough for `SendInput` to register a single click, but not long enough to noticeably
/// slow the macro rhythm (a 40 ms delay would stack up inside the switch/shoot cycle).
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

/// Parse a single key name into an `rdev::Key`.
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

/// The "main key" of a binding name, used by the global listener to match key events
/// (ignores any CTRL/ALT/WIN modifier prefix).
pub fn binding_main_key(name: &str) -> Option<Key> {
    name.split('+')
        .last()
        .and_then(main_key)
}

/// `rdev` key → binding name (capture mode turns a globally-pressed key into a name storable
/// in the config). Esc returns `None` (the caller treats it as cancel-capture).
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

/// Parse a binding name into `(modifier list, main key)`.
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

/// Resolve a binding name to a mouse button.
pub fn binding_to_button(name: &str) -> Option<Button> {
    match name {
        "MB1" => Some(Button::Left),
        "MB2" => Some(Button::Right),
        "MB3" => Some(Button::Middle),
        _ => None,
    }
}

// FIX: the return value used to be ignored; now we log errors in debug builds so that
// rdev driver issues are easier to diagnose.
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

/// Simulate one tap of a binding (mouse button or key, supporting CTRL+/ALT+/WIN+ combos).
///
/// # Modifier-leak fix
///
/// The original implementation could leave a modifier (CTRL/ALT/WIN) permanently "stuck" in
/// one case: if `simulate_press` failed it returned early *before* releasing the modifier it
/// had already pressed.
///
/// The new implementation uses a "press one-by-one, roll back on failure" strategy:
/// 1. each modifier that succeeds is recorded into `pressed_mods` immediately;
/// 2. if any step fails, all recorded modifiers are released **in reverse order** before
///    returning `false`;
/// 3. after the main key is pressed, the modifiers are guaranteed to be released no matter
///    the result.
///
/// This keeps the OS keyboard state clean even if `rdev::simulate` fails mid-way, so it
/// doesn't disturb the player's later native input in the game window.
pub fn tap_binding(name: &str) -> bool {
    // --- Mouse-button path (no modifiers; logic unchanged) ---
    if let Some(btn) = binding_to_button(name) {
        let ok1 = simulate_press(&EventType::ButtonPress(btn));
        thread::sleep(TAP_DELAY);
        let ok2 = simulate_press(&EventType::ButtonRelease(btn));
        return ok1 && ok2;
    }

    // --- Keyboard path ---
    let Some((mods, main)) = parse_chord(name) else {
        return false;
    };

    // Press each modifier one at a time; on any failure, roll back the ones already pressed.
    let mut pressed_mods: Vec<Key> = Vec::with_capacity(mods.len());
    for m in &mods {
        if simulate_press(&EventType::KeyPress(*m)) {
            pressed_mods.push(*m);
        } else {
            // Roll back: release the already-pressed modifiers in reverse order so the OS
            // keyboard state stays clean.
            for pm in pressed_mods.iter().rev() {
                simulate_press(&EventType::KeyRelease(*pm));
            }
            return false;
        }
    }

    // Press the main key → wait TAP_DELAY → release the main key. Whatever `main_ok` is,
    // the modifiers must still be released below.
    let main_ok = simulate_press(&EventType::KeyPress(main));
    thread::sleep(TAP_DELAY);
    simulate_press(&EventType::KeyRelease(main));

    // Release the modifiers in reverse order (mirrors the press order — the nesting the OS
    // expects for press/release).
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

    /// An invalid binding name must not trigger any simulation (no panic, returns false).
    #[test]
    fn tap_binding_invalid_name_returns_false() {
        // This won't actually call rdev::simulate (unit tests have no system hook), but it
        // must at least not panic and must return false.
        let result = tap_binding("INVALID_KEY_NAME_XYZ");
        assert!(!result);
    }

    /// An empty binding name is handled safely.
    #[test]
    fn tap_binding_empty_returns_false() {
        let result = tap_binding("");
        assert!(!result);
    }
}