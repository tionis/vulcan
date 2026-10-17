//! `vulcan schedule`: the vault's scheduled jobs (Roadmap 10.8).

use crate::cli::{ScheduleCommand, ScheduleTrustCommand};
use crate::output::print_json;
use crate::{Cli, CliError, OutputFormat};
use serde::Serialize;
use vulcan_app::execution::Ungated;
use vulcan_app::schedule_trust::{
    pin_checkpoint, resolve_trust, CheckpointPin, ExecutionTrustConfig, JobTrust, TrustCheckpoint,
    TrustResolution,
};
use vulcan_app::schedules::ScheduleLedger;
use vulcan_app::sync::SyncCancellationToken;
use vulcan_core::schedules::{load_schedules, ScheduleJob, ScheduleSet};
use vulcan_core::VaultPaths;
use vulcan_daemon::process::DaemonProcessContext;
use vulcan_daemon::registry::{UpdateWikiRequest, WikiRegistration, WikiRegistry};
use vulcan_daemon::scheduler::{
    occurrence_times, run_manually, schedule_status, trust_repository, JobStatus, RunContext,
    TrustTarget,
};

/// Where this vault's schedule state lives and what this device may do.
struct ScheduleContext {
    registry: WikiRegistry,
    state_root: std::path::PathBuf,
    /// The ledger key: the registration ID, or the canonical path when the
    /// vault is not registered.
    vault: String,
    ceiling: Option<String>,
    device_id: Option<String>,
    registration: Option<WikiRegistration>,
}

impl ScheduleContext {
    /// The vault's trusted-commit target, when it uses that policy.
    fn trust(&self) -> Option<TrustTarget<'_>> {
        self.registration.as_ref().and_then(|registration| {
            TrustTarget::for_registration(registration, &self.state_root, &self.vault)
        })
    }

    fn registered(&self) -> Result<&WikiRegistration, CliError> {
        self.registration.as_ref().ok_or_else(|| {
            CliError::operation(
                "execution trust is device configuration of a registered vault; register it with `vulcan vault add` first",
            )
        })
    }

    /// The schedules jobs run from: the resolved commit's under
    /// trusted-commit, otherwise the working tree's.
    fn schedules(&self, paths: &VaultPaths) -> Result<ScheduleSet, CliError> {
        match self.trust() {
            Some(trust) => Ok(trust.resolve().map_err(CliError::operation)?.schedules),
            None => Ok(load_schedules(paths)),
        }
    }
}

fn schedule_context(cli: &Cli, paths: &VaultPaths) -> Result<ScheduleContext, CliError> {
    let context = DaemonProcessContext::user_default().map_err(CliError::operation)?;
    let registration = context.registry.find_by_path(paths.vault_root()).ok();
    let vault = registration.as_ref().map_or_else(
        || {
            std::fs::canonicalize(paths.vault_root())
                .unwrap_or_else(|_| paths.vault_root().to_path_buf())
                .display()
                .to_string()
        },
        |registration| registration.registration_id.to_string(),
    );
    // A registered vault's ceiling is its registration's; otherwise the
    // caller's own profile bounds the run.
    let ceiling = registration
        .as_ref()
        .and_then(|registration| registration.permissions_profile.clone())
        .or_else(|| cli.permissions.clone());
    Ok(ScheduleContext {
        device_id: context.device_identity.device_id().ok().flatten(),
        state_root: context.state_root,
        registry: context.registry,
        vault,
        ceiling,
        registration,
    })
}

fn find_job(
    paths: &VaultPaths,
    context: &ScheduleContext,
    id: &str,
) -> Result<ScheduleJob, CliError> {
    let set = context.schedules(paths)?;
    if let Some(job) = set.job(id) {
        return Ok(job.clone());
    }
    let reason = set
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.job.as_deref() == Some(id) || diagnostic.job.is_none())
        .map(|diagnostic| format!(": {}", diagnostic.message))
        .unwrap_or_default();
    Err(CliError::operation(format!(
        "no runnable scheduled job `{id}`{reason}"
    )))
}

pub(crate) fn handle_schedule_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &ScheduleCommand,
) -> Result<(), CliError> {
    let context = schedule_context(cli, paths)?;
    let now = chrono::Utc::now();
    match command {
        ScheduleCommand::List | ScheduleCommand::Show { .. } => {
            list(cli, paths, &context, command, now)
        }
        ScheduleCommand::Next { id, count } => next(cli, paths, &context, id, *count, now),
        ScheduleCommand::History { id, limit } => history(cli, &context, id, *limit),
        ScheduleCommand::Run { id, dry_run } => run(cli, paths, &context, id, *dry_run),
        ScheduleCommand::Enable { id, dry_run } | ScheduleCommand::Disable { id, dry_run } => {
            toggle(cli, paths, &context, command, id, *dry_run)
        }
        ScheduleCommand::Trust(command) => trust(cli, &context, command, now),
    }
}

fn list(
    cli: &Cli,
    paths: &VaultPaths,
    context: &ScheduleContext,
    command: &ScheduleCommand,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), CliError> {
    let report = schedule_status(
        paths,
        &context.vault,
        context.ceiling.as_deref(),
        context.device_id.as_deref(),
        &context.state_root,
        now,
        context.trust().as_ref(),
    )
    .map_err(CliError::operation)?;
    if let ScheduleCommand::Show { id } = command {
        let job = report
            .jobs
            .into_iter()
            .find(|job| job.job.id == *id)
            .ok_or_else(|| {
                find_job(paths, context, id)
                    .err()
                    .unwrap_or_else(|| CliError::operation(format!("no scheduled job `{id}`")))
            })?;
        if cli.output == OutputFormat::Json {
            return print_json(&job);
        }
        print_job(&job);
        return Ok(());
    }
    if cli.output == OutputFormat::Json {
        return print_json(&report);
    }
    if let Some(trust) = &report.trust {
        println!("Trust: trusted-commit at `{}`", trust.source);
        if let Some(block) = &trust.blocked {
            println!("  blocked: {}", block.detail);
        }
    }
    if report.jobs.is_empty() && report.diagnostics.is_empty() {
        println!("No scheduled jobs.");
    }
    for job in &report.jobs {
        print_job(job);
    }
    for diagnostic in &report.diagnostics {
        match &diagnostic.job {
            Some(job) => println!("blocked {job}: {}", diagnostic.message),
            None => println!("blocked: {}", diagnostic.message),
        }
    }
    if report.device_id.is_none() {
        println!("This device has no identity, so no job runs here; see `vulcan device init`.");
    }
    Ok(())
}

fn next(
    cli: &Cli,
    paths: &VaultPaths,
    context: &ScheduleContext,
    id: &str,
    count: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), CliError> {
    let job = find_job(paths, context, id)?;
    let occurrences = job
        .next_occurrences(now, count.clamp(1, 100))
        .into_iter()
        .map(|instant| occurrence_times(&job, instant))
        .collect::<Vec<_>>();
    if cli.output == OutputFormat::Json {
        return print_json(&serde_json::json!({
            "job_id": job.id,
            "occurrences": occurrences,
        }));
    }
    for occurrence in occurrences {
        println!("{}  ({} UTC)", occurrence.local, occurrence.utc);
    }
    Ok(())
}

fn history(cli: &Cli, context: &ScheduleContext, id: &str, limit: usize) -> Result<(), CliError> {
    let ledger = ScheduleLedger::open(&context.state_root).map_err(CliError::operation)?;
    let history = ledger
        .history(&context.vault, id, limit.clamp(1, 1_000))
        .map_err(CliError::operation)?;
    if cli.output == OutputFormat::Json {
        return print_json(&history);
    }
    if history.is_empty() {
        println!("No recorded runs of `{id}`.");
    }
    for run in history {
        let scheduled = chrono::DateTime::from_timestamp_millis(run.scheduled_ms)
            .map(|instant| instant.to_rfc3339())
            .unwrap_or_default();
        println!(
            "{scheduled} {:?}{} attempt {}{}",
            run.state,
            if run.manual { " (manual)" } else { "" },
            run.attempt,
            run.outcome
                .map(|outcome| format!(": {outcome}"))
                .unwrap_or_default()
        );
    }
    Ok(())
}

fn run(
    cli: &Cli,
    paths: &VaultPaths,
    context: &ScheduleContext,
    id: &str,
    dry_run: bool,
) -> Result<(), CliError> {
    let job = find_job(paths, context, id)?;
    let cancellation = SyncCancellationToken::default();
    let report = run_manually(
        &job,
        &RunContext {
            registry: &context.registry,
            paths,
            ceiling: context.ceiling.as_deref(),
            gate: &Ungated,
            cancellation: &cancellation,
            trust: context.trust(),
        },
        &context.state_root,
        &context.vault,
        dry_run,
    )
    .map_err(CliError::operation)?;
    if cli.output == OutputFormat::Json {
        return print_json(&report);
    }
    match &report.outcome {
        Some(outcome) => println!("Ran `{}`: {outcome}", report.job_id),
        None => println!(
            "`{}` may run under {} as profile `{}`.",
            report.job_id, report.policy, report.profile
        ),
    }
    Ok(())
}

fn toggle(
    cli: &Cli,
    paths: &VaultPaths,
    context: &ScheduleContext,
    command: &ScheduleCommand,
    id: &str,
    dry_run: bool,
) -> Result<(), CliError> {
    let enabled = matches!(command, ScheduleCommand::Enable { .. });
    let report = vulcan_app::schedules::set_job_enabled(paths, id, enabled, dry_run)
        .map_err(CliError::operation)?;
    if cli.output == OutputFormat::Json {
        return print_json(&report);
    }
    let state = if enabled { "enabled" } else { "disabled" };
    if report.unchanged {
        println!("`{id}` is already {state}.");
    } else if report.dry_run {
        println!("Would mark `{id}` {state} in {}.", report.path);
    } else {
        println!("Marked `{id}` {state} in {}.", report.path);
    }
    if context.trust().is_some() && !report.unchanged {
        println!(
            "This vault uses trusted-commit: the change takes effect once it is committed and signed."
        );
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct TrustStatusReport {
    wiki_id: Option<String>,
    policy: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<TrustResolution>,
}

#[derive(Debug, Serialize)]
struct TrustChangeReport {
    wiki_id: String,
    policy: &'static str,
    dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    checkpoint: Option<TrustCheckpoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<ExecutionTrustConfig>,
    /// Each job's state before and after an `accept`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    before: Vec<JobTrust>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    after: Vec<JobTrust>,
}

fn trust(
    cli: &Cli,
    context: &ScheduleContext,
    command: &ScheduleTrustCommand,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), CliError> {
    let now_ms = now.timestamp_millis();
    match command {
        ScheduleTrustCommand::Status => trust_status(cli, context),
        ScheduleTrustCommand::Enable {
            allowed_signers,
            principals,
            source,
            dry_run,
        } => trust_enable(
            cli,
            context,
            allowed_signers,
            principals,
            source.as_ref(),
            *dry_run,
            now_ms,
        ),
        ScheduleTrustCommand::Disable { dry_run } => trust_disable(cli, context, *dry_run),
        ScheduleTrustCommand::Accept { commit, dry_run } => {
            trust_accept(cli, context, commit.as_deref(), *dry_run, now_ms)
        }
    }
}

fn trust_status(cli: &Cli, context: &ScheduleContext) -> Result<(), CliError> {
    let resolution = context
        .trust()
        .map(|trust| trust.resolve())
        .transpose()
        .map_err(CliError::operation)?;
    let report = TrustStatusReport {
        wiki_id: context
            .registration
            .as_ref()
            .map(|registration| registration.id.to_string()),
        policy: if resolution.is_some() {
            vulcan_app::schedule_trust::TRUSTED_COMMIT_POLICY
        } else {
            "repository-trust"
        },
        resolution,
    };
    if cli.output == OutputFormat::Json {
        return print_json(&report);
    }
    print_trust_status(&report);
    Ok(())
}

fn trust_enable(
    cli: &Cli,
    context: &ScheduleContext,
    allowed_signers: &std::path::Path,
    principals: &[String],
    source: Option<&String>,
    dry_run: bool,
    now_ms: i64,
) -> Result<(), CliError> {
    let registration = context.registered()?;
    let allowed_signers = std::path::absolute(allowed_signers).map_err(CliError::operation)?;
    let config = ExecutionTrustConfig::new(allowed_signers, principals.to_vec(), source.cloned())
        .map_err(CliError::operation)?;
    let ledger = ScheduleLedger::open(&context.state_root).map_err(CliError::operation)?;
    let checkpoint = pin_checkpoint(
        &config,
        &trust_repository(registration),
        &ledger,
        &context.vault,
        &CheckpointPin {
            commit: None,
            how: "enabled",
            now_ms,
            dry_run,
        },
    )
    .map_err(CliError::operation)?;
    context
        .registry
        .update(
            &registration.id,
            &trust_update(Some(config.clone())),
            dry_run,
        )
        .map_err(CliError::operation)?;
    let report = TrustChangeReport {
        wiki_id: registration.id.to_string(),
        policy: vulcan_app::schedule_trust::TRUSTED_COMMIT_POLICY,
        dry_run,
        checkpoint: Some(checkpoint),
        config: Some(config),
        before: Vec::new(),
        after: Vec::new(),
    };
    print_change(cli, &report)
}

fn trust_disable(cli: &Cli, context: &ScheduleContext, dry_run: bool) -> Result<(), CliError> {
    let registration = context.registered()?;
    context
        .registry
        .update(&registration.id, &trust_update(None), dry_run)
        .map_err(CliError::operation)?;
    if !dry_run {
        ScheduleLedger::open(&context.state_root)
            .and_then(|ledger| ledger.clear_trust_checkpoint(&context.vault))
            .map_err(CliError::operation)?;
    }
    print_change(
        cli,
        &TrustChangeReport {
            wiki_id: registration.id.to_string(),
            policy: "repository-trust",
            dry_run,
            checkpoint: None,
            config: None,
            before: Vec::new(),
            after: Vec::new(),
        },
    )
}

fn trust_accept(
    cli: &Cli,
    context: &ScheduleContext,
    commit: Option<&str>,
    dry_run: bool,
    now_ms: i64,
) -> Result<(), CliError> {
    let registration = context.registered()?;
    let config = registration.execution_trust.as_ref().ok_or_else(|| {
        CliError::operation(
            "this vault uses repository trust; run `vulcan schedule trust enable` first",
        )
    })?;
    let repository = trust_repository(registration);
    let ledger = ScheduleLedger::open(&context.state_root).map_err(CliError::operation)?;
    let before = resolve_trust(config, &repository, &ledger, &context.vault).jobs;
    // Preview the result against a scratch ledger holding only the
    // new checkpoint, so a dry run changes nothing.
    let scratch = tempfile::tempdir().map_err(CliError::operation)?;
    let preview = ScheduleLedger::open(scratch.path()).map_err(CliError::operation)?;
    let pin = CheckpointPin {
        commit,
        how: "accepted",
        now_ms,
        dry_run: false,
    };
    let checkpoint = pin_checkpoint(config, &repository, &preview, &context.vault, &pin)
        .map_err(CliError::operation)?;
    let after = resolve_trust(config, &repository, &preview, &context.vault).jobs;
    if !dry_run {
        ledger
            .set_trust_checkpoint(&context.vault, &checkpoint)
            .map_err(CliError::operation)?;
    }
    print_change(
        cli,
        &TrustChangeReport {
            wiki_id: registration.id.to_string(),
            policy: vulcan_app::schedule_trust::TRUSTED_COMMIT_POLICY,
            dry_run,
            checkpoint: Some(checkpoint),
            config: None,
            before,
            after,
        },
    )
}

fn trust_update(execution_trust: Option<ExecutionTrustConfig>) -> UpdateWikiRequest {
    UpdateWikiRequest {
        groups_to_add: Vec::new(),
        groups_to_remove: Vec::new(),
        permissions_profile: None,
        sync_paused: None,
        profile: None,
        execution_trust: Some(execution_trust),
    }
}

fn print_change(cli: &Cli, report: &TrustChangeReport) -> Result<(), CliError> {
    if cli.output == OutputFormat::Json {
        return print_json(report);
    }
    let verb = if report.dry_run { "Would set" } else { "Set" };
    println!("{verb} `{}` to {}.", report.wiki_id, report.policy);
    if let Some(checkpoint) = &report.checkpoint {
        println!(
            "  checkpoint: {} signed by {}",
            checkpoint.commit, checkpoint.signer
        );
    }
    for job in &report.after {
        let was = report
            .before
            .iter()
            .find(|before| before.job_id == job.job_id)
            .map_or("absent", |before| before.state.as_str());
        println!("  {}: {was} -> {}", job.job_id, job.state.as_str());
    }
    Ok(())
}

fn print_trust_status(report: &TrustStatusReport) {
    let Some(resolution) = &report.resolution else {
        println!("Policy: repository-trust (jobs run as the vault defines them, within this device's ceiling).");
        return;
    };
    println!(
        "Policy: trusted-commit at `{}`, principals {}",
        resolution.source,
        resolution.principals.join(", ")
    );
    println!("  allowed signers: {}", resolution.allowed_signers);
    if let Some(tip) = &resolution.tip {
        println!("  tip: {tip}");
    }
    match &resolution.checkpoint {
        Some(checkpoint) => println!(
            "  checkpoint: {} signed by {} ({})",
            checkpoint.commit, checkpoint.signer, checkpoint.how
        ),
        None => println!("  checkpoint: none"),
    }
    for signed in &resolution.frontier {
        println!("  signed: {} by {}", signed.commit, signed.signer);
    }
    if let Some(block) = &resolution.blocked {
        println!("  blocked ({}): {}", block.state.as_str(), block.detail);
    }
    for job in &resolution.jobs {
        println!(
            "  {}: {}{} - {}",
            job.job_id,
            job.state.as_str(),
            if job.pending {
                " (uncommitted edits pending)"
            } else {
                ""
            },
            job.detail
        );
    }
    for diagnostic in &resolution.signer_diagnostics {
        println!("  allowed signers {diagnostic}");
    }
}

fn print_job(job: &JobStatus) {
    let placement = match job.placement {
        vulcan_daemon::scheduler::JobPlacement::Runs => "runs here",
        vulcan_daemon::scheduler::JobPlacement::Disabled => "disabled",
        vulcan_daemon::scheduler::JobPlacement::NotThisDevice => "another device",
        vulcan_daemon::scheduler::JobPlacement::Blocked => "blocked",
    };
    println!(
        "{} ({}): {placement}{}",
        job.job.id,
        job.job.target.name(),
        if job.running { ", running" } else { "" }
    );
    if let Some(next) = &job.next {
        println!("  next: {}", next.local);
    }
    if let Some(error) = &job.authorization.error {
        println!("  not authorized: {error}");
    }
    if let Some(last) = &job.last_success {
        let at = chrono::DateTime::from_timestamp_millis(last.started_ms)
            .map(|instant| instant.to_rfc3339())
            .unwrap_or_default();
        println!("  last success: {at}");
    }
    if let Some(last) = &job.last_failure {
        println!(
            "  last failure: {}",
            last.outcome.as_deref().unwrap_or("no detail")
        );
    }
}
