#![allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::fn_params_excessive_bools
)]

use std::time::Instant;

use crate::output::{
    paginated_items, print_json, print_json_lines, print_selected_human_fields, ListOutputControls,
};
use crate::resolve::resolve_note_argument;
use crate::{
    count_as_f64, export_rows, format_eta, selected_read_permission_filter, AnsiPalette, Cli,
    CliError, OutputFormat, ResolvedExport, VectorQueueCommand, VectorsCommand,
};
use serde_json::Value;
use vulcan_core::{
    cluster_vectors_with_filter, drop_vector_model, index_vectors_with_progress,
    inspect_vector_queue, list_vector_models, query_related_notes_with_filter,
    query_vector_neighbors_with_filter, rebuild_vectors_with_progress,
    repair_vectors_with_progress, vector_duplicates_with_progress, ClusterQuery, ClusterReport,
    RelatedNoteHit, RelatedNotesQuery, RelatedNotesReport, StoredModelInfo, VaultPaths,
    VectorDuplicatePair, VectorDuplicatesQuery, VectorDuplicatesReport, VectorIndexPhase,
    VectorIndexProgress, VectorIndexQuery, VectorIndexReport, VectorNeighborHit,
    VectorNeighborsQuery, VectorNeighborsReport, VectorQueueReport, VectorRebuildQuery,
    VectorRepairQuery, VectorRepairReport,
};

struct VectorIndexProgressReporter {
    palette: AnsiPalette,
    started_at: Instant,
    last_batches_completed: usize,
    prepared: bool,
    verbose: bool,
}

impl VectorIndexProgressReporter {
    fn new(use_color: bool, verbose: bool) -> Self {
        Self {
            palette: AnsiPalette::new(use_color),
            started_at: Instant::now(),
            last_batches_completed: 0,
            prepared: false,
            verbose,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn record(&mut self, progress: &VectorIndexProgress) {
        match progress.phase {
            VectorIndexPhase::Preparing => {
                if self.prepared {
                    return;
                }
                self.prepared = true;
                if self.verbose {
                    eprintln!(
                        "{} {}:{}",
                        self.palette.dim("Provider:"),
                        progress.provider_name,
                        progress.model_name
                    );
                    eprintln!(
                        "{} {}",
                        self.palette.dim("Endpoint:"),
                        redacted_endpoint_for_display(&progress.endpoint_url)
                    );
                    eprint!("{} ", self.palette.dim("API key status:"));
                    if progress.api_key_set {
                        eprintln!("configured");
                    } else if progress.api_key_env.is_some() {
                        eprintln!("{}", self.palette.red("missing"));
                    } else {
                        eprintln!("{}", self.palette.red("not configured"));
                    }
                }
                if progress.pending == 0 {
                    eprintln!(
                        "{} for {}:{} {}",
                        self.palette.cyan("Vector index is up to date"),
                        progress.provider_name,
                        progress.model_name,
                        self.palette.dim(&format!(
                            "(batch size {}, concurrency {}, {} skipped)",
                            progress.batch_size, progress.max_concurrency, progress.skipped
                        ))
                    );
                } else {
                    eprintln!(
                        "{} {} vector chunks with {}:{} {}",
                        self.palette.cyan("Indexing"),
                        self.palette.bold(&progress.pending.to_string()),
                        progress.provider_name,
                        progress.model_name,
                        self.palette.dim(&format!(
                            "(batch size {}, concurrency {}, {} batches)",
                            progress.batch_size, progress.max_concurrency, progress.total_batches
                        ))
                    );
                }
            }
            VectorIndexPhase::Embedding => {
                if progress.batches_completed > self.last_batches_completed {
                    let elapsed = self.started_at.elapsed();
                    let rate =
                        count_as_f64(progress.processed) / elapsed.as_secs_f64().max(f64::EPSILON);
                    let remaining = progress.pending.saturating_sub(progress.processed);
                    eprintln!(
                        "{} {}/{}: {} indexed, {} failed, {} remaining | {} | {}",
                        self.palette.cyan("Completed batch"),
                        self.palette.bold(&progress.batches_completed.to_string()),
                        progress.total_batches.max(1),
                        self.palette.green(&progress.indexed.to_string()),
                        self.palette.red(&progress.failed.to_string()),
                        remaining,
                        self.palette.dim(&format!("{rate:.1} chunks/s")),
                        self.palette
                            .dim(&format!("ETA {}", format_eta(remaining, rate)))
                    );
                    self.last_batches_completed = progress.batches_completed;
                }
                if !progress.batch_failures.is_empty() {
                    let deduped = dedup_failure_messages(&progress.batch_failures);
                    if self.verbose {
                        for (message, count) in &deduped {
                            eprintln!(
                                "  {} {} {}",
                                self.palette.red("FAIL"),
                                message,
                                self.palette.dim(&format!("({count} chunks)"))
                            );
                        }
                    } else if progress.batches_completed == 1 {
                        let (message, count) = &deduped[0];
                        eprintln!(
                            "  {} {} {}",
                            self.palette.red("FAIL"),
                            message,
                            self.palette.dim(&format!(
                                "({count} chunks, use --verbose to see all failures)"
                            ))
                        );
                    }
                }
            }
            VectorIndexPhase::Completed => {
                if progress.dry_run {
                    eprintln!(
                        "{} {} vector chunks across {} batches",
                        self.palette.cyan("Dry run planned"),
                        self.palette.bold(&progress.pending.to_string()),
                        progress.total_batches
                    );
                } else if !self.prepared {
                    eprintln!(
                        "{} {} indexed, {} failed, {} skipped {}",
                        self.palette.cyan("Vector index complete"),
                        self.palette.green(&progress.indexed.to_string()),
                        self.palette.red(&progress.failed.to_string()),
                        progress.skipped,
                        self.palette.dim(&format!(
                            "in {:.3}s",
                            self.started_at.elapsed().as_secs_f64()
                        ))
                    );
                }
            }
        }
    }
}

fn redacted_endpoint_for_display(endpoint_url: &str) -> String {
    let without_fragment = endpoint_url
        .split_once('#')
        .map_or(endpoint_url, |(url, _)| url);
    let without_query = without_fragment
        .split_once('?')
        .map_or(without_fragment, |(url, _)| url);
    let Some((scheme, rest)) = without_query.split_once("://") else {
        return "<invalid endpoint>".to_string();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("{scheme}://{host}{path}")
}

fn dedup_failure_messages(failures: &[(String, String, String)]) -> Vec<(&str, usize)> {
    let mut seen = Vec::new();
    for (_, _, message) in failures {
        if let Some((_, count)) = seen
            .iter_mut()
            .find(|(m, _): &&mut (&str, usize)| *m == message)
        {
            *count += 1;
        } else {
            seen.push((message.as_str(), 1));
        }
    }
    seen
}

fn print_vector_index_report(
    output: OutputFormat,
    report: &VectorIndexReport,
    use_color: bool,
) -> Result<(), CliError> {
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{} vectors with {}:{} {}: {} indexed, {} skipped, {} failed {}",
                if report.dry_run {
                    palette.cyan("Dry run for")
                } else {
                    palette.cyan("Indexed")
                },
                report.provider_name,
                report.model_name,
                palette.dim(&format!(
                    "(dims {}, batch size {}, concurrency {})",
                    report.dimensions, report.batch_size, report.max_concurrency
                )),
                palette.green(&report.indexed.to_string()),
                report.skipped,
                palette.red(&report.failed.to_string()),
                palette.dim(&format!("in {:.3}s", report.elapsed_seconds))
            );
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_vector_queue_report(
    output: OutputFormat,
    report: &VectorQueueReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Vector queue {}:{}: {} pending, {} indexed, {} stale{}",
                report.provider_name,
                report.model_name,
                report.pending_chunks,
                report.indexed_chunks,
                report.stale_vectors,
                if report.model_mismatch {
                    " (model mismatch)"
                } else {
                    ""
                }
            );
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_vector_repair_report(
    output: OutputFormat,
    report: &VectorRepairReport,
    use_color: bool,
) -> Result<(), CliError> {
    let palette = AnsiPalette::new(use_color);
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let status = if report.dry_run {
                palette.cyan("Dry run")
            } else if report.repaired {
                palette.green("Repaired")
            } else {
                palette.cyan("Checked")
            };
            println!(
                "{} vectors for {}:{}: {} pending, {} stale{}",
                status,
                report.provider_name,
                report.model_name,
                report.pending_chunks,
                report.stale_vectors,
                if report.model_mismatch {
                    " (model mismatch)"
                } else {
                    ""
                }
            );
            if let Some(index_report) = report.index_report.as_ref() {
                println!(
                    "{} indexed, {} skipped, {} failed",
                    index_report.indexed, index_report.skipped, index_report.failed
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_vector_neighbors_report(
    output: OutputFormat,
    report: &VectorNeighborsReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_hits = paginated_items(&report.hits, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = vector_neighbor_rows(report, visible_hits);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                if let Some(query_text) = report.query_text.as_deref() {
                    println!(
                        "{} {}",
                        palette.cyan("Vector neighbors for"),
                        palette.bold(query_text)
                    );
                } else if let Some(note_path) = report.note_path.as_deref() {
                    println!(
                        "{} {}",
                        palette.cyan("Vector neighbors for note"),
                        palette.bold(note_path)
                    );
                }
            }
            if visible_hits.is_empty() {
                println!("No vector neighbors.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for (index, hit) in visible_hits.iter().enumerate() {
                    print_vector_neighbor(index, hit, palette);
                }
            }

            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_related_notes_report(
    output: OutputFormat,
    report: &RelatedNotesReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_hits = paginated_items(&report.hits, list_controls);
    let rows = related_note_rows(report, visible_hits);
    let palette = AnsiPalette::new(use_color);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!(
                    "{} {}",
                    palette.cyan("Related notes for"),
                    palette.bold(&report.note_path)
                );
            }
            if visible_hits.is_empty() {
                println!("No related notes.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for (index, hit) in visible_hits.iter().enumerate() {
                    println!(
                        "{}. {} ({:.3}, {} chunks)",
                        index + 1,
                        hit.document_path,
                        hit.similarity,
                        hit.matched_chunks
                    );
                    if !hit.heading_path.is_empty() {
                        println!("   {}", hit.heading_path.join(" > "));
                    }
                    println!("   {}", hit.snippet);
                }
            }

            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_vector_duplicates_report(
    output: OutputFormat,
    report: &VectorDuplicatesReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_pairs = paginated_items(&report.pairs, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = vector_duplicate_rows(report, visible_pairs);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                println!("{}", palette.cyan("Vector duplicates"));
            }
            if visible_pairs.is_empty() {
                println!("No duplicate pairs.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for pair in visible_pairs {
                    print_vector_duplicate(pair);
                }
            }

            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn print_vector_models_report(
    output: OutputFormat,
    models: &[StoredModelInfo],
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows: Vec<Value> = models
        .iter()
        .map(|m| {
            serde_json::json!({
                "cache_key": m.cache_key,
                "provider": m.provider_name,
                "model": m.model_name,
                "dimensions": m.dimensions,
                "normalized": m.normalized,
                "chunks": m.chunk_count,
                "active": m.is_active,
            })
        })
        .collect();

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            let palette = AnsiPalette::new(use_color);
            if stdout_is_tty {
                println!("{}", palette.cyan("Vector models"));
            }
            if models.is_empty() {
                println!("No stored models.");
                return Ok(());
            }
            for model in models {
                let active_marker = if model.is_active { " (active)" } else { "" };
                println!(
                    "{}{}\n  provider: {}  model: {}  dimensions: {}  normalized: {}  chunks: {}",
                    palette.bold(&model.cache_key),
                    active_marker,
                    model.provider_name,
                    model.model_name,
                    model.dimensions,
                    model.normalized,
                    model.chunk_count,
                );
            }
            export_rows(&rows, None, export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, None, export)?;
            print_json_lines(rows, None)
        }
    }
}

fn print_cluster_report(
    output: OutputFormat,
    report: &ClusterReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let visible_clusters = paginated_items(&report.clusters, list_controls);
    let palette = AnsiPalette::new(use_color);
    let rows = cluster_rows(report, visible_clusters);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if stdout_is_tty {
                if report.dry_run {
                    println!("Vector clusters (dry run)");
                } else {
                    println!("Vector clusters");
                }
            }
            if visible_clusters.is_empty() {
                println!("No vector clusters.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for (index, cluster) in visible_clusters.iter().enumerate() {
                    print_cluster_summary(index, cluster, palette);
                }
            }

            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(&rows, list_controls.fields.as_deref(), export)?;
            print_json_lines(rows, list_controls.fields.as_deref())
        }
    }
}

fn vector_neighbor_rows(report: &VectorNeighborsReport, hits: &[VectorNeighborHit]) -> Vec<Value> {
    hits.iter()
        .map(|hit| {
            serde_json::json!({
                "provider_name": report.provider_name,
                "model_name": report.model_name,
                "dimensions": report.dimensions,
                "query_text": report.query_text,
                "note_path": report.note_path,
                "document_path": hit.document_path,
                "chunk_id": hit.chunk_id,
                "heading_path": hit.heading_path,
                "snippet": hit.snippet,
                "distance": hit.distance,
            })
        })
        .collect()
}

fn related_note_rows(report: &RelatedNotesReport, hits: &[RelatedNoteHit]) -> Vec<Value> {
    hits.iter()
        .map(|hit| {
            serde_json::json!({
                "provider_name": report.provider_name,
                "model_name": report.model_name,
                "dimensions": report.dimensions,
                "note_path": report.note_path,
                "document_path": hit.document_path,
                "heading_path": hit.heading_path,
                "snippet": hit.snippet,
                "similarity": hit.similarity,
                "matched_chunks": hit.matched_chunks,
            })
        })
        .collect()
}

fn vector_duplicate_rows(
    report: &VectorDuplicatesReport,
    pairs: &[VectorDuplicatePair],
) -> Vec<Value> {
    pairs
        .iter()
        .map(|pair| {
            serde_json::json!({
                "provider_name": report.provider_name,
                "model_name": report.model_name,
                "dimensions": report.dimensions,
                "threshold": report.threshold,
                "left_document_path": pair.left_document_path,
                "left_chunk_id": pair.left_chunk_id,
                "right_document_path": pair.right_document_path,
                "right_chunk_id": pair.right_chunk_id,
                "similarity": pair.similarity,
            })
        })
        .collect()
}

fn cluster_rows(report: &ClusterReport, clusters: &[vulcan_core::ClusterSummary]) -> Vec<Value> {
    clusters
        .iter()
        .map(|cluster| {
            serde_json::json!({
                "provider_name": report.provider_name,
                "model_name": report.model_name,
                "dimensions": report.dimensions,
                "cluster_count": report.cluster_count,
                "cluster_id": cluster.cluster_id,
                "cluster_label": cluster.cluster_label,
                "keywords": cluster.keywords,
                "chunk_count": cluster.chunk_count,
                "document_count": cluster.document_count,
                "document_path": cluster.exemplar_document_path,
                "heading_path": cluster.exemplar_heading_path,
                "exemplar_document_path": cluster.exemplar_document_path,
                "exemplar_heading_path": cluster.exemplar_heading_path,
                "exemplar_snippet": cluster.exemplar_snippet,
                "top_documents": cluster.top_documents,
            })
        })
        .collect()
}

fn print_vector_neighbor(index: usize, hit: &VectorNeighborHit, palette: AnsiPalette) {
    print_ranked_snippet_hit(
        index,
        &hit.document_path,
        &hit.heading_path,
        "Distance",
        f64::from(hit.distance),
        &hit.snippet,
        palette,
    );
}

fn print_vector_duplicate(pair: &VectorDuplicatePair) {
    println!(
        "- {} <-> {} [{:.3}]",
        pair.left_document_path, pair.right_document_path, pair.similarity
    );
}

fn print_cluster_summary(
    index: usize,
    cluster: &vulcan_core::ClusterSummary,
    palette: AnsiPalette,
) {
    println!(
        "{}. {}",
        index + 1,
        palette.bold(&format!(
            "[{}] {}",
            cluster.cluster_id + 1,
            cluster.cluster_label
        ))
    );
    println!(
        "   {}: {} chunks across {} notes",
        palette.cyan("Size"),
        cluster.chunk_count,
        cluster.document_count
    );
    println!(
        "   {}: {}",
        palette.cyan("Exemplar"),
        if cluster.exemplar_heading_path.is_empty() {
            cluster.exemplar_document_path.clone()
        } else {
            format!(
                "{} > {}",
                cluster.exemplar_document_path,
                cluster.exemplar_heading_path.join(" > ")
            )
        }
    );
    let snippet_lines = cluster.exemplar_snippet.lines().collect::<Vec<&str>>();
    if let Some((first, rest)) = snippet_lines.split_first() {
        println!("   {}: {}", palette.cyan("Snippet"), first.trim());
        for line in rest
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
        {
            println!("            {line}");
        }
    }
    if !cluster.top_documents.is_empty() {
        println!("   {}:", palette.cyan("Top notes"));
        for document in &cluster.top_documents {
            println!("   - {} ({})", document.document_path, document.chunk_count);
        }
    }
    println!();
}

fn print_ranked_snippet_hit(
    index: usize,
    document_path: &str,
    heading_path: &[String],
    metric_label: &str,
    metric_value: f64,
    snippet: &str,
    palette: AnsiPalette,
) {
    let location = if heading_path.is_empty() {
        document_path.to_string()
    } else {
        format!("{document_path} > {}", heading_path.join(" > "))
    };

    println!("{}. {}", index + 1, palette.bold(&location));
    println!("   {}: {metric_value:.3}", palette.cyan(metric_label));

    let lines = snippet
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if let Some((first, rest)) = lines.split_first() {
        println!("   {}: {first}", palette.cyan("Snippet"));
        for line in rest {
            println!("            {line}");
        }
    } else {
        println!("   {}: <empty>", palette.cyan("Snippet"));
    }

    println!();
}

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
    print_cluster_report(
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
    print_related_notes_report(
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
                .then(|| VectorIndexProgressReporter::new(use_stderr_color, verbose));
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
            print_vector_index_report(cli.output, &report, use_stdout_color)?;
            Ok(())
        }
        VectorsCommand::Repair { dry_run } => {
            let mut progress = (cli.output == OutputFormat::Human)
                .then(|| VectorIndexProgressReporter::new(use_stderr_color, false));
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
            print_vector_repair_report(cli.output, &report, use_stdout_color)
        }
        VectorsCommand::Rebuild { dry_run } => {
            let mut progress = (cli.output == OutputFormat::Human)
                .then(|| VectorIndexProgressReporter::new(use_stderr_color, false));
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
            print_vector_index_report(cli.output, &report, use_stdout_color)?;
            Ok(())
        }
        VectorsCommand::Queue { command } => match command {
            VectorQueueCommand::Status => {
                let report = inspect_vector_queue(paths, cli.provider.as_deref())
                    .map_err(CliError::operation)?;
                print_vector_queue_report(cli.output, &report)
            }
            VectorQueueCommand::Run { dry_run } => {
                let verbose = cli.verbose;
                let mut progress = (cli.output == OutputFormat::Human)
                    .then(|| VectorIndexProgressReporter::new(use_stderr_color, verbose));
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
                print_vector_index_report(cli.output, &report, use_stdout_color)?;
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
            print_vector_neighbors_report(
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
            print_vector_duplicates_report(
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
            print_vector_models_report(
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

#[cfg(test)]
mod tests {
    use super::redacted_endpoint_for_display;

    #[test]
    fn redacted_endpoint_for_display_strips_credentials_query_and_fragment() {
        assert_eq!(
            redacted_endpoint_for_display("https://user:secret@example.test/v1?key=secret#frag"),
            "https://example.test/v1"
        );
        assert_eq!(
            redacted_endpoint_for_display("https://example.test/v1"),
            "https://example.test/v1"
        );
        assert_eq!(
            redacted_endpoint_for_display("not a url"),
            "<invalid endpoint>"
        );
    }
}
