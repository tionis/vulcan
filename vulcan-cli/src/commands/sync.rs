use crate::output::print_json;
use crate::{selected_permission_guard, Cli, CliError, OutputFormat, SyncCommand};
use vulcan_app::sync::{
    sync_git_vault, GitRefName, GitRemote, GitSyncAction, GitSyncOptions, VaultSyncReport,
};
use vulcan_core::{PermissionGuard, VaultPaths};

pub(crate) fn handle_sync_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &SyncCommand,
) -> Result<(), CliError> {
    selected_permission_guard(cli, paths)?
        .check_git()
        .map_err(CliError::operation)?;
    let options = match command {
        SyncCommand::Run {
            target,
            max_retries,
            dry_run,
        } => GitSyncOptions {
            remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
            live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
            max_retries: *max_retries,
            dry_run: *dry_run,
        },
        SyncCommand::Status { target } => GitSyncOptions {
            remote: GitRemote::parse(&target.remote).map_err(CliError::operation)?,
            live_ref: GitRefName::parse(&target.live_ref).map_err(CliError::operation)?,
            dry_run: true,
            ..GitSyncOptions::default()
        },
    };
    let report = sync_git_vault(paths, &options).map_err(CliError::operation)?;
    print_sync_report(cli.output, &report)
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
