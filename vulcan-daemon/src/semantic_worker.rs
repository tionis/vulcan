//! Low-frequency daemon adapter for finite semantic automation cycles.

use crate::companion::CompanionSemanticAgent;
use crate::registry::{DaemonSemanticWorkerConfig, WikiRegistry};
use crate::shutdown::ShutdownSignal;
use crate::supervisor::SyncSupervisor;
use crate::worker_gate::HostedWorkerGate;
use notify::Watcher;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use vulcan_app::execution::{MutationGate, Ungated};
use vulcan_app::sync::{GitRefName, GitRemote, SyncCancellationToken};
use vulcan_app::sync_semantic_auto::{
    run_semantic_auto_gated, SemanticAutoOptions, SemanticAutoReport,
};
use vulcan_app::sync_state::SyncStateStore;
use vulcan_core::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};
use vulcan_sync::{GitCliEngine, GitEngine, SyncJobState};

pub const SEMANTIC_WORKER_STATUS_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticWorkerStatus {
    pub version: u32,
    pub checked_unix_ms: u64,
    pub entries: Vec<SemanticWorkerStatusEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticWorkerStatusEntry {
    pub wiki_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<SemanticAutoReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[must_use]
pub fn semantic_worker_status_path(state_root: &Path) -> PathBuf {
    state_root.join("daemon/semantic-worker.json")
}

pub fn load_semantic_worker_status(
    state_root: &Path,
) -> Result<Option<SemanticWorkerStatus>, String> {
    let path = semantic_worker_status_path(state_root);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let report: SemanticWorkerStatus =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if report.version != SEMANTIC_WORKER_STATUS_VERSION {
        return Err(format!(
            "unsupported semantic worker status version {}",
            report.version
        ));
    }
    Ok(Some(report))
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_semantic_worker(
    config: DaemonSemanticWorkerConfig,
    registry: WikiRegistry,
    supervisor: Arc<SyncSupervisor>,
    state_store: Arc<SyncStateStore>,
    daemon_state_root: PathBuf,
    agent: Arc<CompanionSemanticAgent>,
    gate: Arc<HostedWorkerGate>,
    stop: Arc<ShutdownSignal>,
) -> JoinHandle<Result<(), String>> {
    thread::spawn(move || {
        run_semantic_worker(
            &config,
            &registry,
            &supervisor,
            &state_store,
            &daemon_state_root,
            &agent,
            &gate,
            &stop,
        )
    })
}

/// Applying and publishing a semantic plan waits for the host's mutation
/// permit for that vault and repository; planning and the agent call do not.
#[allow(clippy::too_many_arguments)]
pub fn run_semantic_worker(
    config: &DaemonSemanticWorkerConfig,
    registry: &WikiRegistry,
    supervisor: &SyncSupervisor,
    state_store: &SyncStateStore,
    daemon_state_root: &Path,
    agent: &CompanionSemanticAgent,
    gate: &HostedWorkerGate,
    stop: &ShutdownSignal,
) -> Result<(), String> {
    // Native hints avoid Git processes between changes. Missing/failed watches
    // are covered by reconciliation; neither timestamps nor events authorize work.
    let changed = Arc::new(AtomicBool::new(true));
    let mut watched_config = None;
    let mut watcher = None;
    let mut jobs = supervisor.subscribe_changes();
    let reconciliation = Duration::from_secs(config.poll_seconds.max(300));
    let mut next_reconciliation = Instant::now();
    let mut next_due = None;
    let mut last_status = None;
    stop.register_current_thread();
    loop {
        if stop.is_cancelled() {
            return Ok(());
        }
        let registrations = registry.poll().ok();
        if registrations != watched_config {
            changed.store(true, Ordering::Release);
            watcher = registrations.as_ref().and_then(|registrations| {
                semantic_change_watcher(&registrations.vaults, config, Arc::clone(&changed)).ok()
            });
            watched_config = registrations;
        }
        let now = unix_time_ms()?;
        let reconcile = Instant::now() >= next_reconciliation;
        let job_changed = jobs.has_changed().unwrap_or(false);
        if job_changed {
            jobs.borrow_and_update();
        }
        let local_changed = changed.swap(false, Ordering::AcqRel);
        if pass_due(reconcile, job_changed, local_changed, next_due, now) {
            let report = execute_semantic_worker_pass_inner(
                config,
                registry,
                supervisor,
                state_store,
                agent,
                Some(gate),
                now,
                reconcile,
            );
            next_due = worker_next_due(&report, now, config.poll_seconds);
            // checked_unix_ms means the most recent persisted evaluation. A heartbeat
            // at reconciliation bounds staleness without replacing identical reports.
            if status_needs_save(last_status.as_ref(), &report, reconcile) {
                save_status(&semantic_worker_status_path(daemon_state_root), &report)?;
                last_status = Some(report);
            }
            if reconcile {
                next_reconciliation = Instant::now() + reconciliation;
            }
        }
        let now = unix_time_ms()?;
        let timeout = next_due
            .map_or(Duration::from_secs(1), |due| {
                Duration::from_millis(due.saturating_sub(now)).min(Duration::from_secs(1))
            })
            .min(next_reconciliation.saturating_duration_since(Instant::now()));
        // Park is woken immediately by native hints and shutdown. Supervisor and
        // registration edits are observed within one second without running Git.
        thread::park_timeout(timeout);
        let _ = &watcher; // Retain native registrations for the worker lifetime.
    }
}

fn pass_due(reconcile: bool, jobs: bool, local: bool, deadline: Option<u64>, now: u64) -> bool {
    reconcile || jobs || local || deadline.is_some_and(|deadline| now >= deadline)
}

fn status_needs_save(
    previous: Option<&SemanticWorkerStatus>,
    current: &SemanticWorkerStatus,
    heartbeat: bool,
) -> bool {
    heartbeat || previous.is_none_or(|previous| previous.entries != current.entries)
}

fn worker_next_due(report: &SemanticWorkerStatus, now: u64, retry_seconds: u64) -> Option<u64> {
    report
        .entries
        .iter()
        .filter_map(|entry| {
            if entry.error.is_some() {
                Some(now.saturating_add(retry_seconds.saturating_mul(1_000)))
            } else {
                entry
                    .report
                    .as_ref()
                    .and_then(|report| report.next_eligible_unix_ms)
            }
        })
        .min()
}

fn semantic_change_watcher(
    registrations: &[crate::registry::WikiRegistration],
    config: &DaemonSemanticWorkerConfig,
    changed: Arc<AtomicBool>,
) -> Result<notify::RecommendedWatcher, String> {
    let worker = thread::current();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.as_ref().map_or(true, |event| {
            !matches!(event.kind, notify::EventKind::Access(_))
                && event.paths.iter().any(|path| {
                    path.components().any(|part| part.as_os_str() == "refs")
                        || path.file_name().is_some_and(|name| {
                            matches!(
                                name.to_str(),
                                Some(
                                    "HEAD"
                                        | "packed-refs"
                                        | "config"
                                        | "config.toml"
                                        | "config.local.toml"
                                        | "permissions.toml"
                                )
                            )
                        })
                })
        }) {
            changed.store(true, Ordering::Release);
            worker.unpark();
        }
    })
    .map_err(|error| error.to_string())?;
    for registration in registrations
        .iter()
        .filter(|wiki| config.wikis.contains(&wiki.id))
    {
        let paths = VaultPaths::new(&registration.path);
        let _ = watcher.watch(
            &registration.path.join(".vulcan"),
            notify::RecursiveMode::NonRecursive,
        );
        if registration.sync_paused || !registration.capabilities().semantic_history {
            continue;
        }
        let permitted = resolve_permission_profile(
            &paths,
            Some(
                registration
                    .permissions_profile
                    .as_deref()
                    .unwrap_or("unrestricted"),
            ),
        )
        .map(|selection| ProfilePermissionGuard::new(&paths, selection))
        .is_ok_and(|guard| guard.check_git().is_ok());
        if !permitted {
            continue;
        }
        let repository = vulcan_app::sync_state::sync_work_tree(paths.vault_root())
            .ok()
            .and_then(|root| GitCliEngine::default().discover_repository(&root).ok());
        if let Some(repository) = repository {
            // Root catches packed-refs/config replacement; refs catches loose refs.
            for root in [&repository.git_dir, &repository.common_dir] {
                let _ = watcher.watch(root, notify::RecursiveMode::NonRecursive);
                let _ = watcher.watch(&root.join("refs"), notify::RecursiveMode::Recursive);
            }
        }
    }
    Ok(watcher)
}

#[allow(clippy::too_many_arguments)]
pub fn execute_semantic_worker_pass(
    config: &DaemonSemanticWorkerConfig,
    registry: &WikiRegistry,
    supervisor: &SyncSupervisor,
    state_store: &SyncStateStore,
    agent: &CompanionSemanticAgent,
    gate: Option<&HostedWorkerGate>,
    now_unix_ms: u64,
) -> SemanticWorkerStatus {
    execute_semantic_worker_pass_inner(
        config,
        registry,
        supervisor,
        state_store,
        agent,
        gate,
        now_unix_ms,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn execute_semantic_worker_pass_inner(
    config: &DaemonSemanticWorkerConfig,
    registry: &WikiRegistry,
    supervisor: &SyncSupervisor,
    state_store: &SyncStateStore,
    agent: &CompanionSemanticAgent,
    gate: Option<&HostedWorkerGate>,
    now_unix_ms: u64,
    reconcile_remote: bool,
) -> SemanticWorkerStatus {
    let registrations = match registry.poll() {
        Ok(config) => config.vaults,
        Err(error) => {
            return SemanticWorkerStatus {
                version: SEMANTIC_WORKER_STATUS_VERSION,
                checked_unix_ms: now_unix_ms,
                entries: config
                    .wikis
                    .iter()
                    .map(|wiki| SemanticWorkerStatusEntry {
                        wiki_id: wiki.to_string(),
                        report: None,
                        skipped: None,
                        error: Some(error.to_string()),
                    })
                    .collect(),
            };
        }
    };
    let active = match supervisor.list() {
        Ok(active) => active,
        Err(error) => {
            return SemanticWorkerStatus {
                version: SEMANTIC_WORKER_STATUS_VERSION,
                checked_unix_ms: now_unix_ms,
                entries: config
                    .wikis
                    .iter()
                    .map(|wiki| status_error(wiki.as_str(), error.to_string()))
                    .collect(),
            }
        }
    };
    let entries = config
        .wikis
        .iter()
        .map(|wiki_id| {
            let Some(registration) = registrations.iter().find(|wiki| &wiki.id == wiki_id) else {
                return status_error(wiki_id.as_str(), "registered wiki no longer exists");
            };
            if !registration.capabilities().semantic_history {
                return status_skipped(
                    wiki_id.as_str(),
                    "semantic history requires a knowledge profile; set the directory profile to knowledge",
                );
            }
            if registration.sync_paused {
                return status_skipped(wiki_id.as_str(), "automatic synchronization is paused");
            }
            if active.iter().any(|job| {
                job.job.wiki_id.as_deref() == Some(wiki_id.as_str())
                    && matches!(job.job.state, SyncJobState::Queued | SyncJobState::Running)
            }) {
                return status_skipped(wiki_id.as_str(), "a file-tree sync job is active");
            }
            run_for_registration(
                config,
                registration,
                state_store,
                agent,
                gate,
                now_unix_ms,
                reconcile_remote,
            )
        })
        .collect();
    SemanticWorkerStatus {
        version: SEMANTIC_WORKER_STATUS_VERSION,
        checked_unix_ms: now_unix_ms,
        entries,
    }
}

#[allow(clippy::too_many_arguments)]
fn run_for_registration(
    config: &DaemonSemanticWorkerConfig,
    registration: &crate::registry::WikiRegistration,
    state_store: &SyncStateStore,
    agent: &CompanionSemanticAgent,
    gate: Option<&HostedWorkerGate>,
    now_unix_ms: u64,
    reconcile_remote: bool,
) -> SemanticWorkerStatusEntry {
    let vault_gate = gate.map(|gate| gate.for_registration(registration));
    let gate: &dyn MutationGate = match vault_gate.as_ref() {
        Some(gate) => gate,
        None => &Ungated,
    };
    let paths = VaultPaths::new(&registration.path);
    let profile = registration
        .permissions_profile
        .as_deref()
        .unwrap_or("unrestricted");
    let permission = resolve_permission_profile(&paths, Some(profile))
        .map(|selection| ProfilePermissionGuard::new(&paths, selection));
    let result = permission
        .map_err(|error| error.to_string())
        .and_then(|guard| {
            guard.check_git().map_err(|error| error.to_string())?;
            if let Some(endpoint) = agent.provider().network_endpoint() {
                guard
                    .check_network(endpoint)
                    .map_err(|error| error.to_string())?;
            }
            let options = SemanticAutoOptions {
                semantic_ref: GitRefName::parse(config.semantic_ref.clone())
                    .map_err(|error| error.to_string())?,
                remote: GitRemote::parse(config.remote.clone())
                    .map_err(|error| error.to_string())?,
                live_ref: GitRefName::parse(config.live_ref.clone())
                    .map_err(|error| error.to_string())?,
                grouping: vulcan_app::sync_semantic::SemanticGrouping::Agent,
                agent: true,
                publish: config.publish,
                quiet_seconds: config.quiet_seconds,
                maximum_wait_seconds: config.maximum_wait_seconds,
                dry_run: false,
            };
            run_semantic_auto_gated(
                &paths,
                &options,
                Some(agent.provider()),
                &SyncCancellationToken::default(),
                state_store,
                now_unix_ms,
                reconcile_remote,
                gate,
            )
            .map_err(|error| error.to_string())
        });
    match result {
        Ok(report) => SemanticWorkerStatusEntry {
            wiki_id: registration.id.to_string(),
            report: Some(report),
            skipped: None,
            error: None,
        },
        Err(error) => status_error(registration.id.as_str(), error),
    }
}

fn status_skipped(wiki_id: &str, detail: impl Into<String>) -> SemanticWorkerStatusEntry {
    SemanticWorkerStatusEntry {
        wiki_id: wiki_id.to_string(),
        report: None,
        skipped: Some(detail.into()),
        error: None,
    }
}

fn status_error(wiki_id: &str, detail: impl Into<String>) -> SemanticWorkerStatusEntry {
    SemanticWorkerStatusEntry {
        wiki_id: wiki_id.to_string(),
        report: None,
        skipped: None,
        error: Some(detail.into()),
    }
}

/// Status reports are informative and rewritten often, so they are atomic but
/// not synced.
fn save_status(path: &Path, report: &SemanticWorkerStatus) -> Result<(), String> {
    vulcan_core::durable::replace_json(path, report, vulcan_core::durable::Durability::BestEffort)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
fn wait_until_next_poll(stop: &ShutdownSignal, duration: Duration) -> bool {
    stop.wait_timeout(duration)
}

fn unix_time_ms() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis()
        .try_into()
        .map_err(|error| format!("system time is out of range: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        execute_semantic_worker_pass, load_semantic_worker_status, save_status,
        semantic_worker_status_path, wait_until_next_poll, SemanticWorkerStatus,
        SemanticWorkerStatusEntry, SEMANTIC_WORKER_STATUS_VERSION,
    };
    use crate::companion::CompanionSemanticAgent;
    use crate::registry::{
        AddWikiRequest, DaemonSemanticWorkerConfig, UpdateWikiRequest, WikiId, WikiRegistry,
    };
    use crate::shutdown::ShutdownSignal;
    use crate::supervisor::SyncSupervisor;
    use std::time::Duration;
    use tempfile::tempdir;
    use vulcan_app::sync::SyncCancellationToken;
    use vulcan_app::sync_semantic::{
        SemanticAgentIdentity, SemanticAgentOutput, SemanticAgentProvider, SemanticAgentRequest,
    };
    use vulcan_app::sync_state::SyncStateStore;

    struct PanicProvider;

    #[test]
    fn unchanged_scheduling_has_no_passes_or_status_replacements_before_reconciliation() {
        let report = SemanticWorkerStatus {
            version: SEMANTIC_WORKER_STATUS_VERSION,
            checked_unix_ms: 0,
            entries: vec![super::status_skipped("personal", "paused")],
        };
        let mut passes = 0;
        let mut writes = 0;
        for seconds in 1..300 {
            passes += usize::from(super::pass_due(false, false, false, None, seconds * 1_000));
            let refreshed = SemanticWorkerStatus {
                checked_unix_ms: seconds * 1_000,
                ..report.clone()
            };
            writes += usize::from(super::status_needs_save(Some(&report), &refreshed, false));
        }
        assert_eq!((passes, writes), (0, 0));
        assert!(super::pass_due(true, false, false, None, 300_000));
        assert!(super::status_needs_save(Some(&report), &report, true));
        eprintln!("semantic schedule: 299 idle seconds: passes=0, status replacements=0 (old 30s poll: 9 each)");
    }

    #[test]
    fn changes_deadlines_and_errors_schedule_work() {
        assert!(super::pass_due(false, true, false, None, 0));
        assert!(super::pass_due(false, false, true, None, 0));
        assert!(!super::pass_due(false, false, false, Some(42), 41));
        assert!(super::pass_due(false, false, false, Some(42), 42));
        let status = SemanticWorkerStatus {
            version: SEMANTIC_WORKER_STATUS_VERSION,
            checked_unix_ms: 0,
            entries: vec![super::status_error("personal", "offline")],
        };
        assert_eq!(super::worker_next_due(&status, 1_000, 30), Some(31_000));
        assert!(!super::pass_due(false, false, false, Some(31_000), 30_999));
        assert!(super::pass_due(false, false, false, Some(31_000), 31_000));
    }

    impl SemanticAgentProvider for PanicProvider {
        fn identity(&self) -> SemanticAgentIdentity {
            SemanticAgentIdentity {
                provider: "test".to_string(),
                model: "panic".to_string(),
                prompt_contract_version: 1,
            }
        }

        fn propose(
            &self,
            _request: &SemanticAgentRequest,
            _cancellation: &SyncCancellationToken,
        ) -> Result<SemanticAgentOutput, vulcan_app::AppError> {
            panic!("paused wikis must not call the provider")
        }
    }

    #[test]
    fn worker_wait_observes_shutdown_without_waiting_for_the_full_poll() {
        let stop = ShutdownSignal::new(true);
        assert!(wait_until_next_poll(&stop, Duration::from_secs(60)));
    }

    #[test]
    fn worker_skips_paused_wikis_before_calling_the_provider() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        std::fs::create_dir(&vault).expect("vault directory");
        let registry = WikiRegistry::at(temporary.path().join("config/daemon.toml"));
        let id = WikiId::parse("personal").expect("wiki ID");
        registry
            .add(
                &AddWikiRequest {
                    profile: None,
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
                    profile: None,
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
        let store = SyncStateStore::at(temporary.path().join("state"));
        let config = DaemonSemanticWorkerConfig {
            wikis: vec![id.clone()],
            semantic_ref: "refs/heads/main".to_string(),
            remote: "origin".to_string(),
            live_ref: "refs/heads/__vulcan-sync/live".to_string(),
            publish: true,
            quiet_seconds: 900,
            maximum_wait_seconds: 21_600,
            poll_seconds: 30,
        };
        let status = execute_semantic_worker_pass(
            &config,
            &registry,
            &supervisor,
            &store,
            &CompanionSemanticAgent::new(PanicProvider),
            None,
            1_000,
        );
        assert_eq!(
            status.entries[0].skipped.as_deref(),
            Some("automatic synchronization is paused")
        );
        assert!(status.entries[0].error.is_none());

        registry
            .update(
                &id,
                &UpdateWikiRequest {
                    profile: Some(crate::registry::ManagedDirectoryProfile::FilesOnly),
                    groups_to_add: Vec::new(),
                    groups_to_remove: Vec::new(),
                    permissions_profile: None,
                    sync_paused: Some(false),
                },
                false,
            )
            .expect("select files-only profile");
        let status = execute_semantic_worker_pass(
            &config,
            &registry,
            &supervisor,
            &store,
            &CompanionSemanticAgent::new(PanicProvider),
            None,
            1_001,
        );
        assert!(status.entries[0]
            .skipped
            .as_deref()
            .unwrap()
            .contains("requires a knowledge profile"));
    }

    #[test]
    fn latest_worker_status_round_trips() {
        let temporary = tempdir().expect("temporary directory");
        let report = SemanticWorkerStatus {
            version: SEMANTIC_WORKER_STATUS_VERSION,
            checked_unix_ms: 42,
            entries: vec![SemanticWorkerStatusEntry {
                wiki_id: "personal".to_string(),
                report: None,
                skipped: Some("paused".to_string()),
                error: None,
            }],
        };
        save_status(&semantic_worker_status_path(temporary.path()), &report).expect("save status");
        assert_eq!(
            load_semantic_worker_status(temporary.path()).expect("load status"),
            Some(report)
        );
    }
}
