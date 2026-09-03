//! Filesystem watcher scheduling for registered synchronized wikis.

use crate::registry::WikiRegistration;
use crate::supervisor::{SupervisorError, SyncSupervisor, SyncWatchMetadata};
use notify::{Config, Event, EventKind, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use vulcan_app::sync_state::SyncStateStore;
use vulcan_core::VaultPaths;
use vulcan_sync::{GitCliEngine, GitEngine, GitEngineError, SyncJobTrigger};

const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(50);
const WATCH_SAFETY_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Minimum interval between Recovery wake-ups for an unchanged watcher error
/// set. A persistently failing path (dangling symlink, denied file) must not
/// schedule a full sync on every poll scan; one retry per cooldown preserves
/// eventual safety while the failure persists.
const WATCH_ERROR_COOLDOWN: Duration = Duration::from_secs(300);
const MAX_REPORTED_PATHS: usize = 256;
const MAX_REPORTED_ERRORS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonWatchOptions {
    pub debounce_ms: u64,
    pub max_dirty_ms: u64,
}

impl Default for DaemonWatchOptions {
    fn default() -> Self {
        Self {
            debounce_ms: 250,
            max_dirty_ms: 2_000,
        }
    }
}

#[derive(Debug)]
pub enum DaemonWatchError {
    InvalidOptions(String),
    Git(GitEngineError),
    Notify(notify::Error),
    ChannelClosed,
    Supervisor(SupervisorError),
}

impl Display for DaemonWatchError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOptions(detail) => formatter.write_str(detail),
            Self::Git(error) => Display::fmt(error, formatter),
            Self::Notify(error) => Display::fmt(error, formatter),
            Self::ChannelClosed => formatter.write_str("watch channel closed unexpectedly"),
            Self::Supervisor(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for DaemonWatchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Git(error) => Some(error),
            Self::Notify(error) => Some(error),
            Self::Supervisor(error) => Some(error),
            Self::InvalidOptions(_) | Self::ChannelClosed => None,
        }
    }
}

impl From<GitEngineError> for DaemonWatchError {
    fn from(error: GitEngineError) -> Self {
        Self::Git(error)
    }
}

impl From<notify::Error> for DaemonWatchError {
    fn from(error: notify::Error) -> Self {
        Self::Notify(error)
    }
}

impl From<SupervisorError> for DaemonWatchError {
    fn from(error: SupervisorError) -> Self {
        Self::Supervisor(error)
    }
}

#[derive(Debug, Default)]
struct WatchBatch {
    first_dirty: Option<Instant>,
    last_dirty: Option<Instant>,
    event_count: usize,
    untagged_events: usize,
    paths: BTreeSet<String>,
    self_generated_transactions: BTreeSet<String>,
    safety_rescan: bool,
    watcher_errors: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchSource {
    Native,
    Polling,
}

struct RegisteredWatchers {
    _native: Option<RecommendedWatcher>,
    _polling: Option<PollWatcher>,
}

/// Watches one registered worktree and turns event batches into idempotent
/// supervisor triggers. Startup always schedules reconciliation before the
/// event loop begins.
pub fn watch_registered_wiki_until<S>(
    registration: &WikiRegistration,
    supervisor: &SyncSupervisor,
    state_store: &SyncStateStore,
    options: &DaemonWatchOptions,
    should_stop: S,
) -> Result<(), DaemonWatchError>
where
    S: Fn() -> bool,
{
    validate_options(options)?;
    let repository = GitCliEngine::default().discover_repository(&registration.path)?;
    match state_store.load_apply_marker(&repository.git_dir) {
        Ok(Some(_)) => {
            supervisor.enqueue(
                registration.id.as_str(),
                &registration.path,
                SyncJobTrigger::Recovery,
            )?;
        }
        Ok(None) => {
            supervisor.enqueue(
                registration.id.as_str(),
                &registration.path,
                SyncJobTrigger::Resume,
            )?;
        }
        Err(error) => {
            supervisor.enqueue_watch(
                registration.id.as_str(),
                &registration.path,
                SyncWatchMetadata {
                    safety_rescan: true,
                    watcher_errors: vec![format!("cannot read sync apply marker: {error}")],
                    ..SyncWatchMetadata::default()
                },
            )?;
        }
    }

    let (sender, receiver) = mpsc::channel::<(WatchSource, notify::Result<Event>)>();
    let _watchers = register_watchers(&registration.path, &sender, WATCH_SAFETY_POLL_INTERVAL)?;
    drop(sender);

    let paths = VaultPaths::new(&registration.path);
    let debounce = Duration::from_millis(options.debounce_ms);
    let max_dirty = Duration::from_millis(options.max_dirty_ms);
    let mut batch = WatchBatch::default();
    let mut last_error_only: Option<(BTreeSet<String>, Instant)> = None;
    loop {
        if should_stop() {
            return Ok(());
        }
        let now = Instant::now();
        let timeout = batch.next_timeout(now, debounce, max_dirty);
        match receiver.recv_timeout(timeout) {
            Ok((_, Ok(event))) => {
                let now = Instant::now();
                batch.push_event(&paths, &event, now, || {
                    state_store
                        .load_apply_marker(&repository.git_dir)
                        .map(|marker| {
                            marker.map(|marker| {
                                marker.transaction_id.to_string().to_ascii_lowercase()
                            })
                        })
                        .map_err(|error| error.to_string())
                });
            }
            Ok((_, Err(error))) if notify_error_is_internal(&paths, &error) => {}
            Ok((_, Err(error))) if error_paths_are_symlinks(&error) => {}
            Ok((source, Err(error))) => batch.push_watcher_error(
                Instant::now(),
                format!("{} watcher: {error}", source.label()),
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(DaemonWatchError::ChannelClosed);
            }
        }
        if batch.is_ready(Instant::now(), debounce, max_dirty) {
            let metadata = batch.take_metadata();
            if suppress_unchanged_error_batch(&metadata, &mut last_error_only, Instant::now()) {
                continue;
            }
            supervisor.enqueue_watch(registration.id.as_str(), &registration.path, metadata)?;
        }
    }
}

/// Returns true when an error-only batch repeats the last reported error set
/// within cooldown and must not schedule another sync. Batches carrying paths
/// or events always schedule and reset the suppression, as does a changed
/// error set; an expired cooldown schedules one retry and restarts the wait.
fn suppress_unchanged_error_batch(
    metadata: &SyncWatchMetadata,
    last_reported: &mut Option<(BTreeSet<String>, Instant)>,
    now: Instant,
) -> bool {
    let errors: BTreeSet<String> = metadata.watcher_errors.iter().cloned().collect();
    if metadata.paths.is_empty() && metadata.event_count == 0 && !errors.is_empty() {
        if let Some((previous, at)) = last_reported {
            if *previous == errors && now.duration_since(*at) < WATCH_ERROR_COOLDOWN {
                return true;
            }
        }
        *last_reported = Some((errors, now));
        return false;
    }
    *last_reported = None;
    false
}

fn register_watchers(
    path: &Path,
    sender: &mpsc::Sender<(WatchSource, notify::Result<Event>)>,
    safety_poll_interval: Duration,
) -> Result<RegisteredWatchers, notify::Error> {
    let native_sender = sender.clone();
    let native = notify::recommended_watcher(move |event| {
        let _ = native_sender.send((WatchSource::Native, event));
    })
    .and_then(|mut watcher| {
        watcher.watch(path, RecursiveMode::Recursive)?;
        Ok(watcher)
    });

    let polling_sender = sender.clone();
    let polling = PollWatcher::new(
        move |event| {
            let _ = polling_sender.send((WatchSource::Polling, event));
        },
        Config::default()
            .with_poll_interval(safety_poll_interval)
            .with_compare_contents(true),
    )
    .and_then(|mut watcher| {
        watcher.watch(path, RecursiveMode::Recursive)?;
        Ok(watcher)
    });

    match (native, polling) {
        (Ok(native), Ok(polling)) => Ok(RegisteredWatchers {
            _native: Some(native),
            _polling: Some(polling),
        }),
        (Ok(native), Err(_)) => Ok(RegisteredWatchers {
            _native: Some(native),
            _polling: None,
        }),
        (Err(_), Ok(polling)) => Ok(RegisteredWatchers {
            _native: None,
            _polling: Some(polling),
        }),
        (Err(native_error), Err(_)) => Err(native_error),
    }
}

impl WatchSource {
    fn label(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Polling => "polling",
        }
    }
}

fn validate_options(options: &DaemonWatchOptions) -> Result<(), DaemonWatchError> {
    if options.debounce_ms == 0 {
        return Err(DaemonWatchError::InvalidOptions(
            "watch debounce must be greater than zero".to_string(),
        ));
    }
    if options.max_dirty_ms < options.debounce_ms {
        return Err(DaemonWatchError::InvalidOptions(
            "watch maximum dirty age must be at least the debounce interval".to_string(),
        ));
    }
    Ok(())
}

impl WatchBatch {
    fn push_event<F>(
        &mut self,
        paths: &VaultPaths,
        event: &Event,
        now: Instant,
        marker_transaction: F,
    ) -> bool
    where
        F: FnOnce() -> Result<Option<String>, String>,
    {
        let safety_rescan = event.need_rescan();
        if matches!(event.kind, EventKind::Access(_)) && !safety_rescan {
            return false;
        }
        let relevant_paths = event
            .paths
            .iter()
            .filter_map(|path| normalize_watch_path(paths, path))
            .collect::<Vec<_>>();
        if relevant_paths.is_empty() && !safety_rescan {
            return false;
        }

        self.mark_dirty(now);
        self.event_count = self.event_count.saturating_add(1);
        self.safety_rescan |= safety_rescan;
        for path in relevant_paths {
            if self.paths.len() < MAX_REPORTED_PATHS {
                self.paths.insert(path);
            }
        }
        match marker_transaction() {
            Ok(Some(transaction)) => {
                self.self_generated_transactions.insert(transaction);
            }
            Ok(None) => self.untagged_events = self.untagged_events.saturating_add(1),
            Err(error) => {
                self.untagged_events = self.untagged_events.saturating_add(1);
                self.safety_rescan = true;
                if self.watcher_errors.len() < MAX_REPORTED_ERRORS {
                    self.watcher_errors.insert(error);
                }
            }
        }
        true
    }

    fn push_watcher_error(&mut self, now: Instant, error: String) {
        self.mark_dirty(now);
        self.safety_rescan = true;
        if self.watcher_errors.len() < MAX_REPORTED_ERRORS {
            self.watcher_errors.insert(error);
        }
    }

    fn mark_dirty(&mut self, now: Instant) {
        self.first_dirty.get_or_insert(now);
        self.last_dirty = Some(now);
    }

    fn next_timeout(&self, now: Instant, debounce: Duration, max_dirty: Duration) -> Duration {
        let Some(first_dirty) = self.first_dirty else {
            return WATCH_POLL_INTERVAL;
        };
        let debounce_remaining = self
            .last_dirty
            .unwrap_or(first_dirty)
            .checked_add(debounce)
            .unwrap_or(now)
            .saturating_duration_since(now);
        let max_remaining = first_dirty
            .checked_add(max_dirty)
            .unwrap_or(now)
            .saturating_duration_since(now);
        WATCH_POLL_INTERVAL.min(debounce_remaining.min(max_remaining))
    }

    fn is_ready(&self, now: Instant, debounce: Duration, max_dirty: Duration) -> bool {
        self.first_dirty.is_some_and(|first| {
            now.saturating_duration_since(first) >= max_dirty
                || self
                    .last_dirty
                    .is_some_and(|last| now.saturating_duration_since(last) >= debounce)
        })
    }

    fn take_metadata(&mut self) -> SyncWatchMetadata {
        let metadata = SyncWatchMetadata {
            event_count: std::mem::take(&mut self.event_count),
            untagged_events: std::mem::take(&mut self.untagged_events),
            paths: std::mem::take(&mut self.paths).into_iter().collect(),
            self_generated_transactions: std::mem::take(&mut self.self_generated_transactions)
                .into_iter()
                .collect(),
            safety_rescan: std::mem::take(&mut self.safety_rescan),
            watcher_errors: std::mem::take(&mut self.watcher_errors)
                .into_iter()
                .collect(),
        };
        self.first_dirty = None;
        self.last_dirty = None;
        metadata
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

fn notify_error_is_internal(paths: &VaultPaths, error: &notify::Error) -> bool {
    !error.paths.is_empty()
        && error
            .paths
            .iter()
            .all(|path| normalize_watch_path(paths, path).is_none())
}

/// Returns true when every path attached to a watcher error is a symlink.
/// Git versions the link itself — target path as blob content, never
/// resolved — so content observed *through* a link is irrelevant to sync:
/// link creation, deletion, and retargeting are all visible in the parent
/// directory scan without reading through the link, and a dangling link has
/// no observable content at all. Such scan errors (the polling watcher reads
/// file contents to compare) carry no sync signal and must not schedule
/// safety rescans; other errors keep the existing rescan behavior.
fn error_paths_are_symlinks(error: &notify::Error) -> bool {
    !error.paths.is_empty()
        && error.paths.iter().all(|path| {
            std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
        })
}

fn relative_watch_path(paths: &VaultPaths, path: &Path) -> Option<PathBuf> {
    paths
        .relative_to_vault(path)
        .or_else(|| windows_relative_watch_path(paths, path))
}

#[cfg(windows)]
fn windows_relative_watch_path(paths: &VaultPaths, path: &Path) -> Option<PathBuf> {
    path.as_os_str()
        .to_string_lossy()
        .strip_prefix(r"\\?\")
        .map(PathBuf::from)
        .and_then(|normalized| paths.relative_to_vault(&normalized))
}

#[cfg(not(windows))]
fn windows_relative_watch_path(_: &VaultPaths, _: &Path) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{WikiId, WikiRegistration};
    use notify::event::{AccessKind, Flag, ModifyKind};
    use std::process::Command;
    use tempfile::tempdir;
    use ulid::Ulid;

    fn change(path: PathBuf) -> Event {
        Event::new(EventKind::Modify(ModifyKind::Any)).add_path(path)
    }

    fn error_only_batch(errors: &[&str]) -> SyncWatchMetadata {
        SyncWatchMetadata {
            event_count: 0,
            untagged_events: 0,
            paths: Vec::new(),
            self_generated_transactions: Vec::new(),
            safety_rescan: true,
            watcher_errors: errors.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn unchanged_error_batches_cool_down_while_new_signals_schedule() {
        let now = Instant::now();
        let mut last_reported = None;

        // First occurrence always schedules and records the signature.
        assert!(!suppress_unchanged_error_batch(
            &error_only_batch(&["polling watcher: dangling symlink"]),
            &mut last_reported,
            now,
        ));
        // Identical errors within cooldown suppress the wake-up.
        assert!(suppress_unchanged_error_batch(
            &error_only_batch(&["polling watcher: dangling symlink"]),
            &mut last_reported,
            now,
        ));
        // A changed error set schedules again and becomes the signature.
        assert!(!suppress_unchanged_error_batch(
            &error_only_batch(&[
                "polling watcher: dangling symlink",
                "native watcher: denied"
            ]),
            &mut last_reported,
            now,
        ));
        assert!(suppress_unchanged_error_batch(
            &error_only_batch(&[
                "polling watcher: dangling symlink",
                "native watcher: denied"
            ]),
            &mut last_reported,
            now,
        ));
        // An expired cooldown schedules one retry.
        let expired = now
            .checked_sub(WATCH_ERROR_COOLDOWN + Duration::from_secs(1))
            .map(|at| {
                (
                    ["polling watcher: dangling symlink"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    at,
                )
            });
        let mut last_expired = expired;
        assert!(!suppress_unchanged_error_batch(
            &error_only_batch(&["polling watcher: dangling symlink"]),
            &mut last_expired,
            now,
        ));
        // Real paths or events always schedule and clear the suppression.
        let mut with_paths = error_only_batch(&["polling watcher: dangling symlink"]);
        with_paths.paths.push("Notes/todo.md".to_string());
        assert!(!suppress_unchanged_error_batch(
            &with_paths,
            &mut last_reported,
            now,
        ));
        assert!(!suppress_unchanged_error_batch(
            &error_only_batch(&["polling watcher: dangling symlink"]),
            &mut last_reported,
            now,
        ));
    }

    #[test]
    fn batches_ignore_internal_and_access_events_but_tag_apply_events() {
        let temporary = tempdir().expect("temporary directory");
        let paths = VaultPaths::new(temporary.path());
        let now = Instant::now();
        let mut batch = WatchBatch::default();
        assert!(!batch.push_event(
            &paths,
            &Event::new(EventKind::Access(AccessKind::Any))
                .add_path(temporary.path().join("note.md")),
            now,
            || panic!("ignored events must not inspect marker state")
        ));
        assert!(!batch.push_event(
            &paths,
            &change(temporary.path().join(".git/index")),
            now,
            || panic!("internal events must not inspect marker state")
        ));
        assert!(batch.push_event(
            &paths,
            &change(temporary.path().join("notes/alpha.md")),
            now,
            || Ok(Some("transaction-a".to_string()))
        ));
        let metadata = batch.take_metadata();
        assert_eq!(metadata.event_count, 1);
        assert_eq!(metadata.paths, vec!["notes/alpha.md"]);
        assert_eq!(metadata.self_generated_transactions, vec!["transaction-a"]);
        assert_eq!(metadata.untagged_events, 0);
    }

    #[test]
    fn maximum_dirty_age_caps_continuous_save_sequences() {
        let temporary = tempdir().expect("temporary directory");
        let paths = VaultPaths::new(temporary.path());
        let start = Instant::now();
        let debounce = Duration::from_millis(250);
        let maximum = Duration::from_secs(2);
        let mut batch = WatchBatch::default();
        for offset in [0_u64, 200, 400, 1_800] {
            assert!(batch.push_event(
                &paths,
                &change(temporary.path().join("note.md")),
                start + Duration::from_millis(offset),
                || Ok(None)
            ));
        }
        assert!(!batch.is_ready(start + Duration::from_millis(1_999), debounce, maximum));
        assert!(batch.is_ready(start + maximum, debounce, maximum));
        assert_eq!(batch.take_metadata().event_count, 4);
        assert!(!batch.is_ready(start + maximum, debounce, maximum));
    }

    #[test]
    #[cfg(unix)]
    fn symlink_scan_errors_carry_no_sync_signal() {
        use std::os::unix::fs::symlink;

        fn scan_error(paths: Vec<PathBuf>) -> notify::Error {
            notify::Error {
                kind: notify::ErrorKind::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "No such file or directory",
                )),
                paths,
            }
        }

        let temporary = tempdir().expect("temporary directory");
        let dangling = temporary.path().join("dead-link");
        symlink("/nonexistent-target", &dangling).expect("symlink");
        let target = temporary.path().join("target");
        std::fs::write(&target, "contents").expect("target");
        let healthy = temporary.path().join("live-link");
        symlink(&target, &healthy).expect("symlink");
        let regular = temporary.path().join("note.md");
        std::fs::write(&regular, "note").expect("note");

        assert!(error_paths_are_symlinks(&scan_error(vec![dangling])));
        assert!(error_paths_are_symlinks(&scan_error(vec![healthy])));
        assert!(!error_paths_are_symlinks(&scan_error(vec![regular])));
        assert!(!error_paths_are_symlinks(&scan_error(vec![temporary
            .path()
            .join("missing.md")])));
        assert!(!error_paths_are_symlinks(&scan_error(Vec::new())));
    }

    #[test]
    fn rescan_and_callback_errors_force_recovery_metadata() {
        let temporary = tempdir().expect("temporary directory");
        let paths = VaultPaths::new(temporary.path());
        let now = Instant::now();
        let mut batch = WatchBatch::default();
        assert!(batch.push_event(
            &paths,
            &Event::new(EventKind::Other).set_flag(Flag::Rescan),
            now,
            || Err("malformed apply marker".to_string())
        ));
        batch.push_watcher_error(now, "watch queue overflow".to_string());
        let metadata = batch.take_metadata();
        assert!(metadata.safety_rescan);
        assert_eq!(metadata.untagged_events, 1);
        assert_eq!(
            metadata.watcher_errors,
            vec!["malformed apply marker", "watch queue overflow"]
        );
    }

    #[test]
    fn invalid_timing_options_are_rejected() {
        assert!(validate_options(&DaemonWatchOptions {
            debounce_ms: 0,
            max_dirty_ms: 2_000,
        })
        .is_err());
        assert!(validate_options(&DaemonWatchOptions {
            debounce_ms: 500,
            max_dirty_ms: 499,
        })
        .is_err());
    }

    #[test]
    fn watcher_schedules_startup_reconciliation_before_stopping() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        std::fs::create_dir(&vault).expect("vault directory");
        let output = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&vault)
            .output()
            .expect("run git init");
        assert!(output.status.success());
        let registration = WikiRegistration {
            id: WikiId::parse("alpha").expect("wiki id"),
            registration_id: Ulid::new(),
            path: vault.clone(),
            groups: Vec::new(),
            git_dir: None,
            permissions_profile: None,
            sync_backend: Some("git".to_string()),
            platform_profile: None,
            sync_paused: false,
        };
        let supervisor =
            SyncSupervisor::at(temporary.path().join("jobs.json")).expect("supervisor");
        let state_store = SyncStateStore::at(temporary.path().join("state"));

        watch_registered_wiki_until(
            &registration,
            &supervisor,
            &state_store,
            &DaemonWatchOptions::default(),
            || true,
        )
        .expect("watch startup");

        let jobs = supervisor.list().expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].triggers, vec![SyncJobTrigger::Resume]);
    }

    #[test]
    fn polling_backup_detects_same_size_content_changes() {
        let temporary = tempdir().expect("temporary directory");
        let note = temporary.path().join("note.md");
        std::fs::write(&note, "alpha\n").expect("initial note");
        let (sender, receiver) = mpsc::channel();
        let _watchers = register_watchers(temporary.path(), &sender, Duration::from_millis(25))
            .expect("register at least one watcher");
        drop(sender);
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(&note, "bravo\n").expect("same-size update");

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut detected_by_polling = false;
        while Instant::now() < deadline {
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok((WatchSource::Polling, Ok(event))) if event.paths.contains(&note) => {
                    detected_by_polling = true;
                    break;
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(detected_by_polling);
    }
}
