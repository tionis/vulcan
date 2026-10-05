//! Filesystem change notification for retained mdbase read state.
//!
//! The monitor only reports *whether anything relevant may have changed*; it
//! never says what changed and is never a substitute for reconciliation. A
//! retained proof is reusable only while the monitor stays healthy and its
//! generation is unchanged since before the proving walk began. Any watcher
//! error, rescan request, or overflow marks the monitor unhealthy for good, so
//! callers fall back to strict per-request verification.

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

pub struct MdbaseChangeMonitor {
    state: Arc<MonitorState>,
    // Dropping the watcher stops notifications.
    _watcher: RecommendedWatcher,
}

struct MonitorState {
    roots: Vec<PathBuf>,
    generation: AtomicU64,
    healthy: AtomicBool,
}

impl MdbaseChangeMonitor {
    /// Watch `root` recursively. Events confined to derived directories
    /// (`.vulcan`, `.mdbase`, `.git`, `node_modules`) are ignored: caches and
    /// locks live there and records never do.
    pub fn watch(root: &Path) -> Result<Self, notify::Error> {
        // Notifications may report either spelling of the root.
        let mut roots = vec![root.to_path_buf()];
        if let Ok(canonical) = std::fs::canonicalize(root) {
            if canonical != root {
                roots.push(canonical);
            }
        }
        let state = Arc::new(MonitorState {
            roots,
            generation: AtomicU64::new(0),
            healthy: AtomicBool::new(true),
        });
        let callback_state = Arc::clone(&state);
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            callback_state.observe(event);
        })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(Self {
            state,
            _watcher: watcher,
        })
    }

    /// The current change generation, or `None` once the monitor can no
    /// longer vouch for having seen every change.
    #[must_use]
    pub fn generation(&self) -> Option<u64> {
        // Read the generation first: a concurrent failure that raced this
        // read is caught by the health check below or by the next read.
        let generation = self.state.generation.load(Ordering::SeqCst);
        self.state
            .healthy
            .load(Ordering::SeqCst)
            .then_some(generation)
    }

    #[cfg(test)]
    fn invalidate(&self) {
        self.state.healthy.store(false, Ordering::SeqCst);
    }
}

impl MonitorState {
    fn observe(&self, event: notify::Result<Event>) {
        let Ok(event) = event else {
            self.healthy.store(false, Ordering::SeqCst);
            self.generation.fetch_add(1, Ordering::SeqCst);
            return;
        };
        if event.need_rescan() {
            self.healthy.store(false, Ordering::SeqCst);
        }
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        if event.paths.is_empty() || event.paths.iter().any(|path| !self.is_derived(path)) {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn is_derived(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| {
            path.strip_prefix(root).is_ok_and(|relative| {
                relative.components().any(|component| {
                    matches!(
                        component,
                        Component::Normal(name)
                            if [".vulcan", ".mdbase", ".git", "node_modules"]
                                .iter()
                                .any(|derived| name == *derived)
                    )
                })
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_for(condition: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn record_changes_advance_and_derived_changes_do_not() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join(".vulcan")).unwrap();
        std::fs::create_dir_all(root.join("notes")).unwrap();
        let monitor = MdbaseChangeMonitor::watch(&root).unwrap();
        let start = monitor.generation().unwrap();
        std::fs::write(root.join(".vulcan/cache.db"), "derived").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(monitor.generation(), Some(start));
        std::fs::write(root.join("notes/a.md"), "record").unwrap();
        assert!(wait_for(|| monitor
            .generation()
            .is_some_and(|value| value > start)));
        monitor.invalidate();
        assert_eq!(monitor.generation(), None);
    }
}
