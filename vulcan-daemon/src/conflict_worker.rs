//! Daemon adapter that replays paused branches with the resolution agent.
//!
//! A guarded rebase that stops on a conflict pauses the branch lane. For
//! wikis on this worker's allowlist, the resolution agent replays the local
//! commits onto the upstream instead (see [`replay_diverged_branch`]), and a
//! sync publishes the result.

use crate::companion::CompanionResolutionAgent;
use crate::registry::{DaemonConflictWorkerConfig, WikiId, WikiRegistry};
use crate::shutdown::ShutdownSignal;
use crate::supervisor::SyncSupervisor;
use crate::worker_gate::HostedWorkerGate;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vulcan_app::branch_replay::{replay_attempt, replay_diverged_branch, BranchReplayReport};
use vulcan_app::execution::{MutationGate, Ungated};
use vulcan_app::sync::SyncCancellationToken;
use vulcan_core::{
    resolve_device_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};
use vulcan_sync::{SyncJobState, SyncJobTrigger};

pub const CONFLICT_WORKER_STATUS_VERSION: u32 = 1;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_unix_ms: Option<u64>,
    /// The commit a paused branch was replayed to with the agent's help.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replayed_branch: Option<String>,
    /// The `head..upstream` replay that was declined, so it is not retried
    /// until either side moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declined_replay: Option<String>,
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

#[allow(clippy::too_many_arguments)]
pub fn spawn_conflict_worker(
    config: DaemonConflictWorkerConfig,
    registry: WikiRegistry,
    supervisor: Arc<SyncSupervisor>,
    daemon_state_root: PathBuf,
    agent: Arc<CompanionResolutionAgent>,
    gate: Arc<HostedWorkerGate>,
    stop: Arc<ShutdownSignal>,
) -> JoinHandle<Result<(), String>> {
    thread::spawn(move || {
        run_conflict_worker(
            &config,
            &registry,
            &supervisor,
            &daemon_state_root,
            &agent,
            &gate,
            &stop,
        )
    })
}

/// Branches only pause during synchronization, so a pass runs whenever the
/// supervisor's jobs change, and at least every `poll_seconds` to reconcile
/// what was missed. Moving a replayed branch waits for the host's mutation
/// permit for that vault.
#[allow(clippy::too_many_arguments)]
pub fn run_conflict_worker(
    config: &DaemonConflictWorkerConfig,
    registry: &WikiRegistry,
    supervisor: &SyncSupervisor,
    daemon_state_root: &Path,
    agent: &CompanionResolutionAgent,
    gate: &HostedWorkerGate,
    stop: &ShutdownSignal,
) -> Result<(), String> {
    let mut previous = load_conflict_worker_status(daemon_state_root)?;
    let mut jobs = supervisor.subscribe_changes();
    let reconciliation = Duration::from_secs(config.poll_seconds);
    loop {
        let report = execute_conflict_worker_pass(
            config,
            registry,
            supervisor,
            agent,
            Some(gate),
            previous.as_ref(),
            unix_time_ms()?,
        );
        save_status(&conflict_worker_status_path(daemon_state_root), &report)?;
        previous = Some(report);
        let next_reconciliation = std::time::Instant::now() + reconciliation;
        // Job changes are observed within one second without running Git.
        loop {
            let remaining =
                next_reconciliation.saturating_duration_since(std::time::Instant::now());
            if stop.wait_timeout(remaining.min(Duration::from_secs(1))) {
                return Ok(());
            }
            if jobs.has_changed().unwrap_or(false) {
                jobs.borrow_and_update();
                break;
            }
            if remaining.is_zero() {
                break;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn execute_conflict_worker_pass(
    config: &DaemonConflictWorkerConfig,
    registry: &WikiRegistry,
    supervisor: &SyncSupervisor,
    agent: &CompanionResolutionAgent,
    gate: Option<&HostedWorkerGate>,
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
                    .map(|wiki| status_error(wiki, error.to_string(), now_unix_ms))
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
                return status_error(wiki_id, "registered wiki no longer exists", now_unix_ms);
            };
            if !registration.capabilities().agent_resolution {
                return status_skipped(
                    wiki_id,
                    "agent conflict resolution requires a knowledge profile; set the directory profile to knowledge",
                );
            }
            if registration.sync_paused {
                return status_skipped(wiki_id, "automatic synchronization is paused");
            }
            if active.iter().any(|job| {
                job.job.wiki_id.as_deref() == Some(wiki_id.as_str())
                    && matches!(job.job.state, SyncJobState::Queued | SyncJobState::Running)
            }) {
                return status_skipped(wiki_id, "a file-tree sync job is active");
            }
            if let Some(entry) = previous.get(wiki_id.as_str()).filter(|entry| {
                entry
                    .retry_after_unix_ms
                    .is_some_and(|retry| retry > now_unix_ms)
            }) {
                return status_backoff(entry);
            }
            let declined = previous
                .get(wiki_id.as_str())
                .and_then(|entry| entry.declined_replay.as_deref());
            replay(registration, supervisor, agent, gate, declined, now_unix_ms)
        })
        .collect();
    ConflictWorkerStatus {
        version: CONFLICT_WORKER_STATUS_VERSION,
        checked_unix_ms: now_unix_ms,
        entries,
    }
}

fn replay(
    registration: &crate::registry::WikiRegistration,
    supervisor: &SyncSupervisor,
    agent: &CompanionResolutionAgent,
    gate: Option<&HostedWorkerGate>,
    declined: Option<&str>,
    now_unix_ms: u64,
) -> ConflictWorkerStatusEntry {
    match replay_paused_branch(registration, supervisor, agent, gate, declined) {
        Ok(BranchReplayReport::Replayed { to, .. }) => {
            let mut entry = status_skipped(
                &registration.id,
                "replayed the paused branch; a sync publishes it",
            );
            entry.replayed_branch = Some(to.to_string());
            entry
        }
        Ok(BranchReplayReport::Declined {
            head,
            upstream,
            reason,
        }) => {
            let mut entry = status_skipped(
                &registration.id,
                format!("paused branch not replayed: {reason}"),
            );
            entry.declined_replay = Some(replay_attempt(&head, &upstream));
            entry
        }
        Ok(BranchReplayReport::NotDiverged) => {
            status_skipped(&registration.id, "the branch has not diverged")
        }
        Ok(BranchReplayReport::NotPermitted { reason }) => status_skipped(&registration.id, reason),
        Err(error) => status_error(&registration.id, error, now_unix_ms),
    }
}

/// A branch whose rebase stopped on a conflict is paused until someone
/// rebases it. With the wiki's consent here, the resolution agent replays it
/// instead, and a sync publishes the result at once.
fn replay_paused_branch(
    registration: &crate::registry::WikiRegistration,
    supervisor: &SyncSupervisor,
    agent: &CompanionResolutionAgent,
    gate: Option<&HostedWorkerGate>,
    declined: Option<&str>,
) -> Result<BranchReplayReport, String> {
    let paths = VaultPaths::new(&registration.path);
    let profile = registration
        .permissions_profile
        .as_deref()
        .unwrap_or("unrestricted");
    let guard = resolve_device_permission_profile(&paths, Some(profile))
        .map(|selection| ProfilePermissionGuard::new(&paths, selection))
        .map_err(|error| error.to_string())?;
    guard.check_git().map_err(|error| error.to_string())?;
    if let Some(endpoint) = agent.provider().network_endpoint() {
        guard
            .check_network(endpoint)
            .map_err(|error| error.to_string())?;
    }
    let vault_gate = gate.map(|gate| gate.for_registration(registration));
    let gate: &dyn MutationGate = match vault_gate.as_ref() {
        Some(gate) => gate,
        None => &Ungated,
    };
    let report = replay_diverged_branch(
        &paths,
        agent.provider(),
        guard,
        declined,
        &SyncCancellationToken::default(),
        gate,
    )
    .map_err(|error| error.to_string())?;
    if let BranchReplayReport::Replayed {
        from, to, resolved, ..
    } = &report
    {
        eprintln!(
            "wiki `{}`: the resolution agent replayed the paused branch {from} -> {to} (resolved: {})",
            registration.id,
            resolved.join(", ")
        );
        supervisor
            .enqueue(
                registration.id.to_string(),
                registration.path.clone(),
                SyncJobTrigger::Activity,
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(report)
}

fn status_skipped(wiki_id: &WikiId, detail: impl Into<String>) -> ConflictWorkerStatusEntry {
    ConflictWorkerStatusEntry {
        wiki_id: wiki_id.to_string(),
        skipped: Some(detail.into()),
        error: None,
        retry_after_unix_ms: None,
        replayed_branch: None,
        declined_replay: None,
    }
}

fn status_backoff(previous: &ConflictWorkerStatusEntry) -> ConflictWorkerStatusEntry {
    let mut entry = previous.clone();
    entry.skipped = Some("waiting for error backoff".to_string());
    entry
}

fn status_error(
    wiki_id: &WikiId,
    detail: impl Into<String>,
    now_unix_ms: u64,
) -> ConflictWorkerStatusEntry {
    ConflictWorkerStatusEntry {
        wiki_id: wiki_id.to_string(),
        skipped: None,
        error: Some(detail.into()),
        retry_after_unix_ms: Some(now_unix_ms.saturating_add(ERROR_RETRY_DELAY_MS)),
        replayed_branch: None,
        declined_replay: None,
    }
}

/// Status reports are informative and rewritten often, so they are atomic but
/// not synced.
fn save_status(path: &Path, report: &ConflictWorkerStatus) -> Result<(), String> {
    vulcan_core::durable::replace_json(path, report, vulcan_core::durable::Durability::BestEffort)
        .map_err(|error| error.to_string())
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
    use crate::registry::{
        AddWikiRequest, DaemonConflictWorkerConfig, ManagedDirectoryProfile, UpdateWikiRequest,
        WikiId, WikiRegistry,
    };
    use crate::supervisor::SyncSupervisor;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;
    use vulcan_app::sync::SyncCancellationToken;
    use vulcan_app::sync_proposals::{
        ResolutionAgentIdentity, ResolutionAgentOutput, ResolutionAgentPathOutput,
        ResolutionAgentProvider, ResolutionAgentRequest, ResolutionAgentTools,
    };

    fn normalize_checkout_line_endings(contents: &str) -> String {
        contents.replace("\r\n", "\n")
    }

    #[test]
    fn status_round_trips_through_the_daemon_state_directory() {
        let temporary = tempdir().expect("temporary directory");
        assert_eq!(load_conflict_worker_status(temporary.path()), Ok(None));
        let status = ConflictWorkerStatus {
            version: CONFLICT_WORKER_STATUS_VERSION,
            checked_unix_ms: 42,
            entries: vec![ConflictWorkerStatusEntry {
                wiki_id: "notes".to_string(),
                skipped: Some("the branch has not diverged".to_string()),
                error: None,
                retry_after_unix_ms: None,
                replayed_branch: None,
                declined_replay: None,
            }],
        };
        save_status(&conflict_worker_status_path(temporary.path()), &status).expect("save");
        assert_eq!(
            load_conflict_worker_status(temporary.path()),
            Ok(Some(status))
        );
    }

    #[test]
    fn checkout_assertions_accept_crlf_without_changing_the_expected_text() {
        assert_eq!(
            normalize_checkout_line_endings("resolved\r\n"),
            "resolved\n"
        );
        assert_eq!(normalize_checkout_line_endings("resolved\n"), "resolved\n");
    }

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
        git(directory, &["config", "core.autocrlf", "false"]);
    }

    struct CountingProvider {
        content: &'static str,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ResolutionAgentProvider for CountingProvider {
        fn identity(&self) -> ResolutionAgentIdentity {
            ResolutionAgentIdentity {
                provider: "test".to_string(),
                model: "counting".to_string(),
                prompt_contract_version: 1,
            }
        }

        fn propose(
            &self,
            request: &ResolutionAgentRequest,
            _tools: &mut dyn ResolutionAgentTools,
            _cancellation: &SyncCancellationToken,
        ) -> Result<ResolutionAgentOutput, vulcan_app::AppError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ResolutionAgentOutput {
                explanation: "merged".to_string(),
                referenced_context: Vec::new(),
                paths: request
                    .files
                    .iter()
                    .map(|file| ResolutionAgentPathOutput {
                        path: file.path.clone(),
                        content: self.content.as_bytes().to_vec(),
                    })
                    .collect(),
            })
        }
    }

    fn git_stdout(directory: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .output()
            .expect("run git");
        assert!(output.status.success(), "git {arguments:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A registered clone under guarded rebase whose local commit conflicts
    /// with a newer upstream commit.
    fn paused_branch_fixture(
        temporary: &Path,
    ) -> (std::path::PathBuf, WikiRegistry, DaemonConflictWorkerConfig) {
        let remote = temporary.join("remote.git");
        let writer = temporary.join("writer");
        let reader = temporary.join("reader");
        fs::create_dir(&remote).expect("remote directory");
        git(&remote, &["init", "--bare", "--quiet"]);
        fs::create_dir(&writer).expect("writer directory");
        git(&writer, &["init", "--quiet", "--initial-branch", "main"]);
        configure_repository(&writer);
        fs::write(writer.join("Home.md"), "base\n").expect("base note");
        git(&writer, &["add", "Home.md"]);
        git(&writer, &["commit", "--quiet", "-m", "base"]);
        git(
            &writer,
            &["remote", "add", "origin", remote.to_str().expect("remote")],
        );
        git(&writer, &["push", "--quiet", "-u", "origin", "main"]);
        git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git(
            temporary,
            &[
                "clone",
                "--quiet",
                remote.to_str().expect("remote"),
                reader.to_str().expect("reader"),
            ],
        );
        configure_repository(&reader);
        fs::write(writer.join("Home.md"), "writer\n").expect("writer edit");
        git(&writer, &["commit", "--quiet", "-am", "writer edit"]);
        git(&writer, &["push", "--quiet"]);
        fs::write(reader.join("Home.md"), "reader\n").expect("reader edit");
        git(&reader, &["commit", "--quiet", "-am", "reader edit"]);
        git(&reader, &["fetch", "--quiet"]);
        fs::create_dir_all(reader.join(".vulcan")).expect("config directory");
        fs::write(
            reader.join(".vulcan/config.toml"),
            "[sync]\nbranch_pull = \"guarded-rebase\"\n",
        )
        .expect("vault config");
        fs::write(reader.join(".git/info/exclude"), ".vulcan/\n").expect("exclude");

        let registry = WikiRegistry::at(temporary.join("config/daemon.toml"));
        let wiki = WikiId::parse("notes").expect("wiki ID");
        registry
            .add(
                &AddWikiRequest {
                    source: None,
                    profile: None,
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
        let config = DaemonConflictWorkerConfig {
            wikis: vec![wiki],
            poll_seconds: 30,
        };
        (reader, registry, config)
    }

    #[test]
    fn the_worker_replays_a_paused_branch_and_queues_its_publication() {
        let temporary = tempdir().expect("temporary directory");
        let (reader, registry, config) = paused_branch_fixture(temporary.path());
        let supervisor =
            SyncSupervisor::at(temporary.path().join("state/jobs.json")).expect("supervisor");
        let agent = CompanionResolutionAgent::new(CountingProvider {
            content: "writer\nreader\n",
            calls: std::sync::Arc::default(),
        });

        let status = execute_conflict_worker_pass(
            &config,
            &registry,
            &supervisor,
            &agent,
            None,
            None,
            1_000,
        );

        let entry = status.entries.first().expect("status entry");
        assert!(entry.error.is_none(), "{entry:?}");
        assert_eq!(
            entry.replayed_branch.as_deref(),
            Some(git_stdout(&reader, &["rev-parse", "HEAD"]).as_str())
        );
        assert_eq!(
            git_stdout(&reader, &["rev-parse", "HEAD~1"]),
            git_stdout(&reader, &["rev-parse", "origin/main"])
        );
        assert_eq!(
            fs::read_to_string(reader.join("Home.md")).expect("note"),
            "writer\nreader\n"
        );
        let jobs = supervisor.list().expect("jobs");
        assert_eq!(jobs.len(), 1, "a sync publishes the replayed branch");
        assert_eq!(jobs[0].job.wiki_id.as_deref(), Some("notes"));
    }

    #[test]
    fn a_declined_replay_is_not_sent_to_the_agent_again_until_a_side_moves() {
        let temporary = tempdir().expect("temporary directory");
        let (reader, registry, config) = paused_branch_fixture(temporary.path());
        let supervisor =
            SyncSupervisor::at(temporary.path().join("state/jobs.json")).expect("supervisor");
        let calls = std::sync::Arc::<std::sync::atomic::AtomicUsize>::default();
        let provider = CountingProvider {
            content: "<<<<<<< ours\nreader\n=======\nwriter\n>>>>>>> theirs\n",
            calls: calls.clone(),
        };
        let agent = CompanionResolutionAgent::new(provider);
        let head = git_stdout(&reader, &["rev-parse", "HEAD"]);

        let first = execute_conflict_worker_pass(
            &config,
            &registry,
            &supervisor,
            &agent,
            None,
            None,
            1_000,
        );
        let second = execute_conflict_worker_pass(
            &config,
            &registry,
            &supervisor,
            &agent,
            None,
            Some(&first),
            2_000,
        );

        let entry = first.entries.first().expect("status entry");
        assert!(entry.declined_replay.is_some(), "{entry:?}");
        assert!(entry
            .skipped
            .as_deref()
            .is_some_and(|reason| reason.contains("conflict markers")));
        assert_eq!(second.entries[0].declined_replay, entry.declined_replay);
        assert_eq!(git_stdout(&reader, &["rev-parse", "HEAD"]), head);
        assert!(supervisor.list().expect("jobs").is_empty());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Backoff and profile gating share the same persisted-worker fixture.
    fn provider_failures_observe_persisted_backoff() {
        let temporary = tempdir().expect("temporary directory");
        let vault = temporary.path().join("vault");
        fs::create_dir(&vault).expect("vault directory");
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        let wiki = WikiId::parse("notes").expect("wiki ID");
        registry
            .add(
                &AddWikiRequest {
                    source: None,
                    profile: None,
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
            poll_seconds: 30,
        };
        let previous = ConflictWorkerStatus {
            version: CONFLICT_WORKER_STATUS_VERSION,
            checked_unix_ms: 1_000,
            entries: vec![ConflictWorkerStatusEntry {
                wiki_id: "notes".to_string(),
                skipped: None,
                error: Some("provider failed".to_string()),
                retry_after_unix_ms: Some(10_000),
                replayed_branch: None,
                declined_replay: None,
            }],
        };
        let supervisor =
            SyncSupervisor::at(temporary.path().join("state/jobs.json")).expect("supervisor");
        let status = execute_conflict_worker_pass(
            &config,
            &registry,
            &supervisor,
            &CompanionResolutionAgent::new(ResolvingProvider),
            None,
            Some(&previous),
            5_000,
        );
        assert_eq!(
            status.entries[0].skipped.as_deref(),
            Some("waiting for error backoff")
        );
        assert_eq!(status.entries[0].error.as_deref(), Some("provider failed"));
        assert_eq!(status.entries[0].retry_after_unix_ms, Some(10_000));

        let next_status = execute_conflict_worker_pass(
            &config,
            &registry,
            &supervisor,
            &CompanionResolutionAgent::new(ResolvingProvider),
            None,
            Some(&status),
            6_000,
        );
        assert_eq!(
            next_status.entries[0].skipped.as_deref(),
            Some("waiting for error backoff")
        );
        assert_eq!(next_status.entries[0].retry_after_unix_ms, Some(10_000));

        registry
            .update(
                &WikiId::parse("notes").unwrap(),
                &UpdateWikiRequest {
                    sync_backend: None,
                    profile: Some(ManagedDirectoryProfile::FilesOnly),
                    groups_to_add: Vec::new(),
                    groups_to_remove: Vec::new(),
                    permissions_profile: None,
                    sync_paused: None,
                    execution_trust: None,
                    mcp_remote: None,
                },
                false,
            )
            .expect("select files-only profile");
        let files_only = execute_conflict_worker_pass(
            &config,
            &registry,
            &supervisor,
            &CompanionResolutionAgent::new(ResolvingProvider),
            None,
            Some(&next_status),
            7_000,
        );
        assert!(files_only.entries[0]
            .skipped
            .as_deref()
            .unwrap()
            .contains("requires a knowledge profile"));
    }
}

#[cfg(test)]
mod scheduling_tests {
    use super::{load_conflict_worker_status, run_conflict_worker};
    use crate::companion::CompanionResolutionAgent;
    use crate::mutation_scheduler::{MutationScheduler, MutationSchedulerConfig};
    use crate::registry::{DaemonConflictWorkerConfig, WikiId, WikiRegistry};
    use crate::shutdown::ShutdownSignal;
    use crate::supervisor::SyncSupervisor;
    use crate::worker_gate::HostedWorkerGate;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// A sync-job change runs a pass without waiting for the reconciliation
    /// interval; between changes the worker stays idle.
    #[test]
    fn job_changes_wake_the_worker_before_its_reconciliation_interval() {
        struct Unused;
        impl vulcan_app::sync_proposals::ResolutionAgentProvider for Unused {
            fn identity(&self) -> vulcan_app::sync_proposals::ResolutionAgentIdentity {
                vulcan_app::sync_proposals::ResolutionAgentIdentity {
                    provider: "test".to_string(),
                    model: "unused".to_string(),
                    prompt_contract_version: 1,
                }
            }
            fn propose(
                &self,
                _request: &vulcan_app::sync_proposals::ResolutionAgentRequest,
                _tools: &mut dyn vulcan_app::sync_proposals::ResolutionAgentTools,
                _cancellation: &vulcan_app::sync::SyncCancellationToken,
            ) -> Result<vulcan_app::sync_proposals::ResolutionAgentOutput, vulcan_app::AppError>
            {
                panic!("no conflicts are eligible")
            }
        }
        let temporary = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let gate = HostedWorkerGate::new(
            Arc::new(MutationScheduler::new(MutationSchedulerConfig::default()).unwrap()),
            runtime.handle().clone(),
            "worker.conflict",
        );
        let registry = WikiRegistry::at(temporary.path().join("config/daemon.toml"));
        let supervisor =
            SyncSupervisor::at(temporary.path().join("state/jobs.json")).expect("supervisor");
        let config = DaemonConflictWorkerConfig {
            wikis: vec![WikiId::parse("notes").unwrap()],
            poll_seconds: 3_600,
        };
        let state_root = temporary.path().join("daemon-state");
        let agent = CompanionResolutionAgent::new(Unused);
        let stop = ShutdownSignal::new(false);
        let checked = || {
            load_conflict_worker_status(&state_root)
                .ok()
                .flatten()
                .map(|status| status.checked_unix_ms)
        };
        let wait_for = |previous: Option<u64>| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(checked) = checked().filter(|checked| Some(*checked) != previous) {
                    return Some(checked);
                }
                if Instant::now() > deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        std::thread::scope(|threads| {
            let worker = threads.spawn(|| {
                run_conflict_worker(
                    &config,
                    &registry,
                    &supervisor,
                    &state_root,
                    &agent,
                    &gate,
                    &stop,
                )
            });
            let first = wait_for(None).expect("initial pass");
            std::thread::sleep(Duration::from_millis(1_500));
            assert_eq!(checked(), Some(first), "idle between changes");
            supervisor.notify_change();
            assert!(wait_for(Some(first)).is_some(), "job change ran a pass");
            stop.cancel();
            worker.join().unwrap().unwrap();
        });
    }
}
