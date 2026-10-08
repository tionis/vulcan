use crate::{scan_vault, ScanError, ScanMode, ScanSummary, VaultPaths};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const WATCH_SAFETY_RESCAN_INTERVAL: Duration = Duration::from_secs(30);
const WATCH_FALLBACK_POLL_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum WatchError {
    Callback(String),
    ChannelClosed,
    Notify(notify::Error),
    Scan(ScanError),
}

impl Display for WatchError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Callback(message) => formatter.write_str(message),
            Self::ChannelClosed => formatter.write_str("watch channel closed unexpectedly"),
            Self::Notify(error) => write!(formatter, "{error}"),
            Self::Scan(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for WatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Notify(error) => Some(error),
            Self::Scan(error) => Some(error),
            Self::Callback(_) | Self::ChannelClosed => None,
        }
    }
}

impl From<notify::Error> for WatchError {
    fn from(error: notify::Error) -> Self {
        Self::Notify(error)
    }
}

impl From<ScanError> for WatchError {
    fn from(error: ScanError) -> Self {
        Self::Scan(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchOptions {
    pub debounce_ms: u64,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self { debounce_ms: 250 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatchReport {
    pub startup: bool,
    pub event_count: usize,
    pub paths: Vec<String>,
    pub created_paths: Vec<String>,
    pub summary: ScanSummary,
}

#[derive(Debug, Default)]
struct WatchBatch {
    safety_rescan: bool,
    event_count: usize,
    paths: BTreeSet<String>,
    created_paths: BTreeSet<String>,
}

pub fn watch_vault<F, E>(
    paths: &VaultPaths,
    options: &WatchOptions,
    on_report: F,
) -> Result<(), WatchError>
where
    F: FnMut(WatchReport) -> Result<(), E>,
    E: Display,
{
    watch_vault_until(paths, options, || false, on_report)
}

pub fn watch_vault_until<F, S, E>(
    paths: &VaultPaths,
    options: &WatchOptions,
    should_stop: S,
    mut on_report: F,
) -> Result<(), WatchError>
where
    F: FnMut(WatchReport) -> Result<(), E>,
    S: Fn() -> bool,
    E: Display,
{
    let (sender, receiver) = mpsc::channel::<notify::Result<Event>>();
    let watcher: Result<RecommendedWatcher, _> = notify::recommended_watcher(move |event| {
        let _ = sender.send(event);
    });
    if let Ok(mut watcher) = watcher {
        if watcher
            .watch(paths.vault_root(), RecursiveMode::Recursive)
            .is_ok()
        {
            match watch_vault_until_with_registered_watcher(
                paths,
                *options,
                &should_stop,
                &mut on_report,
                watcher,
                &receiver,
                WATCH_SAFETY_RESCAN_INTERVAL,
            ) {
                Ok(()) => return Ok(()),
                Err(error) if recoverable_native_watch_error(&error) && !should_stop() => {}
                Err(error) => return Err(error),
            }
        }
    }

    watch_vault_until_polling(paths, *options, should_stop, on_report)
}

fn recoverable_native_watch_error(error: &WatchError) -> bool {
    matches!(error, WatchError::Notify(_) | WatchError::ChannelClosed)
}

fn watch_vault_until_polling<F, S, E>(
    paths: &VaultPaths,
    options: WatchOptions,
    should_stop: S,
    on_report: F,
) -> Result<(), WatchError>
where
    F: FnMut(WatchReport) -> Result<(), E>,
    S: Fn() -> bool,
    E: Display,
{
    watch_vault_until_polling_with_interval(
        paths,
        options,
        should_stop,
        on_report,
        WATCH_FALLBACK_POLL_INTERVAL,
    )
}

fn watch_vault_until_polling_with_interval<F, S, E>(
    paths: &VaultPaths,
    options: WatchOptions,
    should_stop: S,
    on_report: F,
    poll_interval: Duration,
) -> Result<(), WatchError>
where
    F: FnMut(WatchReport) -> Result<(), E>,
    S: Fn() -> bool,
    E: Display,
{
    let (sender, receiver) = mpsc::channel::<notify::Result<Event>>();
    let watcher = PrunedPoller::start(paths.vault_root(), poll_interval, sender)?;
    // Registration builds the initial comparison snapshot synchronously; the
    // startup scan observes any edits made while that snapshot was assembled.
    watch_vault_until_with_registered_watcher(
        paths,
        options,
        should_stop,
        on_report,
        watcher,
        &receiver,
        WATCH_SAFETY_RESCAN_INTERVAL,
    )
}

fn watch_vault_until_with_registered_watcher<F, S, E, W>(
    paths: &VaultPaths,
    options: WatchOptions,
    should_stop: S,
    mut on_report: F,
    _watcher: W,
    receiver: &mpsc::Receiver<notify::Result<Event>>,
    safety_interval: Duration,
) -> Result<(), WatchError>
where
    F: FnMut(WatchReport) -> Result<(), E>,
    S: Fn() -> bool,
    E: Display,
{
    let startup_summary = scan_vault(paths, ScanMode::Incremental)?;
    on_report(WatchReport {
        startup: true,
        event_count: 0,
        paths: Vec::new(),
        created_paths: Vec::new(),
        summary: startup_summary,
    })
    .map_err(|error| WatchError::Callback(error.to_string()))?;

    let debounce = Duration::from_millis(options.debounce_ms);
    let mut last_safety_scan = Instant::now();
    'watch: loop {
        if should_stop() {
            return Ok(());
        }

        let mut batch = WatchBatch::default();
        loop {
            if should_stop() {
                return Ok(());
            }
            if last_safety_scan.elapsed() >= safety_interval {
                let summary = scan_vault(paths, ScanMode::Incremental)?;
                last_safety_scan = Instant::now();
                if scan_summary_changed(&summary) {
                    on_report(WatchBatch::default().into_report(summary))
                        .map_err(|error| WatchError::Callback(error.to_string()))?;
                    continue 'watch;
                }
            }

            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(event) => match event {
                    Ok(event) => {
                        if batch.push(paths, event) {
                            break;
                        }
                    }
                    Err(error) if notify_error_is_internal(paths, &error) => {}
                    Err(error) => return Err(WatchError::Notify(error)),
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(WatchError::ChannelClosed),
            }
        }

        let batch_started = Instant::now();
        let mut deadline = debounce_deadline(batch_started, batch_started, debounce);
        loop {
            if should_stop() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                break;
            }

            let timeout = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(50));
            match receiver.recv_timeout(timeout) {
                Ok(Ok(event)) => {
                    if batch.push(paths, event) {
                        deadline = debounce_deadline(batch_started, Instant::now(), debounce);
                    }
                }
                Ok(Err(error)) if notify_error_is_internal(paths, &error) => {}
                Ok(Err(error)) => return Err(WatchError::Notify(error)),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if Instant::now() >= deadline {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(WatchError::ChannelClosed),
            }
        }

        let summary = if batch.safety_rescan || last_safety_scan.elapsed() >= safety_interval {
            last_safety_scan = Instant::now();
            scan_vault(paths, ScanMode::Incremental)?
        } else {
            crate::scan::scan_watched_paths(paths, &batch.paths)?
        };
        on_report(batch.into_report(summary))
            .map_err(|error| WatchError::Callback(error.to_string()))?;
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PollFingerprint {
    directory: bool,
    modified: Option<std::time::SystemTime>,
    hash: Option<blake3::Hash>,
}

type PollInventory = BTreeMap<PathBuf, PollFingerprint>;

// notify 8.2's PollWatcher has no pre-traversal exclusion hook. Keep a
// content-comparing inventory of canonical paths instead, with streaming reads.
fn polling_inventory(
    root: &Path,
    stopped: &std::sync::atomic::AtomicBool,
    mut visited: impl FnMut(&Path, bool),
) -> notify::Result<PollInventory> {
    let mut inventory = PollInventory::new();
    let walker = ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .follow_links(true)
        .filter_entry(|entry| {
            entry.depth() == 0 || !matches!(entry.file_name().to_str(), Some(".git" | ".vulcan"))
        })
        .build();
    let mut paths = Vec::new();
    // These configuration files are canonical inputs inside an otherwise private
    // directory. Probe them directly without enumerating cache/history contents.
    for name in ["config.toml", "config.local.toml"] {
        paths.push(root.join(".vulcan").join(name));
    }
    let entries = walker.map(|entry| {
        entry.map(ignore::DirEntry::into_path).map_err(|error| {
            notify::Error::generic(&error.to_string()).add_path(root.to_path_buf())
        })
    });
    for path in entries.chain(paths.into_iter().map(Ok)) {
        if stopped.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }
        let path = path?;
        visited(&path, false);
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(notify::Error::io(error).add_path(path)),
        };
        let hash = if metadata.is_file() {
            visited(&path, true);
            let mut file = std::fs::File::open(&path)
                .map_err(|error| notify::Error::io(error).add_path(path.clone()))?;
            let mut hasher = blake3::Hasher::new();
            let mut buffer = [0; 16 * 1024];
            loop {
                if stopped.load(std::sync::atomic::Ordering::Acquire) {
                    return Ok(inventory);
                }
                let count = match file.read(&mut buffer) {
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    result => {
                        result.map_err(|error| notify::Error::io(error).add_path(path.clone()))?
                    }
                };
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
            }
            Some(hasher.finalize())
        } else {
            None
        };
        inventory.insert(
            path,
            PollFingerprint {
                directory: metadata.is_dir(),
                modified: metadata.modified().ok(),
                hash,
            },
        );
    }
    Ok(inventory)
}

fn inventory_events(previous: &PollInventory, current: &PollInventory) -> Vec<Event> {
    use notify::event::{CreateKind, ModifyKind, RemoveKind};
    let mut events = Vec::new();
    for (path, fingerprint) in current {
        let kind = match previous.get(path) {
            None => EventKind::Create(CreateKind::Any),
            Some(old) if old != fingerprint => EventKind::Modify(ModifyKind::Any),
            _ => continue,
        };
        events.push(Event::new(kind).add_path(path.clone()));
    }
    for path in previous.keys().filter(|path| !current.contains_key(*path)) {
        events.push(Event::new(EventKind::Remove(RemoveKind::Any)).add_path(path.clone()));
    }
    events
}

struct PrunedPoller {
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    wake: mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PrunedPoller {
    fn start(
        root: &Path,
        interval: Duration,
        sender: mpsc::Sender<notify::Result<Event>>,
    ) -> notify::Result<Self> {
        let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut previous = polling_inventory(root, &stopped, |_, _| {})?;
        let root = root.to_path_buf();
        let worker_stop = stopped.clone();
        let (wake, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("vulcan-poll".into())
            .spawn(move || {
                while matches!(
                    receiver.recv_timeout(interval),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    let result = polling_inventory(&root, &worker_stop, |_, _| {});
                    if worker_stop.load(std::sync::atomic::Ordering::Acquire) {
                        break;
                    }
                    match result {
                        Ok(current) => {
                            for event in inventory_events(&previous, &current) {
                                if sender.send(Ok(event)).is_err() {
                                    return;
                                }
                            }
                            previous = current;
                        }
                        Err(error) => {
                            // Do not install a partial inventory or infer deletions.
                            if sender.send(Err(error)).is_err() {
                                return;
                            }
                        }
                    }
                }
            })
            .map_err(notify::Error::io)?;
        Ok(Self {
            stopped,
            wake,
            thread: Some(thread),
        })
    }
}

impl Drop for PrunedPoller {
    fn drop(&mut self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        let _ = self.wake.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn scan_summary_changed(summary: &ScanSummary) -> bool {
    summary.added != 0 || summary.updated != 0 || summary.deleted != 0
}

fn debounce_deadline(started: Instant, now: Instant, debounce: Duration) -> Instant {
    (now + debounce).min(started + debounce.max(Duration::from_secs(2)))
}

fn notify_error_is_internal(paths: &VaultPaths, error: &notify::Error) -> bool {
    !error.paths.is_empty()
        && error
            .paths
            .iter()
            .all(|path| normalize_watch_path(paths, path).is_none())
}

impl WatchBatch {
    fn push(&mut self, paths: &VaultPaths, event: Event) -> bool {
        let rescan = event.need_rescan();
        self.safety_rescan |= rescan;
        if matches!(event.kind, EventKind::Access(_)) && !rescan {
            return false;
        }

        let created = matches!(event.kind, EventKind::Create(_));
        let mut added = rescan;
        for path in event.paths {
            if relative_watch_path(paths, &path).is_some_and(|relative| {
                relative == Path::new(".vulcan").join("config.toml")
                    || relative == Path::new(".vulcan").join("config.local.toml")
            }) {
                self.safety_rescan = true;
                added = true;
            }
            let Some(relative_path) = normalize_watch_path(paths, &path) else {
                continue;
            };
            if created {
                self.created_paths.insert(relative_path.clone());
            }
            self.paths.insert(relative_path);
            added = true;
        }

        if added {
            self.event_count += 1;
        }

        added
    }

    fn into_report(self, summary: ScanSummary) -> WatchReport {
        WatchReport {
            startup: false,
            event_count: self.event_count,
            paths: self.paths.into_iter().collect(),
            created_paths: self.created_paths.into_iter().collect(),
            summary,
        }
    }
}

fn normalize_watch_path(paths: &VaultPaths, path: &Path) -> Option<String> {
    let relative = relative_watch_path(paths, path)?;
    let normalized = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::CurDir => None,
            other => Some(other.as_os_str().to_string_lossy().into_owned()),
        })
        .collect::<Vec<_>>();
    let configuration = normalized.len() == 2
        && normalized[0] == ".vulcan"
        && matches!(normalized[1].as_str(), "config.toml" | "config.local.toml");
    if normalized.is_empty()
        || (!configuration
            && normalized
                .first()
                .is_some_and(|part| matches!(part.as_str(), ".vulcan" | ".git")))
    {
        return None;
    }

    Some(normalized.join("/"))
}

fn relative_watch_path(paths: &VaultPaths, path: &Path) -> Option<PathBuf> {
    paths
        .relative_to_vault(path)
        .or_else(|| windows_relative_watch_path(paths, path))
        .or_else(|| canonical_relative_watch_path(paths, path))
}

/// Backends that watch the canonical root (macOS `FSEvents`) report canonical
/// paths, so a vault opened through a symlink (`/var` is `/private/var` on
/// macOS) would otherwise drop every event.
fn canonical_relative_watch_path(paths: &VaultPaths, path: &Path) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(paths.vault_root()).ok()?;
    path.strip_prefix(canonical).ok().map(Path::to_path_buf)
}

#[cfg(windows)]
fn windows_relative_watch_path(paths: &VaultPaths, path: &Path) -> Option<PathBuf> {
    strip_windows_verbatim_prefix(path).and_then(|normalized| paths.relative_to_vault(&normalized))
}

#[cfg(not(windows))]
fn windows_relative_watch_path(_: &VaultPaths, _: &Path) -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn strip_windows_verbatim_prefix(path: &Path) -> Option<PathBuf> {
    path.as_os_str()
        .to_string_lossy()
        .strip_prefix(r"\\?\")
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, CreateKind, ModifyKind};
    use notify::Config;
    use tempfile::TempDir;

    #[cfg(unix)]
    #[test]
    fn events_reported_under_the_canonical_root_are_kept() {
        let temporary = TempDir::new().unwrap();
        let real = temporary.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = temporary.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let paths = VaultPaths::new(&link);
        let canonical = real.canonicalize().unwrap().join("notes/A.md");
        let mut batch = WatchBatch::default();
        assert!(batch.push(
            &paths,
            Event::new(EventKind::Create(CreateKind::File)).add_path(canonical)
        ));
        assert_eq!(batch.created_paths, ["notes/A.md".to_string()].into());
    }

    #[test]
    fn config_events_reconcile_on_both_backends_without_admitting_transients() {
        let temporary = TempDir::new().unwrap();
        let paths = VaultPaths::new(temporary.path());
        for name in ["config.toml", "config.local.toml"] {
            let path = temporary.path().join(".vulcan").join(name);
            let mut batch = WatchBatch::default();
            assert!(batch.push(
                &paths,
                Event::new(EventKind::Modify(ModifyKind::Any)).add_path(path.clone())
            ));
            assert!(batch.safety_rescan);
            assert_eq!(batch.paths, [format!(".vulcan/{name}")].into());
            assert!(!notify_error_is_internal(
                &paths,
                &notify::Error::generic("configuration unreadable").add_path(path)
            ));
        }
        let transient = temporary.path().join(".vulcan/cache.db-wal");
        let mut batch = WatchBatch::default();
        assert!(!batch.push(
            &paths,
            Event::new(EventKind::Modify(ModifyKind::Any)).add_path(transient.clone())
        ));
        assert!(!batch.safety_rescan);
        assert!(notify_error_is_internal(
            &paths,
            &notify::Error::generic("transient").add_path(transient)
        ));
    }

    #[test]
    fn pruned_poller_reconciles_registration_gap_and_joins_on_shutdown() {
        let temporary = TempDir::new().unwrap();
        let paths = VaultPaths::new(temporary.path());
        std::fs::create_dir_all(paths.vulcan_dir()).unwrap();
        let note = temporary.path().join("Home.md");
        std::fs::write(&note, "old").unwrap();
        scan_vault(&paths, ScanMode::Full).unwrap();
        let (sender, receiver) = mpsc::channel();
        let watcher =
            PrunedPoller::start(temporary.path(), Duration::from_secs(30), sender).unwrap();
        std::fs::write(note, "changed during registration").unwrap();
        let mut reports = Vec::new();
        watch_vault_until_with_registered_watcher(
            &paths,
            WatchOptions::default(),
            || true,
            |report| {
                reports.push(report);
                Ok::<_, std::convert::Infallible>(())
            },
            watcher,
            &receiver,
            WATCH_SAFETY_RESCAN_INTERVAL,
        )
        .unwrap();
        assert_eq!(reports.len(), 1);
        assert!(reports[0].startup);
        assert_eq!(reports[0].summary.updated, 1);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn polling_inventory_prunes_internal_io_before_hashing() {
        let temporary = TempDir::new().unwrap();
        let root = temporary.path();
        for directory in [".git/objects", ".vulcan/history", ".obsidian/plugins/test"] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        for index in 0..512 {
            for directory in [".git/objects", ".vulcan/history"] {
                std::fs::write(root.join(format!("{directory}/{index}")), vec![b'x'; 4096])
                    .unwrap();
            }
        }
        for path in [
            "Home.md",
            ".gitignore",
            ".obsidian/plugins/test/data.json",
            ".vulcan/config.toml",
            ".vulcan/config.local.toml",
        ] {
            std::fs::write(root.join(path), "test").unwrap();
        }
        let baseline = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = baseline.clone();
        let mut old = notify::PollWatcher::with_initial_scan(
            |_| {},
            Config::default()
                .with_manual_polling()
                .with_compare_contents(true),
            move |path: notify::Result<PathBuf>| {
                observed.lock().unwrap().push(path.unwrap());
            },
        )
        .unwrap();
        old.watch(root, RecursiveMode::Recursive).unwrap();
        let old_files = baseline
            .lock()
            .unwrap()
            .iter()
            .filter(|path| path.is_file())
            .count();
        let mut visited = Vec::new();
        let mut hashed = Vec::new();
        let inventory = polling_inventory(root, &false.into(), |path, read| {
            if read {
                hashed.push(path.to_path_buf());
            } else {
                visited.push(path.to_path_buf());
            }
        })
        .unwrap();
        assert_eq!(old_files, 1029);
        assert_eq!(hashed.len(), 5);
        assert_eq!(visited.len(), 9); // root, three Obsidian dirs, five files
        assert_eq!(inventory.len(), 9);
        assert!(!visited
            .iter()
            .any(|path| path.starts_with(root.join(".git/objects"))
                || path.starts_with(root.join(".vulcan/history"))));
        eprintln!("poll fixture: 1024 internal x 4096 bytes; hash reads {old_files} -> {}; visited {} -> {}", hashed.len(), baseline.lock().unwrap().len(), visited.len());
    }

    #[test]
    fn polling_inventory_detects_preserved_mtime_atomic_and_structural_changes() {
        let temporary = TempDir::new().unwrap();
        let root = temporary.path();
        std::fs::create_dir_all(root.join(".vulcan")).unwrap();
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        for path in [
            "Home.md",
            "Atomic.md",
            "Delete.md",
            "Rename.md",
            ".gitignore",
            ".obsidian/app.json",
            ".vulcan/config.toml",
        ] {
            std::fs::write(root.join(path), "old").unwrap();
        }
        let before = polling_inventory(root, &false.into(), |_, _| {}).unwrap();
        let home = root.join("Home.md");
        let mtime = std::fs::metadata(&home).unwrap().modified().unwrap();
        std::fs::write(&home, "new").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&home)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        std::fs::write(root.join("replacement"), "replacement").unwrap();
        std::fs::remove_file(root.join("Atomic.md")).unwrap();
        std::fs::rename(root.join("replacement"), root.join("Atomic.md")).unwrap();
        std::fs::remove_file(root.join("Delete.md")).unwrap();
        std::fs::rename(root.join("Rename.md"), root.join("Renamed.md")).unwrap();
        std::fs::create_dir(root.join("new-dir")).unwrap();
        std::fs::write(root.join("new-dir/New.md"), "new").unwrap();
        for path in [".gitignore", ".obsidian/app.json", ".vulcan/config.toml"] {
            std::fs::write(root.join(path), "new").unwrap();
        }
        let after = polling_inventory(root, &false.into(), |_, _| {}).unwrap();
        let events = inventory_events(&before, &after);
        for path in [
            "Home.md",
            "Atomic.md",
            "Delete.md",
            "Rename.md",
            "Renamed.md",
            "new-dir",
            "new-dir/New.md",
            ".gitignore",
            ".obsidian/app.json",
            ".vulcan/config.toml",
        ] {
            assert!(
                events.iter().any(|event| event.paths == [root.join(path)]),
                "{path}"
            );
        }
        let paths = VaultPaths::new(root);
        let mut batch = WatchBatch::default();
        for event in events {
            batch.push(&paths, event);
        }
        assert!(batch.safety_rescan);
        assert!(batch.created_paths.contains("new-dir/New.md"));
        assert!(inventory_events(&after, &after).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn polling_inventory_follows_root_and_file_symlinks_like_notify() {
        let temporary = TempDir::new().unwrap();
        let root = temporary.path().join("vault");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(temporary.path().join("outside.md"), "old").unwrap();
        std::os::unix::fs::symlink(temporary.path().join("outside.md"), root.join("Link.md"))
            .unwrap();
        let alias = temporary.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let before = polling_inventory(&alias, &false.into(), |_, _| {}).unwrap();
        std::fs::write(temporary.path().join("outside.md"), "new").unwrap();
        let after = polling_inventory(&alias, &false.into(), |_, _| {}).unwrap();
        assert!(inventory_events(&before, &after)
            .iter()
            .any(|event| event.paths == [alias.join("Link.md")]));
    }

    #[test]
    fn continuous_changes_have_a_bounded_batch_age() {
        let start = Instant::now();
        let debounce = Duration::from_millis(250);
        assert_eq!(debounce_deadline(start, start, debounce), start + debounce);
        assert_eq!(
            debounce_deadline(start, start + Duration::from_millis(1900), debounce),
            start + Duration::from_secs(2)
        );
        assert_eq!(
            debounce_deadline(
                start,
                start + Duration::from_secs(1),
                Duration::from_secs(3)
            ),
            start + Duration::from_secs(3)
        );
    }

    #[test]
    fn watch_batch_ignores_access_events_and_internal_paths() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        let mut batch = WatchBatch::default();

        assert!(!batch.push(
            &paths,
            Event {
                kind: EventKind::Access(AccessKind::Any),
                paths: vec![temp_dir.path().join("Notes/Alpha.md")],
                ..Event::default()
            }
        ));
        assert!(!batch.push(
            &paths,
            Event {
                kind: EventKind::Modify(ModifyKind::Any),
                paths: vec![temp_dir.path().join(".vulcan/cache.db")],
                ..Event::default()
            }
        ));
        assert_eq!(batch.event_count, 0);
        assert!(batch.paths.is_empty());
        assert!(!batch.push(
            &paths,
            Event::new(EventKind::Modify(ModifyKind::Any))
                .add_path(temp_dir.path().join(".git/index"))
        ));
        assert!(batch.created_paths.is_empty());
    }

    #[test]
    fn watch_batch_deduplicates_paths_across_events() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        let mut batch = WatchBatch::default();

        assert!(batch.push(
            &paths,
            Event {
                kind: EventKind::Modify(ModifyKind::Any),
                paths: vec![temp_dir.path().join("Notes/Alpha.md")],
                ..Event::default()
            }
        ));
        assert!(batch.push(
            &paths,
            Event {
                kind: EventKind::Create(CreateKind::Any),
                paths: vec![temp_dir.path().join("Notes/Alpha.md")],
                ..Event::default()
            }
        ));

        assert_eq!(batch.event_count, 2);
        assert_eq!(batch.created_paths, ["Notes/Alpha.md".to_string()].into());
        assert_eq!(
            batch.paths.into_iter().collect::<Vec<_>>(),
            vec!["Notes/Alpha.md".to_string()]
        );
    }

    #[test]
    fn normalize_watch_path_ignores_outside_paths() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());

        assert_eq!(
            normalize_watch_path(&paths, &temp_dir.path().join("Notes/Alpha.md")),
            Some("Notes/Alpha.md".to_string())
        );
        assert_eq!(
            normalize_watch_path(&paths, Path::new("/tmp/outside.md")),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn normalize_watch_path_handles_windows_verbatim_prefix() {
        let paths = VaultPaths::new(PathBuf::from(r"C:\vault"));
        let path = PathBuf::from(r"\\?\C:\vault\Notes\Alpha.md");

        assert_eq!(
            normalize_watch_path(&paths, &path),
            Some("Notes/Alpha.md".to_string())
        );
    }

    #[test]
    fn watch_vault_until_returns_when_stop_requested() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        std::fs::write(temp_dir.path().join("Home.md"), "# Home\n").expect("note should write");
        std::fs::create_dir_all(temp_dir.path().join(".vulcan"))
            .expect(".vulcan dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        let mut startup_reports = 0_usize;

        watch_vault_until_polling(
            &paths,
            WatchOptions { debounce_ms: 10 },
            || true,
            |_| {
                startup_reports += 1;
                Ok::<_, std::convert::Infallible>(())
            },
        )
        .expect("watch should stop cleanly");

        assert_eq!(startup_reports, 1);
    }

    #[test]
    fn polling_fallback_detects_same_size_content_changes() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let note = temp_dir.path().join("Home.md");
        std::fs::write(&note, "# Alpha\n").expect("note should write");
        std::fs::create_dir_all(temp_dir.path().join(".vulcan"))
            .expect(".vulcan dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer_stop = std::sync::Arc::clone(&stop);
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            std::fs::write(note, "# Bravo\n").expect("note should update");
            while !writer_stop.load(std::sync::atomic::Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let should_stop = std::sync::Arc::clone(&stop);
        let on_report_stop = std::sync::Arc::clone(&stop);
        let mut changed_paths = Vec::new();

        watch_vault_until_polling_with_interval(
            &paths,
            WatchOptions { debounce_ms: 10 },
            || should_stop.load(std::sync::atomic::Ordering::Acquire),
            |report| {
                if !report.startup {
                    changed_paths.extend(report.paths);
                    on_report_stop.store(true, std::sync::atomic::Ordering::Release);
                }
                Ok::<_, std::convert::Infallible>(())
            },
            Duration::from_millis(50),
        )
        .expect("polling watch should stop cleanly");
        writer.join().expect("writer should stop");

        assert_eq!(changed_paths, ["Home.md"]);
    }

    #[test]
    fn polling_fallback_identifies_created_paths() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        std::fs::create_dir_all(temp_dir.path().join(".vulcan"))
            .expect(".vulcan dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let created_note = temp_dir.path().join("New.md");
        let (startup_sender, startup_receiver) = mpsc::channel();
        let writer_stop = std::sync::Arc::clone(&stop);
        let writer = std::thread::spawn(move || {
            startup_receiver
                .recv()
                .expect("startup scan should complete before creation");
            std::fs::write(created_note, "# New\n").expect("note should be created");
            while !writer_stop.load(std::sync::atomic::Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let should_stop = std::sync::Arc::clone(&stop);
        let on_report_stop = std::sync::Arc::clone(&stop);
        let mut created_paths = Vec::new();

        watch_vault_until_polling_with_interval(
            &paths,
            WatchOptions { debounce_ms: 10 },
            || should_stop.load(std::sync::atomic::Ordering::Acquire),
            |report| {
                if report.startup {
                    startup_sender
                        .send(())
                        .expect("writer should await startup scan");
                } else {
                    created_paths.extend(report.created_paths);
                    on_report_stop.store(true, std::sync::atomic::Ordering::Release);
                }
                Ok::<_, std::convert::Infallible>(())
            },
            Duration::from_millis(50),
        )
        .expect("polling watch should stop cleanly");
        writer.join().expect("writer should stop");

        assert_eq!(created_paths, ["New.md"]);
    }

    #[test]
    fn safety_rescan_detects_changes_when_registered_watcher_is_silent() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let note = temp_dir.path().join("Home.md");
        std::fs::write(&note, "# Alpha\n").expect("note should write");
        std::fs::create_dir_all(temp_dir.path().join(".vulcan"))
            .expect(".vulcan dir should be created");
        let paths = VaultPaths::new(temp_dir.path());
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (startup_sender, startup_receiver) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            startup_receiver
                .recv()
                .expect("startup scan should complete before the update");
            std::fs::write(note, "# Bravo\n").expect("note should update");
        });
        let should_stop = std::sync::Arc::clone(&stop);
        let on_report_stop = std::sync::Arc::clone(&stop);
        let (_sender, receiver) = mpsc::channel::<notify::Result<Event>>();
        let watcher = notify::NullWatcher::new(|_| {}, Config::default())
            .expect("null watcher should initialize");
        let mut safety_report = None;

        watch_vault_until_with_registered_watcher(
            &paths,
            WatchOptions { debounce_ms: 10 },
            || should_stop.load(std::sync::atomic::Ordering::Acquire),
            |report| {
                if report.startup {
                    startup_sender
                        .send(())
                        .expect("writer should await the startup scan");
                } else if report.summary.updated == 1 {
                    safety_report = Some(report);
                    on_report_stop.store(true, std::sync::atomic::Ordering::Release);
                }
                Ok::<_, std::convert::Infallible>(())
            },
            watcher,
            &receiver,
            Duration::from_millis(100),
        )
        .expect("silent watcher should be covered by safety rescans");
        writer.join().expect("writer should finish");

        let report = safety_report.expect("safety rescan should report the update");
        assert_eq!(report.event_count, 0);
        assert!(report.paths.is_empty());
    }

    #[test]
    fn known_edits_scan_only_signaled_files_and_structural_changes_reconcile() {
        let temporary = TempDir::new().unwrap();
        let paths = VaultPaths::new(temporary.path());
        std::fs::create_dir_all(paths.vulcan_dir()).unwrap();
        std::fs::write(temporary.path().join("A.md"), "one").unwrap();
        std::fs::write(temporary.path().join("B.md"), "two").unwrap();
        scan_vault(&paths, ScanMode::Incremental).unwrap();
        let a = temporary.path().join("A.md");
        let mtime = std::fs::metadata(&a).unwrap().modified().unwrap();
        std::fs::write(&a, "new").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&a)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let report = crate::scan::scan_watched_paths(&paths, &["A.md".into()].into()).unwrap();
        assert_eq!(
            (report.discovered, report.updated, report.deleted),
            (1, 1, 0)
        );
        let full = scan_vault(&paths, ScanMode::Incremental).unwrap();
        assert_eq!((full.discovered, full.unchanged), (2, 2));
        std::fs::rename(&a, temporary.path().join("C.md")).unwrap();
        let report =
            crate::scan::scan_watched_paths(&paths, &["A.md".into(), "C.md".into()].into())
                .unwrap();
        // The renamed file keeps its cached identity instead of being deleted and re-added.
        assert_eq!((report.added, report.deleted, report.updated), (0, 0, 1));
        std::fs::write(temporary.path().join(".gitignore"), "C.md\n").unwrap();
        let report =
            crate::scan::scan_watched_paths(&paths, &[".gitignore".into(), "C.md".into()].into())
                .unwrap();
        assert_eq!((report.discovered, report.deleted), (1, 1));
    }

    #[test]
    fn created_and_deleted_files_scan_alone_and_follow_discovery_rules() {
        let temporary = TempDir::new().unwrap();
        let root = temporary.path();
        let paths = VaultPaths::new(root);
        std::fs::create_dir_all(paths.vulcan_dir()).unwrap();
        for (path, contents) in [
            ("A.md", "one"),
            ("B.md", "two"),
            ("dir/F.md", "three"),
            (".gitignore", "ignored/\n"),
        ] {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), contents).unwrap();
        }
        scan_vault(&paths, ScanMode::Incremental).unwrap();
        let scan = |changed: &[&str]| {
            crate::scan::scan_watched_paths(
                &paths,
                &changed.iter().map(ToString::to_string).collect(),
            )
            .unwrap()
        };
        // A new file is indexed on its own, without walking the vault.
        std::fs::write(root.join("D.md"), "four").unwrap();
        let report = scan(&["D.md"]);
        assert_eq!((report.discovered, report.added), (1, 1));
        // A new file under an ignored folder stays out, as a full walk keeps it.
        std::fs::create_dir_all(root.join("ignored")).unwrap();
        std::fs::write(root.join("ignored/E.md"), "five").unwrap();
        let report = scan(&["ignored/E.md"]);
        assert_eq!((report.discovered, report.added), (0, 0));
        // A deleted indexed file is removed on its own.
        std::fs::remove_file(root.join("B.md")).unwrap();
        let report = scan(&["B.md"]);
        assert_eq!((report.discovered, report.deleted), (0, 1));
        let full = scan_vault(&paths, ScanMode::Incremental).unwrap();
        assert_eq!(
            (full.discovered, full.added, full.deleted, full.unchanged),
            (3, 0, 0, 3),
            "targeted scans left the index as a full scan would"
        );
        // A missing unindexed path may be a deleted directory: discovery runs.
        std::fs::remove_dir_all(root.join("dir")).unwrap();
        let report = scan(&["dir"]);
        assert_eq!(report.deleted, 1);
    }

    #[test]
    fn overflow_without_paths_requires_full_reconciliation() {
        let temporary = TempDir::new().unwrap();
        let mut batch = WatchBatch::default();
        assert!(batch.push(
            &VaultPaths::new(temporary.path()),
            Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan)
        ));
        assert!(batch.safety_rescan);
        assert!(batch.paths.is_empty());
    }

    #[test]
    fn debounce_waits_for_full_quiet_period_and_coalesces_spaced_events() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let temporary = TempDir::new().unwrap();
        let paths = VaultPaths::new(temporary.path());
        std::fs::create_dir_all(paths.vulcan_dir()).unwrap();
        let note = temporary.path().join("A.md");
        std::fs::write(&note, "initial").unwrap();
        let (sender, receiver) = mpsc::channel();
        let (started, ready) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            ready.recv().unwrap();
            for text in ["first", "second"] {
                std::fs::write(&note, text).unwrap();
                sender
                    .send(Ok(
                        Event::new(EventKind::Modify(ModifyKind::Any)).add_path(note.clone())
                    ))
                    .unwrap();
                std::thread::sleep(Duration::from_millis(100));
            }
            std::thread::sleep(Duration::from_millis(400));
        });
        let stop = AtomicBool::new(false);
        let start = Instant::now();
        let mut reports = 0;
        watch_vault_until_with_registered_watcher(
            &paths,
            WatchOptions { debounce_ms: 250 },
            || stop.load(Ordering::Acquire) || start.elapsed() > Duration::from_secs(3),
            |report| {
                if report.startup {
                    started.send(()).unwrap();
                } else {
                    assert_eq!(report.event_count, 2);
                    assert!(start.elapsed() >= Duration::from_millis(350));
                    reports += 1;
                    stop.store(true, Ordering::Release);
                }
                Ok::<_, std::convert::Infallible>(())
            },
            notify::NullWatcher::new(|_| {}, Config::default()).unwrap(),
            &receiver,
            WATCH_SAFETY_RESCAN_INTERVAL,
        )
        .unwrap();
        writer.join().unwrap();
        assert_eq!(reports, 1);
    }
}
