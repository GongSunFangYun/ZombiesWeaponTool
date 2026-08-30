//! Real-time config-file watcher daemon thread.
//!
//! The main thread publishes the config file currently in use (`config_source`) into the
//! shared [`WatchedPath`]; this daemon thread **periodically polls** that file's modification
//! time (mtime) and, whenever it notices the file was rewritten externally, sends
//! [`EngineMsg::ConfigReload`] so the main thread can hot-reload it via `hot_read()`.
//!
//! Design notes:
//! - **The main thread remains the sole writer/reader of config state** — the daemon only
//!   "detects a change → notifies", so it never touches TUI state (`cfg` / `shared` /
//!   status line) directly, which avoids data races.
//! - A lightweight mtime poll is used instead of OS file events (the `notify` crate), keeping
//!   the "few dependencies, hand-rolled lightweight components" philosophy (see `router.rs`'s
//!   mini YAML parser and `lang.rs`'s lightweight i18n).
//! - The poll interval defaults to 100 ms, which is "real-time" enough; because it runs on its
//!   own thread it doesn't consume the main TUI loop's time.

use crate::engine::EngineMsg;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

/// The path currently being watched, shared with the main thread. `None` means there is no
/// usable config file source right now. The main thread updates this cell whenever
/// `config_source` changes (see [`crate::App::note_config_source`]).
pub type WatchedPath = Arc<Mutex<Option<PathBuf>>>;

/// Default poll interval.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The outcome of one poll: the new snapshot + whether the main thread should be notified.
type Decide = (Option<(PathBuf, Option<SystemTime>)>, bool);

/// Decide what to do next given the old snapshot and the current `(path, mtime)`:
///
/// | Situation | New snapshot | Notify? |
/// | --- | --- | --- |
/// | No path | cleared | no |
/// | First time a path appears (snapshot is `None`) | set to that path + mtime | no |
/// | Switched to a different file | set to the new file + mtime | no (don't misfire on swap) |
/// | Same file, mtime unchanged | unchanged | no |
/// | Same file, mtime changed (incl. deleted → mtime becomes `None`) | updated to that mtime | yes |
///
/// Pure function, so it is easy to unit test; the thread loop calls it each round.
fn decide(
    snap: &Option<(PathBuf, Option<SystemTime>)>,
    path: Option<PathBuf>,
    mtime: Option<SystemTime>,
) -> Decide {
    match (snap, path) {
        // No file to watch: clear the snapshot.
        (_, None) => (None, false),
        // Snapshot and current path are the same file.
        (Some(old), Some(p)) if old.0 == p => {
            if old.1 == mtime {
                // mtime unchanged: do nothing.
                (Some((p, mtime)), false)
            } else {
                // mtime changed (incl. deleted, mtime became None): update and notify.
                (Some((p, mtime)), true)
            }
        }
        // First time we see the path / switched to another file: reset snapshot, no notify.
        (_, Some(p)) => (Some((p, mtime)), false),
    }
}

/// Start the config-watching daemon thread.
///
/// Every [`POLL_INTERVAL`] it checks the mtime of the currently watched path and uses
/// [`decide`] to determine whether to notify: when the file is **rewritten externally**
/// (mtime changed) or **deleted**, it sends `EngineMsg::ConfigReload`, and the main thread
/// hot-reloads it via `hot_read()`. Switching files / a path appearing for the first time only
/// resets the internal snapshot and does not misfire.
///
/// The returned `JoinHandle` is discarded by the caller, so this is a **daemon thread**: when
/// `main`/the main thread returns, the process exits and the thread is terminated — it never
/// blocks shutdown.
pub fn start_watcher(watched: WatchedPath, tx: Sender<EngineMsg>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // Internal snapshot: the last (path, mtime) we saw, used to compare change/swap.
        let mut snap: Option<(PathBuf, Option<SystemTime>)> = None;
        loop {
            // Read the current watched path and its mtime (missing/unreadable → None, treated
            // as a change signal).
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
        // mtime goes from Some to None (file deleted) → treated as a change, so notify.
        let (s, n) = decide(&snap("a.json", Some(1)), Some(PathBuf::from("a.json")), None);
        assert_eq!(s, snap("a.json", None));
        assert!(n);
    }

    /// End-to-end: wire up the daemon thread, mtime detection, and message notification.
    /// Modifying the file should yield a `ConfigReload` (the thread leaks daemon-style and is
    /// terminated when the test process exits).
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
        let _thread = start_watcher(watched, tx); // daemon: handle discarded

        // Let the listener baseline the initial mtime first, then modify the file.
        thread::sleep(Duration::from_millis(250));
        std::fs::write(&file, r#"{"execution_order":"AB"}"#).unwrap();

        // Should receive ConfigReload quickly (100 ms poll; 3 s headroom for slow machines).
        let got = rx.recv_timeout(Duration::from_secs(3)).expect("should get ConfigReload");
        assert!(matches!(got, EngineMsg::ConfigReload));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
