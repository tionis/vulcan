//! `vulcan schedule`: the vault's scheduled jobs (Roadmap 10.8).

use crate::cli::ScheduleCommand;
use crate::output::print_json;
use crate::{Cli, CliError, OutputFormat};
use vulcan_app::execution::Ungated;
use vulcan_app::sync::SyncCancellationToken;
use vulcan_core::schedules::{load_schedules, ScheduleJob};
use vulcan_core::VaultPaths;
use vulcan_daemon::process::DaemonProcessContext;
use vulcan_daemon::registry::WikiRegistry;
use vulcan_daemon::scheduler::{
    occurrence_times, run_manually, schedule_status, JobStatus, RunContext,
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
    })
}

fn find_job(paths: &VaultPaths, id: &str) -> Result<ScheduleJob, CliError> {
    let set = load_schedules(paths);
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
        ScheduleCommand::Next { id, count } => next(cli, paths, id, *count, now),
        ScheduleCommand::History { id, limit } => history(cli, &context, id, *limit),
        ScheduleCommand::Run { id, dry_run } => run(cli, paths, &context, id, *dry_run),
        ScheduleCommand::Enable { id, dry_run } | ScheduleCommand::Disable { id, dry_run } => {
            toggle(cli, paths, command, id, *dry_run)
        }
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
    )
    .map_err(CliError::operation)?;
    if let ScheduleCommand::Show { id } = command {
        let job = report
            .jobs
            .into_iter()
            .find(|job| job.job.id == *id)
            .ok_or_else(|| {
                find_job(paths, id)
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
    id: &str,
    count: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), CliError> {
    let job = find_job(paths, id)?;
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
    let ledger = vulcan_app::schedules::ScheduleLedger::open(&context.state_root)
        .map_err(CliError::operation)?;
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
    let job = find_job(paths, id)?;
    let cancellation = SyncCancellationToken::default();
    let report = run_manually(
        &job,
        &RunContext {
            registry: &context.registry,
            paths,
            ceiling: context.ceiling.as_deref(),
            gate: &Ungated,
            cancellation: &cancellation,
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
    Ok(())
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
