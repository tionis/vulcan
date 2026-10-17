//! Supervised batched auto-commit (Phase 11): one observation consumer per
//! vault that does not sync with Git, committing accumulated changes on the
//! vault's `[git] batch_interval_seconds`.
//!
//! The consumer only decides when; each batch is the finite
//! `vulcan_app::batch_commit` pass that direct mode could run too. A vault
//! registered with the Git sync backend has no consumer: hidden-ref sync owns
//! its automatic capture.

use crate::companion::CompanionSemanticAgent;
use crate::observation::{ObservationEvent, ObservationSubscription};
use crate::registry::{WikiRegistration, WikiRegistry};
use crate::shutdown::ShutdownSignal;
use crate::watch::DaemonWatchError;
use crate::worker_gate::HostedWorkerGate;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use vulcan_app::batch_commit::{
    batched_commits_enabled, effective_batch_interval_seconds, run_batched_commit,
    BatchedCommitOptions, BatchedCommitReport, BatchedCommitStatus,
};
use vulcan_app::commit::CaptureOwner;
use vulcan_app::execution::{MutationGate, Ungated};
use vulcan_app::sync::SyncCancellationToken;
use vulcan_core::{load_vault_config, GitBatchMessages, GitConfig, VaultPaths, Verbosity};

pub const AUTO_COMMIT_STATUS_VERSION: u32 = 1;
/// How often the consumer rereads `[git]` while it waits.
const CONFIG_REFRESH: Duration = Duration::from_secs(5);

/// Whether hidden-ref sync owns automatic capture for `registration`. A
/// registration without a backend predates `--no-sync` and syncs with Git.
#[must_use]
pub fn sync_owns_capture(registration: &WikiRegistration) -> bool {
    registration
        .sync_backend
        .as_deref()
        .is_none_or(|backend| backend == "git")
}

/// What the consumer of one vault needs beyond its registration.
#[derive(Clone, Default)]
pub struct AutoCommitContext {
    pub registry: Option<WikiRegistry>,
    pub gate: Option<Arc<HostedWorkerGate>>,
    pub agent: Option<Arc<CompanionSemanticAgent>>,
    pub agent_unavailable: Option<String>,
    /// Replaces the vault's batch interval, so tests need not wait for the
    /// shortest supported one.
    pub(crate) interval_override: Option<Duration>,
}

/// The latest batch outcome for one registered vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoCommitStatus {
    pub version: u32,
    pub wiki_id: String,
    pub checked_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<BatchedCommitReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[must_use]
pub fn auto_commit_status_path(state_root: &Path, registration_id: ulid::Ulid) -> PathBuf {
    state_root
        .join("daemon/auto-commit")
        .join(format!("{registration_id}.json"))
}

pub fn load_auto_commit_status(path: &Path) -> Result<Option<AutoCommitStatus>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let status: AutoCommitStatus =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if status.version != AUTO_COMMIT_STATUS_VERSION {
        return Err(format!(
            "unsupported auto-commit status version {}",
            status.version
        ));
    }
    Ok(Some(status))
}

pub const AUTO_COMMIT_STATUS_REPORT_VERSION: u32 = 1;

/// Batched auto-commit state of every registered vault on this device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutoCommitStatusReport {
    pub version: u32,
    pub vaults: Vec<AutoCommitVaultStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoCommitCaptureOwner {
    /// Batched auto-commit, when the vault asks for it.
    AutoCommit,
    /// Git sync's hidden-ref snapshots; no auto-commit consumer runs.
    GitSync,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutoCommitVaultStatus {
    pub wiki_id: String,
    pub path: PathBuf,
    pub capture_owner: AutoCommitCaptureOwner,
    /// The vault's `[git]` settings, as this device reads them.
    pub auto_commit: bool,
    pub strategy: vulcan_core::GitCommitStrategy,
    pub batch_interval_seconds: u64,
    pub batch_messages: GitBatchMessages,
    /// Whether this device lets the vault's batches use the semantic agent.
    pub agent_consent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<AutoCommitStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Reads each registration's `[git]` settings and latest batch outcome.
pub fn auto_commit_status_report(
    registry: &WikiRegistry,
    state_root: &Path,
) -> Result<AutoCommitStatusReport, String> {
    let config = registry.load().map_err(|error| error.to_string())?;
    let vaults = config
        .vaults
        .iter()
        .map(|registration| {
            let git = load_vault_config(&VaultPaths::new(&registration.path))
                .config
                .git;
            let (latest, error) = match load_auto_commit_status(&auto_commit_status_path(
                state_root,
                registration.registration_id,
            )) {
                Ok(latest) => (latest, None),
                Err(error) => (None, Some(error)),
            };
            AutoCommitVaultStatus {
                wiki_id: registration.id.to_string(),
                path: registration.path.clone(),
                capture_owner: if sync_owns_capture(registration) {
                    AutoCommitCaptureOwner::GitSync
                } else {
                    AutoCommitCaptureOwner::AutoCommit
                },
                auto_commit: git.auto_commit,
                strategy: git.strategy,
                batch_interval_seconds: effective_batch_interval_seconds(&git),
                batch_messages: git.batch_messages,
                agent_consent: config
                    .auto_commit_agent
                    .as_ref()
                    .is_some_and(|agent| agent.allows(&registration.id)),
                latest,
                error,
            }
        })
        .collect();
    Ok(AutoCommitStatusReport {
        version: AUTO_COMMIT_STATUS_REPORT_VERSION,
        vaults,
    })
}

/// When the next batch is due. A batch opens with the first change after
/// the previous one and is committed `interval` later, once changes have been
/// quiet for `quiet`; a vault that never goes quiet is committed one more
/// interval later regardless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatchClock {
    dirty_since: Option<Instant>,
    last_change: Option<Instant>,
}

impl BatchClock {
    /// Starts dirty, so changes made while the daemon was down are committed
    /// one interval after it starts.
    pub(crate) fn starting(now: Instant) -> Self {
        Self {
            dirty_since: Some(now),
            last_change: None,
        }
    }

    pub(crate) fn changed(&mut self, now: Instant) {
        self.dirty_since.get_or_insert(now);
        self.last_change = Some(now);
    }

    /// The instant the batch becomes due, or `None` when nothing changed.
    pub(crate) fn due(&self, interval: Duration, quiet: Duration) -> Option<Instant> {
        let opened = self.dirty_since?;
        let due = opened + interval;
        let quiet_at = self.last_change.map_or(due, |last| last + quiet);
        Some(due.max(quiet_at).min(due + interval))
    }

    pub(crate) fn committed(&mut self) {
        self.dirty_since = None;
        self.last_change = None;
    }

    /// Keeps the batch open and retries it one interval from `now`.
    pub(crate) fn retry(&mut self, now: Instant) {
        self.dirty_since = Some(now);
        self.last_change = None;
    }
}

pub(crate) fn consume_auto_commit_observations_with_stop(
    registration: &WikiRegistration,
    context: &AutoCommitContext,
    state_root: &Path,
    subscription: &ObservationSubscription,
    quiet: Duration,
    stop: &ShutdownSignal,
) -> Result<(), DaemonWatchError> {
    stop.register_current_thread();
    let paths = VaultPaths::new(&registration.path);
    let status_path = auto_commit_status_path(state_root, registration.registration_id);
    let mut clock = BatchClock::starting(Instant::now());
    let mut config = load_vault_config(&paths).config.git;
    let mut config_read = Instant::now();
    let _ = subscription.take_reconciliation_required();
    while !stop.is_cancelled() {
        let now = Instant::now();
        if subscription.take_reconciliation_required() {
            clock.changed(now);
        }
        if now.duration_since(config_read) >= CONFIG_REFRESH {
            config = load_vault_config(&paths).config.git;
            config_read = now;
        }
        let interval = context
            .interval_override
            .unwrap_or_else(|| Duration::from_secs(effective_batch_interval_seconds(&config)));
        let due = batched_commits_enabled(&config)
            .then(|| clock.due(interval, quiet))
            .flatten();
        if due.is_some_and(|due| now >= due) {
            let status = run_pass(registration, context, &paths, &config, stop);
            match status.report.as_ref().map(|report| report.status) {
                Some(
                    BatchedCommitStatus::Committed
                    | BatchedCommitStatus::Clean
                    | BatchedCommitStatus::Disabled
                    | BatchedCommitStatus::OwnedBySync,
                ) => clock.committed(),
                Some(BatchedCommitStatus::Blocked | BatchedCommitStatus::Planned) | None => {
                    clock.retry(Instant::now());
                }
            }
            if let Err(error) = vulcan_core::durable::replace_json(
                &status_path,
                &status,
                vulcan_core::durable::Durability::BestEffort,
            ) {
                eprintln!(
                    "cannot record the auto-commit status of `{}`: {error}",
                    registration.id
                );
            }
            continue;
        }
        // Wake when the batch comes due or the configuration is reread.
        let refresh = (config_read + CONFIG_REFRESH).saturating_duration_since(now);
        let wait = due.map_or(refresh, |due| {
            due.saturating_duration_since(now).min(refresh)
        });
        match subscription.recv_until(Some(wait), || stop.is_cancelled()) {
            Ok(ObservationEvent::FilesystemHint(event)) if event.barrier_only() => {}
            Ok(ObservationEvent::FilesystemHint(event)) => {
                if !event.paths.is_empty() || event.safety_rescan {
                    clock.changed(Instant::now());
                }
            }
            Ok(ObservationEvent::PostScan(_)) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(DaemonWatchError::ChannelClosed);
            }
        }
    }
    Ok(())
}

fn run_pass(
    registration: &WikiRegistration,
    context: &AutoCommitContext,
    paths: &VaultPaths,
    config: &GitConfig,
    stop: &ShutdownSignal,
) -> AutoCommitStatus {
    let consent = context
        .registry
        .as_ref()
        .and_then(|registry| registry.poll().ok())
        .and_then(|daemon| daemon.auto_commit_agent)
        .is_some_and(|agent| agent.allows(&registration.id));
    let (agent, agent_unavailable) = if config.batch_messages != GitBatchMessages::Agent {
        (None, None)
    } else if !consent {
        (
            None,
            Some(format!(
                "agent messages are not enabled for `{}` on this device; run `vulcan daemon config set-auto-commit-agent --wiki {}`",
                registration.id, registration.id
            )),
        )
    } else if let Some(agent) = &context.agent {
        (Some(agent.provider()), None)
    } else {
        (
            None,
            Some(context.agent_unavailable.clone().unwrap_or_else(|| {
                "no semantic agent is configured on this device; run `vulcan daemon config set-agent semantic`"
                    .to_string()
            })),
        )
    };
    let vault_gate = context
        .gate
        .as_ref()
        .map(|gate| gate.for_registration(registration));
    let gate: &dyn MutationGate = match vault_gate.as_ref() {
        Some(gate) => gate,
        None => &Ungated,
    };
    // Shutdown cancels the batch, including a model call in progress; a
    // commit already under way finishes first.
    let cancellation = SyncCancellationToken::default();
    let finished = AtomicBool::new(false);
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !finished.load(Ordering::Acquire) {
                if stop.wait_timeout(Duration::from_millis(200)) {
                    cancellation.cancel();
                    return;
                }
            }
        });
        let result = run_batched_commit(
            paths,
            &BatchedCommitOptions {
                agent,
                agent_unavailable,
                capture_owner: Some(if sync_owns_capture(registration) {
                    CaptureOwner::HiddenRefSync {
                        wiki_id: registration.id.to_string(),
                    }
                } else {
                    CaptureOwner::AutoCommit
                }),
                permission_profile: Some(
                    registration
                        .permissions_profile
                        .as_deref()
                        .unwrap_or("unrestricted"),
                ),
                git_config: None,
                resolved_profile: None,
                plugins: true,
                gate,
                cancellation: &cancellation,
                verbosity: Verbosity::Quiet,
                dry_run: false,
            },
        );
        finished.store(true, Ordering::Release);
        result
    });
    let (report, error) = match result {
        Ok(report) => (Some(report), None),
        Err(error) => (None, Some(error.to_string())),
    };
    AutoCommitStatus {
        version: AUTO_COMMIT_STATUS_VERSION,
        wiki_id: registration.id.to_string(),
        checked_unix_ms: unix_time_ms(),
        report,
        error,
    }
}

/// Runs the vault's configured batch once, without the daemon: the entry
/// point of `vulcan git batch-commit` for cron and timers. Capture ownership
/// and agent consent come from this device's registry, exactly as for the
/// daemon's consumer; an unregistered vault has no consent and no owner but
/// itself. The repository lock serializes it with a running daemon's batches.
pub fn run_direct_batch(
    registry: &WikiRegistry,
    paths: &VaultPaths,
    permission_profile: Option<&str>,
    dry_run: bool,
    verbosity: Verbosity,
) -> Result<BatchedCommitReport, String> {
    run_batch_with(
        registry,
        paths,
        permission_profile,
        dry_run,
        verbosity,
        &Ungated,
        &SyncCancellationToken::default(),
        None,
    )
}

/// What a trusted-commit `batch-commit` runs with instead of the working
/// tree: the signed `[git]` table and the profile resolved from it.
pub(crate) struct TrustedBatch<'a> {
    pub git: &'a vulcan_core::GitConfig,
    pub profile: &'a vulcan_core::ResolvedPermissionProfile,
}

/// [`run_direct_batch`] behind `gate` and stopped by `cancellation`, as a
/// scheduled `batch-commit` runs it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_batch_with(
    registry: &WikiRegistry,
    paths: &VaultPaths,
    permission_profile: Option<&str>,
    dry_run: bool,
    verbosity: Verbosity,
    gate: &dyn MutationGate,
    cancellation: &SyncCancellationToken,
    trusted: Option<&TrustedBatch<'_>>,
) -> Result<BatchedCommitReport, String> {
    let config = trusted.map_or_else(
        || load_vault_config(paths).config.git,
        |trusted| trusted.git.clone(),
    );
    let daemon = match registry.load() {
        Ok(daemon) => Some(daemon),
        Err(crate::registry::RegistryError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            None
        }
        Err(error) => return Err(error.to_string()),
    };
    let root = fs::canonicalize(paths.vault_root()).map_err(|error| error.to_string())?;
    let registration = daemon
        .as_ref()
        .and_then(|daemon| daemon.vaults.iter().find(|wiki| wiki.path == root));
    let consent = registration.is_some_and(|registration| {
        daemon
            .as_ref()
            .and_then(|daemon| daemon.auto_commit_agent.as_ref())
            .is_some_and(|agent| agent.allows(&registration.id))
    });
    let mut agent_unavailable = None;
    let agent = if config.batch_messages != GitBatchMessages::Agent {
        None
    } else if !consent {
        agent_unavailable = Some(registration.map_or_else(
            || "agent messages need a registered vault with device consent".to_string(),
            |registration| {
                format!(
                    "agent messages are not enabled for `{}` on this device; run `vulcan daemon config set-auto-commit-agent --wiki {}`",
                    registration.id, registration.id
                )
            },
        ));
        None
    } else {
        match direct_semantic_agent(
            daemon
                .as_ref()
                .and_then(|daemon| daemon.semantic_agent.as_ref()),
        ) {
            Ok(agent) => Some(agent),
            Err(reason) => {
                agent_unavailable = Some(reason);
                None
            }
        }
    };
    let profile = permission_profile.map(str::to_string).or_else(|| {
        registration.map(|registration| {
            registration
                .permissions_profile
                .clone()
                .unwrap_or_else(|| "unrestricted".to_string())
        })
    });
    run_batched_commit(
        paths,
        &BatchedCommitOptions {
            agent: agent.as_ref().map(CompanionSemanticAgent::provider),
            agent_unavailable,
            capture_owner: registration.map(|registration| {
                if sync_owns_capture(registration) {
                    CaptureOwner::HiddenRefSync {
                        wiki_id: registration.id.to_string(),
                    }
                } else {
                    CaptureOwner::AutoCommit
                }
            }),
            permission_profile: profile.as_deref(),
            git_config: trusted.map(|trusted| trusted.git),
            resolved_profile: trusted.map(|trusted| trusted.profile),
            plugins: trusted.is_none(),
            gate,
            cancellation,
            verbosity,
            dry_run,
        },
    )
    .map_err(|error| error.to_string())
}

/// The device's semantic agent, built from its registry entry as the daemon
/// builds it, or why there is none.
fn direct_semantic_agent(
    config: Option<&crate::registry::DaemonAgentConfig>,
) -> Result<CompanionSemanticAgent, String> {
    let Some(config) = config else {
        return Err(
            "no semantic agent is configured on this device; run `vulcan daemon config set-agent semantic`"
                .to_string(),
        );
    };
    #[cfg(feature = "web")]
    {
        let api_key = match config.api_key_env.as_deref() {
            None => None,
            Some(name) => Some(std::env::var(name).map_err(|_| format!("{name} is not set"))?),
        };
        CompanionSemanticAgent::openai_compatible(
            config.base_url.clone(),
            config.model.clone(),
            api_key,
        )
        .map_err(|error| error.to_string())
    }
    #[cfg(not(feature = "web"))]
    {
        let _ = config;
        Err("the semantic agent requires Vulcan's `web` feature".to_string())
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{
        FilesystemHint, ObservationConsumerId, ObservationConsumerKind, ObservationConsumerPolicy,
        ObservationFilter, VaultObservationHub,
    };
    use std::collections::BTreeSet;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("git should launch");
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout).expect("utf-8 output")
    }

    fn registration(path: &Path, backend: &str) -> WikiRegistration {
        WikiRegistration {
            profile: crate::registry::ManagedDirectoryProfile::Knowledge,
            profile_version: None,
            materialization: crate::registry::MaterializationProfile::Full,
            id: crate::registry::WikiId::parse("notes").unwrap(),
            registration_id: ulid::Ulid::new(),
            path: path.to_path_buf(),
            work_tree: None,
            groups: vec![],
            git_dir: None,
            permissions_profile: None,
            sync_backend: Some(backend.to_string()),
            platform_profile: None,
            sync_paused: false,
            execution_trust: None,
        }
    }

    #[test]
    fn only_vaults_without_git_sync_get_batched_auto_commits() {
        let path = Path::new("/notes");
        assert!(sync_owns_capture(&registration(path, "git")));
        assert!(!sync_owns_capture(&registration(path, "none")));
        let mut legacy = registration(path, "git");
        legacy.sync_backend = None;
        assert!(sync_owns_capture(&legacy));
    }

    #[test]
    fn agent_messages_need_this_devices_consent() {
        let temporary = tempfile::tempdir().unwrap();
        let vault = temporary.path().join("vault");
        fs::create_dir_all(vault.join(".vulcan")).unwrap();
        git(&vault, &["-c", "init.defaultBranch=main", "init", "-q"]);
        git(&vault, &["config", "user.name", "Vulcan Test"]);
        git(&vault, &["config", "user.email", "vulcan@example.com"]);
        fs::write(vault.join(".gitignore"), ".vulcan/\n").unwrap();
        fs::write(vault.join("Home.md"), "home\n").unwrap();
        fs::write(
            vault.join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\nstrategy = \"batched\"\nbatch_messages = \"agent\"\n",
        )
        .unwrap();
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        let context = AutoCommitContext {
            registry: Some(registry),
            ..AutoCommitContext::default()
        };
        let paths = VaultPaths::new(&vault);
        let config = load_vault_config(&paths).config.git;
        let status = run_pass(
            &registration(&vault, "none"),
            &context,
            &paths,
            &config,
            &ShutdownSignal::default(),
        );

        let report = status.report.expect("batch report");
        assert_eq!(report.status, BatchedCommitStatus::Committed);
        assert_eq!(report.used_messages, Some(GitBatchMessages::Deterministic));
        assert!(report
            .fallback
            .as_deref()
            .is_some_and(|reason| reason.contains("set-auto-commit-agent --wiki notes")));
    }

    /// Answers only once cancelled, like a provider that never responds.
    struct SilentAgent;

    impl vulcan_app::sync_semantic::SemanticAgentProvider for SilentAgent {
        fn identity(&self) -> vulcan_app::sync_semantic::SemanticAgentIdentity {
            vulcan_app::sync_semantic::SemanticAgentIdentity {
                provider: "silent".to_string(),
                model: "never".to_string(),
                prompt_contract_version: 1,
            }
        }

        fn propose(
            &self,
            _request: &vulcan_app::sync_semantic::SemanticAgentRequest,
            cancellation: &SyncCancellationToken,
        ) -> Result<vulcan_app::sync_semantic::SemanticAgentOutput, vulcan_app::AppError> {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !cancellation.is_cancelled() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(vulcan_app::AppError::operation(
                "semantic agent planning was cancelled",
            ))
        }
    }

    #[test]
    fn shutdown_cancels_a_batch_waiting_on_the_agent() {
        let temporary = tempfile::tempdir().unwrap();
        let vault = temporary.path().join("vault");
        fs::create_dir_all(vault.join(".vulcan")).unwrap();
        git(&vault, &["-c", "init.defaultBranch=main", "init", "-q"]);
        git(&vault, &["config", "user.name", "Vulcan Test"]);
        git(&vault, &["config", "user.email", "vulcan@example.com"]);
        fs::write(vault.join(".gitignore"), ".vulcan/\n").unwrap();
        fs::write(vault.join("Home.md"), "home\n").unwrap();
        fs::write(
            vault.join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\nstrategy = \"batched\"\nbatch_messages = \"agent\"\n",
        )
        .unwrap();
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        registry
            .set_auto_commit_agent(
                crate::registry::DaemonAutoCommitAgentConfig {
                    wikis: vec![crate::registry::WikiId::parse("notes").unwrap()],
                },
                false,
            )
            .unwrap();
        let context = AutoCommitContext {
            registry: Some(registry),
            agent: Some(Arc::new(CompanionSemanticAgent::new(SilentAgent))),
            ..AutoCommitContext::default()
        };
        let stop = Arc::new(ShutdownSignal::default());
        let canceller = Arc::clone(&stop);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            canceller.cancel();
        });
        let paths = VaultPaths::new(&vault);
        let config = load_vault_config(&paths).config.git;
        let started = Instant::now();
        let status = run_pass(
            &registration(&vault, "none"),
            &context,
            &paths,
            &config,
            &stop,
        );

        assert!(started.elapsed() < Duration::from_secs(20));
        assert!(status
            .error
            .as_deref()
            .is_some_and(|error| error.contains("cancelled")));
        assert!(git(&vault, &["status", "--porcelain"]).contains("Home.md"));
    }

    #[test]
    fn the_consumer_commits_observed_changes_once_the_batch_is_due() {
        let temporary = tempfile::tempdir().unwrap();
        let vault = temporary.path().join("vault");
        fs::create_dir_all(vault.join(".vulcan")).unwrap();
        git(&vault, &["-c", "init.defaultBranch=main", "init", "-q"]);
        git(&vault, &["config", "user.name", "Vulcan Test"]);
        git(&vault, &["config", "user.email", "vulcan@example.com"]);
        fs::write(vault.join(".gitignore"), ".vulcan/\n").unwrap();
        fs::write(vault.join("Home.md"), "home\n").unwrap();
        git(&vault, &["add", "."]);
        git(&vault, &["commit", "-q", "-m", "Initial"]);
        fs::write(
            vault.join(".vulcan/config.toml"),
            "[git]\nauto_commit = true\nstrategy = \"batched\"\nmessage = \"vault {action}: {files}\"\n",
        )
        .unwrap();
        let registration = registration(&vault, "none");
        let hub = VaultObservationHub::default();
        let subscription = hub
            .subscribe(
                ObservationConsumerId::parse("auto-commit-notes").unwrap(),
                ObservationConsumerPolicy {
                    kind: ObservationConsumerKind::AutoCommit,
                    queue_capacity: 8,
                    quiet_period_ms: 10,
                    maximum_dirty_ms: 10,
                    filter: ObservationFilter::default(),
                },
            )
            .unwrap();
        let state_root = temporary.path().join("state");
        let context = AutoCommitContext {
            interval_override: Some(Duration::from_millis(200)),
            ..AutoCommitContext::default()
        };
        let stop = Arc::new(ShutdownSignal::default());
        let thread_stop = Arc::clone(&stop);
        let thread_registration = registration.clone();
        let thread_root = state_root.clone();
        let consumer = std::thread::spawn(move || {
            consume_auto_commit_observations_with_stop(
                &thread_registration,
                &context,
                &thread_root,
                &subscription,
                Duration::from_millis(10),
                &thread_stop,
            )
        });

        fs::write(vault.join("Home.md"), "home edited\n").unwrap();
        hub.publish(&ObservationEvent::FilesystemHint(FilesystemHint {
            sequence: 1,
            event_count: 1,
            untagged_events: 1,
            paths: BTreeSet::from(["Home.md".to_string()]),
            self_generated_transactions: BTreeSet::new(),
            safety_rescan: false,
            watcher_errors: vec![],
            barriers: BTreeSet::new(),
        }))
        .unwrap();
        let status_path = auto_commit_status_path(&state_root, registration.registration_id);
        let deadline = Instant::now() + Duration::from_secs(20);
        while git(&vault, &["rev-list", "--count", "HEAD"]).trim() != "2" {
            assert!(Instant::now() < deadline, "the batch was never committed");
            std::thread::sleep(Duration::from_millis(20));
        }
        while load_auto_commit_status(&status_path).unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "the batch status was never recorded"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        stop.cancel();
        consumer.join().unwrap().unwrap();

        assert_eq!(
            git(&vault, &["log", "--format=%s", "-1"]),
            "vault batch: Home.md\n"
        );
        let status = load_auto_commit_status(&status_path).unwrap().unwrap();
        let report = status.report.expect("batch report");
        assert_eq!(report.status, BatchedCommitStatus::Committed);
        assert_eq!(report.commits[0].files, ["Home.md".to_string()]);
    }

    #[test]
    fn a_batch_is_due_one_interval_after_it_opens_once_changes_are_quiet() {
        let start = Instant::now();
        let interval = Duration::from_secs(300);
        let quiet = Duration::from_secs(2);
        let mut clock = BatchClock::starting(start);
        assert_eq!(clock.due(interval, quiet), Some(start + interval));

        clock.committed();
        assert_eq!(clock.due(interval, quiet), None);

        let opened = start + Duration::from_secs(10);
        clock.changed(opened);
        clock.changed(opened + Duration::from_secs(100));
        assert_eq!(clock.due(interval, quiet), Some(opened + interval));

        // An edit just before the deadline postpones it until edits pause.
        let late = opened + Duration::from_secs(299);
        clock.changed(late);
        assert_eq!(clock.due(interval, quiet), Some(late + quiet));
    }

    #[test]
    fn a_vault_that_never_goes_quiet_is_committed_one_interval_late_at_most() {
        let start = Instant::now();
        let interval = Duration::from_secs(60);
        let mut clock = BatchClock::starting(start);
        clock.changed(start + Duration::from_secs(119));
        assert_eq!(
            clock.due(interval, Duration::from_secs(30)),
            Some(start + 2 * interval)
        );
    }

    #[test]
    fn a_blocked_batch_is_retried_one_interval_later() {
        let start = Instant::now();
        let interval = Duration::from_secs(60);
        let mut clock = BatchClock::starting(start);
        let blocked_at = start + interval;
        clock.retry(blocked_at);
        assert_eq!(
            clock.due(interval, Duration::from_secs(1)),
            Some(blocked_at + interval)
        );
    }
}
