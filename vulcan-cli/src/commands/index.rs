use crate::commit::AutoCommitPolicy;
use crate::{
    selected_permission_guard, serve_forever, warn_auto_commit_if_needed, Cli, CliError,
    IndexCommand, PermissionGuard, RepairCommand, ServeOptions,
};
use vulcan_core::{
    rebuild_vault_with_progress, repair_fts, scan_vault_with_progress, watch_vault, PluginEvent,
    RebuildQuery, RepairFtsQuery, ScanMode, VaultPaths, WatchOptions,
};

#[allow(clippy::too_many_lines)]
pub(crate) fn handle_index_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &IndexCommand,
    stdout_is_tty: bool,
    use_stderr_color: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    selected_permission_guard(cli, paths)?
        .check_index()
        .map_err(CliError::operation)?;
    match command {
        IndexCommand::Init(args) => {
            let report = crate::run_init_command(paths, args)?;
            crate::print_init_summary(cli.output, paths, &report)?;
            Ok(())
        }
        IndexCommand::Scan { full, no_commit } => {
            let auto_commit = AutoCommitPolicy::for_scan(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let mut progress = (cli.output == crate::OutputFormat::Human)
                .then(|| crate::ScanProgressReporter::new(use_stderr_color));
            let summary = scan_vault_with_progress(
                paths,
                if *full {
                    ScanMode::Full
                } else {
                    ScanMode::Incremental
                },
                |event| {
                    if let Some(progress) = progress.as_mut() {
                        progress.record(&event);
                    }
                },
            )
            .map_err(CliError::operation)?;
            if summary.added + summary.updated + summary.deleted > 0 {
                auto_commit
                    .commit(paths, "scan", &[], cli.permissions.as_deref(), cli.quiet)
                    .map_err(CliError::operation)?;
            }
            let _ = crate::plugins::dispatch_plugin_event(
                paths,
                cli.permissions.as_deref(),
                PluginEvent::OnScanComplete,
                &serde_json::json!({
                    "kind": PluginEvent::OnScanComplete,
                    "mode": if *full { "full" } else { "incremental" },
                    "summary": &summary,
                }),
                cli.quiet,
            );
            crate::print_scan_summary(cli.output, &summary, use_stdout_color);
            Ok(())
        }
        IndexCommand::Rebuild { dry_run } => {
            let mut progress = (cli.output == crate::OutputFormat::Human)
                .then(|| crate::ScanProgressReporter::new(use_stderr_color));
            let report =
                rebuild_vault_with_progress(paths, &RebuildQuery { dry_run: *dry_run }, |event| {
                    if let Some(progress) = progress.as_mut() {
                        progress.record(&event);
                    }
                })
                .map_err(CliError::operation)?;
            crate::print_rebuild_report(cli.output, &report, use_stdout_color)
        }
        IndexCommand::Repair { command } => match command {
            RepairCommand::Fts { dry_run } => {
                let report = repair_fts(paths, &RepairFtsQuery { dry_run: *dry_run })
                    .map_err(CliError::operation)?;
                crate::print_repair_fts_report(cli.output, &report)
            }
        },
        IndexCommand::Watch {
            debounce_ms,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_scan(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            if cli.output == crate::OutputFormat::Human && stdout_is_tty {
                println!(
                    "Watching {} (debounce {}ms)",
                    paths.vault_root().display(),
                    debounce_ms
                );
            }
            watch_vault(
                paths,
                &WatchOptions {
                    debounce_ms: *debounce_ms,
                },
                |report| {
                    crate::print_watch_report(cli.output, &report)?;
                    if !report.startup
                        && report.summary.added + report.summary.updated + report.summary.deleted
                            > 0
                    {
                        auto_commit
                            .commit(
                                paths,
                                "scan",
                                &report.paths,
                                cli.permissions.as_deref(),
                                cli.quiet,
                            )
                            .map_err(CliError::operation)?;
                    }
                    let _ = crate::plugins::dispatch_plugin_event(
                        paths,
                        cli.permissions.as_deref(),
                        PluginEvent::OnScanComplete,
                        &serde_json::json!({
                            "kind": PluginEvent::OnScanComplete,
                            "mode": "watch",
                            "summary": &report.summary,
                            "paths": &report.paths,
                        }),
                        cli.quiet,
                    );
                    Ok::<(), CliError>(())
                },
            )
            .map_err(CliError::operation)
        }
        IndexCommand::Serve {
            bind,
            no_watch,
            debounce_ms,
            auth_token,
        } => serve_forever(
            paths,
            &ServeOptions {
                bind: bind.clone(),
                watch: !no_watch,
                debounce_ms: *debounce_ms,
                auth_token: auth_token.clone(),
                permissions: cli.permissions.clone(),
            },
        ),
    }
}
