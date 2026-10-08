//! Filesystem change notification for retained mdbase read state.
//!
//! The monitor reports *whether anything relevant may have changed* as a
//! generation, and the paths each generation reported in a bounded log. It is
//! never a substitute for reconciliation: a retained proof is reusable only
//! while the monitor stays healthy and its generation is unchanged since
//! before the proving walk began, and a host that re-checks only the logged
//! paths still stats and reconciles them. Any watcher error, rescan request,
//! or overflow marks the monitor unhealthy for good, so callers fall back to
//! strict per-request verification.
//!
//! A [barrier](MdbaseChangeMonitor::barrier) makes every change that
//! completed before it observable: it creates a file under `.vulcan/` and
//! waits for its notification. The platform watchers deliver one watch's
//! events in order, so every earlier change has been observed by then.

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Generations kept in the changed-path log.
const LOGGED_GENERATIONS: usize = 8192;
/// Barrier file names under `.vulcan/`: the prefix, the process, a sequence.
const BARRIER_PREFIX: &str = ".mdbase-monitor-barrier-";

pub struct MdbaseChangeMonitor {
    state: Arc<MonitorState>,
    // Dropping the watcher stops notifications.
    _watcher: RecommendedWatcher,
}

struct MonitorState {
    roots: Vec<PathBuf>,
    generation: AtomicU64,
    healthy: AtomicBool,
    /// Each generation's root-relative paths, oldest first; `None` for an
    /// event that named none. Advanced under this lock with `generation`.
    log: Mutex<VecDeque<(u64, Option<Vec<String>>)>>,
    /// The highest barrier observed, and its waiters.
    barrier_seen: Mutex<u64>,
    barrier_observed: Condvar,
    barrier_next: AtomicU64,
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
            log: Mutex::default(),
            barrier_seen: Mutex::new(0),
            barrier_observed: Condvar::new(),
            barrier_next: AtomicU64::new(1),
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

    /// The root-relative paths reported after generation `from` up to and
    /// including `to`; `None` when the log no longer reaches back to `from`,
    /// an event named no paths, or the monitor is unhealthy.
    #[must_use]
    pub fn changed_between(&self, from: u64, to: u64) -> Option<BTreeSet<String>> {
        if from == to {
            return self.generation().map(|_| BTreeSet::new());
        }
        let log = lock(&self.state.log);
        if log.front().is_none_or(|(oldest, _)| *oldest > from + 1) {
            return None;
        }
        let mut changed = BTreeSet::new();
        for (generation, paths) in log.iter() {
            if *generation <= from || *generation > to {
                continue;
            }
            changed.extend(paths.as_ref()?.iter().cloned());
        }
        self.generation().map(|_| changed)
    }

    /// Wait until every change that completed before this call has been
    /// observed: create a barrier file under the root's `.vulcan/` and wait
    /// for its notification. `false` when that is impossible within
    /// `timeout`, or the monitor is unhealthy.
    #[must_use]
    pub fn barrier(&self, timeout: Duration) -> bool {
        let Some(root) = self.state.roots.first() else {
            return false;
        };
        let sequence = self.state.barrier_next.fetch_add(1, Ordering::SeqCst);
        let file = root
            .join(".vulcan")
            .join(format!("{BARRIER_PREFIX}{}-{sequence}", std::process::id()));
        if std::fs::write(&file, b"").is_err() {
            return false;
        }
        let deadline = Instant::now() + timeout;
        let mut seen = lock(&self.state.barrier_seen);
        while *seen < sequence {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            seen = self
                .state
                .barrier_observed
                .wait_timeout(seen, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        let reached = *seen >= sequence;
        drop(seen);
        let _ = std::fs::remove_file(&file);
        reached && self.generation().is_some()
    }

    #[cfg(test)]
    fn invalidate(&self) {
        self.state.healthy.store(false, Ordering::SeqCst);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl MonitorState {
    fn observe(&self, event: notify::Result<Event>) {
        let Ok(event) = event else {
            self.healthy.store(false, Ordering::SeqCst);
            self.advance(None);
            return;
        };
        if event.need_rescan() {
            self.healthy.store(false, Ordering::SeqCst);
        }
        if let Some(sequence) = event
            .paths
            .iter()
            .filter_map(|path| Self::barrier(path))
            .max()
        {
            let mut seen = lock(&self.barrier_seen);
            *seen = (*seen).max(sequence);
            self.barrier_observed.notify_all();
        }
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        if event.paths.is_empty() {
            self.advance(None);
        } else if event.paths.iter().any(|path| !self.is_derived(path)) {
            let paths = event
                .paths
                .iter()
                .filter(|path| !self.is_derived(path))
                .map(|path| self.relative(path))
                .collect::<Option<Vec<_>>>();
            self.advance(paths);
        }
    }

    /// Start a generation that reported `paths`.
    fn advance(&self, paths: Option<Vec<String>>) {
        let mut log = lock(&self.log);
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        log.push_back((generation, paths));
        while log.len() > LOGGED_GENERATIONS {
            log.pop_front();
        }
    }

    /// `path` relative to the root with `/` separators; `None` outside it
    /// or when not UTF-8.
    fn relative(&self, path: &Path) -> Option<String> {
        self.roots.iter().find_map(|root| {
            let relative = path.strip_prefix(root).ok()?;
            let parts = relative
                .components()
                .map(|component| match component {
                    Component::Normal(name) => name.to_str(),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            Some(parts.join("/"))
        })
    }

    /// This process's barrier sequence, when `path` is one of its barriers.
    fn barrier(path: &Path) -> Option<u64> {
        let name = path.file_name()?.to_str()?;
        let rest = name.strip_prefix(BARRIER_PREFIX)?;
        let (process, sequence) = rest.split_once('-')?;
        (process == std::process::id().to_string()).then(|| sequence.parse().ok())?
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

    #[test]
    fn the_log_names_changed_paths_and_barriers_observe_earlier_changes() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join(".vulcan")).unwrap();
        std::fs::create_dir_all(root.join("notes")).unwrap();
        let monitor = MdbaseChangeMonitor::watch(&root).unwrap();
        let start = monitor.generation().unwrap();
        std::fs::write(root.join("notes/a.md"), "a").unwrap();
        std::fs::write(root.join("b.md"), "b").unwrap();
        // The barrier returns only after both writes were observed.
        assert!(monitor.barrier(Duration::from_secs(5)));
        let now = monitor.generation().unwrap();
        let changed = monitor.changed_between(start, now).unwrap();
        assert!(
            changed.contains("notes/a.md") && changed.contains("b.md"),
            "{changed:?}"
        );
        // Barrier files are derived: they name no change and are removed.
        assert!(!changed.iter().any(|path| path.contains("barrier")));
        assert!(std::fs::read_dir(root.join(".vulcan"))
            .unwrap()
            .next()
            .is_none());
        assert_eq!(monitor.changed_between(now, now), Some(BTreeSet::new()));
        // A range the log no longer covers is unknown.
        assert_eq!(monitor.changed_between(0, now + 5).map(|_| ()), Some(()));
        {
            let mut log = lock(&monitor.state.log);
            log.pop_front();
        }
        assert_eq!(monitor.changed_between(start, now), None);
        monitor.invalidate();
        assert!(!monitor.barrier(Duration::from_millis(100)));
    }
}
