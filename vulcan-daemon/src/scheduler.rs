//! The daemon's one schedule service (Roadmap 10.8): runs each registered
//! vault's `[[schedules.jobs]]` that name this device, on their triggers.
//!
//! The service decides when; each run is a finite workflow that direct mode
//! can also start (`vulcan schedule run`). [`Scheduler::tick`] is a pure step
//! over an injected instant, so coalescing, overlap, retries, reassignment,
//! and restarts are tested without real time. See
//! `docs/specs/scheduled-execution.md`.

use crate::registry::{WikiRegistration, WikiRegistry};
use crate::shutdown::ShutdownSignal;
use crate::worker_gate::HostedWorkerGate;
use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use vulcan_app::execution::{MutationGate, Ungated};
use vulcan_app::schedule_trust::{
    record_advance, resolve_trust, ExecutionTrustConfig, TrustRepository, TrustResolution,
};
use vulcan_app::schedules::{
    authorize_execution, authorize_trusted_execution, try_lock_job_run, ExecutionAuthorization,
    OccurrenceKey, OccurrenceState, ScheduleLedger, TrustedSnapshot,
};
use vulcan_app::sync::SyncCancellationToken;
use vulcan_core::schedules::{load_schedules, ScheduleJob, ScheduleTarget};
use vulcan_core::{ResolvedPermissionProfile, VaultPaths, Verbosity};

/// How often the service rereads registrations and vault schedules while it
/// waits, which bounds how late a configuration change takes effect.
const RELOAD_INTERVAL: Duration = Duration::from_secs(30);
/// How long a failed run waits before its retry.
const RETRY_DELAY_MS: i64 = 30_000;
/// How often finished history is pruned.
const PRUNE_INTERVAL_MS: i64 = 60 * 60 * 1_000;

/// What a run needs beyond its job: where it runs and under what limits.
pub struct RunContext<'a> {
    pub registry: &'a WikiRegistry,
    pub paths: &'a VaultPaths,
    /// The device's permission ceiling for this vault.
    pub ceiling: Option<&'a str>,
    pub gate: &'a dyn MutationGate,
    pub cancellation: &'a SyncCancellationToken,
    /// Set when the vault uses trusted-commit.
    pub trust: Option<TrustTarget<'a>>,
}

/// What trusted-commit authorization needs for one vault.
pub struct TrustTarget<'a> {
    pub config: &'a ExecutionTrustConfig,
    pub repository: TrustRepository,
    /// Where the schedule ledger, which holds the checkpoint, lives.
    pub state_root: &'a Path,
    /// The ledger key of the vault.
    pub vault_key: &'a str,
}

impl<'a> TrustTarget<'a> {
    /// The registration's trust target, if it uses trusted-commit.
    #[must_use]
    pub fn for_registration(
        registration: &'a WikiRegistration,
        state_root: &'a Path,
        vault_key: &'a str,
    ) -> Option<Self> {
        registration
            .execution_trust
            .as_ref()
            .map(|config| TrustTarget {
                config,
                repository: trust_repository(registration),
                state_root,
                vault_key,
            })
    }

    /// Resolves the vault's trust state now, without advancing anything.
    pub fn resolve(&self) -> Result<TrustResolution, String> {
        let ledger = ScheduleLedger::open(self.state_root).map_err(|error| error.to_string())?;
        Ok(resolve_trust(
            self.config,
            &self.repository,
            &ledger,
            self.vault_key,
        ))
    }
}

/// Where a registration's repository is.
#[must_use]
pub fn trust_repository(registration: &WikiRegistration) -> TrustRepository {
    TrustRepository {
        vault: registration.path.clone(),
        git_dir: registration.git_dir.clone(),
        work_tree: registration.work_tree.clone(),
    }
}

/// Authorizes one run of `job` under the vault's policy. A trusted-commit
/// vault is resolved again here, at dispatch, so a definition that lost its
/// authorization since the last reload does not run.
pub fn authorize_run(
    job: &ScheduleJob,
    context: &RunContext<'_>,
) -> Result<
    (
        ExecutionAuthorization,
        ResolvedPermissionProfile,
        Option<TrustedSnapshot>,
    ),
    String,
> {
    match &context.trust {
        None => authorize_execution(context.paths, job, context.ceiling)
            .map(|(authorization, profile)| (authorization, profile, None))
            .map_err(|error| error.to_string()),
        Some(trust) => {
            let resolution = trust.resolve()?;
            authorize_trusted_execution(context.paths, job, context.ceiling, &resolution)
                .map(|(authorization, profile, snapshot)| (authorization, profile, Some(snapshot)))
                .map_err(|error| error.to_string())
        }
    }
}

/// Authorizes and runs `job` once, as a scheduled occurrence or a manual
/// `vulcan schedule run`. Returns a one-line summary of what it did.
///
/// Under trusted-commit, targets use the signed configuration and never run
/// working-tree code: `scan` runs the scan alone, without scan-triggered
/// auto-commit or plugin events, and `batch-commit` uses the signed `[git]`
/// table without commit plugins.
pub fn run_job(job: &ScheduleJob, context: &RunContext<'_>) -> Result<String, String> {
    let (authorization, profile, snapshot) = authorize_run(job, context)?;
    if context.cancellation.is_cancelled() {
        return Err("cancelled before it started".to_string());
    }
    match &job.target {
        ScheduleTarget::Scan { full } if snapshot.is_some() => {
            let guard = vulcan_core::ProfilePermissionGuard::new(context.paths, profile);
            vulcan_core::PermissionGuard::check_index(&guard).map_err(|error| error.to_string())?;
            let _permit = context.gate.enter().map_err(|error| error.to_string())?;
            if context.cancellation.is_cancelled() {
                return Err("cancelled while waiting for the vault".to_string());
            }
            let summary = vulcan_core::scan_vault_with_progress(
                context.paths,
                if *full {
                    vulcan_core::ScanMode::Full
                } else {
                    vulcan_core::ScanMode::Incremental
                },
                |_| {},
            )
            .map_err(|error| error.to_string())?;
            Ok(format!(
                "scanned: {} added, {} updated, {} deleted",
                summary.added, summary.updated, summary.deleted
            ))
        }
        ScheduleTarget::Scan { full } => {
            let guard = vulcan_core::ProfilePermissionGuard::new(context.paths, profile);
            vulcan_core::PermissionGuard::check_index(&guard).map_err(|error| error.to_string())?;
            let _permit = context.gate.enter().map_err(|error| error.to_string())?;
            if context.cancellation.is_cancelled() {
                return Err("cancelled while waiting for the vault".to_string());
            }
            let summary = vulcan_app::scan::scan_vault_with_automation(
                context.paths,
                if *full {
                    vulcan_core::ScanMode::Full
                } else {
                    vulcan_core::ScanMode::Incremental
                },
                &vulcan_app::commit::AutoCommitPolicy::for_scan(context.paths, false),
                Some(&authorization.profile),
                Verbosity::Quiet,
                |_| {},
            )
            .map_err(|error| error.to_string())?;
            Ok(format!(
                "scanned: {} added, {} updated, {} deleted",
                summary.added, summary.updated, summary.deleted
            ))
        }
        ScheduleTarget::BatchCommit => {
            let report = crate::auto_commit_worker::run_batch_with(
                context.registry,
                context.paths,
                Some(&authorization.profile),
                false,
                Verbosity::Quiet,
                context.gate,
                context.cancellation,
                snapshot
                    .as_ref()
                    .map(|snapshot| crate::auto_commit_worker::TrustedBatch {
                        git: &snapshot.git,
                        profile: &profile,
                    })
                    .as_ref(),
            )?;
            Ok(format!(
                "batch {:?}: {} commit(s){}",
                report.status,
                report.commits.len(),
                report
                    .detail
                    .map(|detail| format!(" ({detail})"))
                    .unwrap_or_default()
            ))
        }
    }
}

/// The ledger key of a registered vault.
#[must_use]
pub fn vault_key(registration: &WikiRegistration) -> String {
    registration.registration_id.to_string()
}

/// A run in progress.
/// A run's cancellation, seen both by its target and by its wait for the
/// vault's mutation permit.
#[derive(Clone, Default)]
struct RunCancellation {
    target: SyncCancellationToken,
    permit: vulcan_app::execution::ExecutionCancellationToken,
}

impl RunCancellation {
    fn cancel(&self) {
        self.target.cancel();
        self.permit.cancel();
    }

    fn is_cancelled(&self) -> bool {
        self.target.is_cancelled()
    }
}

struct Running {
    revision: String,
    /// Which attempt this is, from 1, and how many retries the job allows.
    attempt: i64,
    retries: u8,
    cancellation: RunCancellation,
    handle: JoinHandle<()>,
}

/// Why a job does not run here now, as status reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPlacement {
    Runs,
    Disabled,
    NotThisDevice,
    Blocked,
}

/// Whether `job` runs on the device `device_id` (none without an identity).
#[must_use]
pub fn job_placement(job: &ScheduleJob, device_id: Option<&str>) -> JobPlacement {
    if !job.enabled {
        JobPlacement::Disabled
    } else if device_id.is_some_and(|device| job.runs_on(device)) {
        JobPlacement::Runs
    } else {
        JobPlacement::NotThisDevice
    }
}

/// A run's slot, occurrence, and outcome, sent when its thread finishes.
type FinishedRun = ((String, String), OccurrenceKey, Result<String, String>);

/// The scheduler's state between ticks.
pub struct Scheduler {
    ledger: ScheduleLedger,
    state_root: PathBuf,
    device_id: Option<String>,
    running: BTreeMap<(String, String), Running>,
    /// Failed or interrupted occurrences waiting for a retry, by job: the
    /// occurrence, the attempt it will be, and when it may start.
    retry_at: BTreeMap<(String, String), (OccurrenceKey, i64, i64)>,
    finished: mpsc::Receiver<FinishedRun>,
    finished_sender: mpsc::Sender<FinishedRun>,
    last_prune_ms: i64,
    recovered: bool,
}

/// One vault as the scheduler sees it on a tick.
pub struct ScheduledVault {
    pub registration: WikiRegistration,
    pub jobs: Vec<ScheduleJob>,
}

/// Reads every registration's schedules. A trusted-commit vault contributes
/// only its authorized jobs, as defined at the resolved commit, and a single
/// signed frontier past its checkpoint becomes the new checkpoint.
#[must_use]
pub fn scheduled_vaults(
    registrations: &[WikiRegistration],
    ledger: &ScheduleLedger,
    now_ms: i64,
) -> Vec<ScheduledVault> {
    registrations
        .iter()
        .map(|registration| {
            let jobs = match &registration.execution_trust {
                None => load_schedules(&VaultPaths::new(&registration.path)).jobs,
                Some(config) => {
                    let key = vault_key(registration);
                    let resolution =
                        resolve_trust(config, &trust_repository(registration), ledger, &key);
                    if let Err(error) = record_advance(&resolution, ledger, &key, now_ms) {
                        eprintln!(
                            "cannot record the trust checkpoint of `{}`: {error}",
                            registration.id
                        );
                    }
                    resolution
                        .schedules
                        .jobs
                        .iter()
                        .filter(|job| resolution.authorize(&job.id).is_ok())
                        .cloned()
                        .collect()
                }
            };
            ScheduledVault {
                jobs,
                registration: registration.clone(),
            }
        })
        .collect()
}

impl Scheduler {
    pub fn open(state_root: &Path, device_id: Option<String>) -> Result<Self, String> {
        let (finished_sender, finished) = mpsc::channel();
        Ok(Self {
            ledger: ScheduleLedger::open(state_root).map_err(|error| error.to_string())?,
            state_root: state_root.to_path_buf(),
            device_id,
            running: BTreeMap::new(),
            retry_at: BTreeMap::new(),
            finished,
            finished_sender,
            last_prune_ms: 0,
            recovered: false,
        })
    }

    /// One step at `now`: record finished runs, recover interrupted ones on
    /// the first tick, cancel runs whose job changed, and dispatch every due
    /// occurrence of a job this device runs. Returns when the next occurrence
    /// is due, if any.
    #[allow(clippy::too_many_lines)]
    pub fn tick(
        &mut self,
        now: DateTime<Utc>,
        vaults: &[ScheduledVault],
        registry: &WikiRegistry,
        gate: Option<&Arc<HostedWorkerGate>>,
    ) -> Option<DateTime<Utc>> {
        let now_ms = now.timestamp_millis();
        self.collect_finished(now_ms);
        if !self.recovered {
            self.recover(vaults, now_ms);
            self.recovered = true;
        }
        if now_ms - self.last_prune_ms >= PRUNE_INTERVAL_MS {
            if let Err(error) = self.ledger.prune(now_ms) {
                eprintln!("cannot prune schedule history: {error}");
            }
            self.last_prune_ms = now_ms;
        }
        // A run whose job was removed, disabled, reassigned, or changed is
        // asked to stop; it keeps its own definition until it does.
        let wanted = vaults
            .iter()
            .flat_map(|vault| {
                let key = vault_key(&vault.registration);
                vault
                    .jobs
                    .iter()
                    .filter(|job| self.placement(job) == JobPlacement::Runs)
                    .map(move |job| ((key.clone(), job.id.clone()), job.revision.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        for (key, running) in &self.running {
            if wanted.get(key) != Some(&running.revision) {
                running.cancellation.cancel();
            }
        }
        let mut next: Option<DateTime<Utc>> = None;
        for vault in vaults {
            let vault_id = vault_key(&vault.registration);
            for job in &vault.jobs {
                if self.placement(job) != JobPlacement::Runs {
                    continue;
                }
                let slot = (vault_id.clone(), job.id.clone());
                let watermark = match self.ledger.watermark(&vault_id, job, now_ms) {
                    Ok(watermark) => watermark,
                    Err(error) => {
                        eprintln!("schedule `{}`: {error}", job.id);
                        continue;
                    }
                };
                let watermark = Utc.timestamp_millis_opt(watermark).single().unwrap_or(now);
                if let Some(upcoming) = job.trigger.next_after(watermark.max(now)) {
                    next = Some(next.map_or(upcoming, |next| next.min(upcoming)));
                }
                if self.running.contains_key(&slot) {
                    // At most one follow-up: the latest due occurrence runs
                    // once this one finishes.
                    continue;
                }
                if let Some((key, attempt, at_ms)) = self.retry_at.get(&slot).cloned() {
                    if key.revision != job.revision {
                        // The definition changed; its retry belongs to the old one.
                        self.retry_at.remove(&slot);
                    } else if now_ms >= at_ms {
                        self.retry_at.remove(&slot);
                        if let Err(error) = self.ledger.retry(&key, now_ms) {
                            eprintln!("schedule `{}`: {error}", job.id);
                            continue;
                        }
                        self.start(slot, key, attempt, job, &vault.registration, registry, gate);
                        continue;
                    } else {
                        continue;
                    }
                }
                let Some(occurrence) = job.due(watermark, now) else {
                    continue;
                };
                let key = OccurrenceKey {
                    vault: vault_id.clone(),
                    job_id: job.id.clone(),
                    revision: job.revision.clone(),
                    scheduled_ms: occurrence.timestamp_millis(),
                    manual: false,
                };
                match self.ledger.claim(&key, now_ms) {
                    Ok(true) => self.start(slot, key, 1, job, &vault.registration, registry, gate),
                    Ok(false) => {}
                    Err(error) => eprintln!("schedule `{}`: {error}", job.id),
                }
            }
        }
        next
    }

    /// The ledger, which also holds trust checkpoints.
    #[must_use]
    pub fn ledger(&self) -> &ScheduleLedger {
        &self.ledger
    }

    /// Whether `job` runs on this device.
    #[must_use]
    pub fn placement(&self, job: &ScheduleJob) -> JobPlacement {
        job_placement(job, self.device_id.as_deref())
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        slot: (String, String),
        key: OccurrenceKey,
        attempt: i64,
        job: &ScheduleJob,
        registration: &WikiRegistration,
        registry: &WikiRegistry,
        gate: Option<&Arc<HostedWorkerGate>>,
    ) {
        let cancellation = RunCancellation::default();
        let thread_cancellation = cancellation.clone();
        let retries = job.limits.retries;
        let job = job.clone();
        let registration = registration.clone();
        let registry = registry.clone();
        let gate = gate.cloned();
        let sender = self.finished_sender.clone();
        let state_root = self.state_root.clone();
        let revision = key.revision.clone();
        let thread_key = key;
        let thread_slot = slot.clone();
        let handle = thread::spawn(move || {
            let paths = VaultPaths::new(&registration.path);
            let finished = Arc::new(AtomicBool::new(false));
            // Cooperative runtime limit: past it, the run is asked to stop.
            let limit = Duration::from_secs(job.limits.max_runtime_seconds);
            let watchdog = {
                let finished = Arc::clone(&finished);
                let cancellation = thread_cancellation.clone();
                thread::spawn(move || {
                    let deadline = std::time::Instant::now() + limit;
                    while !finished.load(Ordering::Acquire) {
                        if std::time::Instant::now() >= deadline {
                            cancellation.cancel();
                            return;
                        }
                        thread::sleep(Duration::from_millis(200));
                    }
                })
            };
            let vault_gate = gate.as_ref().map(|gate| {
                gate.for_file_writes(&registration)
                    .cancelled_by(thread_cancellation.permit.clone())
            });
            let gate: &dyn MutationGate = match vault_gate.as_ref() {
                Some(gate) => gate,
                None => &Ungated,
            };
            let ledger_key = vault_key(&registration);
            // A direct `vulcan schedule run` of the same job may hold it; this
            // run follows once it finishes.
            let lock = loop {
                match try_lock_job_run(&state_root, &ledger_key, &job.id) {
                    Ok(Some(lock)) => break Ok(lock),
                    Ok(None) if thread_cancellation.is_cancelled() => {
                        break Err(
                            "cancelled while another run of this job was in progress".to_string()
                        )
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(200)),
                    Err(error) => break Err(error.to_string()),
                }
            };
            let result = lock.and_then(|_lock| {
                run_job(
                    &job,
                    &RunContext {
                        registry: &registry,
                        paths: &paths,
                        ceiling: registration.permissions_profile.as_deref(),
                        gate,
                        cancellation: &thread_cancellation.target,
                        trust: TrustTarget::for_registration(
                            &registration,
                            &state_root,
                            &ledger_key,
                        ),
                    },
                )
            });
            finished.store(true, Ordering::Release);
            let _ = watchdog.join();
            let result = if thread_cancellation.is_cancelled() {
                Err(match result {
                    Ok(summary) => format!("cancelled after it finished: {summary}"),
                    Err(error) => format!("cancelled: {error}"),
                })
            } else {
                result
            };
            let _ = sender.send((thread_slot, thread_key, result));
        });
        self.running.insert(
            slot,
            Running {
                revision,
                attempt,
                retries,
                cancellation,
                handle,
            },
        );
    }

    fn collect_finished(&mut self, now_ms: i64) {
        while let Ok((slot, key, result)) = self.finished.try_recv() {
            if let Some(running) = self.running.remove(&slot) {
                let cancelled = running.cancellation.is_cancelled();
                let _ = running.handle.join();
                let (state, outcome) = match &result {
                    Ok(summary) => (OccurrenceState::Succeeded, summary.clone()),
                    Err(error) if cancelled => (OccurrenceState::Cancelled, error.clone()),
                    Err(error) => (OccurrenceState::Failed, error.clone()),
                };
                if let Err(error) = self.ledger.finish(&key, state, &outcome, now_ms) {
                    eprintln!("schedule `{}`: {error}", key.job_id);
                }
                if state == OccurrenceState::Failed && running.attempt <= i64::from(running.retries)
                {
                    self.retry_at
                        .insert(slot, (key, running.attempt + 1, now_ms + RETRY_DELAY_MS));
                }
            }
        }
    }

    /// Occurrences a previous run left dispatched: retried when their target
    /// is idempotent and retries remain, otherwise marked interrupted.
    fn recover(&mut self, vaults: &[ScheduledVault], now_ms: i64) {
        for vault in vaults {
            let vault_id = vault_key(&vault.registration);
            let Ok(unfinished) = self.ledger.unfinished(&vault_id) else {
                continue;
            };
            for (key, attempt) in unfinished {
                let job = vault
                    .jobs
                    .iter()
                    .find(|job| job.id == key.job_id && job.revision == key.revision);
                let retryable = job.is_some_and(|job| {
                    !key.manual
                        && job.target.idempotent()
                        && attempt <= i64::from(job.limits.retries)
                        && self.placement(job) == JobPlacement::Runs
                });
                if retryable {
                    self.retry_at.insert(
                        (vault_id.clone(), key.job_id.clone()),
                        (key, attempt + 1, now_ms),
                    );
                } else if let Err(error) = self.ledger.finish(
                    &key,
                    OccurrenceState::Interrupted,
                    "the daemon stopped before this run recorded an outcome",
                    now_ms,
                ) {
                    eprintln!("schedule `{}`: {error}", key.job_id);
                }
            }
        }
    }

    /// Asks every run to stop and waits for them.
    pub fn shutdown(mut self, now: DateTime<Utc>) {
        for running in self.running.values() {
            running.cancellation.cancel();
        }
        while !self.running.is_empty() {
            thread::sleep(Duration::from_millis(50));
            self.collect_finished(now.timestamp_millis());
        }
    }

    /// Whether a run of `job_id` in `vault` is in progress.
    #[must_use]
    pub fn is_running(&self, vault: &str, job_id: &str) -> bool {
        self.running
            .contains_key(&(vault.to_string(), job_id.to_string()))
    }
}

/// The supervised service: ticks on the real clock until `stop`.
pub fn run_scheduler_service(
    registry: &WikiRegistry,
    state_root: &Path,
    device_id: Option<String>,
    gate: Option<&Arc<HostedWorkerGate>>,
    stop: &ShutdownSignal,
) -> Result<(), String> {
    let mut scheduler = Scheduler::open(state_root, device_id)?;
    while !stop.is_cancelled() {
        let registrations = registry.poll().map_err(|error| error.to_string())?.vaults;
        let now = Utc::now();
        let vaults = scheduled_vaults(&registrations, scheduler.ledger(), now.timestamp_millis());
        let next = scheduler.tick(now, &vaults, registry, gate);
        let wait = next
            .and_then(|next| (next - Utc::now()).to_std().ok())
            .unwrap_or(RELOAD_INTERVAL)
            .min(RELOAD_INTERVAL)
            .max(Duration::from_millis(200));
        // Finished runs are recorded on the next tick; wake soon while any run.
        let wait = if scheduler.running.is_empty() {
            wait
        } else {
            wait.min(Duration::from_secs(1))
        };
        stop.wait_timeout(wait);
    }
    scheduler.shutdown(Utc::now());
    Ok(())
}

/// An occurrence's instant in UTC and in the job's own time zone (UTC for
/// interval jobs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OccurrenceTimes {
    pub utc: String,
    pub local: String,
    pub timezone: String,
}

#[must_use]
pub fn occurrence_times(job: &ScheduleJob, instant: DateTime<Utc>) -> OccurrenceTimes {
    let timezone = match &job.trigger {
        vulcan_core::schedules::time::Trigger::Cron { timezone, .. } => *timezone,
        vulcan_core::schedules::time::Trigger::Every { .. } => chrono_tz::UTC,
    };
    OccurrenceTimes {
        utc: instant.to_rfc3339(),
        local: instant.with_timezone(&timezone).to_rfc3339(),
        timezone: timezone.name().to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthorizationStatus {
    pub allowed: bool,
    pub policy: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobStatus {
    #[serde(flatten)]
    pub job: ScheduleJob,
    pub placement: JobPlacement,
    pub authorization: AuthorizationStatus,
    /// A run is recorded as dispatched and has not finished.
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<OccurrenceTimes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_success: Option<vulcan_app::schedules::OccurrenceRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<vulcan_app::schedules::OccurrenceRecord>,
}

pub const SCHEDULE_STATUS_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct ScheduleStatusReport {
    pub version: u32,
    pub vault: String,
    /// This device's ID, which `nodes` must list for a job to run here.
    pub device_id: Option<String>,
    pub blocked: bool,
    pub diagnostics: Vec<vulcan_core::schedules::ScheduleDiagnostic>,
    pub jobs: Vec<JobStatus>,
    /// The trusted-commit resolution, for a vault that uses it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust: Option<TrustResolution>,
}

/// The vault's jobs as this device sees them: where each runs, whether the
/// authorizer accepts it, its next run, and its last outcomes. Under
/// trusted-commit the jobs are those at the resolved commit.
pub fn schedule_status(
    paths: &VaultPaths,
    vault: &str,
    ceiling: Option<&str>,
    device_id: Option<&str>,
    state_root: &Path,
    now: DateTime<Utc>,
    trust: Option<&TrustTarget<'_>>,
) -> Result<ScheduleStatusReport, String> {
    let resolution = trust.map(TrustTarget::resolve).transpose()?;
    let set = resolution.as_ref().map_or_else(
        || load_schedules(paths),
        |resolution| resolution.schedules.clone(),
    );
    let ledger = ScheduleLedger::open(state_root).map_err(|error| error.to_string())?;
    let unfinished = ledger
        .unfinished(vault)
        .map_err(|error| error.to_string())?;
    let mut jobs = Vec::with_capacity(set.jobs.len());
    for job in &set.jobs {
        let decision = match &resolution {
            None => authorize_execution(paths, job, ceiling)
                .map(|(authorization, _)| authorization.policy),
            Some(resolution) => authorize_trusted_execution(paths, job, ceiling, resolution)
                .map(|(authorization, _, _)| authorization.policy),
        };
        let policy = if resolution.is_some() {
            vulcan_app::schedule_trust::TRUSTED_COMMIT_POLICY
        } else {
            "repository-trust"
        };
        let authorization = match decision {
            Ok(policy) => AuthorizationStatus {
                allowed: true,
                policy,
                error: None,
            },
            Err(error) => AuthorizationStatus {
                allowed: false,
                policy,
                error: Some(error.to_string()),
            },
        };
        let job_placement = job_placement(job, device_id);
        let next = (job_placement == JobPlacement::Runs)
            .then(|| job.trigger.next_after(now))
            .flatten()
            .map(|instant| occurrence_times(job, instant));
        jobs.push(JobStatus {
            placement: job_placement,
            authorization,
            running: unfinished.iter().any(|(key, _)| key.job_id == job.id),
            next,
            last_success: ledger
                .last_finished(vault, &job.id, true)
                .map_err(|error| error.to_string())?,
            last_failure: ledger
                .last_finished(vault, &job.id, false)
                .map_err(|error| error.to_string())?,
            job: job.clone(),
        });
    }
    Ok(ScheduleStatusReport {
        version: SCHEDULE_STATUS_VERSION,
        vault: vault.to_string(),
        device_id: device_id.map(str::to_string),
        blocked: set.blocked,
        diagnostics: set.diagnostics,
        jobs,
        trust: resolution,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManualRunReport {
    pub job_id: String,
    pub revision: String,
    pub dry_run: bool,
    pub policy: &'static str,
    pub profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// Runs `job` once, now, outside its schedule, recording it in the ledger as
/// a manual occurrence that leaves the schedule's watermark alone. A dry run
/// only authorizes it.
pub fn run_manually(
    job: &ScheduleJob,
    context: &RunContext<'_>,
    state_root: &Path,
    vault: &str,
    dry_run: bool,
) -> Result<ManualRunReport, String> {
    let (authorization, _, _) = authorize_run(job, context)?;
    let mut report = ManualRunReport {
        job_id: job.id.clone(),
        revision: job.revision.clone(),
        dry_run,
        policy: authorization.policy,
        profile: authorization.profile,
        outcome: None,
    };
    if dry_run {
        return Ok(report);
    }
    let Some(_lock) =
        try_lock_job_run(state_root, vault, &job.id).map_err(|error| error.to_string())?
    else {
        return Err(format!(
            "job `{}` is already running on this device; try again when it finishes",
            job.id
        ));
    };
    let mut ledger = ScheduleLedger::open(state_root).map_err(|error| error.to_string())?;
    let now_ms = Utc::now().timestamp_millis();
    let key = OccurrenceKey {
        vault: vault.to_string(),
        job_id: job.id.clone(),
        revision: job.revision.clone(),
        scheduled_ms: now_ms,
        manual: true,
    };
    ledger
        .claim(&key, now_ms)
        .map_err(|error| error.to_string())?;
    let result = run_job(job, context);
    let finished = Utc::now().timestamp_millis();
    let (state, outcome) = match &result {
        Ok(summary) => (OccurrenceState::Succeeded, summary.clone()),
        Err(error) => (OccurrenceState::Failed, error.clone()),
    };
    ledger
        .finish(&key, state, &outcome, finished)
        .map_err(|error| error.to_string())?;
    report.outcome = Some(result?);
    Ok(report)
}

/// The ledger path, for status readers.
#[must_use]
pub fn ledger_path(state_root: &Path) -> PathBuf {
    ScheduleLedger::path(state_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{AddWikiRequest, WikiId};
    use vulcan_app::schedules::OccurrenceRecord;

    struct Fixture {
        _temporary: tempfile::TempDir,
        registry: WikiRegistry,
        vault: PathBuf,
        state: PathBuf,
    }

    fn fixture(jobs: &str, ceiling: Option<&str>) -> Fixture {
        let temporary = tempfile::tempdir().unwrap();
        let vault = temporary.path().join("vault");
        std::fs::create_dir_all(vault.join(".vulcan")).unwrap();
        std::fs::write(vault.join("Home.md"), "# Home\n").unwrap();
        std::fs::write(
            vault.join(".vulcan/config.toml"),
            format!(
                "[permissions.profiles.no_index]\nread = \"all\"\nindex = \"deny\"\n\n[schedules]\nversion = 1\n{jobs}"
            ),
        )
        .unwrap();
        let registry = WikiRegistry::at(temporary.path().join("daemon.toml"));
        registry
            .add(
                &AddWikiRequest {
                    profile: None,
                    id: WikiId::parse("notes").unwrap(),
                    path: vault.clone(),
                    groups: vec![],
                    git_dir: None,
                    permissions_profile: ceiling.map(str::to_string),
                    sync_backend: Some("none".to_string()),
                    platform_profile: None,
                },
                false,
            )
            .unwrap();
        Fixture {
            state: temporary.path().join("state"),
            _temporary: temporary,
            registry,
            vault,
        }
    }

    fn job(id: &str, nodes: &str, extra: &str) -> String {
        format!(
            "[[schedules.jobs]]\nid = \"{id}\"\ntarget = {{ builtin = \"scan\" }}\nnodes = [{nodes}]\ntrigger = {{ every = \"1m\" }}\nauthority = \"unrestricted\"\n{extra}\n"
        )
    }

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + seconds, 0).unwrap()
    }

    /// Ticks at `now` until no run is in progress, as the service would.
    fn settle(scheduler: &mut Scheduler, fixture: &Fixture, now: DateTime<Utc>) {
        tick(scheduler, fixture, now, None);
        drain(scheduler, now);
    }

    /// One tick at `now`, leaving any run it starts in progress.
    fn tick(
        scheduler: &mut Scheduler,
        fixture: &Fixture,
        now: DateTime<Utc>,
        gate: Option<&Arc<HostedWorkerGate>>,
    ) {
        let vaults = scheduled_vaults(
            &fixture.registry.load().unwrap().vaults,
            scheduler.ledger(),
            now.timestamp_millis(),
        );
        scheduler.tick(now, &vaults, &fixture.registry, gate);
    }

    /// Records runs as they finish until none is in progress.
    fn drain(scheduler: &mut Scheduler, now: DateTime<Utc>) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !scheduler.running.is_empty() {
            assert!(std::time::Instant::now() < deadline, "a run never finished");
            thread::sleep(Duration::from_millis(20));
            scheduler.collect_finished(now.timestamp_millis());
        }
    }

    fn history(fixture: &Fixture, job: &str) -> Vec<OccurrenceRecord> {
        history_in(fixture, &fixture.state, job)
    }

    /// `job`'s runs in the ledger under `state`, newest first.
    fn history_in(fixture: &Fixture, state: &Path, job: &str) -> Vec<OccurrenceRecord> {
        let key = vault_key(&fixture.registry.load().unwrap().vaults[0]);
        ScheduleLedger::open(state)
            .unwrap()
            .history(&key, job, 50)
            .unwrap()
    }

    /// A host mutation gate, as the daemon builds for its workers, and its
    /// scheduler.
    fn host_gate() -> (
        tokio::runtime::Runtime,
        Arc<HostedWorkerGate>,
        Arc<crate::mutation_scheduler::MutationScheduler>,
    ) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let scheduler = Arc::new(
            crate::mutation_scheduler::MutationScheduler::new(
                crate::mutation_scheduler::MutationSchedulerConfig::default(),
            )
            .unwrap(),
        );
        let gate = Arc::new(HostedWorkerGate::new(
            Arc::clone(&scheduler),
            runtime.handle().clone(),
            "scheduler.test",
        ));
        (runtime, gate, scheduler)
    }

    /// Waits until a run is queued for the vault's permit.
    fn await_queued(scheduler: &crate::mutation_scheduler::MutationScheduler) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while scheduler.waiting() == 0 {
            assert!(std::time::Instant::now() < deadline, "the run never queued");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn set_config(fixture: &Fixture, edit: impl FnOnce(String) -> String) {
        let path = fixture.vault.join(".vulcan/config.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, edit(text)).unwrap();
    }

    #[test]
    fn due_occurrences_run_once_and_downtime_coalesces() {
        let fixture = fixture(
            &[
                job("rescan", "\"device-a\"", ""),
                job("off", "\"device-a\"", "enabled = false"),
                job("elsewhere", "\"device-b\"", ""),
            ]
            .concat(),
            None,
        );
        let mut scheduler = Scheduler::open(&fixture.state, Some("device-a".to_string())).unwrap();
        // The first sight of a job activates it; nothing earlier runs.
        settle(&mut scheduler, &fixture, at(30));
        assert!(history(&fixture, "rescan").is_empty());

        settle(&mut scheduler, &fixture, at(65));
        let runs = history(&fixture, "rescan");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].state, OccurrenceState::Succeeded, "{runs:?}");
        assert_eq!(runs[0].scheduled_ms, at(60).timestamp_millis());
        assert!(
            fixture.vault.join(".vulcan/cache.db").exists(),
            "it scanned"
        );

        settle(&mut scheduler, &fixture, at(70));
        assert_eq!(history(&fixture, "rescan").len(), 1, "never twice");

        // Ten minutes down: one run, for the latest missed occurrence.
        settle(&mut scheduler, &fixture, at(665));
        let runs = history(&fixture, "rescan");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].scheduled_ms, at(660).timestamp_millis());
        // A clock that jumps back repeats nothing.
        settle(&mut scheduler, &fixture, at(400));
        assert_eq!(history(&fixture, "rescan").len(), 2);

        assert!(history(&fixture, "off").is_empty());
        assert!(history(&fixture, "elsewhere").is_empty());
        assert!(
            Scheduler::open(&fixture.state, None)
                .unwrap()
                .placement(&load_schedules(&VaultPaths::new(&fixture.vault)).jobs[0])
                == JobPlacement::NotThisDevice,
            "a device without an identity runs nothing"
        );
    }

    #[test]
    fn a_failing_run_is_recorded_and_retried_within_its_limit() {
        // The job asks for a profile that cannot index, so its scan fails.
        let fixture = fixture(
            &job("rescan", "\"device-a\"", "limits = { retries = 1 }")
                .replace("authority = \"unrestricted\"", "authority = \"no_index\""),
            None,
        );
        let mut scheduler = Scheduler::open(&fixture.state, Some("device-a".to_string())).unwrap();
        settle(&mut scheduler, &fixture, at(0));
        settle(&mut scheduler, &fixture, at(61));
        let runs = history(&fixture, "rescan");
        assert_eq!(runs[0].state, OccurrenceState::Failed);
        assert!(
            runs[0].outcome.as_deref().unwrap().contains("index"),
            "{runs:?}"
        );
        // Retried once, after the retry delay, as the same occurrence.
        settle(&mut scheduler, &fixture, at(61 + 31));
        let runs = history(&fixture, "rescan");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].attempt, 2);
        assert_eq!(runs[0].state, OccurrenceState::Failed);
        settle(&mut scheduler, &fixture, at(61 + 100));
        let first = history(&fixture, "rescan")
            .into_iter()
            .find(|run| run.scheduled_ms == at(60).timestamp_millis())
            .unwrap();
        assert_eq!(first.attempt, 2, "no retries remain");
    }

    fn run_git(directory: &Path, arguments: &[&str], input: Option<&[u8]>) -> String {
        use std::io::Write as _;
        let mut child = std::process::Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(input) = input {
            child.stdin.take().unwrap().write_all(input).unwrap();
        }
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    /// Commits the vault's work tree, SSH-signed by `key` when given.
    fn commit(vault: &Path, key: Option<&ssh_key::PrivateKey>) -> String {
        use std::fmt::Write as _;
        run_git(vault, &["add", "-A"], None);
        let tree = run_git(vault, &["write-tree"], None);
        let parent = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .current_dir(vault)
            .output()
            .unwrap();
        let parent = String::from_utf8_lossy(&parent.stdout).trim().to_string();
        let mut payload = format!("tree {tree}\n");
        if !parent.is_empty() {
            let _ = writeln!(payload, "parent {parent}");
        }
        payload.push_str("author T <t@example.invalid> 1700000000 +0000\n");
        payload.push_str("committer T <t@example.invalid> 1700000000 +0000\n");
        let message = format!("change {}\n", ulid::Ulid::new());
        let object = match key {
            Some(key) => {
                let signature = key
                    .sign(
                        "git",
                        ssh_key::HashAlg::Sha512,
                        format!("{payload}\n{message}").as_bytes(),
                    )
                    .unwrap()
                    .to_pem(ssh_key::LineEnding::LF)
                    .unwrap();
                let mut header = String::new();
                for line in signature.trim_end().lines() {
                    let _ = writeln!(header, " {line}");
                }
                format!("{payload}gpgsig{header}\n{message}")
            }
            None => format!("{payload}\n{message}"),
        };
        let oid = run_git(
            vault,
            &["hash-object", "-t", "commit", "-w", "--stdin"],
            Some(object.as_bytes()),
        );
        run_git(vault, &["update-ref", "HEAD", &oid], None);
        oid
    }

    #[test]
    fn under_trusted_commit_only_signed_definitions_run() {
        let fixture = fixture(&job("rescan", "\"device-a\"", ""), None);
        run_git(&fixture.vault, &["init", "-q", "-b", "main"], None);
        std::fs::write(fixture.vault.join(".gitignore"), ".vulcan/cache.db*\n").unwrap();
        let admin = ssh_key::PrivateKey::random(
            &mut ssh_key::rand_core::OsRng,
            ssh_key::Algorithm::Ed25519,
        )
        .unwrap();
        commit(&fixture.vault, Some(&admin));
        let signers = fixture.state.with_file_name("execution-signers");
        std::fs::write(
            &signers,
            format!("admin {}\n", admin.public_key().to_openssh().unwrap()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&signers, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let config = ExecutionTrustConfig::new(signers, vec!["admin".to_string()], None).unwrap();
        let registration = fixture.registry.load().unwrap().vaults.remove(0);
        fixture
            .registry
            .update(
                &registration.id,
                &crate::registry::UpdateWikiRequest {
                    groups_to_add: vec![],
                    groups_to_remove: vec![],
                    permissions_profile: None,
                    sync_paused: None,
                    profile: None,
                    execution_trust: Some(Some(config.clone())),
                },
                false,
            )
            .unwrap();
        let key = vault_key(&registration);
        let mut scheduler = Scheduler::open(&fixture.state, Some("device-a".to_string())).unwrap();
        // Without a checkpoint nothing runs.
        settle(&mut scheduler, &fixture, at(0));
        settle(&mut scheduler, &fixture, at(65));
        assert!(history(&fixture, "rescan").is_empty());

        vulcan_app::schedule_trust::pin_checkpoint(
            &config,
            &trust_repository(&registration),
            scheduler.ledger(),
            &key,
            &vulcan_app::schedule_trust::CheckpointPin {
                commit: None,
                how: "enabled",
                now_ms: 0,
                dry_run: false,
            },
        )
        .unwrap();
        settle(&mut scheduler, &fixture, at(70));
        settle(&mut scheduler, &fixture, at(125));
        let runs = history(&fixture, "rescan");
        assert_eq!(runs.len(), 1, "{runs:?}");
        assert_eq!(runs[0].state, OccurrenceState::Succeeded, "{runs:?}");

        // An uncommitted edit is pending and changes nothing.
        let config_path = fixture.vault.join(".vulcan/config.toml");
        let signed_text = std::fs::read_to_string(&config_path).unwrap();
        std::fs::write(
            &config_path,
            signed_text.replace("every = \"1m\"", "every = \"2m\""),
        )
        .unwrap();
        settle(&mut scheduler, &fixture, at(185));
        assert_eq!(history(&fixture, "rescan").len(), 2, "still the signed job");

        // Committed without a signature, the change stops the job.
        commit(&fixture.vault, None);
        settle(&mut scheduler, &fixture, at(245));
        settle(&mut scheduler, &fixture, at(365));
        assert_eq!(
            history(&fixture, "rescan").len(),
            2,
            "unsigned changes never run"
        );
        let status = schedule_status(
            &VaultPaths::new(&fixture.vault),
            &key,
            None,
            Some("device-a"),
            &fixture.state,
            at(365),
            TrustTarget::for_registration(
                &fixture.registry.load().unwrap().vaults[0],
                &fixture.state,
                &key,
            )
            .as_ref(),
        )
        .unwrap();
        assert!(!status.jobs[0].authorization.allowed);
        assert_eq!(status.jobs[0].authorization.policy, "trusted-commit");

        // Signed again, it runs again.
        commit(&fixture.vault, Some(&admin));
        settle(&mut scheduler, &fixture, at(370));
        settle(&mut scheduler, &fixture, at(485));
        assert_eq!(history(&fixture, "rescan").len(), 3);
    }

    #[test]
    fn a_job_cannot_exceed_the_device_ceiling() {
        let fixture = fixture(&job("rescan", "\"device-a\"", ""), Some("readonly"));
        let mut scheduler = Scheduler::open(&fixture.state, Some("device-a".to_string())).unwrap();
        settle(&mut scheduler, &fixture, at(0));
        settle(&mut scheduler, &fixture, at(61));
        let runs = history(&fixture, "rescan");
        assert_eq!(runs[0].state, OccurrenceState::Failed);
        assert!(runs[0]
            .outcome
            .as_deref()
            .unwrap()
            .contains("exceeds this device's ceiling"));
    }

    #[test]
    fn a_restart_retries_idempotent_runs_and_marks_others_interrupted() {
        let fixture = fixture(
            &[
                job("retried", "\"device-a\"", "limits = { retries = 1 }"),
                job("plain", "\"device-a\"", ""),
            ]
            .concat(),
            None,
        );
        let vault = vault_key(&fixture.registry.load().unwrap().vaults[0]);
        let jobs = load_schedules(&VaultPaths::new(&fixture.vault)).jobs;
        {
            let mut ledger = ScheduleLedger::open(&fixture.state).unwrap();
            for job in &jobs {
                ledger
                    .watermark(&vault, job, at(0).timestamp_millis())
                    .unwrap();
                assert!(ledger
                    .claim(
                        &OccurrenceKey {
                            vault: vault.clone(),
                            job_id: job.id.clone(),
                            revision: job.revision.clone(),
                            scheduled_ms: at(60).timestamp_millis(),
                            manual: false,
                        },
                        at(60).timestamp_millis(),
                    )
                    .unwrap());
            }
        }
        // A new scheduler finds both left dispatched.
        let mut scheduler = Scheduler::open(&fixture.state, Some("device-a".to_string())).unwrap();
        settle(&mut scheduler, &fixture, at(90));
        settle(&mut scheduler, &fixture, at(91));
        let retried = history(&fixture, "retried");
        assert_eq!(retried[0].state, OccurrenceState::Succeeded, "{retried:?}");
        assert_eq!(retried[0].attempt, 2);
        let plain = history(&fixture, "plain");
        assert_eq!(plain[0].state, OccurrenceState::Interrupted, "{plain:?}");
    }

    #[test]
    fn a_reload_cancels_a_run_still_waiting_for_the_vault() {
        let fixture = fixture(
            &job("rescan", "\"device-a\"", "limits = { retries = 1 }"),
            None,
        );
        let (_runtime, gate, permits) = host_gate();
        let registration = fixture.registry.load().unwrap().vaults.remove(0);
        let vault = vault_key(&registration);
        let mut scheduler = Scheduler::open(&fixture.state, Some("device-a".to_string())).unwrap();
        tick(&mut scheduler, &fixture, at(0), Some(&gate));

        // A hosted write holds the vault, so the due run waits for it.
        let held = gate.for_file_writes(&registration).enter().unwrap();
        tick(&mut scheduler, &fixture, at(61), Some(&gate));
        await_queued(&permits);
        assert!(scheduler.is_running(&vault, "rescan"));
        let first = history(&fixture, "rescan");
        assert_eq!(first[0].state, OccurrenceState::Dispatched, "{first:?}");

        // The definition changes while it waits: the run stops without the
        // permit and without scanning, and its occurrence is not retried.
        set_config(&fixture, |text| {
            text.replace("every = \"1m\"", "every = \"2m\"")
        });
        tick(&mut scheduler, &fixture, at(62), Some(&gate));
        drain(&mut scheduler, at(62));
        drop(held);
        let runs = history(&fixture, "rescan");
        assert_eq!(runs.len(), 1, "{runs:?}");
        assert_eq!(runs[0].state, OccurrenceState::Cancelled, "{runs:?}");
        assert!(
            !fixture.vault.join(".vulcan/cache.db").exists(),
            "a cancelled run never scanned"
        );

        // The new definition starts fresh and runs on its own trigger.
        tick(&mut scheduler, &fixture, at(100), Some(&gate));
        drain(&mut scheduler, at(100));
        assert_eq!(
            history(&fixture, "rescan").len(),
            1,
            "the old run is not retried"
        );
        tick(&mut scheduler, &fixture, at(125), Some(&gate));
        drain(&mut scheduler, at(125));
        let runs = history(&fixture, "rescan");
        assert_eq!(runs.len(), 2, "{runs:?}");
        assert_eq!(runs[0].state, OccurrenceState::Succeeded, "{runs:?}");
        assert_eq!(runs[0].scheduled_ms, at(120).timestamp_millis());
        assert_ne!(runs[0].revision, runs[1].revision);

        // A malformed reload runs nothing until it is fixed; the unchanged
        // definition then catches up once, as after downtime.
        set_config(&fixture, |text| format!("{text}\n[schedules\n"));
        settle(&mut scheduler, &fixture, at(245));
        assert_eq!(history(&fixture, "rescan").len(), 2);
        set_config(&fixture, |text| text.replace("\n[schedules\n", ""));
        settle(&mut scheduler, &fixture, at(250));
        let runs = history(&fixture, "rescan");
        assert_eq!(runs.len(), 3, "{runs:?}");
        assert_eq!(runs[0].scheduled_ms, at(240).timestamp_millis());
        settle(&mut scheduler, &fixture, at(365));
        assert_eq!(history(&fixture, "rescan").len(), 4);
    }

    #[test]
    fn shutdown_does_not_wait_for_a_run_queued_behind_a_write() {
        let fixture = fixture(&job("rescan", "\"device-a\"", ""), None);
        let (_runtime, gate, permits) = host_gate();
        let registration = fixture.registry.load().unwrap().vaults.remove(0);
        let mut scheduler = Scheduler::open(&fixture.state, Some("device-a".to_string())).unwrap();
        tick(&mut scheduler, &fixture, at(0), Some(&gate));
        let held = gate.for_file_writes(&registration).enter().unwrap();
        tick(&mut scheduler, &fixture, at(61), Some(&gate));
        await_queued(&permits);
        let started = std::time::Instant::now();
        scheduler.shutdown(at(62));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "shutdown waited {:?} for the permit",
            started.elapsed()
        );
        drop(held);
        let runs = history(&fixture, "rescan");
        assert_eq!(runs[0].state, OccurrenceState::Cancelled, "{runs:?}");
    }

    #[test]
    fn offline_runners_keep_their_own_history_and_a_handover_starts_fresh() {
        let fixture = fixture(&job("rescan", "\"device-a\", \"device-b\"", ""), None);
        let state_b = fixture.state.with_file_name("state-b");
        let mut a = Scheduler::open(&fixture.state, Some("device-a".to_string())).unwrap();
        let mut b = Scheduler::open(&state_b, Some("device-b".to_string())).unwrap();
        settle(&mut a, &fixture, at(0));
        settle(&mut b, &fixture, at(0));

        // Both listed: each runs every occurrence it is up for, independently.
        // `b` is offline from 0 to 185 and coalesces what it missed.
        settle(&mut a, &fixture, at(65));
        settle(&mut a, &fixture, at(125));
        settle(&mut b, &fixture, at(185));
        assert_eq!(history_in(&fixture, &fixture.state, "rescan").len(), 2);
        let runs_b = history_in(&fixture, &state_b, "rescan");
        assert_eq!(runs_b.len(), 1, "{runs_b:?}");
        assert_eq!(runs_b[0].scheduled_ms, at(180).timestamp_millis());

        // Reassigned to `b` alone while `a` is offline: `b` keeps running it
        // under the new definition from when it first sees it, and `a`, back
        // online, runs nothing, not even what it missed.
        set_config(&fixture, |text| {
            text.replace(
                "nodes = [\"device-a\", \"device-b\"]",
                "nodes = [\"device-b\"]",
            )
        });
        settle(&mut b, &fixture, at(190));
        settle(&mut b, &fixture, at(245));
        let runs_b = history_in(&fixture, &state_b, "rescan");
        assert_eq!(runs_b.len(), 2, "{runs_b:?}");
        assert_eq!(runs_b[0].scheduled_ms, at(240).timestamp_millis());
        assert_ne!(runs_b[0].revision, runs_b[1].revision);
        settle(&mut a, &fixture, at(665));
        assert_eq!(history_in(&fixture, &fixture.state, "rescan").len(), 2);
    }

    #[test]
    fn narrowing_the_ceiling_stops_the_next_run() {
        let fixture = fixture(&job("rescan", "\"device-a\"", ""), None);
        let mut scheduler = Scheduler::open(&fixture.state, Some("device-a".to_string())).unwrap();
        settle(&mut scheduler, &fixture, at(0));
        settle(&mut scheduler, &fixture, at(65));
        assert_eq!(
            history(&fixture, "rescan")[0].state,
            OccurrenceState::Succeeded
        );

        let registration = fixture.registry.load().unwrap().vaults.remove(0);
        let ceiling = |profile: Option<&str>| crate::registry::UpdateWikiRequest {
            groups_to_add: vec![],
            groups_to_remove: vec![],
            permissions_profile: Some(profile.map(str::to_string)),
            sync_paused: None,
            profile: None,
            execution_trust: None,
        };
        fixture
            .registry
            .update(&registration.id, &ceiling(Some("readonly")), false)
            .unwrap();
        settle(&mut scheduler, &fixture, at(125));
        let runs = history(&fixture, "rescan");
        assert_eq!(runs[0].state, OccurrenceState::Failed, "{runs:?}");
        assert!(runs[0]
            .outcome
            .as_deref()
            .unwrap()
            .contains("exceeds this device's ceiling"));

        // Only the device can raise it again.
        fixture
            .registry
            .update(&registration.id, &ceiling(None), false)
            .unwrap();
        settle(&mut scheduler, &fixture, at(185));
        let runs = history(&fixture, "rescan");
        assert_eq!(runs[0].state, OccurrenceState::Succeeded, "{runs:?}");
    }

    #[test]
    fn a_manual_run_and_the_daemon_never_overlap_one_job() {
        let fixture = fixture(&job("rescan", "\"device-a\"", ""), None);
        let registration = fixture.registry.load().unwrap().vaults.remove(0);
        let vault = vault_key(&registration);
        let paths = VaultPaths::new(&fixture.vault);
        let job = load_schedules(&paths).job("rescan").unwrap().clone();
        let cancellation = SyncCancellationToken::default();
        let context = RunContext {
            registry: &fixture.registry,
            paths: &paths,
            ceiling: None,
            gate: &Ungated,
            cancellation: &cancellation,
            trust: None,
        };
        let mut scheduler = Scheduler::open(&fixture.state, Some("device-a".to_string())).unwrap();
        settle(&mut scheduler, &fixture, at(0));

        // While a direct run holds the job, the daemon's run waits for it.
        let manual = try_lock_job_run(&fixture.state, &vault, "rescan")
            .unwrap()
            .unwrap();
        tick(&mut scheduler, &fixture, at(61), None);
        thread::sleep(Duration::from_millis(300));
        scheduler.collect_finished(at(61).timestamp_millis());
        assert!(scheduler.is_running(&vault, "rescan"));
        assert!(!fixture.vault.join(".vulcan/cache.db").exists());
        drop(manual);
        drain(&mut scheduler, at(62));
        let runs = history(&fixture, "rescan");
        assert_eq!(runs[0].state, OccurrenceState::Succeeded, "{runs:?}");

        // While the daemon's run holds it, a direct run is refused, and
        // allowed again once it finishes.
        let daemon = try_lock_job_run(&fixture.state, &vault, "rescan")
            .unwrap()
            .unwrap();
        let error = run_manually(&job, &context, &fixture.state, &vault, false).unwrap_err();
        assert!(error.contains("already running on this device"), "{error}");
        assert!(
            run_manually(&job, &context, &fixture.state, &vault, true).is_ok(),
            "a dry run only authorizes"
        );
        drop(daemon);
        let report = run_manually(&job, &context, &fixture.state, &vault, false).unwrap();
        assert!(report.outcome.unwrap().starts_with("scanned"));

        // A cancelled run stops waiting.
        let held = try_lock_job_run(&fixture.state, &vault, "rescan")
            .unwrap()
            .unwrap();
        tick(&mut scheduler, &fixture, at(121), None);
        set_config(&fixture, |text| {
            text.replace("every = \"1m\"", "every = \"2m\"")
        });
        tick(&mut scheduler, &fixture, at(122), None);
        drain(&mut scheduler, at(122));
        drop(held);
        let runs = history(&fixture, "rescan");
        let cancelled = runs.iter().find(|run| !run.manual).unwrap();
        assert_eq!(cancelled.state, OccurrenceState::Cancelled, "{runs:?}");
    }
}
