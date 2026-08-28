use crate::output::print_json;
use crate::{
    selected_permission_guard, Cli, CliError, OutputFormat, SyncCommand, SyncSelectionArgs,
};
use serde::Serialize;
use vulcan_app::sync::{
    doctor_git_vault, sync_git_vault, GitRefName, GitRemote, GitSyncAction, GitSyncOptions,
    SyncDoctorReport, SyncDoctorSeverity, VaultSyncReport,
};
use vulcan_core::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, VaultPaths,
};
use vulcan_daemon::registry::{UpdateWikiRequest, WikiId, WikiRegistration, WikiRegistry};
use vulcan_daemon::sync::{sync_registered_wikis, RegisteredSyncReport, RegisteredSyncSelection};

pub(crate) fn handle_sync_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &SyncCommand,
) -> Result<(), CliError> {
    match command {
        SyncCommand::Pause { wiki, dry_run } => {
            return set_automatic_sync(cli.output, paths, wiki.as_deref(), true, *dry_run);
        }
        SyncCommand::Resume { wiki, dry_run } => {
            return set_automatic_sync(cli.output, paths, wiki.as_deref(), false, *dry_run);
        }
        SyncCommand::Doctor { wiki, target } => {
            return run_sync_doctor(cli, paths, wiki.as_deref(), target);
        }
        SyncCommand::Run { .. } | SyncCommand::Status { .. } => {}
    }
    let (options, selection) = match command {
        SyncCommand::Run {
            selection,
            target,
            max_retries,
            dry_run,
        } => (
            GitSyncOptions {
                remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
                live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
                max_retries: *max_retries,
                dry_run: *dry_run,
            },
            registered_selection(selection)?,
        ),
        SyncCommand::Status { selection, target } => (
            GitSyncOptions {
                remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
                live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
                dry_run: true,
                ..GitSyncOptions::default()
            },
            registered_selection(selection)?,
        ),
        SyncCommand::Doctor { .. } | SyncCommand::Pause { .. } | SyncCommand::Resume { .. } => {
            unreachable!()
        }
    };
    if let Some(selection) = selection {
        let registry = WikiRegistry::user_default().map_err(CliError::operation)?;
        let report =
            sync_registered_wikis(&registry, &selection, &options, cli.permissions.as_deref())
                .map_err(CliError::operation)?;
        print_registered_sync_report(cli.output, &report)?;
        if report.failed > 0 || report.conflicted > 0 {
            return Err(CliError::issues(format!(
                "{} registered sync operation(s) failed and {} remain conflicted",
                report.failed, report.conflicted
            )));
        }
        return Ok(());
    }
    selected_permission_guard(cli, paths)?
        .check_git()
        .map_err(CliError::operation)?;
    let report = sync_git_vault(paths, &options).map_err(CliError::operation)?;
    print_sync_report(cli.output, &report)
}

fn run_sync_doctor(
    cli: &Cli,
    selected_paths: &VaultPaths,
    wiki: Option<&str>,
    target: &crate::SyncTargetArgs,
) -> Result<(), CliError> {
    let (paths, registration_profile) = if let Some(wiki) = wiki {
        let id = WikiId::parse(wiki).map_err(CliError::operation)?;
        let status = WikiRegistry::user_default()
            .map_err(CliError::operation)?
            .show(&id)
            .map_err(CliError::operation)?;
        if status
            .registration
            .sync_backend
            .as_deref()
            .is_some_and(|backend| backend != "git")
        {
            return Err(CliError::operation(format!(
                "wiki `{id}` uses unsupported sync backend `{}`",
                status
                    .registration
                    .sync_backend
                    .as_deref()
                    .unwrap_or_default()
            )));
        }
        (
            VaultPaths::new(status.registration.path),
            status.registration.permissions_profile,
        )
    } else {
        (selected_paths.clone(), None)
    };
    let profile = cli
        .permissions
        .as_deref()
        .or(registration_profile.as_deref());
    let selection = resolve_permission_profile(&paths, profile).map_err(CliError::operation)?;
    ProfilePermissionGuard::new(&paths, selection)
        .check_git()
        .map_err(CliError::operation)?;
    let options = GitSyncOptions {
        remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
        live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
        dry_run: true,
        ..GitSyncOptions::default()
    };
    let report = doctor_git_vault(&paths, &options);
    print_sync_doctor_report(cli.output, &report)
}

fn print_sync_doctor_report(
    output: OutputFormat,
    report: &SyncDoctorReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Sync doctor: {} ({})",
        report.vault.display(),
        if report.healthy {
            "no errors"
        } else {
            "errors found"
        }
    );
    for check in &report.checks {
        let severity = match check.severity {
            SyncDoctorSeverity::Pass => "pass",
            SyncDoctorSeverity::Info => "info",
            SyncDoctorSeverity::Warning => "warning",
            SyncDoctorSeverity::Error => "error",
        };
        println!("{severity}\t{}\t{}", check.code, check.message);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AutomaticSyncReport {
    action: &'static str,
    dry_run: bool,
    wiki: WikiRegistration,
}

fn set_automatic_sync(
    output: OutputFormat,
    paths: &VaultPaths,
    wiki: Option<&str>,
    paused: bool,
    dry_run: bool,
) -> Result<(), CliError> {
    let registry = WikiRegistry::user_default().map_err(CliError::operation)?;
    let id = match wiki {
        Some(wiki) => WikiId::parse(wiki).map_err(CliError::operation)?,
        None => {
            registry
                .find_by_path(paths.vault_root())
                .map_err(CliError::operation)?
                .id
        }
    };
    let wiki = registry
        .update(
            &id,
            &UpdateWikiRequest {
                groups_to_add: Vec::new(),
                groups_to_remove: Vec::new(),
                permissions_profile: None,
                sync_paused: Some(paused),
            },
            dry_run,
        )
        .map_err(CliError::operation)?;
    let report = AutomaticSyncReport {
        action: if paused { "pause" } else { "resume" },
        dry_run,
        wiki,
    };
    if output == OutputFormat::Json {
        print_json(&report)
    } else {
        println!(
            "Automatic sync {} for wiki `{}`{}.",
            if paused { "paused" } else { "resumed" },
            report.wiki.id,
            if dry_run { " (dry run)" } else { "" }
        );
        Ok(())
    }
}

fn registered_selection(
    selection: &SyncSelectionArgs,
) -> Result<Option<RegisteredSyncSelection>, CliError> {
    if selection.all {
        Ok(Some(RegisteredSyncSelection::All))
    } else if let Some(group) = &selection.group {
        WikiId::parse(group).map_err(CliError::operation)?;
        Ok(Some(RegisteredSyncSelection::Group(group.clone())))
    } else {
        selection
            .wiki
            .as_deref()
            .map(WikiId::parse)
            .transpose()
            .map(|id| id.map(RegisteredSyncSelection::Wiki))
            .map_err(CliError::operation)
    }
}

fn print_registered_sync_report(
    output: OutputFormat,
    report: &RegisteredSyncReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    println!(
        "Registered sync {}: {} succeeded, {} conflicted, {} failed",
        report.selection, report.succeeded, report.conflicted, report.failed
    );
    for item in &report.items {
        if let Some(sync) = &item.report {
            println!(
                "{}\t{:?}\t{}",
                item.wiki_id,
                sync.sync.outcome,
                item.path.display()
            );
        } else if let Some(error) = &item.error {
            println!("{}\terror\t{}: {error}", item.wiki_id, item.path.display());
        }
    }
    Ok(())
}

fn print_sync_report(output: OutputFormat, report: &VaultSyncReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Sync {:?}: {} -> {}",
                report.sync.outcome, report.sync.remote, report.sync.refs.live
            );
            if let Some(accepted) = &report.sync.accepted {
                println!("Accepted: {accepted}");
            }
            if report
                .sync
                .actions
                .contains(&GitSyncAction::WorktreeApplied)
            {
                println!("Applied the accepted tree to the vault.");
            }
            if let Some(conflict) = &report.sync.conflict {
                println!(
                    "Conflict: local {} vs remote {}",
                    conflict.local, conflict.remote
                );
                if !conflict.diagnostics.is_empty() {
                    println!("{}", conflict.diagnostics);
                }
            }
            if let Some(refresh) = &report.cache_refresh {
                println!(
                    "Cache refreshed: {} added, {} updated, {} deleted",
                    refresh.added, refresh.updated, refresh.deleted
                );
            }
            if let Some(recovered) = &report.state.recovered_from {
                println!(
                    "Recovery: recaptured after interrupted transaction {} in {:?} state.",
                    recovered.transaction_id, recovered.phase
                );
            }
            if let Some(retained) = &report.state.retained {
                println!(
                    "Retained state: transaction {} is {:?} at {}.",
                    retained.transaction_id,
                    retained.phase,
                    report.state.journal_path.display()
                );
            }
            Ok(())
        }
    }
}
