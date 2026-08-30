//! ZombiesWeaponTool —— 基于 ratatui/crossterm 的终端 TUI。
//!
//! 一个为「僵尸模式 / BO 类」第一人称射击游戏辅助的**全局键鼠宏工具**：
//! 监听全局执行热键，按配置循环模拟「自动切枪 + 右键连点」的组合操作序列，
//! 支持三武器槽、执行顺序集合、长按/切换模式、乱序、时间抖动、速切配置路由、
//! 配置热读取/热保存与会话恢复。
//!
//! ## 模块职责
//! - [`config`]：配置结构体、JSON 读写与校验；
//! - [`engine`]：rdev 全局监听 + 模拟输入执行循环；
//! - [`keymap`]：绑定名（可读字符串）↔ rdev 键/鼠标 的映射与模拟；
//! - [`router`]：`zwtcfg_router.yaml` 轻量解析 + 无效条目标记；
//! - [`session`]：会话状态（上次配置/路由）持久化；
//! - [`lang`]：中/英文国际化。
//!
//! ## 语言
//! 默认**英文**；可在「操作」行的 `Switch Language` / `语言切换` 项切换为中文，
//! 选择持久化到会话文件 `.zwt`，下次启动自动恢复。也支持 `--lang zh|en` /
//! `ZWT_LANG=zh|en` 显式覆盖（见 [`lang::init`]、[`lang::Lang::toggle`]）。

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

/// 标题 art：ZombiesWeaponTool 完整一体 ASCII 字符画，使用 raw string 避免反斜杠转义问题。
const BANNER: [&str; 6] = [
    r#"  _____               _     _         __        __                         _____           _ "#,
    r#" |__  /___  _ __ ___ | |__ (_) ___  __\ \      / /__  __ _ _ __   ___  _ _|_   _|__   ___ | |"#,
    r#"   / // _ \| '_ ` _ \| '_ \| |/ _ \/ __\ \ /\ / / _ \/ _` | '_ \ / _ \| '_ \| |/ _ \ / _ \| |"#,
    r#"  / /| (_) | | | | | | |_) | |  __/\__ \\ V  V /  __/ (_| | |_) | (_) | | | | | (_) | (_) | |"#,
    r#" /____\___/|_| |_| |_|_.__/|_|\___||___/ \_/\_/  \__|\__,_| .__/ \___/|_| |_|_|\___/ \___/|_|"#,
    r#"                                                          |_|                                "#,
];

/// GitHub 开源仓库地址：显示在标题横幅底行。
const TOP_BAR: &str = "© GongSunFangYun | https://github.com/GongSunFangYun/ZombiesWeaponTool";

/// 字段总数：5 绑定 + 1 顺序 + 1 方式 + 1 开关 + 4 数字 + 4 动作
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

/// 每一行的字段索引。绑定2行（武器槽一行/热键一行）/ 顺序1行 / 时间2行 / 动作1行。
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
    /// 正常导航
    Normal,
    /// 按键捕获中（绑定字段）
    Capture { field: usize },
    /// 数字编辑中（带输入缓冲）
    Edit { field: usize, buf: String },
    /// 执行顺序编辑中（只允许 ABCD 各一次）
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

/// 路由条目：配置路径 + 逐条校验结果。
/// 无效条目（文件不存在 / JSON 语法或规范错误）在状态行标红，速切时跳过。
struct RouterEntry {
    path: PathBuf,
    valid: bool,
    error: String,
}

struct App {
    cfg: Config,
    /// 供引擎线程读取的共享配置副本
    shared: Arc<Mutex<Config>>,
    engine: Arc<Engine>,
    msg_rx: mpsc::Receiver<EngineMsg>,
    /// 是否处于捕获/编辑模式（共享给全局监听线程，避免误触引擎）
    interacting: Arc<AtomicBool>,
    /// 当前配置来源文件（用于导出默认名、热保存写入目标、热读取监控对象）
    config_source: Option<PathBuf>,
    /// 当前配置源文件的修改时间（热读取：对比外部修改并自动重新加载）
    config_mtime: Option<SystemTime>,
    /// 监听守护线程共享的「当前被监听路径」；`config_source` 变化时同步更新（见 note_config_source）
    watched_path: WatchedPath,
    /// 已载入路由 yaml 的路径（供会话持久化）
    router_path: Option<PathBuf>,
    /// 会话状态文件路径（默认 ~/.zwt，测试注入临时路径）
    session_path: PathBuf,
    /// 速切路由：载入 zwtcfg_router.yaml 后解析出的配置条目（含校验结果）
    router_files: Vec<RouterEntry>,
    /// 当前路由位置（按列表顺序循环）
    router_index: usize,
    focus: usize,
    mode: Mode,
    #[allow(dead_code)]
    status: Option<StatusMsg>,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        // 显式运行时语言覆盖（--lang / ZWT_LANG）；有则优先，会话恢复不再改语言
        let lang_override = lang::init();
        let shared = Arc::new(Mutex::new(Config::default()));
        let engine = Arc::new(Engine::new(shared.clone()));
        let interacting = Arc::new(AtomicBool::new(false));
        let watched_path: WatchedPath = Arc::new(Mutex::new(None));
        let (tx, msg_rx) = mpsc::channel();
        let _listener = engine::start_listener(engine.clone(), interacting.clone(), tx.clone());
        // 配置监听守护线程：发现配置文件被外部改写即通知主线程热重载（句柄丢弃 → daemon）
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
        // 会话恢复：应用持久化语言 + 重载上次路由表并应用上次使用的配置；无状态/损坏则走默认流程
        app.restore_session(lang_override);
        // 没有加载/恢复文案时，给出初始操作提示（用最终确定后的语言渲染，避免语言被
        // 会话覆盖后首帧提示仍是旧语言）
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
        // 记录当前配置源 mtime（供热读取对比），并把配置同步给引擎线程
        app.touch_config_mtime();
        // 把当前配置源同步给监听线程（此时可能已由会话恢复设置了路径）
        app.note_config_source();
        *app.shared.lock().unwrap() = app.cfg.clone();
        app
    }

    /// 启动会话恢复：读会话状态文件，重载上次路由表并应用上次使用的配置。
    /// 无状态/损坏 → 走默认配置流程；记录的文件被删 → 回退链（路由第一有效 → zwtcfg.json）。
    ///
    /// 同时恢复上次使用的语言，优先级：`lang_override`（--lang / ZWT_LANG）>
    /// 会话记录 `.zwt` > 默认英文。
    fn restore_session(&mut self, lang_override: Option<Lang>) {
        let Some(state) = session::load_from(&self.session_path) else {
            self.load_default_config();
            return;
        };
        // 语言：显式运行时覆盖优先；否则恢复会话中上次的选择
        if lang_override.is_none() {
            if let Some(l) = state.language {
                Lang::set(l);
            }
        }
        self.restore_with_state(state);
    }

    /// 按给定会话状态恢复（核心恢复逻辑，测试注入状态）。
    fn restore_with_state(&mut self, state: session::SessionState) {
        // 1. 自动重载上次路由表（yaml 缺失/损坏则跳过，仅报状态）
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
        // 2. 应用当前配置（优先级：上次配置 → 路由第一有效 → 默认流程）
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
        // 3. 路由高亮与激活配置对齐；否则恢复记录下标（越界/无效回退第一有效）
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
        // 4. 持久化恢复后的状态（幂等）
        self.save_session();
    }

    /// 默认配置流程：zwtcfg.json 存在则加载，否则基于当前配置生成。
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
                    // 加载不写盘（磁盘仅在编辑确认时写入），引擎同步由 new() 末尾统一完成
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
                    // 生成的默认配置即当前配置源（热保存写入、热读取监控的对象）
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

    /// 持久化会话状态到状态文件：记录当前配置源、路由 yaml 与路由位置。
    /// 失败仅设状态消息，不阻断流程。
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
        // Windows 下每次按键会产生 Press/Release 两次事件（按住还有 Repeat），
        // 只处理 Press，避免一次按键触发两次操作、以及双击 Esc 失效。
        if let Event::Key(k) = &ev {
            if k.kind != KeyEventKind::Press {
                return None;
            }
        }
        match self.mode.clone() {
            Mode::Capture { field } => self.handle_capture(field, ev),
            Mode::Edit { field, buf } => self.handle_edit(field, buf, ev),
            // 执行顺序集合编辑的键盘输入改由全局监听线程经 CaptureKey 转发（rdev），
            // 此处忽略 crossterm 键盘，避免终端快速输入卡键
            Mode::OrderEdit { .. } => None,
            Mode::Normal => self.handle_normal(ev),
        }
    }

    fn handle_capture(&mut self, field: usize, ev: Event) -> Option<DialogKind> {
        match ev {
            // 键盘绑定（含 Alt、Esc 取消）全部由全局监听线程经 CaptureKey 转发，
            // 这里忽略 crossterm 键盘事件，避免 Alt 在终端被误读为 Esc 导致取消。
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

    /// 统一绑定入口：唯一化检查 → 设置 → 退出捕获 → 自动保存。
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
            return; // 留在捕获模式，可重新按其他键
        }
        self.set_binding(field, name);
        self.mode = Mode::Normal;
        self.status = Some(StatusMsg {
            text: tr("按键已绑定", "Key bound").into(),
            kind: MsgKind::Ok,
        });
        self.auto_save();
    }

    /// 检查 name 是否已被其他绑定字段使用，返回冲突字段的显示名。
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

    /// 执行顺序集合编辑（rdev 全局按键驱动，避免终端快速输入卡键）。
    /// 只接受 A/B/C，最多三位且不重复；重复/超限给提示，Esc 取消，Enter 确认。
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
                // 集合唯一性：超过三位或元素已存在时静默忽略
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
                    // FIX: 防止 shoot_interval / switch_interval 被设为 0。
                    // interval=0 会使引擎 run_loop 进入 200ms 忙等重试死循环，
                    // 并在 jitter() 中产生 0ms 间隔触发 busy-loop 烧 CPU。
                    // 偏移字段（offset）允许为 0（表示无随机偏移），只拦截主 interval 字段。
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
                // 乱序执行下也可编辑集合（选哪些元素参与乱序）
                self.mode = Mode::OrderEdit {
                    field: F_ORDER,
                    buf: self.cfg.execution_order.clone(),
                };
                None
            }
            F_MODE => {
                // 执行方式：长按 ↔ 切换
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

    /// 切换界面语言（中文 ↔ 英文），并把选择持久化到会话文件 `.zwt`。
    /// 状态文案用**切换后的新语言**显示，避免旧语言下滞留。
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

    /// 仅同步引擎线程的配置副本（不写盘）。加载/速切/刷新/热读取等非编辑操作使用，
    /// 避免覆盖用户手动配置的 zwtcfg.json —— 磁盘只在编辑确认时写入。
    fn sync_engine(&mut self) {
        *self.shared.lock().unwrap() = self.cfg.clone();
    }

    /// 记录当前配置源文件的修改时间（供热读取对比外部修改）；文件不可读时清空。
    fn touch_config_mtime(&mut self) {
        self.config_mtime = self
            .config_source
            .as_ref()
            .and_then(|p| fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
    }

    /// 把当前 `config_source` 同步给监听守护线程。
    /// 每次 `config_source` 变化后都要调用（加载/速切/热读取成功等），让监听线程跟踪最新文件。
    fn note_config_source(&mut self) {
        *self.watched_path.lock().unwrap() = self.config_source.clone();
    }

    /// 热保存：用户确认编辑（Enter）后，保存一次到**当前使用的配置文件**（config_source；
    /// 未载入任何文件时写 zwtcfg.json），并同步给引擎线程。
    /// 绑定、执行方式/乱序切换、执行集合确认、数字编辑确认都会调用；
    /// 取消（Esc）不保存。
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

    /// 热读取：轮询检测当前配置源文件是否被外部修改（mtime 变化），是则自动重新加载并同步引擎。
    /// 仅在文件可读且解析/校验成功时应用；失败或文件被删除时保持当前配置不覆盖。
    fn hot_read(&mut self) {
        let Some(src) = self.config_source.clone() else { return };
        let mtime = match fs::metadata(&src).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => {
                // 文件被删除：保持当前配置，提示一次
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
            return; // 未变化
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
                // 外部写入中途 / 内容损坏：不覆盖当前配置，下次轮询再试
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
        // 默认文件名 = 当前配置来源文件名；无来源时退回 zwtcfg.json，用户可任意改名
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
        // 不限制文件名/扩展名，任意配置文件可读
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

    /// 加载指定 JSON 配置并应用：更新 cfg/来源，同步引擎行为。
    /// 只同步不写盘 —— 磁盘保存仅在编辑确认时发生（见 auto_save）。
    /// 失败返回 Err(具体原因) 且不改变当前配置。
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

    /// 载入指定路由 yaml：解析 → 逐条校验 → 写入无效标记注释 → 填 router_files/router_path，
    /// 并自动加载第一个有效配置。返回 Err(原因) 表示整体载入失败（读失败/语法或规范错误），
    /// 此时不改变现有路由。
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
        // 无效条目：在 yaml 对应位置写入标记注释（同时清除旧标记，避免堆积）
        if !invalid.is_empty() {
            let rewritten = router::rewrite_with_markers(&text, &invalid);
            fs::write(path, rewritten).map_err(|e| tfmt!("写入无效标记注释失败: {}", "Failed to write invalid marker comment: {}", e))?;
        }
        self.router_files = entries;
        self.router_path = Some(path.to_path_buf());
        // 自动加载第一个有效配置；失败仅报状态不阻断（路由本身已载入）
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

    /// 载入速切路由对话框：选取 zwtcfg_router.yaml 后走 load_router_path。
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

    /// 速切到下一个有效路由配置（按列表顺序循环）。
    /// 逐个跳过无效条目；文件在载入后被删除/损坏也实时重新校验并跳过。
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
                    // 无效：跳过（标红；若载入前有效则更新为无效状态）
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

    // ---- 字段取值/赋值 ----

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
            // 语言切换：显示「对侧」语言（当前中文 → Switch Language，当前英文 → 语言切换）
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
                // 偏移区间字段自动加 ± 前缀，编辑时只需输数字
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

/// 是否为偏移区间字段（展示自动加 ± 前缀）。
fn is_offset_field(fi: usize) -> bool {
    matches!(fi, F_SWITCH_OFF | F_SHOOT_OFF)
}

/// 校验路由条目：文件必须存在且可解析为合法配置。
/// 返回空串表示有效；否则返回供展示 / 写入标记注释的错误原因。
fn validate_router_entry(path: &Path) -> String {
    if !path.exists() {
        return tr("文件不存在", "File does not exist").into();
    }
    match config::load_from_path(path) {
        Ok(_) => String::new(),
        Err(e) => e,
    }
}

/// 校验执行顺序：1~3 个字符，只能由 A/B/C 组成，每个最多一次。
/// A / AB / ABC 表示单武器、双武器轮换、三武器循环。
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

/// 把若干行字段渲染成 Line 列表。
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
        Constraint::Length(9), // banner (6行art + 1行仓库URL + 上下边框)
        Constraint::Length(5), // 按键绑定 (2 行)
        Constraint::Length(5), // 执行顺序
        Constraint::Length(6), // 时间配置 (2 行 + 说明)
        Constraint::Length(3), // 操作
        Constraint::Min(3),    // 帮助/状态（操作提示 + 状态与配置文件）
    ])
        .split(area);

    // 标题横幅：绿→蓝平滑渐变 art（不折行）+ 紧贴的 GitHub 仓库地址行
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

    // 按键绑定
    f.render_widget(
        Paragraph::new(rows_to_lines(&ROWS[0..2], app)).block(Block::bordered().title(
            tr(" 按键绑定 ", " Key Bindings "),
        )),
        chunks[1],
    );

    // 执行顺序集合
    let mut order_lines = rows_to_lines(&ROWS[2..3], app);
    order_lines.push(Line::styled(
        tr(
            "A=武器槽#1  B=武器槽#2  C=武器槽#3    集合元素示例: A=#1单例  AB=#1+#2轮换  ABC=#1+#2+#3循环",
            "A=Slot#1  B=Slot#2  C=Slot#3    Set examples: A=single#1  AB=#1+#2 alternate  ABC=#1+#2+#3 cycle",
        ),
        Style::default().fg(Color::DarkGray),
    ));
    // 选中执行顺序集合时，按当前模式显示对应警告；选中执行方式时显示模式说明
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

    // 时间配置
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

    // 操作
    f.render_widget(
        Paragraph::new(rows_to_lines(&ROWS[5..6], app)).block(Block::bordered().title(
            tr(" 操作 ", " Operations "),
        )),
        chunks[4],
    );

    // 帮助/状态
    let mut help: Vec<Line> = Vec::new();
    help.push(Line::styled(
        tr(
            "操作: Tab/方向键 移动   Enter 编辑/确认/切换   绑定: 直接按键   执行热键: 开/停循环   Esc 退出",
            "Controls: Tab/Arrows move   Enter edit/confirm/toggle   Bind: press a key   Execution hotkey: start/stop   Esc quit",
        ),
        Style::default().fg(Color::DarkGray),
    ));
    // 引擎状态 + 当前配置文件同行展示；已载入路由表时列出全部配置并高亮当前
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
            // 无效条目标红；有效条目中当前使用的高亮，其余暗色
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
        // 同步交互模式标志给全局监听线程（捕获/编辑中不触发引擎）
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
        // 配置监听守护线程发现文件被外部改写 → 热重载（轮询已移至 watcher 线程，不在主循环做）
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
                    // 捕获模式 / 执行顺序集合编辑 都由全局转发的按键驱动（rdev，含 Alt）
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
        // UI 测试断言中文文案：用线程本地覆盖固定语言为中文，
        // 避免与其它并行测试的全局语言切换相互干扰。
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

    /// 构造一个「有效」的路由条目（测试辅助）。
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

    /// 把 buffer 按行收集成字符串。宽字符（CJK）占 2 格、延续格是空格，跳过以免 CJK 间插入空格。
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
        assert!(text.contains(TOP_BAR), "开源仓库地址未渲染");
        // 苦力怕已取消，配置区正常
        assert!(!text.contains('▄'), "Creeper 未移除");
        assert!(text.contains("武器槽#1绑定"), "配置区未渲染");
    }

    /// 状态行：无路由时显示当前配置文件；已载入路由时列出全部并保留当前项。
    #[test]
    fn status_line_shows_config_and_router() {
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = test_app();

        // 未载入路由：显示当前配置文件
        app.config_source = Some(PathBuf::from("zwtcfg.json"));
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = buffer_rows(terminal.backend().buffer()).join("\n");
        assert!(
            text.contains("○ 已停止 | 配置文件: zwtcfg.json"),
            "单配置未显示: {text}"
        );

        // 已载入路由：列出全部配置
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

    /// 速切配置热键与载入路由动作渲染。
    #[test]
    fn router_field_and_action_render() {
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = test_app();
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = buffer_rows(terminal.backend().buffer()).join("\n");
        assert!(text.contains("速切配置热键: RALT"), "速切热键默认值未渲染: {text}");
        assert!(text.contains("载入路由"), "载入路由动作未渲染: {text}");

        // 改绑速切热键后显示新按键名
        app.cfg.router_hotkey = "G".into();
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = buffer_rows(terminal.backend().buffer()).join("\n");
        assert!(text.contains("速切配置热键: G"), "绑定未渲染: {text}");
    }

    /// 无效路由条目标红，有效条目正常渲染。
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

        // 找到含 "bad.json" 的行，重扫该行定位 'b' 所在 buffer x（跳过 CJK 延续格）
        let rows = buffer_rows(buf);
        let row = rows
            .iter()
            .position(|r| r.contains("bad.json"))
            .expect("bad.json 应出现在状态行");
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
                pos_of_b = Some(x - 7); // 'b' 之后还有 7 个字符
            }
            if is_cjk(ch) {
                skip_next = true;
            }
        }
        let x = pos_of_b.expect("bad.json 文本未定位");
        assert_eq!(
            buf.cell((x, row as u16)).unwrap().fg,
            Color::Red,
            "无效条目应渲染为红色"
        );
    }

    /// 速切跳过无效条目、循环回到有效条目，并实时重校验文件变化。
    #[test]
    fn router_next_skips_invalid_and_cycles() {
        use std::fs;
        let dir = std::env::temp_dir().join("zwt_router_test");
        fs::create_dir_all(&dir).unwrap();
        let good1 = dir.join("good1.json");
        let bad = dir.join("bad.json"); // 不存在
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
        app.router_next(); // #1 → 下一个有效：跳过无效的 #2，跳到 #3
        assert_eq!(app.router_index, 2, "应跳过无效条目");
        assert_eq!(app.cfg.execution_order, "B", "应加载 #3 的配置");
        app.router_next(); // 循环回 #1
        assert_eq!(app.router_index, 0, "应循环回第一个");
        assert_eq!(app.cfg.execution_order, "A", "应加载 #1 的配置");

        let _ = fs::remove_dir_all(&dir);
    }

    /// 热读取：外部修改当前配置源文件（mtime 变化）→ 自动重新加载。
    #[test]
    fn hot_read_reloads_external_change() {
        use std::fs;
        let dir = std::env::temp_dir().join("zwt_hotread_test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cfg.json");
        fs::write(&path, r#"{"execution_order":"A","execution_hotkey":"LALT"}"#).unwrap();

        let mut app = test_app();
        app.config_source = Some(path.clone());
        app.config_mtime = Some(SystemTime::UNIX_EPOCH); // 模拟"外部已改写"
        app.hot_read();
        assert_eq!(app.cfg.execution_order, "A", "热读取应加载外部内容");
        assert!(app.config_mtime.is_some(), "应记录新 mtime");

        // 再次外部改写 → 再次热读取
        fs::write(&path, r#"{"execution_order":"B","execution_hotkey":"LALT"}"#).unwrap();
        app.config_mtime = Some(SystemTime::UNIX_EPOCH); // 强制视为已变化
        app.hot_read();
        assert_eq!(app.cfg.execution_order, "B", "第二次热读取应更新");

        let _ = fs::remove_dir_all(&dir);
    }

    /// 热读取：mtime 未变化时不重新加载（避免覆盖内存中已偏离磁盘的编辑）。
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
        app.cfg.execution_order = "C".into(); // 内存已偏离磁盘
        app.hot_read();
        assert_eq!(app.cfg.execution_order, "C", "mtime 未变不应重载");

        let _ = fs::remove_dir_all(&dir);
    }

    /// 热读取：外部写入损坏内容时不覆盖当前配置。
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
        app.cfg.execution_order = "ABC".into(); // 内存值
        fs::write(&path, "{not valid json}").unwrap();
        app.hot_read();
        assert_eq!(app.cfg.execution_order, "ABC", "损坏文件不应覆盖当前配置");

        let _ = fs::remove_dir_all(&dir);
    }

    /// 热保存：编辑确认后写入「当前使用的配置文件」（config_source），而非固定 zwtcfg.json。
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
        assert!(text.contains("ABC"), "保存目标应是 config_source: {text}");
        assert!(text.contains("F6"), "保存目标应是 config_source: {text}");

        let _ = fs::remove_dir_all(&dir);
    }

    /// 执行方式字段渲染：默认「切换」，Enter 切换后显示「长按」，选中时显示对应提示。
    #[test]
    fn execution_mode_field_renders_with_hint() {
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = test_app();
        app.focus = F_MODE;
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = buffer_rows(terminal.backend().buffer()).join("\n");
        assert!(text.contains("执行方式: 切换"), "默认模式未渲染: {text}");
        assert!(text.contains("切换模式"), "切换模式提示未渲染: {text}");

        app.cfg.execution_mode = ExecutionMode::Hold;
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = buffer_rows(terminal.backend().buffer()).join("\n");
        assert!(text.contains("执行方式: 长按"), "hold 未渲染: {text}");
        assert!(text.contains("长按模式"), "长按模式提示未渲染: {text}");
    }

    #[test]
    fn banner_art_unified() {
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = test_app();
        terminal.draw(|f| ui(f, &app)).unwrap();
        let rows = buffer_rows(terminal.backend().buffer());
        // 第一行 art（去掉左框线）应包含 banner 顶行特征字符串
        let art_row = rows[1].trim_start_matches('│');
        assert!(
            art_row.contains("_____") && art_row.contains("_____           _"),
            "banner art 行错: {:?}",
            art_row
        );
    }

    /// 载入路由：解析 → 校验 → 应用第一个有效配置，并把会话持久化到状态文件。
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
        app.session_path = dir.join("state.json"); // 每测试隔离状态文件，避免并发写同一临时文件
        app.load_router_path(&yaml).unwrap();

        assert_eq!(app.router_path.as_deref(), Some(yaml.as_path()));
        assert_eq!(app.router_files.len(), 2);
        assert_eq!(app.router_index, 0, "应高亮第一个有效条目");
        assert_eq!(app.cfg.execution_order, "A", "应自动加载第一个有效配置");
        assert_eq!(app.config_source.as_deref(), Some(cfg1.as_path()));

        // 会话已持久化：记录路由与当前配置
        let state = session::load_from(&app.session_path).expect("会话应已写入");
        assert_eq!(state.last_router.as_deref(), Some(yaml.to_str().unwrap()));
        assert_eq!(state.last_config.as_deref(), Some(cfg1.to_str().unwrap()));
        assert_eq!(state.router_index, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    /// 载入路由：无效条目在 yaml 中写入 `# [无效]` 标记，且仍应用第一个有效配置。
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
        assert!(!app.router_files[1].valid, "缺失文件应标记为无效");
        assert_eq!(app.router_index, 0);

        let text = fs::read_to_string(&yaml).unwrap();
        assert!(text.contains("# [无效]"), "应写入无效标记: {text}");
        assert!(text.contains("missing.json"), "标记应指向缺失文件: {text}");

        let _ = fs::remove_dir_all(&dir);
    }

    /// 载入路由：yaml 语法/规范错误 → Err 且不改变现有路由（沿用旧表）。
    #[test]
    fn load_router_path_spec_error_keeps_old_router() {
        use std::fs;
        let dir = std::env::temp_dir().join("zwt_router_spec_err");
        fs::create_dir_all(&dir).unwrap();
        let yaml = dir.join("router.yaml");
        fs::write(&yaml, "config: a.json\nconfig: b.json\n").unwrap(); // 重复 config 键

        let mut app = test_app();
        app.router_files = vec![entry("old.json")];
        app.router_path = Some(PathBuf::from("old.yaml"));

        let err = app.load_router_path(&yaml).unwrap_err();
        assert!(err.contains("重复"), "错误信息应说明原因: {err}");
        assert_eq!(app.router_files.len(), 1, "规范错误不应改动现有路由");
        assert_eq!(app.router_path.as_deref(), Some(Path::new("old.yaml")));

        let _ = fs::remove_dir_all(&dir);
    }

    /// 会话恢复：重载路由表、恢复上次使用的配置，并把路由高亮对齐到该配置。
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
            router_index: 0, // 记录高亮与配置不一致 → 应按 last_config 对齐
            language: None,
        };
        app.restore_with_state(state);

        assert_eq!(app.router_files.len(), 2, "路由应自动重载");
        assert_eq!(app.config_source.as_deref(), Some(cfg2.as_path()), "应恢复上次配置");
        assert_eq!(app.cfg.execution_order, "B");
        assert_eq!(app.router_index, 1, "高亮应对齐到恢复的配置");
    }

    /// 会话恢复：上次配置被删除 → 回退到路由第一有效条目，不崩溃。
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

    /// 启动恢复集成：从会话文件读取状态并自动重载路由/应用配置。
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

        assert_eq!(app.router_files.len(), 1, "启动应自动重载路由");
        assert_eq!(app.config_source.as_deref(), Some(cfg1.as_path()));
    }

    /// 语言切换项渲染「对侧」语言：当前中文显示 `Switch Language`，当前英文显示 `语言切换`。
    #[test]
    fn language_action_renders_cross_lang_label() {
        let backend = TestBackend::new(140, 36);
        let mut terminal = Terminal::new(backend).unwrap();

        // 当前中文 → 按钮显示英文（目标语言）
        Lang::test_set(Lang::Zh);
        let app = test_app();
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = buffer_rows(terminal.backend().buffer()).join("\n");
        assert!(text.contains("[ Switch Language ]"), "中文下应显示英文目标: {text}");

        // 当前英文 → 按钮显示中文
        Lang::test_set(Lang::En);
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = buffer_rows(terminal.backend().buffer()).join("\n");
        assert!(text.contains("[ 语言切换 ]"), "英文下应显示中文目标: {text}");

        Lang::test_clear();
    }

    /// 切换语言后：全局（线程本地）语言被翻转，且把选择持久化到会话文件（`language` 字段更新）。
    #[test]
    fn switch_language_toggles_and_persists() {
        let mut app = test_app();
        let dir = std::env::temp_dir().join("zwt_lang_persist");
        std::fs::create_dir_all(&dir).unwrap();
        app.session_path = dir.join("state.json");

        // 由中文切换到英文
        Lang::test_set(Lang::Zh);
        app.switch_language();
        assert_eq!(Lang::get(), Lang::En, "切换后应为英文");
        let state = session::load_from(&app.session_path).expect("会话应已写入");
        assert_eq!(state.language, Some(Lang::En), "会话应记录英文");

        // 再切回中文
        app.switch_language();
        assert_eq!(Lang::get(), Lang::Zh, "应能切回中文");
        let state = session::load_from(&app.session_path).expect("会话应已写入");
        assert_eq!(state.language, Some(Lang::Zh), "会话应记录中文");

        let _ = std::fs::remove_dir_all(&dir);
        Lang::test_clear();
    }
}