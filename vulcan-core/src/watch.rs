use crate::{scan_vault, ScanError, ScanMode, ScanSummary, VaultPaths};
use notify::{Config, Event, EventKind, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{Display, Formatter};
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
    let mut watcher = PollWatcher::new(
        move |event| {
            let _ = sender.send(event);
        },
        Config::default()
            .with_poll_interval(poll_interval)
            .with_compare_contents(true),
    )?;
    watcher.watch(paths.vault_root(), RecursiveMode::Recursive)?;
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
    W: Watcher,
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
    if normalized.is_empty()
        || normalized
            .first()
            .is_some_and(|part| matches!(part.as_str(), ".vulcan" | ".git"))
    {
        return None;
    }

    Some(normalized.join("/"))
}

fn relative_watch_path(paths: &VaultPaths, path: &Path) -> Option<PathBuf> {
    paths
        .relative_to_vault(path)
        .or_else(|| windows_relative_watch_path(paths, path))
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
    use tempfile::TempDir;

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
        assert_eq!((report.added, report.deleted), (1, 1));
        std::fs::write(temporary.path().join(".gitignore"), "C.md\n").unwrap();
        let report =
            crate::scan::scan_watched_paths(&paths, &[".gitignore".into(), "C.md".into()].into())
                .unwrap();
        assert_eq!((report.discovered, report.deleted), (1, 1));
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
