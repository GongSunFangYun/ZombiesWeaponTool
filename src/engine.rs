//! Execution engine: listens for the execution hotkey via `rdev`, driving the simulated
//! input sequence through a toggle switch.

use crate::config::{Config, ExecutionMode};
use crate::keymap;
use rand::{Rng, RngExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Control messages sent from background threads to the main thread (shared by the global
/// input listener, the engine loop, and the config watcher).
pub enum EngineMsg {
    /// The global listener has flipped the engine switch; the main thread should refresh the UI.
    Toggled(bool),
    /// During capture mode, a globally-pressed key is forwarded to the TUI for binding
    /// (catching keys like Alt that the terminal can't see).
    CaptureKey(rdev::Key),
    /// The quick-switch hotkey was pressed: the main thread should advance to the next JSON
    /// config in the router table (`zwtcfg_router.yaml`).
    RouterNext,
    /// The config watcher thread noticed the active config file was rewritten externally
    /// (mtime changed) and asks the main thread to hot-reload it.
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

    /// Start the engine (idempotent). Returns `false` if already running and does not
    /// spawn a second thread.
    ///
    /// Uses `compare_exchange` (an atomic CAS) so exactly one caller wins the `false → true`
    /// transition and is responsible for spawning `run_loop` — this prevents two loops from
    /// injecting simulated input concurrently and stacking on top of each other.
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

    /// Stop the engine. Returns `true` if it was actually running (a real state change).
    /// `run_loop` notices `false` on its next `running.load` and exits on its own.
    pub fn stop(&self) -> bool {
        self.running.swap(false, Ordering::AcqRel)
    }

    /// Flip the switch. On an off → on transition it starts the engine thread and returns the
    /// new state.
    ///
    /// Race-safe: reading `true` stores `false` (idempotent); reading `false` calls `start()`,
    /// whose internal CAS guarantees only one thread actually spawns.
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

/// Start the global key listener. Blocking — must run on its own thread;
/// - TUI normal mode: on the execution hotkey the listener thread drives the engine switch
///   directly (truly global) and notifies the main thread to refresh the UI. The trigger
///   semantics depend on the execution mode:
///   - `Hold`: start on press, stop on release (`start`/`stop` are idempotent, so a held
///     key's `Repeat` events don't re-trigger);
///   - `Toggle`: flip on press, with a 300 ms debounce to ignore `Repeat` while held.
/// - TUI capture/edit mode (`interacting`): pressed keys are forwarded to the TUI for
///   binding and do **not** drive the engine — otherwise binding the start key could
///   accidentally start the engine and cause system-level click injection.
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
                // Capture/edit: forward only press events for TUI binding (including keys
                // the terminal can't see, e.g. Alt).
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
                        // Hold: start on press, stop on release; start/stop are idempotent,
                        // and we only notify the UI on an actual state change.
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
                        // Toggle: flip on press only; debounce ignores a Repeat within 300 ms.
                        if is_press && last_toggle.elapsed() >= Duration::from_millis(300) {
                            last_toggle = Instant::now();
                            let on = engine.toggle();
                            let _ = tx.send(EngineMsg::Toggled(on));
                        }
                    }
                }
            } else if is_router && is_press {
                // Quick-switch hotkey: advance to the next config only on press; same 300 ms
                // debounce so a held key doesn't cycle repeatedly.
                if last_router.elapsed() >= Duration::from_millis(300) {
                    last_router = Instant::now();
                    let _ = tx.send(EngineMsg::RouterNext);
                }
            }
        });
    })
}

/// Engine loop: right-click autoclick (shoot interval) and auto weapon-switching (switch
/// interval) are **scheduled independently**, each firing on its own cadence so they never
/// lock each other out; the wait uses a high-precision hybrid sleep so intervals land close
/// to their real millisecond values.
///
/// # Hot-switching / hot-reading
///
/// The config is read from the shared copy on **every loop iteration** (not a per-round
/// snapshot), so a quick-switch, a hot-read of an external edit, or a hotkey/binding change
/// takes effect on the very next scheduled action — without waiting a whole round (all
/// weapons switched once) to apply.
fn run_loop(running: Arc<AtomicBool>, cfg: Arc<Mutex<Config>>) {
    // Raise the system timer resolution to 1 ms while the engine runs (restored on stop).
    let _timer = HighResTimer::new();
    let mut rng = rand::rng();
    while running.load(Ordering::Relaxed) {
        // The first round's first switch delay is based on the config at start time.
        let first = cfg.lock().unwrap().clone();
        // Pending weapon queue: rebuilt whenever it is exhausted; when random order is on,
        // the slots selected by the execution order are shuffled.
        let mut queue: Vec<String> = Vec::new();
        let mut next_shot_at = Instant::now();
        let mut next_switch_at = Instant::now() + jitter(
            first.weapon_switch_interval_ms,
            first.weapon_switch_interval_offset_ms,
            &mut rng,
        );

        loop {
            // Read the latest config each iteration so hot-switch/hot-read apply immediately.
            let c = cfg.lock().unwrap().clone();
            if c.shoot_interval_ms == 0 {
                // A zero shoot interval is meaningless; retry after a short wait.
                sleep_interruptible(Duration::from_millis(200), &running);
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                // Re-schedule the timestamps once the config recovers, so a stale deadline
                // doesn't fire immediately.
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
                keymap::tap_binding("MB2"); // shoot (right-click autoclick)
                next_shot_at = now + jitter(c.shoot_interval_ms, c.shoot_interval_offset_ms, &mut rng);
            }
            if now >= next_switch_at {
                if queue.is_empty() {
                    queue = build_slots(&c, &mut rng);
                    if queue.is_empty() {
                        // No usable weapon binding: skip this round.
                        break;
                    }
                }
                let slot = queue.remove(0);
                keymap::tap_binding(&slot); // switch weapon
                next_switch_at = now + jitter(
                    c.weapon_switch_interval_ms,
                    c.weapon_switch_interval_offset_ms,
                    &mut rng,
                );
            }
            if !running.load(Ordering::Relaxed) {
                break;
            }
            // Sleep precisely until the nearer of the next two event deadlines.
            precise_sleep_until(next_shot_at.min(next_switch_at));
        }
    }
}

/// Raises the system timer resolution to 1 ms (`timeBeginPeriod`) while the engine runs, and
/// restores it on drop (`timeEndPeriod`). This affects the whole system's timer precision, so
/// it is only active while the engine is running.
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

/// High-precision hybrid wait: sleep to 2 ms before the target, then spin to the exact moment.
/// Sleep precision is limited by the system timer, so the trailing spin guarantees
/// sub-millisecond accuracy.
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

/// Build the list of weapon keys participating in the round from the execution order
/// (`A`/`B`/`C`); shuffle every round if random order is enabled.
///
/// # `execution_order` dedup fix
///
/// The original implementation did not filter duplicate characters in `execution_order`
/// (e.g. `"AABB"`), so the same weapon slot could be pushed into the queue more than once and
/// the switch sequence would not match user intent. The new implementation deduplicates with a
/// `seen` set so each `A`/`B`/`C` is enqueued at most once.
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
            continue; // dedup: skip repeated chars
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

/// Standard interval ± random offset.
///
/// # busy-loop fix
///
/// The original implementation, when `offset > base`, clamped the negative-direction random
/// value to `0` via `.max(0)`. That made `next_shot_at` / `next_switch_at` fail to advance, so
/// `precise_sleep_until` returned immediately, forming a busy-loop that burned a full CPU core.
///
/// Fix strategy:
/// 1. clamp `offset` to `base` first, so the random result is always > 0;
/// 2. use `saturating_add` / `saturating_sub` for both directions to avoid overflow;
/// 3. floor at 1 ms, so even an external `base=0` cannot trigger a busy-loop (the run loop
///    already skips early when `shoot_interval=0`, but a defensive `max(1)` is safer).
fn jitter(base: u64, offset: u64, rng: &mut impl Rng) -> Duration {
    // Clamp offset to base so the interval's lower bound stays >= 0 (and its minimum > 0).
    let offset = offset.min(base);
    let delta = rng.random_range(0..=offset);
    let ms = if rng.random_bool(0.5) {
        base.saturating_add(delta)
    } else {
        base.saturating_sub(delta)
    };
    Duration::from_millis(ms.max(1)) // floor at 1 ms to eliminate any busy-loop
}

/// Chunked sleep that checks the running flag, so a stop is honored promptly.
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

    /// Dedup of repeated order chars produces the correct result.
    #[test]
    fn build_slots_deduplicates_repeated_order_chars() {
        let mut c = cfg();
        c.execution_order = "AABB".into(); // illegal repeated input
        c.weapon_slot1 = "2".into();
        c.weapon_slot2 = "3".into();
        let mut rng = rand::rng();
        // Deduped, this is equivalent to "AB" — no repeated slot should appear.
        let slots = build_slots(&c, &mut rng);
        assert_eq!(slots, vec!["2".to_string(), "3".to_string()]);
    }

    /// Invalid chars are silently ignored (no panic).
    #[test]
    fn build_slots_ignores_invalid_order_chars() {
        let mut c = cfg();
        c.execution_order = "AXB".into(); // 'X' is invalid
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

    /// When `offset > base` it must not yield 0 ms (busy-loop protection).
    #[test]
    fn jitter_never_zero_when_offset_exceeds_base() {
        let mut rng = rand::rng();
        for _ in 0..500 {
            let d = jitter(10, 50, &mut rng); // offset > base
            assert!(d.as_millis() >= 1, "jitter returned 0ms, busy-loop risk");
        }
    }

    /// Even the extreme `base=0` case must not return 0 ms.
    #[test]
    fn jitter_never_zero_with_zero_base() {
        let mut rng = rand::rng();
        for _ in 0..100 {
            let d = jitter(0, 0, &mut rng);
            assert!(d.as_millis() >= 1);
        }
    }

    /// `stop()` on an idle engine returns `false` (no state change) and it stays stopped.
    /// We deliberately don't call `start()`, which would make `run_loop` inject real
    /// simulated input into the system.
    #[test]
    fn stop_on_idle_engine_returns_false() {
        let shared = Arc::new(Mutex::new(Config::default()));
        let engine = Arc::new(Engine::new(shared));
        assert!(!engine.stop());
        assert!(!engine.running());
    }
}