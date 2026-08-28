use crate::output::print_json;
use crate::{
    selected_permission_guard, Cli, CliError, OutputFormat, SyncCommand, SyncSelectionArgs,
};
use vulcan_app::sync::{
    sync_git_vault, GitRefName, GitRemote, GitSyncAction, GitSyncOptions, VaultSyncReport,
};
use vulcan_core::{PermissionGuard, VaultPaths};
use vulcan_daemon::registry::{WikiId, WikiRegistry};
use vulcan_daemon::sync::{sync_registered_wikis, RegisteredSyncReport, RegisteredSyncSelection};

pub(crate) fn handle_sync_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &SyncCommand,
) -> Result<(), CliError> {
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
            Ok(())
        }
    }
}
