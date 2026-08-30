//! Low-frequency daemon adapter for finite semantic automation cycles.

use crate::companion::CompanionSemanticAgent;
use crate::registry::{DaemonSemanticWorkerConfig, WikiRegistry};
use crate::supervisor::SyncSupervisor;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::NamedTempFile;
use vulcan_app::sync::{GitRefName, GitRemote, SyncCancellationToken};
use vulcan_app::sync_semantic_auto::{run_semantic_auto, SemanticAutoOptions, SemanticAutoReport};
use vulcan_app::sync_state::SyncStateStore;
use vulcan_core::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};
use vulcan_sync::SyncJobState;

pub const SEMANTIC_WORKER_STATUS_VERSION: u32 = 1;
const WAIT_SLICE: Duration = Duration::from_millis(250);

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

pub fn spawn_semantic_worker(
    config: DaemonSemanticWorkerConfig,
    registry: WikiRegistry,
    supervisor: Arc<SyncSupervisor>,
    state_store: Arc<SyncStateStore>,
    daemon_state_root: PathBuf,
    agent: Arc<CompanionSemanticAgent>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<Result<(), String>> {
    thread::spawn(move || loop {
        let report = execute_semantic_worker_pass(
            &config,
            &registry,
            &supervisor,
            &state_store,
            &agent,
            unix_time_ms()?,
        );
        save_status(&semantic_worker_status_path(&daemon_state_root), &report)?;
        if wait_until_next_poll(&stop, Duration::from_secs(config.poll_seconds)) {
            return Ok(());
        }
    })
}

pub fn execute_semantic_worker_pass(
    config: &DaemonSemanticWorkerConfig,
    registry: &WikiRegistry,
    supervisor: &SyncSupervisor,
    state_store: &SyncStateStore,
    agent: &CompanionSemanticAgent,
    now_unix_ms: u64,
) -> SemanticWorkerStatus {
    let registrations = match registry.load() {
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
    let active = supervisor.list().unwrap_or_default();
    let entries = config
        .wikis
        .iter()
        .map(|wiki_id| {
            let Some(registration) = registrations.iter().find(|wiki| &wiki.id == wiki_id) else {
                return status_error(wiki_id.as_str(), "registered wiki no longer exists");
            };
            if registration.sync_paused {
                return status_skipped(wiki_id.as_str(), "automatic synchronization is paused");
            }
            if active.iter().any(|job| {
                job.job.wiki_id.as_deref() == Some(wiki_id.as_str())
                    && matches!(job.job.state, SyncJobState::Queued | SyncJobState::Running)
            }) {
                return status_skipped(wiki_id.as_str(), "a file-tree sync job is active");
            }
            run_for_registration(config, registration, state_store, agent, now_unix_ms)
        })
        .collect();
    SemanticWorkerStatus {
        version: SEMANTIC_WORKER_STATUS_VERSION,
        checked_unix_ms: now_unix_ms,
        entries,
    }
}

fn run_for_registration(
    config: &DaemonSemanticWorkerConfig,
    registration: &crate::registry::WikiRegistration,
    state_store: &SyncStateStore,
    agent: &CompanionSemanticAgent,
    now_unix_ms: u64,
) -> SemanticWorkerStatusEntry {
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
            run_semantic_auto(
                &paths,
                &options,
                Some(agent.provider()),
                &SyncCancellationToken::default(),
                state_store,
                now_unix_ms,
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

fn save_status(path: &Path, report: &SemanticWorkerStatus) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "semantic worker status path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temporary
        .write_all(&serde_json::to_vec_pretty(report).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    temporary
        .write_all(b"\n")
        .map_err(|error| error.to_string())?;
    temporary
        .persist(path)
        .map_err(|error| error.error.to_string())?;
    Ok(())
}

fn wait_until_next_poll(stop: &AtomicBool, duration: Duration) -> bool {
    let mut remaining = duration;
    while !stop.load(Ordering::Acquire) && !remaining.is_zero() {
        let slice = remaining.min(WAIT_SLICE);
        thread::sleep(slice);
        remaining = remaining.saturating_sub(slice);
    }
    stop.load(Ordering::Acquire)
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
        SemanticWorkerStatusEntry, SEMANTIC_WORKER_STATUS_VERSION, WAIT_SLICE,
    };
    use crate::companion::CompanionSemanticAgent;
    use crate::registry::{
        AddWikiRequest, DaemonSemanticWorkerConfig, UpdateWikiRequest, WikiId, WikiRegistry,
    };
    use crate::supervisor::SyncSupervisor;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;
    use tempfile::tempdir;
    use vulcan_app::sync::SyncCancellationToken;
    use vulcan_app::sync_semantic::{
        SemanticAgentIdentity, SemanticAgentOutput, SemanticAgentProvider, SemanticAgentRequest,
    };
    use vulcan_app::sync_state::SyncStateStore;

    struct PanicProvider;

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
        let stop = AtomicBool::new(true);
        assert!(wait_until_next_poll(&stop, Duration::from_secs(60)));
        assert!(WAIT_SLICE < Duration::from_secs(1));
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
        let store = SyncStateStore::at(temporary.path().join("state"));
        let config = DaemonSemanticWorkerConfig {
            wikis: vec![id],
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
            1_000,
        );
        assert_eq!(
            status.entries[0].skipped.as_deref(),
            Some("automatic synchronization is paused")
        );
        assert!(status.entries[0].error.is_none());
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
