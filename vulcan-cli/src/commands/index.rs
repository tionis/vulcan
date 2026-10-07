use crate::commit::AutoCommitPolicy;
use crate::output::print_json;
use crate::{
    selected_permission_guard, selected_permission_profile, serve_forever,
    warn_auto_commit_if_needed, Cli, CliError, IndexCommand, MdbaseWriteRepairCommand,
    OrdinaryWriteRepairCommand, OutputFormat, PermissionGuard, RepairCommand, ServeOptions,
};
use serde_json::json;
use vulcan_app::scan::scan_vault_with_automation;
use vulcan_core::ordinary_write::{
    accept_current_ordinary_write_batch, inspect_ordinary_write_batch, recover_ordinary_write_batch,
};
use vulcan_core::{
    rebuild_vault_with_progress, repair_fts, scan_vault, watch_vault, PluginEvent, RebuildQuery,
    RepairFtsQuery, ScanMode, VaultPaths, WatchOptions,
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
            let report = crate::run_init_command(paths, args, &cli.vault_discovery()?)?;
            crate::print_init_summary(cli.output, paths, &report)?;
            Ok(())
        }
        IndexCommand::Scan { full, no_commit } => {
            let auto_commit = AutoCommitPolicy::for_scan(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let mut progress = (cli.output == crate::OutputFormat::Human)
                .then(|| crate::ScanProgressReporter::new(use_stderr_color));
            let summary = scan_vault_with_automation(
                paths,
                if *full {
                    ScanMode::Full
                } else {
                    ScanMode::Incremental
                },
                &auto_commit,
                cli.permissions.as_deref(),
                cli.quiet,
                |event| {
                    if let Some(progress) = progress.as_mut() {
                        progress.record(&event);
                    }
                },
            )
            .map_err(CliError::operation)?;
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
        IndexCommand::Repair { command } => handle_repair_command(cli, paths, command),
        IndexCommand::Watch {
            debounce_ms,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_scan(paths, *no_commit);
            let trigger_guard = selected_permission_guard(cli, paths)?;
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
                    let triggered_paths = crate::run_template_creation_triggers(
                        paths,
                        &report.created_paths,
                        cli.permissions.as_deref(),
                        cli.quiet,
                        &trigger_guard,
                    )?;
                    if !triggered_paths.is_empty() {
                        scan_vault(paths, ScanMode::Incremental).map_err(CliError::operation)?;
                    }
                    crate::print_watch_report(cli.output, &report)?;
                    if !report.startup
                        && report.summary.added + report.summary.updated + report.summary.deleted
                            > 0
                    {
                        let mut changed_paths = report.paths.clone();
                        changed_paths.extend(triggered_paths);
                        changed_paths.sort();
                        changed_paths.dedup();
                        auto_commit
                            .commit(
                                paths,
                                "scan",
                                &changed_paths,
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

pub(crate) fn handle_repair_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &RepairCommand,
) -> Result<(), CliError> {
    selected_permission_guard(cli, paths)?
        .check_index()
        .map_err(CliError::operation)?;
    match command {
        RepairCommand::Fts { dry_run } => {
            let report = repair_fts(paths, &RepairFtsQuery { dry_run: *dry_run })
                .map_err(CliError::operation)?;
            crate::print_repair_fts_report(cli.output, &report)
        }
        RepairCommand::MdbaseWrite { command } => {
            handle_mdbase_write_repair(cli.output, paths, cli.permissions.as_deref(), command)
        }
        RepairCommand::OrdinaryWrite { command } => {
            let grant = selected_permission_profile(cli, paths)?.grant;
            if !grant.read.is_unrestricted() || !grant.write.is_unrestricted() {
                return Err(CliError::operation(
                    "ordinary write recovery requires full-vault read and write authority",
                ));
            }
            handle_ordinary_write_repair(cli.output, paths, command)
        }
    }
}

fn handle_mdbase_write_repair(
    output: OutputFormat,
    paths: &VaultPaths,
    profile: Option<&str>,
    command: &MdbaseWriteRepairCommand,
) -> Result<(), CliError> {
    use vulcan_app::mdbase::{
        accept_current_mdbase_write, build_mdbase_write_repair_status, recover_mdbase_write,
    };
    let print_review = |review: &vulcan_core::mdbase::MdbaseWriteReview| {
        println!(
            "Pending mdbase {}: {}",
            review.operation, review.transaction_id
        );
        println!("Recovery: {}", review.recovery);
        println!("Recoverable: {}", review.recoverable);
        println!("Review token: {}", review.review_token);
        for change in &review.changes {
            println!("  {}: {}", change.path, change.state);
        }
    };
    match command {
        MdbaseWriteRepairCommand::Status => {
            let status =
                build_mdbase_write_repair_status(paths, profile).map_err(CliError::operation)?;
            match output {
                OutputFormat::Json => print_json(&status)?,
                OutputFormat::Human | OutputFormat::Markdown => match status.review.as_ref() {
                    Some(review) => print_review(review),
                    None => println!("No pending mdbase write"),
                },
            }
            Ok(())
        }
        MdbaseWriteRepairCommand::Recover { dry_run } => {
            let report =
                recover_mdbase_write(paths, profile, *dry_run).map_err(CliError::operation)?;
            match output {
                OutputFormat::Json => print_json(&report)?,
                OutputFormat::Human | OutputFormat::Markdown => match report.review.as_ref() {
                    None => println!("No pending mdbase write"),
                    Some(review) if *dry_run => print_review(review),
                    Some(review) => println!(
                        "Recovered mdbase {} {} ({}); derived state refreshed",
                        review.operation, review.transaction_id, review.recovery
                    ),
                },
            }
            Ok(())
        }
        MdbaseWriteRepairCommand::AcceptCurrent {
            transaction_id,
            review_token,
            confirm,
            dry_run,
        } => {
            if !confirm {
                return Err(CliError::operation(
                    "accept-current requires --confirm after reviewing every affected file",
                ));
            }
            let report =
                accept_current_mdbase_write(paths, profile, transaction_id, review_token, *dry_run)
                    .map_err(CliError::operation)?;
            match output {
                OutputFormat::Json => print_json(&report)?,
                OutputFormat::Human | OutputFormat::Markdown => {
                    if *dry_run {
                        println!("Current files reviewed; transaction remains pending");
                    } else {
                        println!(
                            "Current files accepted; journal retired to {}",
                            report.accepted.retired_journal.as_deref().unwrap_or("?")
                        );
                    }
                }
            }
            Ok(())
        }
    }
}

fn handle_ordinary_write_repair(
    output: OutputFormat,
    paths: &VaultPaths,
    command: &OrdinaryWriteRepairCommand,
) -> Result<(), CliError> {
    match command {
        OrdinaryWriteRepairCommand::Status => {
            let review = inspect_ordinary_write_batch(paths).map_err(CliError::operation)?;
            match output {
                OutputFormat::Json => {
                    print_json(&json!({"pending": review.is_some(), "review": review}))?;
                }
                OutputFormat::Human | OutputFormat::Markdown => {
                    if let Some(review) = review {
                        println!("Pending ordinary write: {}", review.transaction_id);
                        println!("Recoverable: {}", review.recoverable);
                        println!("Review token: {}", review.review_token);
                        for change in review.changes {
                            println!("  {}: {}", change.path, change.state);
                        }
                    } else {
                        println!("No pending ordinary write journal");
                    }
                }
            }
            Ok(())
        }
        OrdinaryWriteRepairCommand::RollForward { dry_run } => {
            if *dry_run {
                let review = inspect_ordinary_write_batch(paths).map_err(CliError::operation)?;
                match output {
                    OutputFormat::Json => print_json(&json!({"dry_run": true, "review": review}))?,
                    OutputFormat::Human | OutputFormat::Markdown => {
                        println!(
                            "{}",
                            if review.is_some() {
                                "Pending batch inspected; no files changed"
                            } else {
                                "No pending ordinary write journal"
                            }
                        );
                    }
                }
                return Ok(());
            }
            let recovered = recover_ordinary_write_batch(paths).map_err(CliError::operation)?;
            let scan = recovered
                .as_ref()
                .map(|_| scan_vault(paths, ScanMode::Incremental))
                .transpose()
                .map_err(CliError::operation)?;
            match output {
                OutputFormat::Json => print_json(&json!({"recovered": recovered, "scan": scan}))?,
                OutputFormat::Human | OutputFormat::Markdown => {
                    println!(
                        "{}",
                        if recovered.is_some() {
                            "Ordinary write recovered and vault index refreshed"
                        } else {
                            "No pending ordinary write journal"
                        }
                    );
                }
            }
            Ok(())
        }
        OrdinaryWriteRepairCommand::AcceptCurrent {
            transaction_id,
            review_token,
            confirm,
            dry_run,
        } => {
            if !confirm {
                return Err(CliError::operation(
                    "accept-current requires --confirm after reviewing every affected file",
                ));
            }
            let accepted =
                accept_current_ordinary_write_batch(paths, transaction_id, review_token, *dry_run)
                    .map_err(CliError::operation)?;
            let scan = (!dry_run)
                .then(|| scan_vault(paths, ScanMode::Incremental))
                .transpose()
                .map_err(CliError::operation)?;
            match output {
                OutputFormat::Json => print_json(&json!({"accepted": accepted, "scan": scan}))?,
                OutputFormat::Human | OutputFormat::Markdown => {
                    println!(
                        "{}",
                        if *dry_run {
                            "Current files reviewed; journal remains pending"
                        } else {
                            "Current files accepted; journal retired and vault index refreshed"
                        }
                    );
                }
            }
            Ok(())
        }
    }
}
