//! Conservative daemon adapter for automatic agent-backed conflict resolution.

use crate::companion::CompanionResolutionAgent;
use crate::registry::{DaemonConflictWorkerConfig, WikiId, WikiRegistry};
use crate::shutdown::ShutdownSignal;
use crate::supervisor::SyncSupervisor;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tempfile::NamedTempFile;
use vulcan_app::sync::{GitRefName, GitRemote, SyncCancellationToken};
use vulcan_app::sync_conflicts::{
    get_sync_conflict_page_with_state_store, list_sync_conflicts_with_state_store,
    SyncConflictGroupKind, SyncConflictGroupState,
};
use vulcan_app::sync_proposals::{
    create_and_auto_accept_resolution_proposal_with_state_store, ApproveResolutionProposalOptions,
    ResolutionProposalOptions,
};
use vulcan_app::sync_state::SyncStateStore;
use vulcan_core::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};
use vulcan_sync::{GitConflictClass, MergeFileKind, SyncJobState};

pub const CONFLICT_WORKER_STATUS_VERSION: u32 = 1;
const CONFLICT_PAGE_LIMIT: usize = 256;
const MAX_HIGH_CONFIDENCE_PATH_BYTES: u64 = 1024 * 1024;
const MAX_HIGH_CONFIDENCE_REQUEST_BYTES: u64 = 4 * 1024 * 1024;
const ERROR_RETRY_DELAY_MS: u64 = 5 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictWorkerStatus {
    pub version: u32,
    pub checked_unix_ms: u64,
    pub entries: Vec<ConflictWorkerStatusEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictWorkerStatusEntry {
    pub wiki_id: String,
    pub unresolved_conflicts: usize,
    pub eligible_groups: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_unix_ms: Option<u64>,
}

#[must_use]
pub fn conflict_worker_status_path(state_root: &Path) -> PathBuf {
    state_root.join("daemon/conflict-worker.json")
}

pub fn load_conflict_worker_status(
    state_root: &Path,
) -> Result<Option<ConflictWorkerStatus>, String> {
    let path = conflict_worker_status_path(state_root);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let report: ConflictWorkerStatus =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if report.version != CONFLICT_WORKER_STATUS_VERSION {
        return Err(format!(
            "unsupported conflict worker status version {}",
            report.version
        ));
    }
    Ok(Some(report))
}

pub fn spawn_conflict_worker(
    config: DaemonConflictWorkerConfig,
    registry: WikiRegistry,
    supervisor: Arc<SyncSupervisor>,
    state_store: Arc<SyncStateStore>,
    daemon_state_root: PathBuf,
    agent: Arc<CompanionResolutionAgent>,
    stop: Arc<ShutdownSignal>,
) -> JoinHandle<Result<(), String>> {
    thread::spawn(move || {
        run_conflict_worker(
            &config,
            &registry,
            &supervisor,
            &state_store,
            &daemon_state_root,
            &agent,
            &stop,
        )
    })
}

pub fn run_conflict_worker(
    config: &DaemonConflictWorkerConfig,
    registry: &WikiRegistry,
    supervisor: &SyncSupervisor,
    state_store: &SyncStateStore,
    daemon_state_root: &Path,
    agent: &CompanionResolutionAgent,
    stop: &ShutdownSignal,
) -> Result<(), String> {
    let mut previous = load_conflict_worker_status(daemon_state_root)?;
    loop {
        let report = execute_conflict_worker_pass(
            config,
            registry,
            supervisor,
            state_store,
            agent,
            previous.as_ref(),
            unix_time_ms()?,
        );
        save_status(&conflict_worker_status_path(daemon_state_root), &report)?;
        previous = Some(report);
        if stop.wait_timeout(Duration::from_secs(config.poll_seconds)) {
            return Ok(());
        }
    }
}

pub fn execute_conflict_worker_pass(
    config: &DaemonConflictWorkerConfig,
    registry: &WikiRegistry,
    supervisor: &SyncSupervisor,
    state_store: &SyncStateStore,
    agent: &CompanionResolutionAgent,
    previous: Option<&ConflictWorkerStatus>,
    now_unix_ms: u64,
) -> ConflictWorkerStatus {
    let registrations = match registry.load() {
        Ok(config) => config.vaults,
        Err(error) => {
            return ConflictWorkerStatus {
                version: CONFLICT_WORKER_STATUS_VERSION,
                checked_unix_ms: now_unix_ms,
                entries: config
                    .wikis
                    .iter()
                    .map(|wiki| status_error(wiki, 0, 0, error.to_string(), now_unix_ms))
                    .collect(),
            };
        }
    };
    let active = supervisor.list().unwrap_or_default();
    let previous = previous
        .map(|status| {
            status
                .entries
                .iter()
                .map(|entry| (entry.wiki_id.as_str(), entry))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let entries = config
        .wikis
        .iter()
        .map(|wiki_id| {
            let Some(registration) = registrations.iter().find(|wiki| &wiki.id == wiki_id) else {
                return status_error(
                    wiki_id,
                    0,
                    0,
                    "registered wiki no longer exists",
                    now_unix_ms,
                );
            };
            if registration.sync_paused {
                return status_skipped(wiki_id, 0, "automatic synchronization is paused");
            }
            if active.iter().any(|job| {
                job.job.wiki_id.as_deref() == Some(wiki_id.as_str())
                    && matches!(job.job.state, SyncJobState::Queued | SyncJobState::Running)
            }) {
                return status_skipped(wiki_id, 0, "a file-tree sync job is active");
            }
            if let Some(entry) = previous.get(wiki_id.as_str()).filter(|entry| {
                entry
                    .retry_after_unix_ms
                    .is_some_and(|retry| retry > now_unix_ms)
            }) {
                return status_backoff(entry);
            }
            run_for_registration(config, registration, state_store, agent, now_unix_ms)
        })
        .collect();
    ConflictWorkerStatus {
        version: CONFLICT_WORKER_STATUS_VERSION,
        checked_unix_ms: now_unix_ms,
        entries,
    }
}

fn run_for_registration(
    config: &DaemonConflictWorkerConfig,
    registration: &crate::registry::WikiRegistration,
    state_store: &SyncStateStore,
    agent: &CompanionResolutionAgent,
    now_unix_ms: u64,
) -> ConflictWorkerStatusEntry {
    let paths = VaultPaths::new(&registration.path);
    let conflicts = match list_sync_conflicts_with_state_store(&paths, state_store) {
        Ok(conflicts) => conflicts,
        Err(error) => return status_error(&registration.id, 0, 0, error.to_string(), now_unix_ms),
    };
    if conflicts.conflicts.is_empty() {
        return status_skipped(&registration.id, 0, "no unresolved conflicts");
    }
    for conflict in &conflicts.conflicts {
        let groups = match high_confidence_groups(
            &paths,
            state_store,
            &conflict.id,
            config.max_groups_per_run,
        ) {
            Ok(groups) => groups,
            Err(error) => {
                return status_error(&registration.id, conflicts.count, 0, error, now_unix_ms);
            }
        };
        if groups.is_empty() {
            continue;
        }
        let profile = registration
            .permissions_profile
            .as_deref()
            .unwrap_or("unrestricted");
        let result = resolve_permission_profile(&paths, Some(profile))
            .map(|selection| ProfilePermissionGuard::new(&paths, selection))
            .map_err(|error| error.to_string())
            .and_then(|guard| {
                guard.check_git().map_err(|error| error.to_string())?;
                if let Some(endpoint) = agent.provider().network_endpoint() {
                    guard
                        .check_network(endpoint)
                        .map_err(|error| error.to_string())?;
                }
                let _claim = agent
                    .claim_conflict(format!("{}:{}", registration.path.display(), conflict.id))
                    .map_err(|error| error.to_string())?;
                create_and_auto_accept_resolution_proposal_with_state_store(
                    &paths,
                    &conflict.id,
                    &ResolutionProposalOptions {
                        permission_profile: profile.to_string(),
                        focused_context: Vec::new(),
                        allow_broad_context: false,
                        group_ids: groups.clone(),
                    },
                    &ApproveResolutionProposalOptions {
                        remote: GitRemote::parse(config.remote.clone())
                            .map_err(|error| error.to_string())?,
                        live_ref: GitRefName::parse(config.live_ref.clone())
                            .map_err(|error| error.to_string())?,
                        dry_run: false,
                        automatic: true,
                    },
                    agent.provider(),
                    &SyncCancellationToken::default(),
                    state_store,
                )
                .map_err(|error| error.to_string())
            });
        return match result {
            Ok(report) => ConflictWorkerStatusEntry {
                wiki_id: registration.id.to_string(),
                unresolved_conflicts: conflicts.count,
                eligible_groups: groups.len(),
                conflict_id: Some(conflict.id.clone()),
                proposal_id: Some(report.proposal.proposal_id),
                resolution_commit: report.approval.resolution_commit,
                skipped: None,
                error: None,
                retry_after_unix_ms: None,
            },
            Err(error) => status_error(
                &registration.id,
                conflicts.count,
                groups.len(),
                error,
                now_unix_ms,
            ),
        };
    }
    status_skipped(
        &registration.id,
        conflicts.count,
        "no pending conflict groups met the high-confidence policy",
    )
}

fn high_confidence_groups(
    paths: &VaultPaths,
    state_store: &SyncStateStore,
    conflict_id: &str,
    limit: usize,
) -> Result<Vec<String>, String> {
    let mut offset = 0;
    let mut selected = Vec::new();
    let mut selected_bytes = 0_u64;
    loop {
        let detail = get_sync_conflict_page_with_state_store(
            paths,
            conflict_id,
            offset,
            CONFLICT_PAGE_LIMIT,
            state_store,
        )
        .map_err(|error| error.to_string())?;
        let pending = detail
            .progress
            .groups
            .iter()
            .filter(|group| group.state == SyncConflictGroupState::Pending)
            .map(|group| group.id.as_str())
            .collect::<BTreeSet<_>>();
        for path in &detail.record.paths {
            if selected.len() == limit {
                return Ok(selected);
            }
            if path.group_kind != SyncConflictGroupKind::Path
                || !pending.contains(path.group_id.as_str())
            {
                continue;
            }
            let Some(classification) = path.classification.as_ref() else {
                continue;
            };
            if classification.class != GitConflictClass::OverlappingText
                || !matches!(
                    classification.file_kind,
                    MergeFileKind::Markdown | MergeFileKind::Text
                )
            {
                continue;
            }
            let sides = [&path.base, &path.local, &path.remote];
            if sides.iter().any(|side| {
                side.object_id.is_none()
                    || !matches!(side.mode.as_deref(), Some("100644" | "100755"))
                    || side
                        .bytes
                        .is_none_or(|bytes| bytes > MAX_HIGH_CONFIDENCE_PATH_BYTES)
            }) {
                continue;
            }
            let bytes = sides.iter().filter_map(|side| side.bytes).sum::<u64>();
            if selected_bytes.saturating_add(bytes) > MAX_HIGH_CONFIDENCE_REQUEST_BYTES {
                continue;
            }
            selected_bytes += bytes;
            selected.push(path.group_id.clone());
        }
        let Some(page) = detail.path_page else {
            break;
        };
        let Some(next) = page.next_offset else {
            break;
        };
        offset = next;
    }
    Ok(selected)
}

fn status_skipped(
    wiki_id: &WikiId,
    unresolved_conflicts: usize,
    detail: impl Into<String>,
) -> ConflictWorkerStatusEntry {
    ConflictWorkerStatusEntry {
        wiki_id: wiki_id.to_string(),
        unresolved_conflicts,
        eligible_groups: 0,
        conflict_id: None,
        proposal_id: None,
        resolution_commit: None,
        skipped: Some(detail.into()),
        error: None,
        retry_after_unix_ms: None,
    }
}

fn status_backoff(previous: &ConflictWorkerStatusEntry) -> ConflictWorkerStatusEntry {
    let mut entry = previous.clone();
    entry.skipped = Some("waiting for provider error backoff".to_string());
    entry
}

fn status_error(
    wiki_id: &WikiId,
    unresolved_conflicts: usize,
    eligible_groups: usize,
    detail: impl Into<String>,
    now_unix_ms: u64,
) -> ConflictWorkerStatusEntry {
    ConflictWorkerStatusEntry {
        wiki_id: wiki_id.to_string(),
        unresolved_conflicts,
        eligible_groups,
        conflict_id: None,
        proposal_id: None,
        resolution_commit: None,
        skipped: None,
        error: Some(detail.into()),
        retry_after_unix_ms: Some(now_unix_ms.saturating_add(ERROR_RETRY_DELAY_MS)),
    }
}

fn save_status(path: &Path, report: &ConflictWorkerStatus) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "conflict worker status path has no parent".to_string())?;
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
        conflict_worker_status_path, execute_conflict_worker_pass, load_conflict_worker_status,
        save_status, ConflictWorkerStatus, ConflictWorkerStatusEntry,
        CONFLICT_WORKER_STATUS_VERSION,
    };
    use crate::companion::CompanionResolutionAgent;
    use crate::registry::{AddWikiRequest, DaemonConflictWorkerConfig, WikiId, WikiRegistry};
    use crate::supervisor::SyncSupervisor;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;
    use vulcan_app::sync::{
        sync_git_vault_with_state_store, GitSyncOptions, SyncCancellationToken,
    };
    use vulcan_app::sync_conflicts::list_sync_conflicts_with_state_store;
    use vulcan_app::sync_proposals::{
        ResolutionAgentIdentity, ResolutionAgentOutput, ResolutionAgentPathOutput,
        ResolutionAgentProvider, ResolutionAgentRequest, ResolutionAgentTools,
    };
    use vulcan_app::sync_state::SyncStateStore;
    use vulcan_core::VaultPaths;

    struct ResolvingProvider;

    impl ResolutionAgentProvider for ResolvingProvider {
        fn identity(&self) -> ResolutionAgentIdentity {
            ResolutionAgentIdentity {
                provider: "test".to_string(),
                model: "resolver".to_string(),
                prompt_contract_version: 1,
            }
        }

        fn propose(
            &self,
            request: &ResolutionAgentRequest,
            _tools: &mut dyn ResolutionAgentTools,
            _cancellation: &SyncCancellationToken,
        ) -> Result<ResolutionAgentOutput, vulcan_app::AppError> {
            Ok(ResolutionAgentOutput {
                explanation: "Both edits are represented by the resolved text.".to_string(),
                referenced_context: Vec::new(),
                paths: request
                    .files
                    .iter()
                    .map(|file| ResolutionAgentPathOutput {
                        path: file.path.clone(),
                        content: b"resolved\n".to_vec(),
                    })
                    .collect(),
            })
        }
    }

    fn git(directory: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn configure_repository(directory: &Path) {
        git(directory, &["config", "user.name", "Vulcan Test"]);
        git(
            directory,
            &["config", "user.email", "vulcan@example.invalid"],
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Keep the real-Git lifecycle visible as one acceptance case.
    fn worker_auto_accepts_only_the_bounded_text_group_and_persists_status() {
        let temporary = tempdir().expect("temporary directory");
        let remote = temporary.path().join("remote.git");
        fs::create_dir(&remote).expect("remote directory");
        git(&remote, &["init", "--bare", "--quiet"]);
        let writer = temporary.path().join("writer");
        fs::create_dir(&writer).expect("writer directory");
        git(&writer, &["init", "--quiet"]);
        configure_repository(&writer);
        fs::write(writer.join("Home.md"), "base\n").expect("base note");
        git(&writer, &["add", "Home.md"]);
        git(&writer, &["commit", "--quiet", "-m", "base"]);
        git(
            &writer,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        git(&writer, &["push", "--quiet", "-u", "origin", "HEAD:main"]);
        git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        let reader = temporary.path().join("reader");
        git(
            temporary.path(),
            &[
                "clone",
                "--quiet",
                remote.to_str().expect("remote path"),
                reader.to_str().expect("reader path"),
            ],
        );
        configure_repository(&reader);
        let store = SyncStateStore::at(temporary.path().join("state/sync"));
        sync_git_vault_with_state_store(
            &VaultPaths::new(&writer),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("bootstrap writer");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&reader),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("bootstrap reader");
        fs::write(writer.join("Home.md"), "writer\n").expect("writer edit");
        fs::write(reader.join("Home.md"), "reader\n").expect("reader edit");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&writer),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("publish writer");
        sync_git_vault_with_state_store(
            &VaultPaths::new(&reader),
            &GitSyncOptions::default(),
            &store,
        )
        .expect("preserve reader conflict");
        fs::create_dir_all(reader.join(".vulcan")).expect("local config directory");
        fs::write(
            reader.join(".vulcan/config.local.toml"),
            "[sync]\nagent_auto_accept = true\n",
        )
        .expect("enable local auto accept");

        let registry = WikiRegistry::at(temporary.path().join("config/daemon.toml"));
        let wiki = WikiId::parse("notes").expect("wiki ID");
        registry
            .add(
                &AddWikiRequest {
                    id: wiki.clone(),
                    path: reader.clone(),
                    groups: Vec::new(),
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("git".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .expect("register reader");
        let supervisor =
            SyncSupervisor::at(temporary.path().join("state/jobs.json")).expect("supervisor");
        let config = DaemonConflictWorkerConfig {
            wikis: vec![wiki],
            remote: "origin".to_string(),
            live_ref: "refs/heads/__vulcan-sync/live".to_string(),
            max_groups_per_run: 128,
            poll_seconds: 30,
        };
        let agent = CompanionResolutionAgent::new(ResolvingProvider);
        let status = execute_conflict_worker_pass(
            &config,
            &registry,
            &supervisor,
            &store,
            &agent,
            None,
            1_000,
        );
        let entry = status.entries.first().expect("status entry");
        assert_eq!(entry.eligible_groups, 1);
        assert!(
            entry.proposal_id.is_some(),
            "proposal failed: {:?}",
            entry.error
        );
        assert!(entry.resolution_commit.is_some());
        assert_eq!(
            fs::read_to_string(reader.join("Home.md")).unwrap(),
            "resolved\n"
        );
        assert_eq!(
            list_sync_conflicts_with_state_store(&VaultPaths::new(&reader), &store)
                .expect("conflict list")
                .count,
            0
        );

        let status_path = conflict_worker_status_path(temporary.path());
        save_status(&status_path, &status).expect("save status");
        assert_eq!(
            load_conflict_worker_status(temporary.path()).expect("load status"),
            Some(status)
        );
    }

    #[test]
    fn provider_failures_observe_persisted_backoff() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).expect("vault directory");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        let wiki = WikiId::parse("notes").expect("wiki ID");
        registry
            .add(
                &AddWikiRequest {
                    id: wiki.clone(),
                    path: vault,
                    groups: Vec::new(),
                    git_dir: None,
                    permissions_profile: None,
                    sync_backend: Some("git".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .expect("register vault");
        let config = DaemonConflictWorkerConfig {
            wikis: vec![wiki],
            remote: "origin".to_string(),
            live_ref: "refs/heads/__vulcan-sync/live".to_string(),
            max_groups_per_run: 1,
            poll_seconds: 30,
        };
        let previous = ConflictWorkerStatus {
            version: CONFLICT_WORKER_STATUS_VERSION,
            checked_unix_ms: 1_000,
            entries: vec![ConflictWorkerStatusEntry {
                wiki_id: "notes".to_string(),
                unresolved_conflicts: 1,
                eligible_groups: 1,
                conflict_id: None,
                proposal_id: None,
                resolution_commit: None,
                skipped: None,
                error: Some("provider failed".to_string()),
                retry_after_unix_ms: Some(10_000),
            }],
        };
        let store = SyncStateStore::at(temporary.path().join("state/sync"));
        let supervisor =
            SyncSupervisor::at(temporary.path().join("state/jobs.json")).expect("supervisor");
        let status = execute_conflict_worker_pass(
            &config,
            &registry,
            &supervisor,
            &store,
            &CompanionResolutionAgent::new(ResolvingProvider),
            Some(&previous),
            5_000,
        );
        assert_eq!(
            status.entries[0].skipped.as_deref(),
            Some("waiting for provider error backoff")
        );
        assert_eq!(status.entries[0].error.as_deref(), Some("provider failed"));
        assert_eq!(status.entries[0].retry_after_unix_ms, Some(10_000));

        let next_status = execute_conflict_worker_pass(
            &config,
            &registry,
            &supervisor,
            &store,
            &CompanionResolutionAgent::new(ResolvingProvider),
            Some(&status),
            6_000,
        );
        assert_eq!(
            next_status.entries[0].skipped.as_deref(),
            Some("waiting for provider error backoff")
        );
        assert_eq!(next_status.entries[0].retry_after_unix_ms, Some(10_000));
    }
}
