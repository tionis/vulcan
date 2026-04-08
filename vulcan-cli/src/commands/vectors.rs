#![allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::fn_params_excessive_bools
)]

use crate::output::ListOutputControls;
use crate::resolve::resolve_note_argument;
use crate::{
    selected_read_permission_filter, Cli, CliError, OutputFormat, VectorQueueCommand,
    VectorsCommand,
};
use vulcan_core::{
    cluster_vectors_with_filter, drop_vector_model, index_vectors_with_progress,
    inspect_vector_queue, list_vector_models, query_related_notes_with_filter,
    query_vector_neighbors_with_filter, rebuild_vectors_with_progress,
    repair_vectors_with_progress, vector_duplicates_with_progress, ClusterQuery, RelatedNotesQuery,
    VaultPaths, VectorDuplicatesQuery, VectorIndexQuery, VectorNeighborsQuery, VectorRebuildQuery,
    VectorRepairQuery,
};

pub(crate) fn handle_cluster_command(
    cli: &Cli,
    paths: &VaultPaths,
    clusters: usize,
    dry_run: bool,
    export: &crate::ExportArgs,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    let read_filter = selected_read_permission_filter(cli, paths)?;
    let report = cluster_vectors_with_filter(
        paths,
        &ClusterQuery {
            provider: cli.provider.clone(),
            clusters,
            dry_run,
        },
        read_filter.as_ref(),
    )
    .map_err(CliError::operation)?;
    let export = crate::resolve_cli_export(export)?;
    crate::print_cluster_report(
        cli.output,
        &report,
        list_controls,
        stdout_is_tty,
        use_stdout_color,
        export.as_ref(),
    )?;
    Ok(())
}

pub(crate) fn handle_related_command(
    cli: &Cli,
    paths: &VaultPaths,
    note: Option<&str>,
    export: &crate::ExportArgs,
    interactive_note_selection: bool,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    let note = resolve_note_argument(paths, note, interactive_note_selection, "note")?;
    let read_filter = selected_read_permission_filter(cli, paths)?;
    let report = query_related_notes_with_filter(
        paths,
        &RelatedNotesQuery {
            provider: cli.provider.clone(),
            note,
            limit: cli.limit.unwrap_or(10).saturating_add(cli.offset),
        },
        read_filter.as_ref(),
    )
    .map_err(CliError::operation)?;
    let export = crate::resolve_cli_export(export)?;
    crate::print_related_notes_report(
        cli.output,
        &report,
        list_controls,
        stdout_is_tty,
        use_stdout_color,
        export.as_ref(),
    )?;
    Ok(())
}

pub(crate) fn handle_vectors_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &VectorsCommand,
    interactive_note_selection: bool,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
    use_stderr_color: bool,
) -> Result<(), CliError> {
    match command {
        VectorsCommand::Index { dry_run } => {
            let verbose = cli.verbose;
            let mut progress = (cli.output == OutputFormat::Human)
                .then(|| crate::VectorIndexProgressReporter::new(use_stderr_color, verbose));
            let report = index_vectors_with_progress(
                paths,
                &VectorIndexQuery {
                    provider: cli.provider.clone(),
                    dry_run: *dry_run,
                    verbose,
                },
                |event| {
                    if let Some(progress) = progress.as_mut() {
                        progress.record(&event);
                    }
                },
            )
            .map_err(CliError::operation)?;
            crate::print_vector_index_report(cli.output, &report, use_stdout_color)?;
            Ok(())
        }
        VectorsCommand::Repair { dry_run } => {
            let mut progress = (cli.output == OutputFormat::Human)
                .then(|| crate::VectorIndexProgressReporter::new(use_stderr_color, false));
            let report = repair_vectors_with_progress(
                paths,
                &VectorRepairQuery {
                    provider: cli.provider.clone(),
                    dry_run: *dry_run,
                },
                |event| {
                    if let Some(progress) = progress.as_mut() {
                        progress.record(&event);
                    }
                },
            )
            .map_err(CliError::operation)?;
            crate::print_vector_repair_report(cli.output, &report, use_stdout_color)
        }
        VectorsCommand::Rebuild { dry_run } => {
            let mut progress = (cli.output == OutputFormat::Human)
                .then(|| crate::VectorIndexProgressReporter::new(use_stderr_color, false));
            let report = rebuild_vectors_with_progress(
                paths,
                &VectorRebuildQuery {
                    provider: cli.provider.clone(),
                    dry_run: *dry_run,
                },
                |event| {
                    if let Some(progress) = progress.as_mut() {
                        progress.record(&event);
                    }
                },
            )
            .map_err(CliError::operation)?;
            crate::print_vector_index_report(cli.output, &report, use_stdout_color)?;
            Ok(())
        }
        VectorsCommand::Queue { command } => match command {
            VectorQueueCommand::Status => {
                let report = inspect_vector_queue(paths, cli.provider.as_deref())
                    .map_err(CliError::operation)?;
                crate::print_vector_queue_report(cli.output, &report)
            }
            VectorQueueCommand::Run { dry_run } => {
                let verbose = cli.verbose;
                let mut progress = (cli.output == OutputFormat::Human)
                    .then(|| crate::VectorIndexProgressReporter::new(use_stderr_color, verbose));
                let report = index_vectors_with_progress(
                    paths,
                    &VectorIndexQuery {
                        provider: cli.provider.clone(),
                        dry_run: *dry_run,
                        verbose,
                    },
                    |event| {
                        if let Some(progress) = progress.as_mut() {
                            progress.record(&event);
                        }
                    },
                )
                .map_err(CliError::operation)?;
                crate::print_vector_index_report(cli.output, &report, use_stdout_color)?;
                Ok(())
            }
        },
        VectorsCommand::Neighbors {
            query,
            note,
            export,
        } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let resolved_note = if note.is_some() || query.is_none() {
                Some(resolve_note_argument(
                    paths,
                    note.as_deref(),
                    interactive_note_selection && query.is_none(),
                    "note",
                )?)
            } else {
                None
            };
            let report = query_vector_neighbors_with_filter(
                paths,
                &VectorNeighborsQuery {
                    provider: cli.provider.clone(),
                    text: query.clone(),
                    note: resolved_note,
                    limit: cli.limit.unwrap_or(10).saturating_add(cli.offset),
                },
                read_filter.as_ref(),
            )
            .map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            crate::print_vector_neighbors_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )?;
            Ok(())
        }
        VectorsCommand::Cluster {
            clusters,
            dry_run,
            export,
        } => handle_cluster_command(
            cli,
            paths,
            *clusters,
            *dry_run,
            export,
            list_controls,
            stdout_is_tty,
            use_stdout_color,
        ),
        VectorsCommand::Related { note, export } => handle_related_command(
            cli,
            paths,
            note.as_deref(),
            export,
            interactive_note_selection,
            list_controls,
            stdout_is_tty,
            use_stdout_color,
        ),
        VectorsCommand::Duplicates {
            threshold,
            limit,
            export,
        } => {
            let effective_limit = (*limit).max(1);
            let is_tty = stdout_is_tty;
            let report = vector_duplicates_with_progress(
                paths,
                &VectorDuplicatesQuery {
                    provider: cli.provider.clone(),
                    threshold: *threshold,
                    limit: effective_limit,
                },
                |completed, total| {
                    if is_tty && total > 0 {
                        let pct = completed * 100 / total;
                        eprint!("\rScanning vectors: {completed}/{total} ({pct}%)   ");
                    }
                },
            )
            .map_err(CliError::operation)?;
            if is_tty {
                eprintln!(); // clear progress line
            }
            let export = crate::resolve_cli_export(export)?;
            crate::print_vector_duplicates_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )?;
            Ok(())
        }
        VectorsCommand::Models { export } => {
            let models = list_vector_models(paths).map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            crate::print_vector_models_report(
                cli.output,
                &models,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )?;
            Ok(())
        }
        VectorsCommand::DropModel { key } => {
            let dropped = drop_vector_model(paths, key).map_err(CliError::operation)?;
            if dropped {
                if cli.output == OutputFormat::Json {
                    println!("{}", serde_json::json!({"dropped": true, "cache_key": key}));
                } else {
                    eprintln!("Dropped model: {key}");
                }
            } else if cli.output == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::json!({"dropped": false, "cache_key": key})
                );
            } else {
                eprintln!("No model found with cache key: {key}");
            }
            Ok(())
        }
    }
}
