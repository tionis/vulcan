//! `vulcan daemon plan` and `vulcan daemon apply`: make this device match
//! the declarative `daemon.toml` (`docs/specs/declarative-device-config.md`).
//! The plan and pruning live in `vulcan_daemon::declarative`; this module adds
//! the steps that need the CLI's clone transport and process helpers.

use super::daemon::{native_service_plan, spawn_detached};
use crate::{Cli, CliError, DaemonCommand, OutputFormat};
use serde::Serialize;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};
use vulcan_daemon::declarative::{
    plan_device, prune_drift, restart_pending_reasons, DevicePlan, PlanAction, PlanItem, PlanKind,
    PlanRequest, PruneOutcome, ServiceCheck,
};
use vulcan_daemon::process::{
    request_daemon_shutdown, running_daemon, DaemonProcessContext, DaemonRuntimeRecord,
};
use vulcan_daemon::registry::DaemonConfig;
use vulcan_daemon::service::{
    apply_daemon_service, inspect_daemon_service, verify_service_target, DaemonServiceAction,
    DaemonServicePlatform,
};

const READY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Serialize)]
struct ApplyStep {
    kind: PlanKind,
    id: String,
    action: PlanAction,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct ApplyReport {
    dry_run: bool,
    plan: DevicePlan,
    steps: Vec<ApplyStep>,
    pruned: Vec<PruneOutcome>,
}

pub(crate) fn run(
    cli: &Cli,
    context: &DaemonProcessContext,
    command: &DaemonCommand,
) -> Result<(), CliError> {
    match command {
        DaemonCommand::Plan {
            file,
            prune,
            no_service,
        } => run_plan(cli, context, file.as_deref(), *prune, *no_service),
        DaemonCommand::Apply {
            file,
            dry_run,
            prune,
            no_service,
            accept_remote,
        } => run_apply(
            cli,
            context,
            file.as_deref(),
            *dry_run,
            *prune,
            *no_service,
            accept_remote,
        ),
        _ => unreachable!("only plan and apply are routed here"),
    }
}

fn run_plan(
    cli: &Cli,
    context: &DaemonProcessContext,
    file: Option<&Path>,
    prune: bool,
    no_service: bool,
) -> Result<(), CliError> {
    let plan = build_plan(context, file, no_service)?;
    match cli.output {
        OutputFormat::Json => crate::output::print_json(&plan)?,
        OutputFormat::Human | OutputFormat::Markdown => print_plan(&plan, prune),
    }
    plan_exit(&plan, prune)
}

fn run_apply(
    cli: &Cli,
    context: &DaemonProcessContext,
    file: Option<&Path>,
    dry_run: bool,
    prune: bool,
    no_service: bool,
    accept_remote: &[String],
) -> Result<(), CliError> {
    let writes = !dry_run && file.is_none();
    let accepted = if writes {
        accept_remote_identities(context, accept_remote)?
    } else {
        Vec::new()
    };
    let plan = build_plan(context, file, no_service)?;
    if dry_run || file.is_some() {
        match cli.output {
            OutputFormat::Json => crate::output::print_json(&ApplyReport {
                dry_run: true,
                plan: plan.clone(),
                steps: Vec::new(),
                pruned: Vec::new(),
            })?,
            OutputFormat::Human | OutputFormat::Markdown => {
                print_plan(&plan, prune);
                if file.is_some() {
                    println!(
                        "Nothing applied: --file only checks a file; copy it to {} and run `vulcan daemon apply`.",
                        context.registry.path().display()
                    );
                }
            }
        }
        return plan_exit(&plan, prune);
    }
    if !plan.blocking_errors().is_empty() {
        match cli.output {
            OutputFormat::Json => crate::output::print_json(&ApplyReport {
                dry_run: false,
                plan,
                steps: Vec::new(),
                pruned: Vec::new(),
            })?,
            OutputFormat::Human | OutputFormat::Markdown => {
                print_plan(&plan, prune);
                println!("Nothing changed: fix the errors above first.");
            }
        }
        return Err(CliError::reported_failure());
    }
    let mut steps = accepted;
    // Storing identities is what loading the file does.
    let config = context.registry.load();
    for item in plan.items_of(PlanKind::Identity, PlanAction::Create) {
        steps.push(step(
            item,
            config.as_ref().err().map(ToString::to_string),
            None,
        ));
    }
    let config = config.map_err(CliError::operation)?;
    for item in plan.items_of(PlanKind::Vault, PlanAction::Clone) {
        let result = config
            .vaults
            .iter()
            .find(|wiki| wiki.id.as_str() == item.id)
            .ok_or_else(|| CliError::operation("vault disappeared from daemon.toml"))
            .and_then(|wiki| super::vault::clone_declared_wiki(cli, wiki));
        match result {
            Ok(value) => steps.push(step(item, None, Some(value))),
            Err(error) => steps.push(step(item, Some(error.to_string()), None)),
        }
    }
    if !no_service {
        let converged = converge_after_clones(cli, context, &plan, &steps, &config)?;
        steps.extend(converged);
    }
    let pruned = if prune {
        prune_drift(context, &plan)
    } else {
        Vec::new()
    };
    let failed = steps.iter().any(|step| !step.ok)
        || pruned.iter().any(|outcome| outcome.error.is_some())
        || plan.has_errors();
    let report = ApplyReport {
        dry_run: false,
        plan,
        steps,
        pruned,
    };
    match cli.output {
        OutputFormat::Json => crate::output::print_json(&report)?,
        OutputFormat::Human | OutputFormat::Markdown => print_apply(&report, prune),
    }
    if failed {
        Err(CliError::reported_failure())
    } else {
        Ok(())
    }
}

/// Converges the daemon. A cloned vault can bring a remote from its
/// `[mcp_remote]` that the plan, made before the clone, could not see, so
/// the plan is rebuilt first.
fn converge_after_clones(
    cli: &Cli,
    context: &DaemonProcessContext,
    plan: &DevicePlan,
    steps: &[ApplyStep],
    config: &DaemonConfig,
) -> Result<Vec<ApplyStep>, CliError> {
    let cloned = steps
        .iter()
        .any(|step| step.ok && step.action == PlanAction::Clone);
    if !cloned {
        return Ok(converge_daemon(cli, context, plan, config));
    }
    let plan = build_plan(context, None, false)?;
    let config = context.registry.load().map_err(CliError::operation)?;
    Ok(converge_daemon(cli, context, &plan, &config))
}

/// Accepts the `IndieAuth` identities named by `--accept-remote` before the
/// plan, so the plan and the restart use them.
fn accept_remote_identities(
    context: &DaemonProcessContext,
    ids: &[String],
) -> Result<Vec<ApplyStep>, CliError> {
    ids.iter()
        .map(|id| {
            let accepted = context
                .registry
                .accept_remote_identity(id)
                .map_err(CliError::operation)?;
            Ok(ApplyStep {
                kind: PlanKind::McpRemote,
                id: id.clone(),
                action: PlanAction::Update,
                ok: true,
                detail: Some(match accepted {
                    Some((previous, asked)) => {
                        format!("accepted IndieAuth identity {asked} (was {previous})")
                    }
                    None => "no identity change was waiting".to_string(),
                }),
                result: None,
            })
        })
        .collect()
}

fn build_plan(
    context: &DaemonProcessContext,
    file: Option<&Path>,
    no_service: bool,
) -> Result<DevicePlan, CliError> {
    let service = if no_service {
        ServiceCheck::Skipped
    } else if DaemonServicePlatform::native().is_err() {
        ServiceCheck::Unsupported
    } else {
        let plan = native_service_plan(context, DaemonServiceAction::Install)?;
        ServiceCheck::Inspected(inspect_daemon_service(&plan).map_err(CliError::operation)?)
    };
    let executable = std::env::current_exe().map_err(CliError::operation)?;
    Ok(plan_device(
        context,
        &PlanRequest {
            file,
            service,
            executable: &executable,
        },
    ))
}

fn plan_exit(plan: &DevicePlan, prune: bool) -> Result<(), CliError> {
    if plan.has_errors() {
        Err(CliError::reported_failure())
    } else if plan.has_changes(prune) {
        Err(CliError::changes_pending())
    } else {
        Ok(())
    }
}

fn step(item: &PlanItem, error: Option<String>, result: Option<serde_json::Value>) -> ApplyStep {
    ApplyStep {
        kind: item.kind,
        id: item.id.clone(),
        action: item.action,
        ok: error.is_none(),
        detail: error.or_else(|| item.reason.clone()),
        result,
    }
}

/// Installs or refreshes the service and makes sure the daemon runs the
/// current executable and configuration.
fn converge_daemon(
    cli: &Cli,
    context: &DaemonProcessContext,
    plan: &DevicePlan,
    config: &DaemonConfig,
) -> Vec<ApplyStep> {
    let pending = |kind| {
        plan.items
            .iter()
            .find(|item| item.kind == kind)
            .filter(|item| matches!(item.action, PlanAction::Create | PlanAction::Update))
    };
    let service = pending(PlanKind::Service);
    let process = pending(PlanKind::Daemon);
    let Some(first) = process.or(service) else {
        return Vec::new();
    };
    // Check the service before stopping anything, so a refusal leaves the
    // running daemon alone.
    let service_plan = if DaemonServicePlatform::native().is_ok() {
        match native_service_plan(context, DaemonServiceAction::Restart).and_then(|plan| {
            verify_service_target(&plan)
                .map(|()| plan)
                .map_err(CliError::operation)
        }) {
            Ok(plan) => Some(plan),
            Err(error) => {
                return [service, process]
                    .into_iter()
                    .flatten()
                    .map(|item| step(item, Some(error.to_string()), None))
                    .collect();
            }
        }
    } else {
        None
    };
    let previous = running_daemon(context);
    if let Some(previous) = &previous {
        if let Err(error) = stop_daemon(context, previous) {
            return vec![step(first, Some(error), None)];
        }
    }
    let result = if let Some(plan) = service_plan {
        apply_daemon_service(plan, false)
            .map_err(CliError::operation)
            .map(|_| ())
    } else if process.is_some() {
        spawn_detached(cli, context).map(|_| ())
    } else {
        return Vec::new();
    };
    let error = result
        .map_err(|error| error.to_string())
        .and_then(|()| wait_for_restart(context, previous.as_ref(), config))
        .err();
    [service, process]
        .into_iter()
        .flatten()
        .map(|item| step(item, error.clone(), None))
        .collect()
}

/// Asks the daemon to stop gracefully (it runs final syncs first) and
/// confirms it did. Restarting while it still runs could start a second
/// daemon next to a manually started one.
fn stop_daemon(
    context: &DaemonProcessContext,
    previous: &DaemonRuntimeRecord,
) -> Result<(), String> {
    request_daemon_shutdown(context)
        .map_err(|error| format!("could not stop the daemon: {error}"))?;
    match running_daemon(context) {
        Some(record)
            if record.pid == previous.pid
                && record.started_unix_ms == previous.started_unix_ms =>
        {
            Err(format!(
                "the daemon (pid {}) is still finishing its final syncs; run `vulcan daemon apply` again once `vulcan daemon status` shows it stopped",
                record.pid
            ))
        }
        _ => Ok(()),
    }
}

/// Waits for a new daemon that runs the current executable and
/// configuration, so apply never reports a restart that did not happen.
fn wait_for_restart(
    context: &DaemonProcessContext,
    previous: Option<&DaemonRuntimeRecord>,
    config: &DaemonConfig,
) -> Result<(), String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut reasons = vec!["no daemon answered".to_string()];
    while Instant::now() < deadline {
        if let Some(record) = running_daemon(context) {
            reasons = restart_pending_reasons(previous, &record, config);
            if reasons.is_empty() {
                return Ok(());
            }
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(format!(
        "the daemon was not running the current version within {} seconds ({}); see `vulcan daemon status`",
        READY_TIMEOUT.as_secs(),
        reasons.join("; ")
    ))
}

fn action_label(action: PlanAction) -> &'static str {
    match action {
        PlanAction::Ok => "ok",
        PlanAction::Skip => "skip",
        PlanAction::Drift => "drift",
        PlanAction::Create => "create",
        PlanAction::Update => "update",
        PlanAction::Clone => "clone",
        PlanAction::Error => "error",
    }
}

fn kind_label(kind: PlanKind) -> &'static str {
    match kind {
        PlanKind::Config => "config",
        PlanKind::Vault => "vault",
        PlanKind::McpRemote => "mcp_remote",
        PlanKind::Identity => "identity",
        PlanKind::Service => "service",
        PlanKind::Daemon => "daemon",
        PlanKind::ScanState => "scan_state",
        PlanKind::ApiToken => "api_token",
        PlanKind::OauthState => "oauth_state",
    }
}

fn print_plan(plan: &DevicePlan, prune: bool) {
    let owner = plan
        .managed_by
        .as_ref()
        .map(|owner| format!(" (managed by {owner})"))
        .unwrap_or_default();
    println!("Plan for {}{owner}", plan.file.display());
    let mut unchanged = 0;
    for item in &plan.items {
        if item.action == PlanAction::Ok && item.reason.is_none() {
            unchanged += 1;
            continue;
        }
        let action = if item.action == PlanAction::Drift && prune {
            "prune"
        } else {
            action_label(item.action)
        };
        let reason = item
            .reason
            .as_ref()
            .map(|reason| format!("  {reason}"))
            .unwrap_or_default();
        println!(
            "  {action:<7} {:<12} {}{reason}",
            kind_label(item.kind),
            item.id
        );
    }
    let count =
        |test: fn(PlanAction) -> bool| plan.items.iter().filter(|item| test(item.action)).count();
    let changes = count(|action| {
        matches!(
            action,
            PlanAction::Create | PlanAction::Update | PlanAction::Clone
        )
    });
    let drift = count(|action| action == PlanAction::Drift);
    let errors = count(|action| action == PlanAction::Error);
    let drift_note = if prune {
        format!("{drift} to prune")
    } else {
        format!("{drift} drift (removed only with --prune)")
    };
    println!("{changes} to change, {unchanged} unchanged, {drift_note}, {errors} errors");
}

fn print_apply(report: &ApplyReport, prune: bool) {
    print_plan(&report.plan, prune);
    for step in &report.steps {
        let status = if step.ok { "done" } else { "failed" };
        let detail = step
            .detail
            .as_ref()
            .map(|detail| format!(": {detail}"))
            .unwrap_or_default();
        println!(
            "{status:<7} {} {} {}{detail}",
            action_label(step.action),
            kind_label(step.kind),
            step.id
        );
    }
    for outcome in &report.pruned {
        match &outcome.error {
            None => println!("pruned  {} {}", kind_label(outcome.kind), outcome.id),
            Some(error) => println!(
                "failed  prune {} {}: {error}",
                kind_label(outcome.kind),
                outcome.id
            ),
        }
    }
    if report.steps.is_empty() && report.pruned.is_empty() {
        println!("Nothing to do.");
    }
}
