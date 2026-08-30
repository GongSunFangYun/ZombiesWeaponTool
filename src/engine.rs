//! 执行引擎：rdev 全局监听执行热键，切换开关驱动循环模拟序列。

use crate::config::{Config, ExecutionMode};
use crate::keymap;
use rand::{Rng, RngExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// 后台线程发往主线程的控制消息（全局输入监听、引擎循环、配置监听共用此通道）。
pub enum EngineMsg {
    /// 全局监听线程已切换引擎开关，通知主线程刷新 UI。
    Toggled(bool),
    /// 捕获模式期间，全局按下的键盘按键转发给 TUI 用于绑定（含 Alt 等终端抓不到的键）。
    CaptureKey(rdev::Key),
    /// 速切配置热键被按下：主线程按路由列表（zwtcfg_router.yaml）切到下一个 JSON 配置。
    RouterNext,
    /// 配置监听守护线程发现当前配置文件被外部改写（mtime 变化），通知主线程热重载。
    ConfigReload,
}

pub struct Engine {
    running: Arc<AtomicBool>,
    cfg: Arc<Mutex<Config>>,
}

impl Engine {
    pub fn new(cfg: Arc<Mutex<Config>>) -> Self {
        Engine {
            running: Arc::new(AtomicBool::new(false)),
            cfg,
        }
    }

    pub fn running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// 启动引擎（幂等）。已运行时返回 false，不重复启动线程。
    ///
    /// 用 `compare_exchange`（原子 CAS）保证 false → true 只有一个调用者成功，
    /// 由它负责 spawn `run_loop`，避免两个循环同时注入模拟输入互相叠加。
    pub fn start(&self) -> bool {
        match self.running.compare_exchange(
            false,
            true,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                self.spawn_loop();
                true
            }
            Err(_) => false,
        }
    }

    /// 停止引擎。若原本就在运行返回 true（有实际状态变化）。
    /// run_loop 会在下次 `running.load` 时检测到 false 并自行退出。
    pub fn stop(&self) -> bool {
        self.running.swap(false, Ordering::AcqRel)
    }

    /// 翻转开关。从关→开时启动引擎线程，返回切换后的状态。
    ///
    /// 竞态安全：读取到 true 时 store false（幂等）；读取到 false 时走 `start()`，
    /// 其内部 CAS 保证只有一个线程能真正 spawn。
    pub fn toggle(&self) -> bool {
        if self.running.load(Ordering::Relaxed) {
            self.running.store(false, Ordering::Release);
            false
        } else {
            self.start()
        }
    }

    fn spawn_loop(&self) {
        let running = self.running.clone();
        let cfg = self.cfg.clone();
        thread::spawn(move || run_loop(running, cfg));
    }
}

/// 启动全局按键监听。阻塞，须放入独立线程；
/// - TUI 正常模式：命中执行热键 → 监听线程直接驱动引擎开关（真正全局），通知主线程刷新 UI。
///   执行方式决定触发语义：
///   - `Hold`：按下启动、松开停止（start/stop 幂等，长按 Repeat 不会重复触发）；
///   - `Toggle`：点击翻转开关，300ms 防抖忽略按住产生的 Repeat。
/// - TUI 捕获/编辑模式（interacting）：按下的键转发给 TUI 用于绑定，**不驱动引擎**，
///   避免绑定启动键时误启引擎导致系统级模拟点击异常
pub fn start_listener(
    engine: Arc<Engine>,
    interacting: Arc<AtomicBool>,
    tx: Sender<EngineMsg>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut last_toggle = Instant::now();
        let mut last_router = Instant::now();
        let _ = rdev::listen(move |ev| {
            let k = match ev.event_type {
                rdev::EventType::KeyPress(k) | rdev::EventType::KeyRelease(k) => Some(k),
                _ => None,
            };
            let Some(k) = k else { return };
            let is_press = matches!(ev.event_type, rdev::EventType::KeyPress(_));

            if interacting.load(Ordering::Relaxed) {
                // 捕获/编辑中：仅转发按下事件供 TUI 绑定（含 Alt 等终端抓不到的键）
                if is_press {
                    let _ = tx.send(EngineMsg::CaptureKey(k));
                }
                return;
            }

            let cfg = engine.cfg.lock().unwrap().clone();
            let is_exec = keymap::binding_main_key(&cfg.execution_hotkey) == Some(k);
            let is_router = keymap::binding_main_key(&cfg.router_hotkey) == Some(k);
            if is_exec {
                match cfg.execution_mode {
                    ExecutionMode::Hold => {
                        // 长按：按下启动、松开停止；start/stop 幂等，仅在状态变化时通知 UI
                        let changed = if is_press {
                            engine.start()
                        } else {
                            engine.stop()
                        };
                        if changed {
                            let _ = tx.send(EngineMsg::Toggled(is_press));
                        }
                    }
                    ExecutionMode::Toggle => {
                        // 切换：只在按下时翻转；防抖：按住产生的 Repeat 忽略 300ms 内重复触发
                        if is_press && last_toggle.elapsed() >= Duration::from_millis(300) {
                            last_toggle = Instant::now();
                            let on = engine.toggle();
                            let _ = tx.send(EngineMsg::Toggled(on));
                        }
                    }
                }
            } else if is_router && is_press {
                // 速切配置热键：只在按下时切到下一个配置；同样防抖避免按住 Repeat 连切
                if last_router.elapsed() >= Duration::from_millis(300) {
                    last_router = Instant::now();
                    let _ = tx.send(EngineMsg::RouterNext);
                }
            }
        });
    })
}

/// 引擎循环：右键连点（射击间隔）与自动切枪（切换武器间隔）**独立定时调度**，
/// 各按自身间隔触发，互不锁死；等待用高精度混合等待，间隔接近真实毫秒值。
///
/// # 热切换/热读取
///
/// 配置在**每次循环迭代**从共享副本读取（而非每轮快照），
/// 因此速切配置、热读取外部修改、热键/绑定变更都会在下一次调度时立即生效，
/// 无需等一整轮（全部武器切一遍）才应用。
fn run_loop(running: Arc<AtomicBool>, cfg: Arc<Mutex<Config>>) {
    // 引擎运行期间把系统定时器分辨率提到 1ms（停止时自动恢复）
    let _timer = HighResTimer::new();
    let mut rng = rand::rng();
    while running.load(Ordering::Relaxed) {
        // 首轮首个切枪延迟基于启动时的配置
        let first = cfg.lock().unwrap().clone();
        // 待切武器队列：每次耗尽重建，乱序开启时按执行顺序选中槽位数洗牌
        let mut queue: Vec<String> = Vec::new();
        let mut next_shot_at = Instant::now();
        let mut next_switch_at = Instant::now() + jitter(
            first.weapon_switch_interval_ms,
            first.weapon_switch_interval_offset_ms,
            &mut rng,
        );

        loop {
            // 每次迭代读取最新配置：热切换/热读取即时生效
            let c = cfg.lock().unwrap().clone();
            if c.shoot_interval_ms == 0 {
                // 射击间隔为 0 无意义，稍候重试
                sleep_interruptible(Duration::from_millis(200), &running);
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                // 配置恢复后重新调度时间点，避免旧时刻直接触发
                next_shot_at = Instant::now();
                next_switch_at = Instant::now() + jitter(
                    c.weapon_switch_interval_ms,
                    c.weapon_switch_interval_offset_ms,
                    &mut rng,
                );
                continue;
            }
            let now = Instant::now();
            if now >= next_shot_at {
                keymap::tap_binding("MB2"); // 射击（右键连点）
                next_shot_at = now + jitter(c.shoot_interval_ms, c.shoot_interval_offset_ms, &mut rng);
            }
            if now >= next_switch_at {
                if queue.is_empty() {
                    queue = build_slots(&c, &mut rng);
                    if queue.is_empty() {
                        // 没有可用武器绑定，停一轮
                        break;
                    }
                }
                let slot = queue.remove(0);
                keymap::tap_binding(&slot); // 切武器
                next_switch_at = now + jitter(
                    c.weapon_switch_interval_ms,
                    c.weapon_switch_interval_offset_ms,
                    &mut rng,
                );
            }
            if !running.load(Ordering::Relaxed) {
                break;
            }
            // 精确等到最近的下一个事件时刻
            precise_sleep_until(next_shot_at.min(next_switch_at));
        }
    }
}

/// 引擎运行期间提升系统定时器分辨率到 1ms（timeBeginPeriod），
/// Drop 时恢复（timeEndPeriod）。全局影响系统定时精度，仅引擎运行时启用。
struct HighResTimer;

impl HighResTimer {
    fn new() -> Self {
        unsafe {
            winapi::um::timeapi::timeBeginPeriod(1);
        }
        HighResTimer
    }
}

impl Drop for HighResTimer {
    fn drop(&mut self) {
        unsafe {
            winapi::um::timeapi::timeEndPeriod(1);
        }
    }
}

/// 高精度混合等待：先 sleep 到目标前 2ms，再 spin 到精确时刻。
/// sleep 精度受系统定时器影响，尾部 spin 兜底保证亚毫秒精度。
fn precise_sleep_until(end: Instant) {
    let spin_from = end
        .checked_sub(Duration::from_millis(2))
        .unwrap_or(end);
    while Instant::now() < spin_from {
        thread::sleep(Duration::from_millis(1));
    }
    while Instant::now() < end {
        std::hint::spin_loop();
    }
}

/// 从执行顺序（A/B/C）构建参与武器键列表；乱序开启时每轮打乱。
///
/// # execution_order 去重修复
///
/// 原实现对 `execution_order` 中重复字符（如 "AABB"）不做过滤，
/// 会把同一武器槽重复推入队列，导致切枪序列与用户预期不符。
/// 新实现用 `seen` 集合对字符去重，每个 A/B/C 最多入队一次。
fn build_slots(c: &Config, rng: &mut impl Rng) -> Vec<String> {
    let mut seen = [false; 3]; // A=0, B=1, C=2
    let mut slots = Vec::new();
    for ch in c.execution_order.bytes() {
        let idx = match ch {
            b'A' => 0usize,
            b'B' => 1,
            b'C' => 2,
            _ => continue,
        };
        if seen[idx] {
            continue; // 去重：重复字符跳过
        }
        seen[idx] = true;
        let name = match idx {
            0 => &c.weapon_slot1,
            1 => &c.weapon_slot2,
            _ => &c.weapon_slot3,
        };
        if !name.is_empty() {
            slots.push(name.clone());
        }
    }
    if c.random_execution {
        let n = slots.len();
        for i in (1..n).rev() {
            let j = rng.random_range(0..=i);
            slots.swap(i, j);
        }
    }
    slots
}

/// 标准间隔 ± 随机偏移。
///
/// # busy-loop 修复
///
/// 原实现当 `offset > base` 时，负方向的随机值会被 `.max(0)` 截断为 0，
/// 导致 `next_shot_at` / `next_switch_at` 不推进，`precise_sleep_until`
/// 立即返回，形成 busy-loop，烧满一个 CPU 核心。
///
/// 修复策略：
/// 1. `offset` 先被夹到 `base` 以内，保证随机结果始终 > 0；
/// 2. 对两个方向分别用 `saturating_add` / `saturating_sub` 避免溢出；
/// 3. 兜底最小值 1ms，防止外部传入 base=0 时仍触发 busy-loop（
///    run_loop 已在 shoot_interval=0 时提前跳过，但防御性 max(1) 更安全）。
fn jitter(base: u64, offset: u64, rng: &mut impl Rng) -> Duration {
    // offset 不超过 base，保证区间下界 >= 0（且最小值 > 0）
    let offset = offset.min(base);
    let delta = rng.random_range(0..=offset);
    let ms = if rng.random_bool(0.5) {
        base.saturating_add(delta)
    } else {
        base.saturating_sub(delta)
    };
    Duration::from_millis(ms.max(1)) // 兜底最小 1ms，杜绝 busy-loop
}

/// 分片睡眠，期间检查运行标志，保证停止响应及时。
fn sleep_interruptible(d: Duration, running: &AtomicBool) {
    let chunk = Duration::from_millis(50);
    let mut remaining = d;
    while remaining > Duration::ZERO && running.load(Ordering::Relaxed) {
        let s = remaining.min(chunk);
        thread::sleep(s);
        remaining -= s;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn build_slots_respects_order_and_filters_empty() {
        let mut c = cfg();
        c.execution_order = "AB".into();
        c.weapon_slot1 = "2".into();
        c.weapon_slot2 = "3".into();
        c.weapon_slot3 = "4".into();
        let mut rng = rand::rng();
        assert_eq!(build_slots(&c, &mut rng), vec!["2".to_string(), "3".to_string()]);
    }

    #[test]
    fn build_slots_filters_empty_bindings() {
        let mut c = cfg();
        c.execution_order = "ABC".into();
        c.weapon_slot1 = "2".into();
        c.weapon_slot2 = String::new();
        c.weapon_slot3 = "4".into();
        let mut rng = rand::rng();
        assert_eq!(build_slots(&c, &mut rng), vec!["2".to_string(), "4".to_string()]);
    }

    #[test]
    fn build_slots_returns_empty_when_all_unbound() {
        let c = Config::default();
        let mut c = c;
        c.weapon_slot1.clear();
        c.weapon_slot2.clear();
        c.weapon_slot3.clear();
        let mut rng = rand::rng();
        assert!(build_slots(&c, &mut rng).is_empty());
    }

    /// 新增：重复字符去重后结果正确。
    #[test]
    fn build_slots_deduplicates_repeated_order_chars() {
        let mut c = cfg();
        c.execution_order = "AABB".into(); // 非法重复输入
        c.weapon_slot1 = "2".into();
        c.weapon_slot2 = "3".into();
        let mut rng = rand::rng();
        // 去重后等价于 "AB"，不应出现重复槽位
        let slots = build_slots(&c, &mut rng);
        assert_eq!(slots, vec!["2".to_string(), "3".to_string()]);
    }

    /// 新增：非法字符被静默忽略，不 panic。
    #[test]
    fn build_slots_ignores_invalid_order_chars() {
        let mut c = cfg();
        c.execution_order = "AXB".into(); // 'X' 非法
        c.weapon_slot1 = "2".into();
        c.weapon_slot2 = "3".into();
        let mut rng = rand::rng();
        let slots = build_slots(&c, &mut rng);
        assert_eq!(slots, vec!["2".to_string(), "3".to_string()]);
    }

    #[test]
    fn jitter_within_range() {
        let mut rng = rand::rng();
        for _ in 0..200 {
            let d = jitter(300, 30, &mut rng);
            let ms = d.as_millis() as i64;
            assert!((270..=330).contains(&ms), "jitter {ms} out of range");
        }
    }

    /// 新增：offset > base 时不应产生 0ms（busy-loop 防护）。
    #[test]
    fn jitter_never_zero_when_offset_exceeds_base() {
        let mut rng = rand::rng();
        for _ in 0..500 {
            let d = jitter(10, 50, &mut rng); // offset > base
            assert!(d.as_millis() >= 1, "jitter returned 0ms, busy-loop risk");
        }
    }

    /// 新增：base=0 极端情况下也不返回 0ms。
    #[test]
    fn jitter_never_zero_with_zero_base() {
        let mut rng = rand::rng();
        for _ in 0..100 {
            let d = jitter(0, 0, &mut rng);
            assert!(d.as_millis() >= 1);
        }
    }

    /// 空闲引擎 stop() 应返回 false（无状态变化），且保持停止。
    /// 不调用 start()，避免 run_loop 向系统注入真实模拟输入。
    #[test]
    fn stop_on_idle_engine_returns_false() {
        let shared = Arc::new(Mutex::new(Config::default()));
        let engine = Arc::new(Engine::new(shared));
        assert!(!engine.stop());
        assert!(!engine.running());
    }
}