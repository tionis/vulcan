use crate::{scan_vault, ScanError, ScanMode, ScanSummary, VaultPaths};
use notify::{Config, Event, EventKind, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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
    pub summary: ScanSummary,
}

#[derive(Debug, Default)]
struct WatchBatch {
    event_count: usize,
    paths: BTreeSet<String>,
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
    on_report: F,
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
            return watch_vault_until_with_registered_watcher(
                paths,
                *options,
                should_stop,
                on_report,
                watcher,
                &receiver,
            );
        }
    }

    watch_vault_until_polling(paths, *options, should_stop, on_report)
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
    let (sender, receiver) = mpsc::channel::<notify::Result<Event>>();
    let poll_interval = Duration::from_millis(options.debounce_ms.max(50));
    let mut watcher = PollWatcher::new(
        move |event| {
            let _ = sender.send(event);
        },
        Config::default()
            .with_poll_interval(poll_interval)
            .with_compare_contents(true),
    )?;
    watcher.watch(paths.vault_root(), RecursiveMode::Recursive)?;
    // PollWatcher establishes its initial comparison snapshot asynchronously.
    // Let one poll complete before the startup report makes the watcher visible
    // as ready; the startup scan below still observes writes from this interval.
    std::thread::sleep(poll_interval.max(Duration::from_millis(250)));
    watch_vault_until_with_registered_watcher(
        paths,
        options,
        should_stop,
        on_report,
        watcher,
        &receiver,
    )
}

fn watch_vault_until_with_registered_watcher<F, S, E, W>(
    paths: &VaultPaths,
    options: WatchOptions,
    should_stop: S,
    mut on_report: F,
    _watcher: W,
    receiver: &mpsc::Receiver<notify::Result<Event>>,
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
        summary: startup_summary,
    })
    .map_err(|error| WatchError::Callback(error.to_string()))?;

    let debounce = Duration::from_millis(options.debounce_ms);
    loop {
        if should_stop() {
            return Ok(());
        }

        let mut batch = WatchBatch::default();
        loop {
            if should_stop() {
                return Ok(());
            }

            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(event) => match event {
                    Ok(event) => {
                        if batch.push(paths, event) {
                            break;
                        }
                    }
                    Err(error) => return Err(WatchError::Notify(error)),
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(WatchError::ChannelClosed),
            }
        }

        let mut deadline = Instant::now() + debounce;
        loop {
            if should_stop() {
                return Ok(());
            }

            let timeout = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(50));
            match receiver.recv_timeout(timeout) {
                Ok(Ok(event)) => {
                    if batch.push(paths, event) {
                        deadline = Instant::now() + debounce;
                    }
                }
                Ok(Err(error)) => return Err(WatchError::Notify(error)),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(WatchError::ChannelClosed),
            }
        }

        let summary = scan_vault(paths, ScanMode::Incremental)?;
        on_report(batch.into_report(summary))
            .map_err(|error| WatchError::Callback(error.to_string()))?;
    }
}

impl WatchBatch {
    fn push(&mut self, paths: &VaultPaths, event: Event) -> bool {
        if matches!(event.kind, EventKind::Access(_)) {
            return false;
        }

        let mut added = false;
        for path in event.paths {
            let Some(relative_path) = normalize_watch_path(paths, &path) else {
                continue;
            };
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
    if normalized.is_empty() || normalized.first().is_some_and(|part| part == ".vulcan") {
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

        watch_vault_until_polling(
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
        )
        .expect("polling watch should stop cleanly");
        writer.join().expect("writer should stop");

        assert_eq!(changed_paths, ["Home.md"]);
    }
}
