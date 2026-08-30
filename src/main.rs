//! ZombiesWeaponTool — a `ratatui`/`crossterm` terminal TUI.
//!
//! A **global keyboard/mouse macro tool** for Zombies / Black-Ops-style first-person shooters:
//! it listens for a global execution hotkey and, per the config, cycles through a combined
//! "auto weapon-switch + right-click autoclick" sequence. Supports three weapon slots, an
//! execution order set, hold/toggle modes, random ordering, timing jitter, a quick-switch
//! config router, config hot-read/hot-save, and session restore.
//!
//! ## Module responsibilities
//! - [`config`]: the config struct, JSON read/write, and validation;
//! - [`engine`]: `rdev` global listening + the simulated-input execution loop;
//! - [`keymap`]: mapping & simulation between readable binding names and `rdev` keys/mouse;
//! - [`router`]: lightweight `zwtcfg_router.yaml` parsing + invalid-entry markers;
//! - [`session`]: session-state persistence (last config/router);
//! - [`watcher`]: background config-file watcher daemon thread;
//! - [`lang`]: Chinese/English internationalization.
//!
//! ## Language
//! Defaults to **English**; switch to Chinese via the `Switch Language` / `语言切换` entry on
//! the Operations row, persisted to the session file `.zwt` and restored on next launch. You
//! can also force a language with `--lang zh|en` / `ZWT_LANG=zh|en` (see [`lang::init`],
//! [`lang::Lang::toggle`]).

use std::{fs, io};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode,
        KeyEventKind, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame, Terminal,
};

mod config;
mod engine;
mod keymap;
mod lang;
mod router;
mod session;
mod watcher;

use config::{Config, ExecutionMode};
use engine::{Engine, EngineMsg};
use lang::{tr, Lang};
use watcher::WatchedPath;

/// Title art: a single-piece ASCII drawing of `ZombiesWeaponTool`, written as a raw string
/// so backslashes don't need escaping.
const BANNER: [&str; 6] = [
    r#"  _____               _     _         __        __                         _____           _ "#,
    r#" |__  /___  _ __ ___ | |__ (_) ___  __\ \      / /__  __ _ _ __   ___  _ _|_   _|__   ___ | |"#,
    r#"   / // _ \| '_ ` _ \| '_ \| |/ _ \/ __\ \ /\ / / _ \/ _` | '_ \ / _ \| '_ \| |/ _ \ / _ \| |"#,
    r#"  / /| (_) | | | | | | |_) | |  __/\__ \\ V  V /  __/ (_| | |_) | (_) | | | | | (_) | (_) | |"#,
    r#" /____\___/|_| |_| |_|_.__/|_|\___||___/ \_/\_/  \__|\__,_| .__/ \___/|_| |_|_|\___/ \___/|_|"#,
    r#"                                                          |_|                                "#,
];

/// Open-source GitHub repository URL, shown on the banner's bottom line.
const TOP_BAR: &str = "© GongSunFangYun | https://github.com/GongSunFangYun/ZombiesWeaponTool";

/// Total field count: 5 bindings + 1 order + 1 mode + 1 toggle + 4 numbers + 4 actions.
const TOTAL_FIELDS: usize = 16;

const F_WEAPON1: usize = 0;
const F_WEAPON2: usize = 1;
const F_WEAPON3: usize = 2;
const F_EXEC_KEY: usize = 3;
const F_ROUTER: usize = 4;
const F_ORDER: usize = 5;
const F_MODE: usize = 6;
const F_RANDOM: usize = 7;
const F_SWITCH: usize = 8;
const F_SWITCH_OFF: usize = 9;
const F_SHOOT: usize = 10;
const F_SHOOT_OFF: usize = 11;
const A_EXPORT: usize = 12;
const A_LOAD: usize = 13;
const A_ROUTER: usize = 14;
const A_LANG: usize = 15;

/// Field indices per row: two binding rows (weapon slots / hotkeys), one order row, two
/// timing rows, one actions row.
const ROWS: [&[usize]; 6] = [
    &[F_WEAPON1, F_WEAPON2, F_WEAPON3],
    &[F_EXEC_KEY, F_ROUTER],
    &[F_ORDER, F_MODE, F_RANDOM],
    &[F_SWITCH, F_SWITCH_OFF],
    &[F_SHOOT, F_SHOOT_OFF],
    &[A_EXPORT, A_LOAD, A_ROUTER, A_LANG],
];

#[derive(Clone, Copy, PartialEq)]
enum FieldKind {
    Binding,
    Number,
    Order,
    Toggle,
    Action,
}

#[derive(Clone, PartialEq)]
enum Mode {
    /// Normal navigation.
    Normal,
    /// Capturing a key for a binding field.
    Capture { field: usize },
    /// Editing a number (with an input buffer).
    Edit { field: usize, buf: String },
    /// Editing the execution-order set (A/B/C, each at most once).
    OrderEdit { field: usize, buf: String },
}

#[derive(Clone, Copy, PartialEq)]
#[allow(dead_code)]
enum MsgKind {
    Info,
    Ok,
    Err,
}

#[allow(dead_code)]
struct StatusMsg {
    text: String,
    kind: MsgKind,
}

#[derive(Clone, Copy)]
enum DialogKind {
    Export,
    Load,
    Router,
}

/// A router entry: the config path plus its per-entry validation result.
/// Invalid entries (missing file / JSON syntax or spec error) are shown in red on the status
/// line and are skipped during quick-switching.
struct RouterEntry {
    path: PathBuf,
    valid: bool,
    error: String,
}

struct App {
    cfg: Config,
    /// Shared config copy read by the engine thread.
    shared: Arc<Mutex<Config>>,
    engine: Arc<Engine>,
    msg_rx: mpsc::Receiver<EngineMsg>,
    /// Whether we're in capture/edit mode (shared with the global listener so it doesn't
    /// accidentally drive the engine).
    interacting: Arc<AtomicBool>,
    /// The current config source file (used for export default name, hot-save target, and the
    /// hot-read watch subject).
    config_source: Option<PathBuf>,
    /// The config source's modification time (hot-read: compare against external edits and
    /// auto-reload).
    config_mtime: Option<SystemTime>,
    /// The currently-watched path shared with the watcher daemon thread; kept in sync with
    /// `config_source` (see `note_config_source`).
    watched_path: WatchedPath,
    /// Loaded router YAML path (for session persistence).
    router_path: Option<PathBuf>,
    /// Session-state file path (default `~/.zwt`; tests inject a temp path).
    session_path: PathBuf,
    /// Quick-switch router: entries parsed from `zwtcfg_router.yaml` (with validation results).
    router_files: Vec<RouterEntry>,
    /// Current router position (cycles the list in order).
    router_index: usize,
    focus: usize,
    mode: Mode,
    #[allow(dead_code)]
    status: Option<StatusMsg>,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        // Explicit runtime language override (--lang / ZWT_LANG); if present it wins and
        // session restore won't change the language.
        let lang_override = lang::init();
        let shared = Arc::new(Mutex::new(Config::default()));
        let engine = Arc::new(Engine::new(shared.clone()));
        let interacting = Arc::new(AtomicBool::new(false));
        let watched_path: WatchedPath = Arc::new(Mutex::new(None));
        let (tx, msg_rx) = mpsc::channel();
        let _listener = engine::start_listener(engine.clone(), interacting.clone(), tx.clone());
        // Config watcher daemon thread: notifies the main thread to hot-reload when the config
        // file is rewritten externally (handle discarded → daemon).
        let _watcher = watcher::start_watcher(watched_path.clone(), tx);
        let mut app = App {
            cfg: Config::default(),
            shared,
            engine,
            msg_rx,
            interacting,
            config_source: None,
            config_mtime: None,
            watched_path,
            router_path: None,
            session_path: session::state_path(),
            router_files: Vec::new(),
            router_index: 0,
            focus: 0,
            mode: Mode::Normal,
            status: None,
            should_quit: false,
        };
        // Session restore: apply the persisted language + reload the last router table and
        // apply the last-used config; with no state / corrupted state, fall through to defaults.
        app.restore_session(lang_override);
        // If no load/restore message was produced, show the initial operation hint. It's
        // rendered in the *final* language, so a language override from the session doesn't
        // leave the first frame showing the old language.
        if app.status.is_none() {
            app.status = Some(StatusMsg {
                text: tr(
                    "Tab 切换项目, Enter 编辑/确认, 执行热键 启动/停止操作, Esc 退出",
                    "Tab to switch field, Enter to edit/confirm, execution hotkey to start/stop, Esc to exit",
                )
                .into(),
                kind: MsgKind::Info,
            });
        }
        // Record the config source's mtime (for hot-read comparison) and sync it to the engine.
        app.touch_config_mtime();
        // Publish the current config source to the watcher thread (it may already be set by
        // session restore).
        app.note_config_source();
        *app.shared.lock().unwrap() = app.cfg.clone();
        app
    }

    /// Start session restore: read the session state file, reload the last router table, and
    /// apply the last-used config. With no state / corrupted state → default config flow; if the
    /// recorded file was deleted → fallback chain (first valid router entry → `zwtcfg.json`).
    ///
    /// Also restores the last-used language. Precedence: `lang_override` (`--lang`/`ZWT_LANG`) >
    /// session record `.zwt` > English default.
    fn restore_session(&mut self, lang_override: Option<Lang>) {
        let Some(state) = session::load_from(&self.session_path) else {
            self.load_default_config();
            return;
        };
        // Language: an explicit runtime override wins; otherwise restore the session's choice.
        if lang_override.is_none() {
            if let Some(l) = state.language {
                Lang::set(l);
            }
        }
        self.restore_with_state(state);
    }

    /// Restore from a given session state (the core restore logic; tests inject the state).
    fn restore_with_state(&mut self, state: session::SessionState) {
        // 1. Auto-reload the last router table (skip with a status message if the YAML is
        //    missing/corrupt).
        if let Some(rp) = &state.last_router {
            let rp = PathBuf::from(rp);
            if rp.exists() {
                if let Err(e) = self.load_router_path(&rp) {
                    self.status = Some(StatusMsg {
                        text: tfmt!(
                            "自动恢复路由失败（跳过）: {}",
                            "Auto-restore router failed (skipped): {}",
                            e
                        ),
                        kind: MsgKind::Err,
                    });
                }
            }
        }
        // 2. Apply the current config (precedence: last config → first valid router → default).
        let applied = if let Some(cp) = &state.last_config {
            let cp = PathBuf::from(cp);
            if cp.exists() {
                self.apply_config_path(&cp).is_ok()
            } else {
                false
            }
        } else {
            false
        };
        if !applied {
            if let Some(idx) = self.router_files.iter().position(|e| e.valid) {
                let p = self.router_files[idx].path.clone();
                if self.apply_config_path(&p).is_ok() {
                    self.router_index = idx;
                } else {
                    self.load_default_config();
                }
            } else {
                self.load_default_config();
            }
        }
        // 3. Align router highlight with the active config; else recover the recorded index
        //    (fall back to the first valid one if out of bounds / invalid).
        if !self.router_files.is_empty() {
            if let Some(cp) = self.config_source.as_ref() {
                if let Some(idx) = self.router_files.iter().position(|e| {
                    e.valid && e.path.to_string_lossy().eq_ignore_ascii_case(&cp.to_string_lossy())
                }) {
                    self.router_index = idx;
                    self.save_session();
                    return;
                }
            }
            let recorded = state.router_index;
            self.router_index = if recorded < self.router_files.len()
                && self.router_files[recorded].valid
            {
                recorded
            } else {
                self.router_files.iter().position(|e| e.valid).unwrap_or(0)
            };
        }
        // 4. Persist the restored state (idempotent).
        self.save_session();
    }

    /// Default config flow: load `zwtcfg.json` if present, otherwise generate one from the
    /// current in-memory config.
    fn load_default_config(&mut self) {
        let default_path = Path::new("zwtcfg.json");
        if default_path.exists() {
            match config::load_from_path(default_path) {
                Ok(cfg) => {
                    self.cfg = cfg;
                    self.config_source = Some(default_path.to_path_buf());
                    self.note_config_source();
                    self.status = Some(StatusMsg {
                        text: tfmt!(
                            "已自动加载 {}",
                            "Auto-loaded {}",
                            default_path.display()
                        ),
                        kind: MsgKind::Ok,
                    });
                    // Loading doesn't write to disk (disk is only written on edit-confirm);
                    // engine sync is done once at the end of `new()`.
                }
                Err(e) => {
                    self.status = Some(StatusMsg {
                        text: tfmt!(
                            "自动加载 zwtcfg.json 失败: {}",
                            "Failed to auto-load zwtcfg.json: {}",
                            e
                        ),
                        kind: MsgKind::Err,
                    });
                }
            }
        } else {
            match config::save_to_path(&self.cfg, default_path) {
                Ok(()) => {
                    // The generated default config becomes the current source (hot-save target
                    // and hot-read watch subject).
                    self.config_source = Some(default_path.to_path_buf());
                    self.note_config_source();
                    self.status = Some(StatusMsg {
                        text: tfmt!(
                            "已生成默认配置 {}",
                            "Generated default config {}",
                            default_path.display()
                        ),
                        kind: MsgKind::Ok,
                    });
                }
                Err(e) => {
                    self.status = Some(StatusMsg {
                        text: tfmt!(
                            "生成默认配置失败: {}",
                            "Failed to generate default config: {}",
                            e
                        ),
                        kind: MsgKind::Err,
                    });
                }
            }
        }
    }

    /// Persist the session state to the state file: records the current config source, router
    /// YAML, and router position. On failure only a status message is set; the flow is not
    /// blocked.
    fn save_session(&mut self) {
        let state = session::SessionState {
            last_config: self
                .config_source
                .as_ref()
                .map(|p| p.display().to_string()),
            last_router: self.router_path.as_ref().map(|p| p.display().to_string()),
            router_index: self.router_index,
            language: Some(Lang::get()),
        };
        if let Err(e) = session::save_to(&self.session_path, &state) {
            self.status = Some(StatusMsg {
                text: tfmt!(
                    "保存会话状态失败: {}",
                    "Failed to save session state: {}",
                    e
                ),
                kind: MsgKind::Err,
            });
        }
    }

    fn handle_event(&mut self, ev: Event) -> Option<DialogKind> {
        // On Windows each key produces a Press and Release (and a Repeat while held); we only
        // handle Press so a single tap doesn't trigger the action twice and double-Esc still
        // quits.
        if let Event::Key(k) = &ev {
            if k.kind != KeyEventKind::Press {
                return None;
            }
        }
        match self.mode.clone() {
            Mode::Capture { field } => self.handle_capture(field, ev),
            Mode::Edit { field, buf } => self.handle_edit(field, buf, ev),
            // Order-set editing is driven by the global listener thread via `CaptureKey`
            // (rdev); ignore crossterm keyboard here to avoid key-stuck on fast terminal input.
            Mode::OrderEdit { .. } => None,
            Mode::Normal => self.handle_normal(ev),
        }
    }

    fn handle_capture(&mut self, field: usize, ev: Event) -> Option<DialogKind> {
        match ev {
            // Key bindings (incl. Alt, Esc-cancel) are all forwarded by the global listener
            // thread via `CaptureKey`; crossterm key events are ignored here so Alt isn't
            // misread as Esc (which would cancel capture).
            Event::Key(_) => {}
            Event::Mouse(m) => {
                if let MouseEventKind::Down(btn) = m.kind {
                    self.bind(field, mouse_button_name(btn));
                }
            }
            _ => {}
        }
        None
    }

    /// Unified bind entry point: uniqueness check → set → exit capture → auto-save.
    fn bind(&mut self, field: usize, name: String) {
        if let Some(dup) = self.find_binding_conflict(field, &name) {
            self.status = Some(StatusMsg {
                text: tfmt!(
                    "绑定失败: {} 已被 {} 使用",
                    "Bind failed: {} already used by {}",
                    name,
                    dup
                ),
                kind: MsgKind::Err,
            });
            return; // stay in capture mode so the user can press a different key
        }
        self.set_binding(field, name);
        self.mode = Mode::Normal;
        self.status = Some(StatusMsg {
            text: tr("按键已绑定", "Key bound").into(),
            kind: MsgKind::Ok,
        });
        self.auto_save();
    }

    /// Check whether `name` is already used by another binding field; return the display name
    /// of the conflicting field.
    fn find_binding_conflict(&self, field: usize, name: &str) -> Option<&'static str> {
        let bindings: [(&'static str, usize, &str); 5] = [
            (
                tr("武器槽#1", "Weapon Slot #1"),
                F_WEAPON1,
                self.cfg.weapon_slot1.as_str(),
            ),
            (
                tr("武器槽#2", "Weapon Slot #2"),
                F_WEAPON2,
                self.cfg.weapon_slot2.as_str(),
            ),
            (
                tr("武器槽#3", "Weapon Slot #3"),
                F_WEAPON3,
                self.cfg.weapon_slot3.as_str(),
            ),
            (
                tr("执行热键", "Execution Hotkey"),
                F_EXEC_KEY,
                self.cfg.execution_hotkey.as_str(),
            ),
            (
                tr("速切配置热键", "Quick-switch Hotkey"),
                F_ROUTER,
                self.cfg.router_hotkey.as_str(),
            ),
        ];
        bindings
            .into_iter()
            .find(|(_, idx, v)| *idx != field && *v == name)
            .map(|(label, _, _)| label)
    }

    /// Edit the execution-order set (driven by rdev global keys to avoid key-stuck on fast
    /// terminal input). Only accepts A/B/C, up to three letters, no repeats; a repeat/over-limit
    /// prompts, Esc cancels, Enter confirms.
    fn order_edit_key(&mut self, field: usize, k: rdev::Key) {
        let buf = match &self.mode {
            Mode::OrderEdit { buf, .. } => buf.clone(),
            _ => return,
        };
        match k {
            rdev::Key::Escape => {
                self.mode = Mode::Normal;
                self.status = None;
            }
            rdev::Key::Backspace => {
                let mut buf = buf;
                buf.pop();
                self.mode = Mode::OrderEdit { field, buf };
            }
            rdev::Key::Return => {
                if is_valid_order(&buf) {
                    self.cfg.execution_order = buf;
                    self.mode = Mode::Normal;
                    self.status = Some(StatusMsg {
                        text: tr("执行顺序已更新", "Execution order updated").into(),
                        kind: MsgKind::Ok,
                    });
                    self.auto_save();
                } else {
                    self.status = Some(StatusMsg {
                        text: tr(
                            "执行顺序无效: 从 A/B/C 选 1~3 个，每个最多一次 (如 A, AB, ABC)",
                            "Invalid execution order: pick 1~3 from A/B/C, each at most once (e.g. A, AB, ABC)",
                        )
                        .into(),
                        kind: MsgKind::Err,
                    });
                }
            }
            rdev::Key::KeyA | rdev::Key::KeyB | rdev::Key::KeyC => {
                let up = match k {
                    rdev::Key::KeyA => 'A',
                    rdev::Key::KeyB => 'B',
                    _ => 'C',
                };
                // Set uniqueness: silently ignore if it would exceed 3 chars or is already present
                if buf.len() < 3 && !buf.contains(up) {
                    let mut buf = buf;
                    buf.push(up);
                    self.mode = Mode::OrderEdit { field, buf };
                    self.status = None;
                }
            }
            _ => {}
        }
    }

    fn handle_edit(&mut self, field: usize, buf: String, ev: Event) -> Option<DialogKind> {
        let mut buf = buf;
        match ev {
            Event::Key(k) => match k.code {
                KeyCode::Char(c) if c.is_ascii_digit() => buf.push(c),
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Enter => {
                    let v = buf.parse::<u64>().unwrap_or(0);
                    // FIX: prevent `shoot_interval` / `switch_interval` from being set to 0.
                    // A zero interval sends the engine's `run_loop` into a 200 ms busy-retry
                    // spin and makes `jitter()` produce a 0 ms interval → CPU-burning busy-loop.
                    // Offset fields may be 0 (meaning "no jitter"); only the main interval field
                    // is clamped.
                    let is_interval = matches!(field, F_SWITCH | F_SHOOT);
                    if is_interval && v == 0 {
                        self.status = Some(StatusMsg {
                            text: tr(
                                "间隔不能为 0，请输入一个正整数（单位 ms）",
                                "Interval cannot be 0; enter a positive integer (ms)",
                            )
                            .into(),
                            kind: MsgKind::Err,
                        });
                        self.mode = Mode::Edit { field, buf };
                        return None;
                    }
                    self.set_numeric(field, v);
                    self.mode = Mode::Normal;
                    self.status = Some(StatusMsg {
                        text: tr("数值已更新", "Value updated").into(),
                        kind: MsgKind::Ok,
                    });
                    self.auto_save();
                    return None;
                }
                KeyCode::Esc => {
                    self.mode = Mode::Normal;
                    self.status = None;
                    return None;
                }
                _ => {}
            },
            _ => {}
        }
        self.mode = Mode::Edit { field, buf };
        None
    }

    fn handle_normal(&mut self, ev: Event) -> Option<DialogKind> {
        match ev {
            Event::Key(k) => match k.code {
                KeyCode::Tab => self.move_horizontal(1),
                KeyCode::BackTab => self.move_horizontal(-1),
                KeyCode::Right => self.move_horizontal(1),
                KeyCode::Left => self.move_horizontal(-1),
                KeyCode::Up => self.move_vertical(-1),
                KeyCode::Down => self.move_vertical(1),
                KeyCode::Enter => {
                    return self.confirm();
                }
                KeyCode::Esc => {
                    self.should_quit = true;
                }
                _ => {}
            },
            _ => {}
        }
        None
    }

    fn confirm(&mut self) -> Option<DialogKind> {
        self.status = None;
        match self.focus {
            f if f <= F_ROUTER => {
                self.mode = Mode::Capture { field: f };
                None
            }
            F_ORDER => {
                // The set is also editable under random order (choose which elements shuffle).
                self.mode = Mode::OrderEdit {
                    field: F_ORDER,
                    buf: self.cfg.execution_order.clone(),
                };
                None
            }
            F_MODE => {
                // Execution mode: Hold ↔ Toggle.
                self.cfg.execution_mode = match self.cfg.execution_mode {
                    ExecutionMode::Hold => ExecutionMode::Toggle,
                    ExecutionMode::Toggle => ExecutionMode::Hold,
                };
                let label = if self.cfg.execution_mode == ExecutionMode::Hold {
                    tr("长按", "Hold")
                } else {
                    tr("切换", "Toggle")
                };
                self.status = Some(StatusMsg {
                    text: tfmt!("执行方式已切换为{}", "Execution mode switched to {}", label),
                    kind: MsgKind::Ok,
                });
                self.auto_save();
                None
            }
            F_RANDOM => {
                self.cfg.random_execution = !self.cfg.random_execution;
                let on = if self.cfg.random_execution {
                    tr("开", "ON")
                } else {
                    tr("关", "OFF")
                };
                self.status = Some(StatusMsg {
                    text: tfmt!("乱序执行已{}", "Random order {}", on),
                    kind: MsgKind::Ok,
                });
                self.auto_save();
                None
            }
            f if f >= F_SWITCH && f <= F_SHOOT_OFF => {
                let v = self.numeric_value(f);
                self.mode = Mode::Edit {
                    field: f,
                    buf: v.to_string(),
                };
                None
            }
            A_EXPORT => Some(DialogKind::Export),
            A_LOAD => Some(DialogKind::Load),
            A_ROUTER => Some(DialogKind::Router),
            A_LANG => {
                self.switch_language();
                None
            }
            _ => None,
        }
    }

    /// Switch the UI language (Chinese ↔ English) and persist the choice to the session file
    /// `.zwt`. The status message is shown in the **new** language, so it doesn't linger in the
    /// old one.
    fn switch_language(&mut self) {
        let new = Lang::toggle();
        let text = match new {
            Lang::En => "Language switched to English",
            Lang::Zh => "语言已切换为中文",
        };
        self.status = Some(StatusMsg {
            text: text.into(),
            kind: MsgKind::Ok,
        });
        self.save_session();
    }

    fn move_horizontal(&mut self, dir: i32) {
        self.status = None;
        self.focus = (self.focus as i32 + dir).rem_euclid(TOTAL_FIELDS as i32) as usize;
    }

    fn move_vertical(&mut self, dir: i32) {
        self.status = None;
        let cur_row = ROWS
            .iter()
            .position(|r| r.contains(&self.focus))
            .unwrap_or(0);
        let col = ROWS[cur_row]
            .iter()
            .position(|&f| f == self.focus)
            .unwrap_or(0);
        let new_row = (cur_row as i32 + dir).clamp(0, ROWS.len() as i32 - 1) as usize;
        let row = ROWS[new_row];
        self.focus = row
            .get(col)
            .copied()
            .unwrap_or_else(|| row[row.len() - 1]);
    }

    /// Sync only the engine thread's config copy (no disk write). Used by non-edit operations
    /// like load / quick-switch / hot-read, so it never overwrites the user's manually-configured
    /// `zwtcfg.json` — the disk is only written on edit-confirm.
    fn sync_engine(&mut self) {
        *self.shared.lock().unwrap() = self.cfg.clone();
    }

    /// Record the config source's mtime (for hot-read to compare external edits); cleared if the
    /// file is unreadable.
    fn touch_config_mtime(&mut self) {
        self.config_mtime = self
            .config_source
            .as_ref()
            .and_then(|p| fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
    }

    /// Publish the current `config_source` to the watcher daemon thread. Must be called after
    /// every `config_source` change (load / quick-switch / hot-read success, etc.) so the
    /// watcher follows the latest file.
    fn note_config_source(&mut self) {
        *self.watched_path.lock().unwrap() = self.config_source.clone();
    }

    /// Hot-save: after the user confirms an edit (Enter), save once to the **config file in
    /// use** (`config_source`; `zwtcfg.json` if none was loaded) and sync to the engine thread.
    /// Called by binding, mode/random toggles, order-set confirm, and numeric edit confirm;
    /// cancel (Esc) does not save.
    fn auto_save(&mut self) {
        let path = self
            .config_source
            .clone()
            .unwrap_or_else(|| PathBuf::from("zwtcfg.json"));
        if let Err(e) = config::save_to_path(&self.cfg, &path) {
            self.status = Some(StatusMsg {
                text: tfmt!("自动保存失败: {}", "Auto-save failed: {}", e),
                kind: MsgKind::Err,
            });
        }
        self.touch_config_mtime();
        self.sync_engine();
    }

    /// Hot-read: detect whether the current config source was modified externally (mtime
    /// changed); if so, reload it and sync the engine. Applied only when the file is readable and
    /// parses/validates; on failure or when the file is deleted, the current config is kept (not
    /// overwritten).
    fn hot_read(&mut self) {
        let Some(src) = self.config_source.clone() else { return };
        let mtime = match fs::metadata(&src).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => {
                // File deleted: keep the current config, notify once.
                if self.config_mtime.is_some() {
                    self.status = Some(StatusMsg {
                        text: tfmt!(
                            "配置文件 {} 已被删除（保持当前配置）",
                            "Config file {} was deleted (keeping current config)",
                            src.display()
                        ),
                        kind: MsgKind::Err,
                    });
                    self.config_mtime = None;
                }
                return;
            }
        };
        if Some(mtime) == self.config_mtime {
            return; // unchanged
        }
        match config::load_from_path(&src) {
            Ok(cfg) => {
                self.cfg = cfg;
                self.config_source = Some(src.clone());
                self.note_config_source();
                self.config_mtime = Some(mtime);
                self.sync_engine();
                self.status = Some(StatusMsg {
                    text: tfmt!(
                        "已热读取外部修改 {}（运行中已生效）",
                        "Hot-loaded external change {} (applied while running)",
                        src.display()
                    ),
                    kind: MsgKind::Ok,
                });
            }
            Err(e) => {
                // External write mid-flight / content corrupt: don't overwrite; retry next poll.
                self.status = Some(StatusMsg {
                    text: tfmt!(
                        "热读取失败（保持当前配置，稍后重试）: {}",
                        "Hot-load failed (keeping current config, will retry): {}",
                        e
                    ),
                    kind: MsgKind::Err,
                });
            }
        }
    }

    fn export_config_dialog(&mut self) {
        // Default filename = the current config source's file name; fall back to zwtcfg.json when
        // there's no source. The user may rename it.
        let default_name = self
            .config_source
            .as_ref()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "zwtcfg.json".to_string());
        let file = rfd::FileDialog::new()
            .set_title(tr("导出配置", "Export Config"))
            .set_file_name(default_name.as_str())
            .save_file();
        match file {
            Some(path) => match config::save_to_path(&self.cfg, &path) {
                Ok(()) => self.status = Some(StatusMsg {
                    text: tfmt!("已导出到 {}", "Exported to {}", path.display()),
                    kind: MsgKind::Ok,
                }),
                Err(e) => self.status = Some(StatusMsg {
                    text: tfmt!("导出失败: {}", "Export failed: {}", e),
                    kind: MsgKind::Err,
                }),
            },
            None => self.status = Some(StatusMsg {
                text: tr("已取消导出", "Export cancelled").into(),
                kind: MsgKind::Info,
            }),
        }
    }

    fn load_config_dialog(&mut self) {
        // No filename/extension restriction: any readable config file is allowed.
        let file = rfd::FileDialog::new()
            .set_title(tr("读取配置", "Load Config"))
            .pick_file();
        match file {
            Some(path) => match self.apply_config_path(&path) {
                Ok(()) => self.status = Some(StatusMsg {
                    text: tfmt!(
                        "已读取 {} (当前行为基于该配置)",
                        "Loaded {} (current behavior is based on this config)",
                        path.display()
                    ),
                    kind: MsgKind::Ok,
                }),
                Err(e) => self.status = Some(StatusMsg {
                    text: tfmt!("读取失败: {}", "Load failed: {}", e),
                    kind: MsgKind::Err,
                }),
            },
            None => self.status = Some(StatusMsg {
                text: tr("已取消读取", "Load cancelled").into(),
                kind: MsgKind::Info,
            }),
        }
    }

    /// Load and apply a given JSON config: update `cfg`/source and sync engine behavior.
    /// Only syncs, never writes to disk — the disk is written only on edit-confirm (see
    /// `auto_save`). On failure returns `Err(reason)` and leaves the current config untouched.
    fn apply_config_path(&mut self, path: &Path) -> Result<(), String> {
        let cfg = config::load_from_path(path)?;
        self.cfg = cfg;
        self.config_source = Some(path.to_path_buf());
        self.note_config_source();
        self.touch_config_mtime();
        self.sync_engine();
        self.save_session();
        Ok(())
    }

    /// Load a given router YAML: parse → validate each entry → write invalid-marker comments →
    /// populate `router_files`/`router_path`, and auto-load the first valid config. Returns
    /// `Err(reason)` for a whole-load failure (read failure / syntax or spec error), in which
    /// case the existing router is left unchanged.
    fn load_router_path(&mut self, path: &Path) -> Result<(), String> {
        let text = fs::read_to_string(path)
            .map_err(|e| tfmt!("读取路由文件失败: {}", "Failed to read router file: {}", e))?;
        let items = router::parse_router_yaml(&text)
            .map_err(|e| tfmt!("路由文件语法/规范错误: {}", "Router file syntax/spec error: {}", e))?;
        let base = path.parent().unwrap_or(Path::new("."));
        let mut entries: Vec<RouterEntry> = Vec::with_capacity(items.len());
        let mut invalid: Vec<(usize, String)> = Vec::new();
        for item in items {
            let p = if Path::new(&item.name).is_absolute() {
                PathBuf::from(&item.name)
            } else {
                base.join(&item.name)
            };
            let error = validate_router_entry(&p);
            let valid = error.is_empty();
            if !valid {
                invalid.push((item.line, format!("# [无效] {}: {}", item.name, error)));
            }
            entries.push(RouterEntry { path: p, valid, error });
        }
        // Invalid entries: write a marker comment at the matching position in the YAML (also
        // clearing stale markers to avoid accumulation).
        if !invalid.is_empty() {
            let rewritten = router::rewrite_with_markers(&text, &invalid);
            fs::write(path, rewritten).map_err(|e| tfmt!("写入无效标记注释失败: {}", "Failed to write invalid marker comment: {}", e))?;
        }
        self.router_files = entries;
        self.router_path = Some(path.to_path_buf());
        // Auto-load the first valid config; failure only sets a status, doesn't block (the
        // router itself is already loaded).
        if let Some(first_valid) = self.router_files.iter().position(|e| e.valid) {
            self.router_index = first_valid;
            let p = self.router_files[first_valid].path.clone();
            if let Err(e) = self.apply_config_path(&p) {
                self.status = Some(StatusMsg {
                    text: tfmt!(
                        "已载入路由但加载 #{} 失败: {}",
                        "Router loaded but failed to load #{}: {}",
                        first_valid + 1,
                        e
                    ),
                    kind: MsgKind::Err,
                });
            }
        }
        self.save_session();
        Ok(())
    }

    /// Quick-switch router dialog: pick `zwtcfg_router.yaml` then go through `load_router_path`.
    fn load_router_dialog(&mut self) {
        let file = rfd::FileDialog::new()
            .set_title(tr("载入路由配置", "Load Router Config"))
            .set_file_name("zwtcfg_router.yaml")
            .pick_file();
        let Some(path) = file else {
            self.status = Some(StatusMsg {
                text: tr("已取消载入路由", "Router load cancelled").into(),
                kind: MsgKind::Info,
            });
            return;
        };
        match self.load_router_path(&path) {
            Ok(()) => {
                let n_total = self.router_files.len();
                let n_invalid = self.router_files.iter().filter(|e| !e.valid).count();
                self.status = Some(StatusMsg {
                    text: tfmt!(
                        "已载入路由 {} ({} 个配置，{} 个无效)",
                        "Router loaded {} ({} configs, {} invalid)",
                        path.display(),
                        n_total,
                        n_invalid
                    ),
                    kind: if n_invalid == 0 { MsgKind::Ok } else { MsgKind::Info },
                });
            }
            Err(e) => {
                self.status = Some(StatusMsg {
                    text: tfmt!("载入路由失败: {}", "Router load failed: {}", e),
                    kind: MsgKind::Err,
                });
            }
        }
    }

    /// Quick-switch to the next valid router config (cycling the list in order).
    /// Skips invalid entries one by one; a file deleted/corrupted after load is re-validated on
    /// the fly and skipped too.
    fn router_next(&mut self) {
        if self.router_files.is_empty() {
            self.status = Some(StatusMsg {
                text: tr(
                    "未载入路由 (zwtcfg_router.yaml)，请先在操作中载入",
                    "No router loaded (zwtcfg_router.yaml); load one from Operations first",
                )
                .into(),
                kind: MsgKind::Err,
            });
            return;
        }
        let n = self.router_files.len();
        let mut attempts = 0;
        let mut idx = self.router_index;
        while attempts < n {
            idx = (idx + 1) % n;
            attempts += 1;
            let path = self.router_files[idx].path.clone();
            match config::load_from_path(&path) {
                Ok(cfg) => {
                    self.router_files[idx].valid = true;
                    self.router_files[idx].error = String::new();
                    self.router_index = idx;
                    self.cfg = cfg;
                    self.config_source = Some(path.clone());
                    self.note_config_source();
                    self.touch_config_mtime();
                    self.sync_engine();
                    self.save_session();
                    self.status = Some(StatusMsg {
                        text: tfmt!(
                            "速切: {}/{} → {}",
                            "Quick-switch: {}/{} → {}",
                            idx + 1,
                            n,
                            path.display()
                        ),
                        kind: MsgKind::Ok,
                    });
                    return;
                }
                Err(e) => {
                    // Invalid: skip (marked red; if it was valid before, update it to invalid).
                    self.router_files[idx].valid = false;
                    self.router_files[idx].error = e;
                }
            }
        }
        self.status = Some(StatusMsg {
            text: tr(
                "速切失败: 路由中所有配置均无效",
                "Quick-switch failed: all router configs are invalid",
            )
            .into(),
            kind: MsgKind::Err,
        });
    }

    fn suspend_tui(&self) {
        disable_raw_mode().ok();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            Show
        );
    }

    fn resume_tui(&self) {
        let _ = execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            Hide
        );
        let _ = enable_raw_mode();
    }

    // ---- Field get/set ----

    fn set_binding(&mut self, field: usize, name: String) {
        match field {
            F_WEAPON1 => self.cfg.weapon_slot1 = name,
            F_WEAPON2 => self.cfg.weapon_slot2 = name,
            F_WEAPON3 => self.cfg.weapon_slot3 = name,
            F_EXEC_KEY => self.cfg.execution_hotkey = name,
            F_ROUTER => self.cfg.router_hotkey = name,
            _ => {}
        }
    }

    fn numeric_value(&self, field: usize) -> u64 {
        match field {
            F_SWITCH => self.cfg.weapon_switch_interval_ms,
            F_SWITCH_OFF => self.cfg.weapon_switch_interval_offset_ms,
            F_SHOOT => self.cfg.shoot_interval_ms,
            F_SHOOT_OFF => self.cfg.shoot_interval_offset_ms,
            _ => 0,
        }
    }

    fn set_numeric(&mut self, field: usize, v: u64) {
        match field {
            F_SWITCH => self.cfg.weapon_switch_interval_ms = v,
            F_SWITCH_OFF => self.cfg.weapon_switch_interval_offset_ms = v,
            F_SHOOT => self.cfg.shoot_interval_ms = v,
            F_SHOOT_OFF => self.cfg.shoot_interval_offset_ms = v,
            _ => {}
        }
    }

    fn field(&self, fi: usize) -> (&'static str, String, FieldKind) {
        match fi {
            F_WEAPON1 => (
                tr("武器槽#1绑定", "Weapon Slot #1"),
                self.cfg.weapon_slot1.clone(),
                FieldKind::Binding,
            ),
            F_WEAPON2 => (
                tr("武器槽#2绑定", "Weapon Slot #2"),
                self.cfg.weapon_slot2.clone(),
                FieldKind::Binding,
            ),
            F_WEAPON3 => (
                tr("武器槽#3绑定", "Weapon Slot #3"),
                self.cfg.weapon_slot3.clone(),
                FieldKind::Binding,
            ),
            F_EXEC_KEY => (
                tr("执行热键绑定", "Execution Hotkey"),
                self.cfg.execution_hotkey.clone(),
                FieldKind::Binding,
            ),
            F_ROUTER => (
                tr("速切配置热键", "Quick-switch Hotkey"),
                self.cfg.router_hotkey.clone(),
                FieldKind::Binding,
            ),
            F_ORDER => (
                tr("执行集合", "Execution Set"),
                self.cfg.execution_order.clone(),
                FieldKind::Order,
            ),
            F_MODE => (
                tr("执行方式", "Execution Mode"),
                if self.cfg.execution_mode == ExecutionMode::Hold {
                    tr("长按", "Hold").to_string()
                } else {
                    tr("切换", "Toggle").to_string()
                },
                FieldKind::Toggle,
            ),
            F_RANDOM => (
                tr("乱序执行", "Random Order"),
                if self.cfg.random_execution {
                    tr("开", "ON").to_string()
                } else {
                    tr("关", "OFF").to_string()
                },
                FieldKind::Toggle,
            ),
            F_SWITCH => (
                tr("切换武器间隔(ms)", "Switch Weapon Interval (ms)"),
                self.cfg.weapon_switch_interval_ms.to_string(),
                FieldKind::Number,
            ),
            F_SWITCH_OFF => (
                tr("切换武器间隔偏移区间(ms)", "Switch Weapon Jitter (ms)"),
                self.cfg.weapon_switch_interval_offset_ms.to_string(),
                FieldKind::Number,
            ),
            F_SHOOT => (
                tr("射击间隔(ms)", "Shoot Interval (ms)"),
                self.cfg.shoot_interval_ms.to_string(),
                FieldKind::Number,
            ),
            F_SHOOT_OFF => (
                tr("射击间隔偏移区间(ms)", "Shoot Jitter (ms)"),
                self.cfg.shoot_interval_offset_ms.to_string(),
                FieldKind::Number,
            ),
            A_EXPORT => (tr("导出配置", "Export Config"), String::new(), FieldKind::Action),
            A_LOAD => (tr("读取配置", "Load Config"), String::new(), FieldKind::Action),
            A_ROUTER => (tr("载入路由", "Load Router"), String::new(), FieldKind::Action),
            // Language switch: shows the *opposite* language (Chinese UI → `Switch Language`,
            // English UI → `语言切换`).
            A_LANG => (tr("Switch Language", "语言切换"), String::new(), FieldKind::Action),
            _ => unreachable!(),
        }
    }

    fn is_active(&self, fi: usize) -> bool {
        match &self.mode {
            Mode::Normal => self.focus == fi,
            Mode::Capture { field } => *field == fi,
            Mode::Edit { field, .. } => *field == fi,
            Mode::OrderEdit { field, .. } => *field == fi,
        }
    }

    fn is_capture(&self, fi: usize) -> bool {
        matches!(&self.mode, Mode::Capture { field } if *field == fi)
    }

    fn field_span(&self, fi: usize) -> Span<'static> {
        let active = self.is_active(fi);
        let (label, mut value, kind) = self.field(fi);

        let style = if active {
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };

        let text = match kind {
            FieldKind::Binding => {
                if self.is_capture(fi) {
                    value = tr(
                        "按下一个按键或鼠标键...",
                        "Press a key or mouse button...",
                    )
                    .to_string();
                } else if value.is_empty() {
                    value = tr("未绑定", "Unbound").to_string();
                }
                format!("{label}: {value}")
            }
            FieldKind::Number => {
                if let Mode::Edit { field, buf } = &self.mode {
                    if *field == fi {
                        value = format!("{buf}▌");
                    }
                }
                // Offset fields get a ± prefix automatically, so editing only needs the number.
                if is_offset_field(fi) {
                    value = format!("±{value}");
                }
                format!("{label}: {value}")
            }
            FieldKind::Order => {
                if let Mode::OrderEdit { field, buf } = &self.mode {
                    if *field == fi {
                        value = format!("{buf}▌");
                    }
                }
                format!("{label}: {value}")
            }
            FieldKind::Toggle => format!("{label}: {value}"),
            FieldKind::Action => format!("[ {label} ]"),
        };
        Span::styled(text, style)
    }
}

/// Whether a field is an offset-range field (displayed with an automatic ± prefix).
fn is_offset_field(fi: usize) -> bool {
    matches!(fi, F_SWITCH_OFF | F_SHOOT_OFF)
}

/// Validate a router entry: the file must exist and parse as a valid config.
/// Empty string means valid; otherwise returns the reason for display / marker comment.
fn validate_router_entry(path: &Path) -> String {
    if !path.exists() {
        return tr("文件不存在", "File does not exist").into();
    }
    match config::load_from_path(path) {
        Ok(_) => String::new(),
        Err(e) => e,
    }
}

/// Validate an execution order: 1–3 chars, each one of A/B/C, each at most once.
/// `A`/`AB`/`ABC` mean single weapon, two-weapon rotation, and three-weapon cycle.
fn is_valid_order(s: &str) -> bool {
    if s.is_empty() || s.len() > 3 {
        return false;
    }
    let mut seen = [false; 3];
    for c in s.bytes() {
        let idx = match c {
            b'A' => 0,
            b'B' => 1,
            b'C' => 2,
            _ => return false,
        };
        if seen[idx] {
            return false;
        }
        seen[idx] = true;
    }
    true
}

fn mouse_button_name(btn: MouseButton) -> String {
    match btn {
        MouseButton::Left => "MB1".to_string(),
        MouseButton::Right => "MB2".to_string(),
        MouseButton::Middle => "MB3".to_string(),
    }
}

/// Render several rows of fields into a `Line` list.
fn rows_to_lines(rows: &[&[usize]], app: &App) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for row in rows {
        let mut spans: Vec<Span> = Vec::new();
        for &fi in *row {
            spans.push(app.field_span(fi));
            spans.push(Span::raw("    "));
        }
        if !spans.is_empty() {
            spans.pop();
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn ui(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(9), // banner (6 rows art + 1 repo URL + top/bottom borders)
        Constraint::Length(5), // key bindings (2 rows)
        Constraint::Length(5), // execution order
        Constraint::Length(6), // timing config (2 rows + note)
        Constraint::Length(3), // operations
        Constraint::Min(3),    // help/status (operation hint + status & config file)
    ])
        .split(area);

    // Title banner: smooth green→blue gradient art (no wrap) + the GitHub repo URL line below.
    const GRADIENT: [Color; 6] = [
        Color::Green,
        Color::Green,
        Color::Cyan,
        Color::Cyan,
        Color::Blue,
        Color::Blue,
    ];
    let mut banner_lines: Vec<Line> = Vec::new();
    for (i, row) in BANNER.iter().enumerate() {
        banner_lines.push(Line::styled(*row, Style::default().fg(GRADIENT[i])));
    }
    let decl_style = Style::default().fg(Color::DarkGray);
    banner_lines.push(Line::styled(TOP_BAR, decl_style));
    f.render_widget(
        Paragraph::new(banner_lines).block(Block::bordered()),
        chunks[0],
    );

    // Key bindings.
    f.render_widget(
        Paragraph::new(rows_to_lines(&ROWS[0..2], app)).block(Block::bordered().title(
            tr(" 按键绑定 ", " Key Bindings "),
        )),
        chunks[1],
    );

    // Execution order set.
    let mut order_lines = rows_to_lines(&ROWS[2..3], app);
    order_lines.push(Line::styled(
        tr(
            "A=武器槽#1  B=武器槽#2  C=武器槽#3    集合元素示例: A=#1单例  AB=#1+#2轮换  ABC=#1+#2+#3循环",
            "A=Slot#1  B=Slot#2  C=Slot#3    Set examples: A=single#1  AB=#1+#2 alternate  ABC=#1+#2+#3 cycle",
        ),
        Style::default().fg(Color::DarkGray),
    ));
    // When the order set is focused, show a per-mode warning; when the mode field is focused,
    // show that mode's description.
    if app.is_active(F_ORDER) {
        let warn = if app.cfg.random_execution {
            tr(
                "[!] 乱序执行模式下将基于所选集合元素打乱随机排列执行",
                "[!] Random-order mode shuffles the selected set elements",
            )
        } else {
            tr(
                "[!] 正序执行模式下将基于所选集合元素自左向右循环执行",
                "[!] Sequential mode cycles the selected set elements left-to-right",
            )
        };
        order_lines.push(Line::styled(
            warn,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    } else if app.is_active(F_MODE) {
        let hint = match app.cfg.execution_mode {
            ExecutionMode::Hold => tr(
                "长按模式需要持续按住热键才可执行操作",
                "Hold mode requires you to keep holding the hotkey to run",
            ),
            ExecutionMode::Toggle => tr(
                "切换模式仅需点击热键即可实现启动与停止操作",
                "Toggle mode only takes a tap of the hotkey to start or stop",
            ),
        };
        order_lines.push(Line::styled(
            hint,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(
        Paragraph::new(order_lines).block(Block::bordered().title(
            tr(" 执行顺序 ", " Execution Order "),
        )),
        chunks[2],
    );

    // Timing config.
    let mut cfg_lines = rows_to_lines(&ROWS[3..5], app);
    cfg_lines.push(Line::styled(
        tr(
            "切换武器间隔: 定时自动切枪 | 射击间隔: 右键连点器 (每个 ±N 随机)",
            "Switch interval: timed auto weapon-switch | Shoot interval: right-click autoclicker (each ±N random)",
        ),
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(
        Paragraph::new(cfg_lines).block(Block::bordered().title(
            tr(" 时间配置(ms) ", " Timing Config (ms) "),
        )),
        chunks[3],
    );

    // Operations.
    f.render_widget(
        Paragraph::new(rows_to_lines(&ROWS[5..6], app)).block(Block::bordered().title(
            tr(" 操作 ", " Operations "),
        )),
        chunks[4],
    );

    // Help / status.
    let mut help: Vec<Line> = Vec::new();
    help.push(Line::styled(
        tr(
            "操作: Tab/方向键 移动   Enter 编辑/确认/切换   绑定: 直接按键   执行热键: 开/停循环   Esc 退出",
            "Controls: Tab/Arrows move   Enter edit/confirm/toggle   Bind: press a key   Execution hotkey: start/stop   Esc quit",
        ),
        Style::default().fg(Color::DarkGray),
    ));
    // Engine state + current config file on the same line; when a router table is loaded, list
    // all configs and highlight the active one.
    let running = app.engine.running();
    let dim = Style::default().fg(Color::DarkGray);
    let hi = Style::default().fg(Color::Green).add_modifier(Modifier::BOLD);
    let mut status: Vec<Span> = vec![Span::styled(
        if running {
            tr("● 执行中", "● Running").to_string()
        } else {
            tr("○ 已停止", "○ Stopped").to_string()
        },
        Style::default()
            .fg(if running { Color::Green } else { Color::DarkGray })
            .add_modifier(Modifier::BOLD),
    )];
    if app.router_files.is_empty() {
        let name = app
            .config_source
            .as_ref()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| tr("默认配置", "default config").to_string());
        status.push(Span::styled(tr(" | 配置文件: ", " | Config: "), dim));
        status.push(Span::styled(name, Style::default().fg(Color::White)));
    } else {
        status.push(Span::styled(tr(" | 路由: ", " | Router: "), dim));
        for (i, entry) in app.router_files.iter().enumerate() {
            if i > 0 {
                status.push(Span::styled(" | ", dim));
            }
            let name = entry
                .path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| entry.path.display().to_string());
            // Invalid entries red; of the valid ones the active is highlighted, the rest dimmed.
            let style = if !entry.valid {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else if i == app.router_index {
                hi
            } else {
                dim
            };
            status.push(Span::styled(name, style));
        }
    }
    help.push(Line::from(status));
    f.render_widget(Paragraph::new(help), chunks[5]);
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> io::Result<()> {
    loop {
        // Publish the interaction flag to the global listener (capture/edit blocks the engine).
        app.interacting.store(app.mode != Mode::Normal, Ordering::Relaxed);

        terminal.draw(|f| ui(f, app))?;
        if event::poll(Duration::from_millis(200))? {
            let ev = event::read()?;
            let dlg = app.handle_event(ev);
            if let Some(dk) = dlg {
                app.suspend_tui();
                match dk {
                    DialogKind::Export => app.export_config_dialog(),
                    DialogKind::Load => app.load_config_dialog(),
                    DialogKind::Router => app.load_router_dialog(),
                }
                app.resume_tui();
                terminal.clear()?;
            }
        }
        // Config watcher daemon noticed an external rewrite → hot-reload (polling moved to the
        // watcher thread, no longer done in the main loop).
        while let Ok(msg) = app.msg_rx.try_recv() {
            match msg {
                EngineMsg::Toggled(on) => {
                    app.status = Some(StatusMsg {
                        text: if on {
                            tr(
                                "执行已开始 (切到游戏窗口生效)",
                                "Execution started (switch to the game window to take effect)",
                            )
                            .into()
                        } else {
                            tr("执行已停止", "Execution stopped").into()
                        },
                        kind: if on { MsgKind::Ok } else { MsgKind::Info },
                    });
                }
                EngineMsg::RouterNext => {
                    app.router_next();
                }
                EngineMsg::ConfigReload => {
                    app.hot_read();
                }
                EngineMsg::CaptureKey(k) => {
                    // Both capture mode and order-set editing are driven by globally-forwarded
                    // keys (rdev, incl. Alt).
                    let target = match &app.mode {
                        Mode::Capture { field } => Some((*field, true)),
                        Mode::OrderEdit { field, .. } => Some((*field, false)),
                        _ => None,
                    };
                    if let Some((field, is_capture)) = target {
                        if is_capture {
                            if k == rdev::Key::Escape {
                                app.mode = Mode::Normal;
                                app.status = None;
                            } else if let Some(name) = keymap::rdev_key_name(k) {
                                app.bind(field, name);
                            }
                        } else {
                            app.order_edit_key(field, k);
                        }
                    }
                }
            }
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, Hide)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    let res = run(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        Show
    )?;

    res
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn test_app() -> App {
        // UI tests assert Chinese strings, so pin the language to Chinese via a thread-local
        // override — this avoids interference from other parallel tests' global-language switches.
        Lang::test_set(Lang::Zh);
        let shared = Arc::new(Mutex::new(Config::default()));
        let (_tx, msg_rx) = mpsc::channel();
        App {
            cfg: Config::default(),
            shared: shared.clone(),
            engine: Arc::new(Engine::new(shared)),
            msg_rx,
            interacting: Arc::new(AtomicBool::new(false)),
            config_source: None,
            config_mtime: None,
            watched_path: Arc::new(Mutex::new(None)),
            router_path: None,
            session_path: std::env::temp_dir().join("zwt_test_state"),
            router_files: Vec::new(),
            router_index: 0,
            focus: 0,
            mode: Mode::Normal,
            status: None,
            should_quit: false,
        }
    }

    /// Build a "valid" router entry (test helper).
    fn entry(name: &str) -> RouterEntry {
        RouterEntry {
            path: PathBuf::from(name),
            valid: true,
            error: String::new(),
        }
    }

    fn is_cjk(c: char) -> bool {
        let cp = c as u32;
        matches!(
            cp,
            0x1100..=0x115F
                | 0x2E80..=0x303E
                | 0x3041..=0x33FF
                | 0x3400..=0x4DBF
                | 0x4E00..=0x9FFF
                | 0xA000..=0xA4CF
                | 0xAC00..=0xD7A3
                | 0xF900..=0xFAFF
                | 0xFE30..=0xFE4F
                | 0xFF00..=0xFF60
                | 0xFFE0..=0xFFE6
        )
    }

    /// Collect the buffer into per-line strings. Wide chars (CJK) take 2 columns and their
    /// continuation cell is a space; they're skipped so CJK doesn't get a space inserted between.
    fn buffer_rows(buf: &ratatui::buffer::Buffer) -> Vec<String> {
        let (w, h) = (buf.area.width as usize, buf.area.height as usize);
        (0..h)
            .map(|y| {
                let mut out = String::new();
                let mut skip_next = false;
                for x in 0..w {
                    if skip_next {
                        skip_next = false;
                        continue;
                    }
                    let Some(cell) = buf.cell((x as u16, y as u16)) else { continue };
                    let s = cell.symbol();
                    if s.is_empty() {
                        continue;
                    }
                    let ch = s.chars().next().unwrap();
                    out.push(ch);
                    if is_cjk(ch) {
                        skip_next = true;
                    }
                }
                out
            })
            .collect()
    }

    #[test]
    fn renders_full_ui_with_annotations() {
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = test_app();
        terminal.draw(|f| ui(f, &app)).unwrap();

        let rows = buffer_rows(terminal.backend().buffer());
        let text = rows.join("\n");
        assert!(text.contains(TOP_BAR), "open-source repo URL not rendered");
        // Creeper is gone; the config area renders normally.
        assert!(!text.contains('▄'), "Creeper not removed");
        assert!(text.contains("武器槽#1绑定"), "config area not rendered");
    }

    /// Status line: without a router it shows the current config file; with a router it lists
    /// all configs and keeps the active one.
    #[test]
    fn status_line_shows_config_and_router() {
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = test_app();

        // No router: shows the current config file.
        app.config_source = Some(PathBuf::from("zwtcfg.json"));
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = buffer_rows(terminal.backend().buffer()).join("\n");
        assert!(
            text.contains("○ 已停止 | 配置文件: zwtcfg.json"),
            "single config not shown: {text}"
        );

        // Router loaded: lists all configs.
        app.router_files = vec![
            entry("first.json"),
            entry("second.json"),
            entry("third.json"),
        ];
        app.router_index = 1;
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = buffer_rows(terminal.backend().buffer()).join("\n");
        assert!(text.contains("路由"), "路由标识未显示: {text}");
        assert!(
            text.contains("first.json")
                && text.contains("second.json")
                && text.contains("third.json"),
            "路由列表不完整: {text}"
        );
    }

    /// The quick-switch hotkey and the load-router action are rendered.
    #[test]
    fn router_field_and_action_render() {
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = test_app();
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = buffer_rows(terminal.backend().buffer()).join("\n");
        assert!(text.contains("速切配置热键: RALT"), "quick-switch default not rendered: {text}");
        assert!(text.contains("载入路由"), "load-router action not rendered: {text}");

        // After rebinding the quick-switch hotkey, the new key name is shown.
        app.cfg.router_hotkey = "G".into();
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = buffer_rows(terminal.backend().buffer()).join("\n");
        assert!(text.contains("速切配置热键: G"), "binding not rendered: {text}");
    }

    /// An invalid router entry renders red; valid entries render normally.
    #[test]
    fn router_invalid_entry_renders_red() {
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = test_app();
        app.router_files = vec![
            entry("ok.json"),
            RouterEntry {
                path: PathBuf::from("bad.json"),
                valid: false,
                error: "文件不存在".into(),
            },
        ];
        app.router_index = 0;
        terminal.draw(|f| ui(f, &app)).unwrap();
        let buf = terminal.backend().buffer();

        // Find the line containing "bad.json", then re-scan it to locate the buffer x of 'b'
        // (skipping CJK continuation cells).
        let rows = buffer_rows(buf);
        let row = rows
            .iter()
            .position(|r| r.contains("bad.json"))
            .expect("bad.json should appear on the status line");
        let mut text = String::new();
        let mut pos_of_b = None;
        let mut skip_next = false;
        for x in 0..buf.area.width {
            if skip_next {
                skip_next = false;
                continue;
            }
            let Some(cell) = buf.cell((x, row as u16)) else { continue };
            let s = cell.symbol();
            if s.is_empty() {
                continue;
            }
            let ch = s.chars().next().unwrap();
            text.push(ch);
            if text.ends_with("bad.json") {
                pos_of_b = Some(x - 7); // there are 7 chars after 'b'
            }
            if is_cjk(ch) {
                skip_next = true;
            }
        }
        let x = pos_of_b.expect("bad.json text not located");
        assert_eq!(
            buf.cell((x, row as u16)).unwrap().fg,
            Color::Red,
            "invalid entry should render red"
        );
    }

    /// Quick-switch skips invalid entries, cycles back to valid ones, and re-validates a file
    /// that changed since load.
    #[test]
    fn router_next_skips_invalid_and_cycles() {
        use std::fs;
        let dir = std::env::temp_dir().join("zwt_router_test");
        fs::create_dir_all(&dir).unwrap();
        let good1 = dir.join("good1.json");
        let bad = dir.join("bad.json"); // does not exist
        let good2 = dir.join("good2.json");
        fs::write(&good1, r#"{"execution_order":"A","execution_hotkey":"LALT"}"#).unwrap();
        fs::write(&good2, r#"{"execution_order":"B","execution_hotkey":"LALT"}"#).unwrap();

        let mut app = test_app();
        app.router_files = vec![
            RouterEntry {
                path: good1.clone(),
                valid: true,
                error: String::new(),
            },
            RouterEntry {
                path: bad.clone(),
                valid: false,
                error: "文件不存在".into(),
            },
            RouterEntry {
                path: good2.clone(),
                valid: true,
                error: String::new(),
            },
        ];
        app.router_index = 0;
        app.router_next(); // #1 → next valid: skip invalid #2, jump to #3
        assert_eq!(app.router_index, 2, "should skip invalid entries");
        assert_eq!(app.cfg.execution_order, "B", "should load #3's config");
        app.router_next(); // cycle back to #1
        assert_eq!(app.router_index, 0, "should cycle back to the first");
        assert_eq!(app.cfg.execution_order, "A", "应加载 #1 的配置");

        let _ = fs::remove_dir_all(&dir);
    }

    /// Hot-read: an external edit to the config source file (mtime change) → auto-reload.
    #[test]
    fn hot_read_reloads_external_change() {
        use std::fs;
        let dir = std::env::temp_dir().join("zwt_hotread_test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cfg.json");
        fs::write(&path, r#"{"execution_order":"A","execution_hotkey":"LALT"}"#).unwrap();

        let mut app = test_app();
        app.config_source = Some(path.clone());
        app.config_mtime = Some(SystemTime::UNIX_EPOCH); // simulate "externally rewritten"
        app.hot_read();
        assert_eq!(app.cfg.execution_order, "A", "hot-read should load external content");
        assert!(app.config_mtime.is_some(), "should record a new mtime");

        // Second external rewrite → hot-read again.
        fs::write(&path, r#"{"execution_order":"B","execution_hotkey":"LALT"}"#).unwrap();
        app.config_mtime = Some(SystemTime::UNIX_EPOCH); // force "changed"
        app.hot_read();
        assert_eq!(app.cfg.execution_order, "B", "second hot-read should update");

        let _ = fs::remove_dir_all(&dir);
    }

    /// Hot-read: when mtime didn't change, don't reload (avoids overwriting an in-memory edit
    /// that has drifted from the disk).
    #[test]
    fn hot_read_ignores_unchanged_file() {
        use std::fs;
        let dir = std::env::temp_dir().join("zwt_hotread_unchanged");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cfg.json");
        fs::write(&path, r#"{"execution_order":"A","execution_hotkey":"LALT"}"#).unwrap();

        let mut app = test_app();
        app.config_source = Some(path.clone());
        app.config_mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
        app.cfg.execution_order = "C".into(); // in-memory drifted from disk
        app.hot_read();
        assert_eq!(app.cfg.execution_order, "C", "unchanged mtime should not reload");

        let _ = fs::remove_dir_all(&dir);
    }

    /// Hot-read: external corrupt content must not overwrite the current config.
    #[test]
    fn hot_read_keeps_current_on_corrupt() {
        use std::fs;
        let dir = std::env::temp_dir().join("zwt_hotread_corrupt");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cfg.json");
        fs::write(&path, r#"{"execution_order":"A"}"#).unwrap();

        let mut app = test_app();
        app.config_source = Some(path.clone());
        app.config_mtime = Some(SystemTime::UNIX_EPOCH);
        app.cfg.execution_order = "ABC".into(); // in-memory value
        fs::write(&path, "{not valid json}").unwrap();
        app.hot_read();
        assert_eq!(app.cfg.execution_order, "ABC", "corrupt file should not overwrite");

        let _ = fs::remove_dir_all(&dir);
    }

    /// Hot-save: after an edit-confirm, write to the **config file in use** (`config_source`),
    /// not a hard-coded `zwtcfg.json`.
    #[test]
    fn auto_save_writes_to_current_source() {
        use std::fs;
        let dir = std::env::temp_dir().join("zwt_hotsave_test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("custom.json");
        fs::write(&path, r#"{"execution_order":"A","execution_hotkey":"LALT"}"#).unwrap();

        let mut app = test_app();
        app.config_source = Some(path.clone());
        app.cfg.execution_order = "ABC".into();
        app.cfg.execution_hotkey = "F6".into();
        app.auto_save();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("ABC"), "save target should be config_source: {text}");
        assert!(text.contains("F6"), "save target should be config_source: {text}");

        let _ = fs::remove_dir_all(&dir);
    }

    /// The execution-mode field renders: default `Toggle` (切换), shows `Hold` (长按) after
    /// toggling, and shows the matching hint when focused.
    #[test]
    fn execution_mode_field_renders_with_hint() {
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = test_app();
        app.focus = F_MODE;
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = buffer_rows(terminal.backend().buffer()).join("\n");
        assert!(text.contains("执行方式: 切换"), "default mode not rendered: {text}");
        assert!(text.contains("切换模式"), "toggle hint not rendered: {text}");

        app.cfg.execution_mode = ExecutionMode::Hold;
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = buffer_rows(terminal.backend().buffer()).join("\n");
        assert!(text.contains("执行方式: 长按"), "hold not rendered: {text}");
        assert!(text.contains("长按模式"), "hold hint not rendered: {text}");
    }

    #[test]
    fn banner_art_unified() {
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = test_app();
        terminal.draw(|f| ui(f, &app)).unwrap();
        let rows = buffer_rows(terminal.backend().buffer());
        // The first art line (after stripping the left border) should contain the banner's top
        // row signature string.
        let art_row = rows[1].trim_start_matches('│');
        assert!(
            art_row.contains("_____") && art_row.contains("_____           _"),
            "banner art line wrong: {:?}",
            art_row
        );
    }

    /// Load router: parse → validate → apply the first valid config, and persist the session.
    #[test]
    fn load_router_path_loads_and_applies_first_valid() {
        use std::fs;
        let dir = std::env::temp_dir().join("zwt_load_router_test");
        fs::create_dir_all(&dir).unwrap();
        let cfg1 = dir.join("one.json");
        let cfg2 = dir.join("two.json");
        fs::write(&cfg1, r#"{"execution_order":"A","execution_hotkey":"LALT"}"#).unwrap();
        fs::write(&cfg2, r#"{"execution_order":"B","execution_hotkey":"LALT"}"#).unwrap();
        let yaml = dir.join("router.yaml");
        fs::write(&yaml, "config:\n  - one.json\n  - two.json\n").unwrap();

        let mut app = test_app();
        app.session_path = dir.join("state.json"); // isolate the state file per test, so parallel tests don't write the same temp file
        app.load_router_path(&yaml).unwrap();

        assert_eq!(app.router_path.as_deref(), Some(yaml.as_path()));
        assert_eq!(app.router_files.len(), 2);
        assert_eq!(app.router_index, 0, "should highlight the first valid entry");
        assert_eq!(app.cfg.execution_order, "A", "should auto-load the first valid config");
        assert_eq!(app.config_source.as_deref(), Some(cfg1.as_path()));

        // Session persisted: records the router and the current config.
        let state = session::load_from(&app.session_path).expect("session should be written");
        assert_eq!(state.last_router.as_deref(), Some(yaml.to_str().unwrap()));
        assert_eq!(state.last_config.as_deref(), Some(cfg1.to_str().unwrap()));
        assert_eq!(state.router_index, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    /// Load router: invalid entries get a `# [无效]` marker in the YAML, and the first valid
    /// config is still applied.
    #[test]
    fn load_router_path_writes_invalid_markers() {
        use std::fs;
        let dir = std::env::temp_dir().join("zwt_router_markers");
        fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.json");
        fs::write(&good, r#"{"execution_order":"A"}"#).unwrap();
        let yaml = dir.join("router.yaml");
        fs::write(&yaml, "config:\n  - good.json\n  - missing.json\n").unwrap();

        let mut app = test_app();
        app.session_path = dir.join("state.json");
        app.load_router_path(&yaml).unwrap();

        assert_eq!(app.router_files.len(), 2);
        assert!(app.router_files[0].valid);
        assert!(!app.router_files[1].valid, "missing file should be marked invalid");
        assert_eq!(app.router_index, 0);

        let text = fs::read_to_string(&yaml).unwrap();
        assert!(text.contains("# [无效]"), "should write an invalid marker: {text}");
        assert!(text.contains("missing.json"), "marker should point at the missing file: {text}");

        let _ = fs::remove_dir_all(&dir);
    }

    /// Load router: a YAML syntax/spec error → `Err` and the existing router is kept (the old
    /// table is reused).
    #[test]
    fn load_router_path_spec_error_keeps_old_router() {
        use std::fs;
        let dir = std::env::temp_dir().join("zwt_router_spec_err");
        fs::create_dir_all(&dir).unwrap();
        let yaml = dir.join("router.yaml");
        fs::write(&yaml, "config: a.json\nconfig: b.json\n").unwrap(); // duplicate config key

        let mut app = test_app();
        app.router_files = vec![entry("old.json")];
        app.router_path = Some(PathBuf::from("old.yaml"));

        let err = app.load_router_path(&yaml).unwrap_err();
        assert!(err.contains("重复"), "error should state the reason: {err}");
        assert_eq!(app.router_files.len(), 1, "spec error must not modify the old router");
        assert_eq!(app.router_path.as_deref(), Some(Path::new("old.yaml")));

        let _ = fs::remove_dir_all(&dir);
    }

    /// Session restore: reloads the router table, restores the last-used config, and aligns the
    /// router highlight to that config.
    #[test]
    fn restore_with_state_restores_router_and_config() {
        use std::fs;
        let dir = std::env::temp_dir().join("zwt_restore_test");
        fs::create_dir_all(&dir).unwrap();
        let cfg1 = dir.join("one.json");
        let cfg2 = dir.join("two.json");
        fs::write(&cfg1, r#"{"execution_order":"A"}"#).unwrap();
        fs::write(&cfg2, r#"{"execution_order":"B"}"#).unwrap();
        let yaml = dir.join("router.yaml");
        fs::write(&yaml, "config:\n  - one.json\n  - two.json\n").unwrap();

        let mut app = test_app();
        app.session_path = dir.join("state.json");
        let state = session::SessionState {
            last_config: Some(cfg2.to_str().unwrap().to_string()),
            last_router: Some(yaml.to_str().unwrap().to_string()),
            router_index: 0, // recorded highlight differs from config → align to last_config
            language: None,
        };
        app.restore_with_state(state);

        assert_eq!(app.router_files.len(), 2, "router should auto-reload");
        assert_eq!(app.config_source.as_deref(), Some(cfg2.as_path()), "should restore the last config");
        assert_eq!(app.cfg.execution_order, "B");
        assert_eq!(app.router_index, 1, "highlight should align to the restored config");
    }

    /// Session restore: when the last config was deleted, fall back to the first valid router
    /// entry without crashing.
    #[test]
    fn restore_with_state_falls_back_when_config_missing() {
        use std::fs;
        let dir = std::env::temp_dir().join("zwt_restore_fallback");
        fs::create_dir_all(&dir).unwrap();
        let cfg1 = dir.join("one.json");
        fs::write(&cfg1, r#"{"execution_order":"A"}"#).unwrap();
        let yaml = dir.join("router.yaml");
        fs::write(&yaml, "config:\n  - one.json\n").unwrap();

        let mut app = test_app();
        app.session_path = dir.join("state.json");
        let state = session::SessionState {
            last_config: Some(dir.join("gone.json").to_str().unwrap().to_string()),
            last_router: Some(yaml.to_str().unwrap().to_string()),
            router_index: 0,
            language: None,
        };
        app.restore_with_state(state);

        assert_eq!(app.config_source.as_deref(), Some(cfg1.as_path()), "应回退到路由第一有效");
        assert_eq!(app.router_index, 0);
        assert_eq!(app.cfg.execution_order, "A");
    }

    /// Startup restore integration: read the state from the session file and auto-reload the
    /// router / apply the config.
    #[test]
    fn restore_session_reads_state_file() {
        use std::fs;
        let dir = std::env::temp_dir().join("zwt_restore_session");
        fs::create_dir_all(&dir).unwrap();
        let cfg1 = dir.join("one.json");
        fs::write(&cfg1, r#"{"execution_order":"A"}"#).unwrap();
        let yaml = dir.join("router.yaml");
        fs::write(&yaml, "config:\n  - one.json\n").unwrap();

        let mut app = test_app();
        let state_path = dir.join("state.json");
        app.session_path = state_path.clone();
        let state = session::SessionState {
            last_config: Some(cfg1.to_str().unwrap().to_string()),
            last_router: Some(yaml.to_str().unwrap().to_string()),
            router_index: 0,
            language: None,
        };
        session::save_to(&state_path, &state).unwrap();

        app.restore_session(None);

        assert_eq!(app.router_files.len(), 1, "startup should auto-reload the router");
        assert_eq!(app.config_source.as_deref(), Some(cfg1.as_path()));
    }

    /// The language-switch entry renders the *opposite* language: current Chinese shows
    /// `Switch Language`, current English shows `语言切换`.
    #[test]
    fn language_action_renders_cross_lang_label() {
        let backend = TestBackend::new(140, 36);
        let mut terminal = Terminal::new(backend).unwrap();

        // Current Chinese → button shows English (the target language).
        Lang::test_set(Lang::Zh);
        let app = test_app();
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = buffer_rows(terminal.backend().buffer()).join("\n");
        assert!(text.contains("[ Switch Language ]"), "Chinese UI should show the English target: {text}");

        // Current English → button shows Chinese.
        Lang::test_set(Lang::En);
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = buffer_rows(terminal.backend().buffer()).join("\n");
        assert!(text.contains("[ 语言切换 ]"), "English UI should show the Chinese target: {text}");

        Lang::test_clear();
    }

    /// After switching, the (thread-local) language is flipped and the choice is persisted to
    /// the session file (the `language` field is updated).
    #[test]
    fn switch_language_toggles_and_persists() {
        let mut app = test_app();
        let dir = std::env::temp_dir().join("zwt_lang_persist");
        std::fs::create_dir_all(&dir).unwrap();
        app.session_path = dir.join("state.json");

        // Switch from Chinese to English.
        Lang::test_set(Lang::Zh);
        app.switch_language();
        assert_eq!(Lang::get(), Lang::En, "should switch to English");
        let state = session::load_from(&app.session_path).expect("session should be written");
        assert_eq!(state.language, Some(Lang::En), "session should record English");

        // Switch back to Chinese.
        app.switch_language();
        assert_eq!(Lang::get(), Lang::Zh, "should switch back to Chinese");
        let state = session::load_from(&app.session_path).expect("session should be written");
        assert_eq!(state.language, Some(Lang::Zh), "session should record Chinese");

        let _ = std::fs::remove_dir_all(&dir);
        Lang::test_clear();
    }
}