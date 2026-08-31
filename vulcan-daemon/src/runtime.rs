//! Registry-driven lifecycle and periodic trigger coordination.

use crate::registry::{RegistryError, WikiRegistration, WikiRegistry};
use crate::supervisor::{SupervisorError, SyncSupervisor, SyncWatchMetadata};
use crate::watch::{watch_registered_wiki_until, DaemonWatchError, DaemonWatchOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use vulcan_app::sync_state::SyncStateStore;
use vulcan_sync::SyncJobTrigger;

const RUNTIME_STOP_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncTriggerRuntimeOptions {
    pub registry_refresh_ms: u64,
    pub remote_poll_ms: u64,
    pub watch: DaemonWatchOptions,
}

impl Default for SyncTriggerRuntimeOptions {
    fn default() -> Self {
        Self {
            registry_refresh_ms: 1_000,
            remote_poll_ms: 5 * 60 * 1_000,
            watch: DaemonWatchOptions::default(),
        }
    }
}

#[derive(Debug)]
pub enum SyncTriggerRuntimeError {
    InvalidOptions(String),
    Registry(RegistryError),
    Supervisor(SupervisorError),
}

impl Display for SyncTriggerRuntimeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOptions(detail) => formatter.write_str(detail),
            Self::Registry(error) => Display::fmt(error, formatter),
            Self::Supervisor(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for SyncTriggerRuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry(error) => Some(error),
            Self::Supervisor(error) => Some(error),
            Self::InvalidOptions(_) => None,
        }
    }
}

impl From<RegistryError> for SyncTriggerRuntimeError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

impl From<SupervisorError> for SyncTriggerRuntimeError {
    fn from(error: SupervisorError) -> Self {
        Self::Supervisor(error)
    }
}

struct WatcherTask {
    registration: WikiRegistration,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<Result<(), DaemonWatchError>>,
}

/// Reconciles daemon watcher ownership with the device-local registry and adds
/// periodic remote triggers. Job execution remains owned by the supervisor
/// worker and uses the same finite application transaction as direct mode.
pub fn run_sync_trigger_runtime_until<S>(
    registry: &WikiRegistry,
    supervisor: &Arc<SyncSupervisor>,
    state_store: &SyncStateStore,
    options: &SyncTriggerRuntimeOptions,
    should_stop: S,
) -> Result<(), SyncTriggerRuntimeError>
where
    S: Fn() -> bool,
{
    validate_options(options)?;
    let registry_refresh = Duration::from_millis(options.registry_refresh_ms);
    let remote_poll = Duration::from_millis(options.remote_poll_ms);
    let mut watchers = BTreeMap::<String, WatcherTask>::new();
    let mut next_registry_refresh = Instant::now();
    let mut next_remote_poll = Instant::now() + remote_poll;

    loop {
        if should_stop() {
            stop_all_watchers(&mut watchers);
            return Ok(());
        }
        let now = Instant::now();
        if now >= next_registry_refresh {
            if let Err(error) = reconcile_watchers(
                registry,
                supervisor,
                state_store,
                options.watch,
                &mut watchers,
            ) {
                stop_all_watchers(&mut watchers);
                return Err(error);
            }
            next_registry_refresh = now + registry_refresh;
        }
        if now >= next_remote_poll {
            if let Err(error) = enqueue_periodic_reconciliation(registry, supervisor) {
                stop_all_watchers(&mut watchers);
                return Err(error);
            }
            next_remote_poll = now + remote_poll;
        }

        let timeout = next_registry_refresh
            .saturating_duration_since(Instant::now())
            .min(next_remote_poll.saturating_duration_since(Instant::now()))
            .min(RUNTIME_STOP_POLL);
        if !timeout.is_zero() {
            thread::sleep(timeout);
        }
    }
}

fn validate_options(options: &SyncTriggerRuntimeOptions) -> Result<(), SyncTriggerRuntimeError> {
    if options.registry_refresh_ms == 0 {
        return Err(SyncTriggerRuntimeError::InvalidOptions(
            "sync registry refresh interval must be greater than zero".to_string(),
        ));
    }
    if options.remote_poll_ms == 0 {
        return Err(SyncTriggerRuntimeError::InvalidOptions(
            "sync remote poll interval must be greater than zero".to_string(),
        ));
    }
    if options.watch.debounce_ms == 0 || options.watch.max_dirty_ms < options.watch.debounce_ms {
        return Err(SyncTriggerRuntimeError::InvalidOptions(
            "sync watch timing options are invalid".to_string(),
        ));
    }
    Ok(())
}

fn desired_watchers(registrations: Vec<WikiRegistration>) -> BTreeMap<String, WikiRegistration> {
    registrations
        .into_iter()
        .filter(|registration| {
            !registration.sync_paused
                && registration
                    .sync_backend
                    .as_deref()
                    .is_none_or(|backend| backend == "git")
        })
        .map(|registration| (registration.id.as_str().to_string(), registration))
        .collect()
}

fn reconcile_watchers(
    registry: &WikiRegistry,
    supervisor: &Arc<SyncSupervisor>,
    state_store: &SyncStateStore,
    watch_options: DaemonWatchOptions,
    watchers: &mut BTreeMap<String, WatcherTask>,
) -> Result<(), SyncTriggerRuntimeError> {
    let desired = desired_watchers(registry.load()?.vaults);
    let mut failed_this_cycle = BTreeSet::new();
    let finished = watchers
        .iter()
        .filter(|(_, task)| task.handle.is_finished())
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in finished {
        if let Some(task) = watchers.remove(&id) {
            let registration = task.registration.clone();
            let detail = join_watcher(task);
            supervisor.enqueue_watch(
                registration.id.as_str(),
                &registration.path,
                SyncWatchMetadata {
                    safety_rescan: true,
                    watcher_errors: vec![detail],
                    ..SyncWatchMetadata::default()
                },
            )?;
            failed_this_cycle.insert(id);
        }
    }

    let stale = watchers
        .iter()
        .filter(|(id, task)| {
            desired.get(*id).is_none_or(|registration| {
                watcher_registration_changed(&task.registration, registration)
            })
        })
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in stale {
        if let Some(task) = watchers.remove(&id) {
            stop_watcher(task);
        }
    }

    for (id, registration) in desired {
        if !watchers.contains_key(&id) && !failed_this_cycle.contains(&id) {
            watchers.insert(
                id,
                spawn_watcher(
                    registration,
                    Arc::clone(supervisor),
                    state_store.clone(),
                    watch_options,
                ),
            );
        }
    }
    Ok(())
}

fn watcher_registration_changed(current: &WikiRegistration, desired: &WikiRegistration) -> bool {
    current.id != desired.id
        || current.registration_id != desired.registration_id
        || current.path != desired.path
        || current.git_dir != desired.git_dir
        || current.sync_backend != desired.sync_backend
        || current.sync_paused != desired.sync_paused
}

fn spawn_watcher(
    registration: WikiRegistration,
    supervisor: Arc<SyncSupervisor>,
    state_store: SyncStateStore,
    options: DaemonWatchOptions,
) -> WatcherTask {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread_registration = registration.clone();
    let handle = thread::spawn(move || {
        watch_registered_wiki_until(
            &thread_registration,
            &supervisor,
            &state_store,
            &options,
            || thread_stop.load(Ordering::Acquire),
        )
    });
    WatcherTask {
        registration,
        stop,
        handle,
    }
}

fn enqueue_periodic_reconciliation(
    registry: &WikiRegistry,
    supervisor: &SyncSupervisor,
) -> Result<(), SyncTriggerRuntimeError> {
    for registration in desired_watchers(registry.load()?.vaults).into_values() {
        supervisor.enqueue(
            registration.id.as_str(),
            &registration.path,
            SyncJobTrigger::Poll,
        )?;
    }
    Ok(())
}

fn stop_all_watchers(watchers: &mut BTreeMap<String, WatcherTask>) {
    for (_, task) in std::mem::take(watchers) {
        stop_watcher(task);
    }
}

fn stop_watcher(task: WatcherTask) {
    task.stop.store(true, Ordering::Release);
    let _ = task.handle.join();
}

fn join_watcher(task: WatcherTask) -> String {
    match task.handle.join() {
        Ok(Ok(())) => "watcher exited unexpectedly".to_string(),
        Ok(Err(error)) => format!("watcher failed: {error}"),
        Err(_) => "watcher thread panicked".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{AddWikiRequest, UpdateWikiRequest, WikiId};
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::tempdir;

    fn registration(id: &str, paused: bool, backend: Option<&str>) -> WikiRegistration {
        WikiRegistration {
            id: WikiId::parse(id).expect("wiki id"),
            registration_id: ulid::Ulid::new(),
            path: PathBuf::from(format!("/{id}")),
            groups: Vec::new(),
            git_dir: None,
            permissions_profile: None,
            sync_backend: backend.map(str::to_string),
            platform_profile: None,
            sync_paused: paused,
        }
    }

    #[test]
    fn desired_watcher_set_excludes_paused_and_non_git_registrations() {
        let desired = desired_watchers(vec![
            registration("alpha", false, None),
            registration("beta", false, Some("git")),
            registration("paused", true, Some("git")),
            registration("other", false, Some("seafile")),
        ]);
        assert_eq!(
            desired.keys().cloned().collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
    }

    #[test]
    fn watcher_restart_identity_tracks_only_watcher_relevant_registration_fields() {
        let current = registration("alpha", false, Some("git"));
        let mut metadata_only = current.clone();
        metadata_only.groups.push("daily".to_string());
        metadata_only.permissions_profile = Some("automation".to_string());
        assert!(!watcher_registration_changed(&current, &metadata_only));

        let mut moved = current.clone();
        moved.path = PathBuf::from("/moved-alpha");
        assert!(watcher_registration_changed(&current, &moved));
        let mut detached = current.clone();
        detached.git_dir = Some(PathBuf::from("/private/git/alpha"));
        assert!(watcher_registration_changed(&current, &detached));
    }

    #[test]
    fn runtime_starts_watchers_and_adds_periodic_poll_triggers() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        std::fs::create_dir(&vault).expect("vault directory");
        let output = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&vault)
            .output()
            .expect("run git init");
        assert!(output.status.success());
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        registry
            .add(
                &AddWikiRequest {
                    id: WikiId::parse("alpha").expect("wiki id"),
                    path: vault,
                    groups: Vec::new(),
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("git".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .expect("register wiki");
        let supervisor =
            Arc::new(SyncSupervisor::at(temporary.path().join("jobs.json")).expect("supervisor"));
        let state_store = SyncStateStore::at(temporary.path().join("state"));
        let started = Instant::now();
        let observed_periodic_poll = || {
            supervisor.list().is_ok_and(|jobs| {
                jobs.iter()
                    .any(|job| job.triggers.contains(&SyncJobTrigger::Poll))
            })
        };
        run_sync_trigger_runtime_until(
            &registry,
            &supervisor,
            &state_store,
            &SyncTriggerRuntimeOptions {
                registry_refresh_ms: 10,
                remote_poll_ms: 20,
                watch: DaemonWatchOptions {
                    debounce_ms: 5,
                    max_dirty_ms: 20,
                },
            },
            || observed_periodic_poll() || started.elapsed() >= Duration::from_secs(2),
        )
        .expect("trigger runtime");

        let jobs = supervisor.list().expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].triggers.contains(&SyncJobTrigger::Resume));
        assert!(jobs[0].triggers.contains(&SyncJobTrigger::Poll));
    }

    #[test]
    fn pausing_a_registration_removes_it_from_periodic_reconciliation() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        std::fs::create_dir(&vault).expect("vault directory");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        let id = WikiId::parse("alpha").expect("wiki id");
        registry
            .add(
                &AddWikiRequest {
                    id: id.clone(),
                    path: vault,
                    groups: Vec::new(),
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("git".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .expect("register wiki");
        registry
            .update(
                &id,
                &UpdateWikiRequest {
                    groups_to_add: Vec::new(),
                    groups_to_remove: Vec::new(),
                    permissions_profile: None,
                    sync_paused: Some(true),
                },
                false,
            )
            .expect("pause wiki");
        let supervisor =
            SyncSupervisor::at(temporary.path().join("jobs.json")).expect("supervisor");

        enqueue_periodic_reconciliation(&registry, &supervisor).expect("periodic trigger");
        assert!(supervisor.list().expect("jobs").is_empty());
    }
}
