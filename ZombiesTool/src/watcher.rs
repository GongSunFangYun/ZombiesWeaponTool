//! 配置文件实时监听守护线程。
//!
//! 主线程把「当前正在使用的配置文件路径」（`config_source`）放到共享的 [`WatchedPath`] 单元里；
//! 本守护线程则**周期性轮询**该文件的修改时间（mtime），一旦发现被外部改写，就通过
//! [`EngineMsg::ConfigReload`] 通知主线程，由主线程复用 `hot_read()` 完成热重载。
//!
//! 设计要点：
//! - **主线程仍是唯一读写配置状态的地方**——守护线程只负责「发现变化 → 通知」，避免跨线程
//!   直接改 TUI 状态（`cfg` / `shared` / 状态行）带来数据竞争。
//! - 用轻量 mtime 轮询而非操作系统文件事件（`notify` 库），保持本项目「少依赖、自写轻量组件」的风格
//!   （参见 `router.rs` 的轻量 YAML 解析、`lang.rs` 的轻量国际化）。
//! - 轮询间隔默认 100ms，足够「实时」；因是独立线程，不占用主 TUI 循环时间。

use crate::engine::EngineMsg;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

/// 主线程共享的「当前被监听路径」。`None` 表示当前没有可用的配置文件源。
/// 主线程在每次 `config_source` 变化时更新本单元（见 [`crate::App::note_config_source`]）。
pub type WatchedPath = Arc<Mutex<Option<PathBuf>>>;

/// 默认轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 一轮轮询的结果：更新后的快照 + 是否需要通知主线程。
type Decide = (Option<(PathBuf, Option<SystemTime>)>, bool);

/// 根据旧快照与当前 (路径, mtime) 决定下一步：
///
/// | 情形 | 新快照 | 是否通知 |
/// | --- | --- | --- |
/// | 无路径 | 清空 | 否 |
/// | 首次见到路径（快照为 None） | 设为该路径+mtime | 否 |
/// | 切换成了另一文件 | 设为新文件+mtime | 否（换文件不误报） |
/// | 同一文件且 mtime 未变 | 不变 | 否 |
/// | 同一文件且 mtime 变了（含被删，mtime 变 None） | 更新为该 mtime | 是 |
///
/// 纯函数，便于单元测试；线程循环每轮调用它。
fn decide(
    snap: &Option<(PathBuf, Option<SystemTime>)>,
    path: Option<PathBuf>,
    mtime: Option<SystemTime>,
) -> Decide {
    match (snap, path) {
        // 当前无被监听文件：清空快照。
        (_, None) => (None, false),
        // 快照与当前是同一文件。
        (Some(old), Some(p)) if old.0 == p => {
            if old.1 == mtime {
                // mtime 未变：不动。
                (Some((p, mtime)), false)
            } else {
                // mtime 变了（含被删，mtime 变 None）：更新并通知。
                (Some((p, mtime)), true)
            }
        }
        // 首次见到路径 / 切换到别的文件：重置快照，不通知。
        (_, Some(p)) => (Some((p, mtime)), false),
    }
}

/// 启动配置监听守护线程。
///
/// 每 [`POLL_INTERVAL`] 检查一次当前被监听路径的 mtime，用 [`decide`] 决定是否通知：
/// 文件被**外部改写**（mtime 变化）或**被删除**时，发送 `EngineMsg::ConfigReload`，
/// 由主线程复用 `hot_read()` 完成热重载；换文件/首次出现只重置内部快照，不误报。
///
/// 返回的 `JoinHandle` 被调用方丢弃，因此该线程是**守护线程**：主函数/主线程结束时进程退出，
/// 线程随之终止，不会阻塞程序退出。
pub fn start_watcher(watched: WatchedPath, tx: Sender<EngineMsg>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // 内部快照：上一次看到的 (路径, 修改时间)。用于比对是否变化 / 是否换文件。
        let mut snap: Option<(PathBuf, Option<SystemTime>)> = None;
        loop {
            // 取出当前被监听路径，并读取其 mtime（文件不存在/不可读 → None，视为变化信号）。
            let (path, mtime) = {
                let w = watched.lock().unwrap();
                match w.as_ref() {
                    Some(p) => {
                        let mt = fs::metadata(p).and_then(|m| m.modified()).ok();
                        (Some(p.clone()), mt)
                    }
                    None => (None, None),
                }
            };

            let (new_snap, notify) = decide(&snap, path, mtime);
            snap = new_snap;
            if notify {
                let _ = tx.send(EngineMsg::ConfigReload);
            }

            thread::sleep(POLL_INTERVAL);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(p: &str, t: Option<u64>) -> Option<(PathBuf, Option<SystemTime>)> {
        Some((PathBuf::from(p), t.map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s))))
    }

    #[test]
    fn first_path_resets_without_notify() {
        let (s, n) = decide(&None, Some(PathBuf::from("a.json")), Some(SystemTime::UNIX_EPOCH));
        assert_eq!(s, snap("a.json", Some(0)));
        assert!(!n);
    }

    #[test]
    fn no_path_clears_snapshot() {
        let (s, n) = decide(&snap("a.json", Some(1)), None, None);
        assert_eq!(s, None);
        assert!(!n);
    }

    #[test]
    fn same_path_unchanged_no_notify() {
        let (s, n) = decide(&snap("a.json", Some(1)), Some(PathBuf::from("a.json")), Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1)));
        assert_eq!(s, snap("a.json", Some(1)));
        assert!(!n);
    }

    #[test]
    fn same_path_changed_notifies() {
        let (s, n) = decide(&snap("a.json", Some(1)), Some(PathBuf::from("a.json")), Some(SystemTime::UNIX_EPOCH + Duration::from_secs(2)));
        assert_eq!(s, snap("a.json", Some(2)));
        assert!(n);
    }

    #[test]
    fn path_switched_resets_without_notify() {
        let (s, n) = decide(&snap("a.json", Some(1)), Some(PathBuf::from("b.json")), Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1)));
        assert_eq!(s, snap("b.json", Some(1)));
        assert!(!n);
    }

    #[test]
    fn file_deleted_notifies() {
        // mtime 从 Some 变成 None（文件被删）→ 视为变化，通知。
        let (s, n) = decide(&snap("a.json", Some(1)), Some(PathBuf::from("a.json")), None);
        assert_eq!(s, snap("a.json", None));
        assert!(n);
    }

    /// 端到端：把守护线程、mtime 检测、消息通知串起来验证。
    /// 修改文件后应收到 `ConfigReload`（线程会以 daemon 方式泄漏，随测试进程退出终止）。
    #[test]
    fn watcher_thread_notifies_on_file_change() {
        use std::sync::mpsc;
        use std::thread;

        let dir = std::env::temp_dir().join("zwt_watcher_test");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cfg.json");
        std::fs::write(&file, r#"{"execution_order":"A"}"#).unwrap();

        let watched: WatchedPath = Arc::new(Mutex::new(Some(file.clone())));
        let (tx, rx) = mpsc::channel();
        let _thread = start_watcher(watched, tx); // daemon：句柄丢弃

        // 让监听线程先基线化首次 mtime，再改动文件。
        thread::sleep(Duration::from_millis(250));
        std::fs::write(&file, r#"{"execution_order":"AB"}"#).unwrap();

        // 应在短时间内收到 ConfigReload（轮询 100ms，给 3s 余量防慢机）。
        let got = rx.recv_timeout(Duration::from_secs(3)).expect("应收到 ConfigReload");
        assert!(matches!(got, EngineMsg::ConfigReload));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
