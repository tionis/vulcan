mod cli;

pub use cli::{BasesCommand, Cli, Command, OutputFormat, SearchMode, VectorsCommand};

use clap::Parser;
use serde::Serialize;
use serde_json::{Map, Value};
use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::io;
use std::io::IsTerminal;
use std::path::PathBuf;
use vulcan_core::{
    doctor_vault, evaluate_base_file, index_vectors, initialize_vault, move_note, query_backlinks,
    query_links, query_notes, query_vector_neighbors, scan_vault, search_vault, BacklinkRecord,
    BacklinksReport, BasesEvalReport, DoctorDiagnosticIssue, DoctorLinkIssue, DoctorReport,
    InitSummary, MoveSummary, NoteQuery, NoteRecord, NotesReport, OutgoingLinkRecord,
    OutgoingLinksReport, ScanMode, ScanSummary, SearchHit, SearchQuery, SearchReport, VaultPaths,
    VectorIndexQuery, VectorIndexReport, VectorNeighborHit, VectorNeighborsQuery,
    VectorNeighborsReport,
};

#[derive(Debug)]
pub struct CliError {
    exit_code: u8,
    message: String,
}

impl CliError {
    fn not_implemented(command: &str) -> Self {
        Self {
            exit_code: 2,
            message: format!("{command} is not implemented yet"),
        }
    }

    fn io(error: &io::Error) -> Self {
        Self {
            exit_code: 1,
            message: format!("failed to read current working directory: {error}"),
        }
    }

    fn operation(error: impl Display) -> Self {
        Self {
            exit_code: 1,
            message: error.to_string(),
        }
    }

    #[must_use]
    pub fn exit_code(&self) -> u8 {
        self.exit_code
    }
}

impl Display for CliError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

pub fn run() -> Result<(), CliError> {
    run_from(std::env::args_os())
}

pub fn run_from<I, T>(args: I) -> Result<(), CliError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::parse_from(args);
    dispatch(&cli)
}

#[allow(clippy::too_many_lines)]
fn dispatch(cli: &Cli) -> Result<(), CliError> {
    let paths = VaultPaths::new(resolve_vault_root(&cli.vault)?);
    let list_controls = ListOutputControls::from_cli(cli);
    let stdout_is_tty = io::stdout().is_terminal();

    match cli.command {
        Command::Backlinks { ref note } => {
            let report = query_backlinks(&paths, note).map_err(CliError::operation)?;
            print_backlinks_report(cli.output, &report, &list_controls, stdout_is_tty)?;
            Ok(())
        }
        Command::Bases { ref command } => match command {
            BasesCommand::Eval { file } => {
                let report = evaluate_base_file(&paths, file).map_err(CliError::operation)?;
                print_bases_report(cli.output, &report, &list_controls, stdout_is_tty)?;
                Ok(())
            }
        },
        Command::Describe => Err(CliError::not_implemented("describe")),
        Command::Doctor => {
            let report = doctor_vault(&paths).map_err(CliError::operation)?;
            print_doctor_report(cli.output, &paths, &report)?;
            Ok(())
        }
        Command::Init => {
            let summary = initialize_vault(&paths).map_err(CliError::operation)?;
            print_init_summary(cli.output, &summary)?;
            Ok(())
        }
        Command::Move {
            ref source,
            ref dest,
            dry_run,
        } => {
            let summary = move_note(&paths, source, dest, dry_run).map_err(CliError::operation)?;
            print_move_summary(cli.output, &summary)?;
            Ok(())
        }
        Command::Links { ref note } => {
            let report = query_links(&paths, note).map_err(CliError::operation)?;
            print_links_report(cli.output, &report, &list_controls, stdout_is_tty)?;
            Ok(())
        }
        Command::Notes {
            ref filters,
            ref sort,
            desc,
        } => {
            let report = query_notes(
                &paths,
                &NoteQuery {
                    filters: filters.clone(),
                    sort_by: sort.clone(),
                    sort_descending: desc,
                },
            )
            .map_err(CliError::operation)?;
            print_notes_report(cli.output, &report, &list_controls, stdout_is_tty)?;
            Ok(())
        }
        Command::Search {
            ref query,
            mode,
            ref tag,
            ref path_prefix,
            ref has_property,
            context_size,
        } => {
            let report = search_vault(
                &paths,
                &SearchQuery {
                    text: query.clone(),
                    tag: tag.clone(),
                    path_prefix: path_prefix.clone(),
                    has_property: has_property.clone(),
                    provider: cli.provider.clone(),
                    mode: match mode {
                        SearchMode::Keyword => vulcan_core::search::SearchMode::Keyword,
                        SearchMode::Hybrid => vulcan_core::search::SearchMode::Hybrid,
                    },
                    limit: cli.limit.map(|limit| limit.saturating_add(cli.offset)),
                    context_size,
                },
            )
            .map_err(CliError::operation)?;
            print_search_report(cli.output, &report, &list_controls, stdout_is_tty)?;
            Ok(())
        }
        Command::Vectors { ref command } => match command {
            VectorsCommand::Index => {
                let report = index_vectors(
                    &paths,
                    &VectorIndexQuery {
                        provider: cli.provider.clone(),
                    },
                )
                .map_err(CliError::operation)?;
                print_vector_index_report(cli.output, &report)?;
                Ok(())
            }
            VectorsCommand::Neighbors { query, note } => {
                let report = query_vector_neighbors(
                    &paths,
                    &VectorNeighborsQuery {
                        provider: cli.provider.clone(),
                        text: query.clone(),
                        note: note.clone(),
                        limit: cli.limit.unwrap_or(10).saturating_add(cli.offset),
                    },
                )
                .map_err(CliError::operation)?;
                print_vector_neighbors_report(cli.output, &report, &list_controls, stdout_is_tty)?;
                Ok(())
            }
        },
        Command::Scan { full } => {
            let summary = scan_vault(
                &paths,
                if full {
                    ScanMode::Full
                } else {
                    ScanMode::Incremental
                },
            )
            .map_err(CliError::operation)?;
            print_scan_summary(cli.output, &summary);
            Ok(())
        }
    }
}

fn print_search_report(
    output: OutputFormat,
    report: &SearchReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
) -> Result<(), CliError> {
    let visible_hits = paginated_items(&report.hits, list_controls);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!("Search hits for {} ({:?})", report.query, report.mode);
            }
            if visible_hits.is_empty() {
                println!("No search hits.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in search_hit_rows(report, visible_hits) {
                    print_selected_human_fields(&row, fields);
                }
            } else {
                for hit in visible_hits {
                    print_search_hit(hit);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(
            search_hit_rows(report, visible_hits),
            list_controls.fields.as_deref(),
        ),
    }
}

fn print_notes_report(
    output: OutputFormat,
    report: &NotesReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
) -> Result<(), CliError> {
    let visible_notes = paginated_items(&report.notes, list_controls);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!("Notes query");
            }
            if visible_notes.is_empty() {
                println!("No notes matched.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in note_rows(report, visible_notes) {
                    print_selected_human_fields(&row, fields);
                }
            } else {
                for note in visible_notes {
                    print_note(note);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(
            note_rows(report, visible_notes),
            list_controls.fields.as_deref(),
        ),
    }
}

fn print_vector_index_report(
    output: OutputFormat,
    report: &VectorIndexReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!(
                "Indexed vectors with {}:{} (dims {}): {} indexed, {} skipped, {} failed in {:.3}s",
                report.provider_name,
                report.model_name,
                report.dimensions,
                report.indexed,
                report.skipped,
                report.failed,
                report.elapsed_seconds
            );
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
) -> Result<(), CliError> {
    let visible_hits = paginated_items(&report.hits, list_controls);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                if let Some(query_text) = report.query_text.as_deref() {
                    println!("Vector neighbors for {query_text}");
                } else if let Some(note_path) = report.note_path.as_deref() {
                    println!("Vector neighbors for note {note_path}");
                }
            }
            if visible_hits.is_empty() {
                println!("No vector neighbors.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in vector_neighbor_rows(report, visible_hits) {
                    print_selected_human_fields(&row, fields);
                }
            } else {
                for hit in visible_hits {
                    print_vector_neighbor(hit);
                }
            }

            Ok(())
        }
        OutputFormat::Json => print_json_lines(
            vector_neighbor_rows(report, visible_hits),
            list_controls.fields.as_deref(),
        ),
    }
}

fn print_bases_report(
    output: OutputFormat,
    report: &BasesEvalReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
) -> Result<(), CliError> {
    let rows = bases_rows(report);
    let visible_rows = paginated_items(&rows, list_controls);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!("Bases eval {}", report.file);
            }
            if visible_rows.is_empty() {
                println!("No bases rows.");
            } else if let Some(fields) = list_controls.fields.as_deref() {
                for row in visible_rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                for row in visible_rows {
                    print_bases_row(row);
                }
            }

            if !report.diagnostics.is_empty() {
                println!("Diagnostics:");
                for diagnostic in &report.diagnostics {
                    if let Some(path) = diagnostic.path.as_deref() {
                        println!("- {path}: {}", diagnostic.message);
                    } else {
                        println!("- {}", diagnostic.message);
                    }
                }
            }

            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_links_report(
    output: OutputFormat,
    report: &OutgoingLinksReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
) -> Result<(), CliError> {
    let visible_links = paginated_items(&report.links, list_controls);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!("Links for {} ({:?})", report.note_path, report.matched_by);
            }
            if visible_links.is_empty() {
                println!("No outgoing links.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in outgoing_link_rows(report, visible_links) {
                    print_selected_human_fields(&row, fields);
                }
            } else {
                for link in visible_links {
                    print_outgoing_link(link);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(
            outgoing_link_rows(report, visible_links),
            list_controls.fields.as_deref(),
        ),
    }
}

fn print_backlinks_report(
    output: OutputFormat,
    report: &BacklinksReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
) -> Result<(), CliError> {
    let visible_backlinks = paginated_items(&report.backlinks, list_controls);

    match output {
        OutputFormat::Human => {
            if stdout_is_tty {
                println!(
                    "Backlinks for {} ({:?})",
                    report.note_path, report.matched_by
                );
            }
            if visible_backlinks.is_empty() {
                println!("No backlinks.");
                return Ok(());
            }

            if let Some(fields) = list_controls.fields.as_deref() {
                for row in backlink_rows(report, visible_backlinks) {
                    print_selected_human_fields(&row, fields);
                }
            } else {
                for backlink in visible_backlinks {
                    print_backlink(backlink);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json_lines(
            backlink_rows(report, visible_backlinks),
            list_controls.fields.as_deref(),
        ),
    }
}

fn print_init_summary(output: OutputFormat, summary: &InitSummary) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!(
                "Initialized {} (config {}, cache {})",
                summary.vault_root.display(),
                if summary.created_config {
                    "created"
                } else {
                    "existing"
                },
                if summary.created_cache {
                    "created"
                } else {
                    "existing"
                },
            );
            Ok(())
        }
        OutputFormat::Json => print_json(summary),
    }
}

fn print_scan_summary(output: OutputFormat, summary: &ScanSummary) {
    match output {
        OutputFormat::Human => {
            println!(
                "Scanned {} files: {} added, {} updated, {} unchanged, {} deleted",
                summary.discovered,
                summary.added,
                summary.updated,
                summary.unchanged,
                summary.deleted
            );
        }
        OutputFormat::Json => {
            print_json(summary).expect("scan summary JSON serialization should succeed");
        }
    }
}

fn print_move_summary(output: OutputFormat, summary: &MoveSummary) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if summary.dry_run {
                println!(
                    "Dry run: move {} -> {}",
                    summary.source_path, summary.destination_path
                );
            } else {
                println!(
                    "Moved {} -> {}",
                    summary.source_path, summary.destination_path
                );
            }

            if summary.rewritten_files.is_empty() {
                println!("No link rewrites.");
                return Ok(());
            }

            for file in &summary.rewritten_files {
                println!("- {}", file.path);
                for change in &file.changes {
                    println!("  {} -> {}", change.before, change.after);
                }
            }

            Ok(())
        }
        OutputFormat::Json => print_json(summary),
    }
}

fn print_doctor_report(
    output: OutputFormat,
    paths: &VaultPaths,
    report: &DoctorReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            println!("Doctor summary for {}", paths.vault_root().display());
            println!("- unresolved links: {}", report.summary.unresolved_links);
            println!(
                "- ambiguous link targets: {}",
                report.summary.ambiguous_links
            );
            println!("- parse failures: {}", report.summary.parse_failures);
            println!("- stale index rows: {}", report.summary.stale_index_rows);
            println!(
                "- missing index rows: {}",
                report.summary.missing_index_rows
            );
            println!("- orphan notes: {}", report.summary.orphan_notes);
            println!("- HTML links: {}", report.summary.html_links);

            if report.summary == zero_summary() {
                println!("No issues found.");
                return Ok(());
            }

            print_link_section("Unresolved links", &report.unresolved_links);
            print_link_section("Ambiguous link targets", &report.ambiguous_links);
            print_diagnostic_section("Parse failures", &report.parse_failures);
            print_path_section("Stale index rows", &report.stale_index_rows);
            print_path_section("Missing index rows", &report.missing_index_rows);
            print_path_section("Orphan notes", &report.orphan_notes);
            print_diagnostic_section("HTML links", &report.html_links);
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_json<T: Serialize>(value: &T) -> Result<(), CliError> {
    println!(
        "{}",
        serde_json::to_string(value).map_err(CliError::operation)?
    );
    Ok(())
}

fn print_json_lines(rows: Vec<Value>, fields: Option<&[String]>) -> Result<(), CliError> {
    for row in rows {
        let selected = select_fields(row, fields);
        println!(
            "{}",
            serde_json::to_string(&selected).map_err(CliError::operation)?
        );
    }

    Ok(())
}

fn resolve_vault_root(vault: &PathBuf) -> Result<PathBuf, CliError> {
    if vault.is_absolute() {
        return Ok(vault.clone());
    }

    Ok(std::env::current_dir()
        .map_err(|error| CliError::io(&error))?
        .join(vault))
}

fn print_link_section(title: &str, issues: &[DoctorLinkIssue]) {
    if issues.is_empty() {
        return;
    }

    println!();
    println!("{title}:");
    for issue in issues {
        let path = issue.document_path.as_deref().unwrap_or("<unknown>");
        if issue.matches.is_empty() {
            if let Some(target) = issue.target.as_deref() {
                println!("- {path}: {target} ({})", issue.message);
            } else {
                println!("- {path}: {}", issue.message);
            }
        } else if let Some(target) = issue.target.as_deref() {
            println!(
                "- {path}: {target} ({}) [{}]",
                issue.message,
                issue.matches.join(", ")
            );
        } else {
            println!("- {path}: {} [{}]", issue.message, issue.matches.join(", "));
        }
    }
}

fn print_diagnostic_section(title: &str, issues: &[DoctorDiagnosticIssue]) {
    if issues.is_empty() {
        return;
    }

    println!();
    println!("{title}:");
    for issue in issues {
        let path = issue.document_path.as_deref().unwrap_or("<unknown>");
        if let Some(byte_range) = issue.byte_range.as_ref() {
            println!(
                "- {path}: {} (bytes {}-{})",
                issue.message, byte_range.start, byte_range.end
            );
        } else {
            println!("- {path}: {}", issue.message);
        }
    }
}

fn print_path_section(title: &str, paths: &[String]) {
    if paths.is_empty() {
        return;
    }

    println!();
    println!("{title}:");
    for path in paths {
        println!("- {path}");
    }
}

fn outgoing_link_rows(report: &OutgoingLinksReport, links: &[OutgoingLinkRecord]) -> Vec<Value> {
    links
        .iter()
        .map(|link| {
            serde_json::json!({
                "note_path": report.note_path,
                "matched_by": report.matched_by,
                "raw_text": link.raw_text,
                "link_kind": link.link_kind,
                "display_text": link.display_text,
                "target_path_candidate": link.target_path_candidate,
                "target_heading": link.target_heading,
                "target_block": link.target_block,
                "resolved_target_path": link.resolved_target_path,
                "resolution_status": link.resolution_status,
                "context": link.context,
            })
        })
        .collect()
}

fn backlink_rows(report: &BacklinksReport, backlinks: &[BacklinkRecord]) -> Vec<Value> {
    backlinks
        .iter()
        .map(|backlink| {
            serde_json::json!({
                "note_path": report.note_path,
                "matched_by": report.matched_by,
                "source_path": backlink.source_path,
                "raw_text": backlink.raw_text,
                "link_kind": backlink.link_kind,
                "display_text": backlink.display_text,
                "context": backlink.context,
            })
        })
        .collect()
}

fn search_hit_rows(report: &SearchReport, hits: &[SearchHit]) -> Vec<Value> {
    hits.iter()
        .map(|hit| {
            serde_json::json!({
                "query": report.query,
                "mode": report.mode,
                "tag": report.tag,
                "path_prefix": report.path_prefix,
                "has_property": report.has_property,
                "document_path": hit.document_path,
                "chunk_id": hit.chunk_id,
                "heading_path": hit.heading_path,
                "snippet": hit.snippet,
                "rank": hit.rank,
            })
        })
        .collect()
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

fn note_rows(report: &NotesReport, notes: &[NoteRecord]) -> Vec<Value> {
    notes
        .iter()
        .map(|note| {
            serde_json::json!({
                "filters": report.filters,
                "sort_by": report.sort_by,
                "sort_descending": report.sort_descending,
                "document_path": note.document_path,
                "file_name": note.file_name,
                "file_ext": note.file_ext,
                "file_mtime": note.file_mtime,
                "properties": note.properties,
            })
        })
        .collect()
}

fn bases_rows(report: &BasesEvalReport) -> Vec<Value> {
    report
        .views
        .iter()
        .flat_map(|view| {
            view.rows.iter().map(|row| {
                serde_json::json!({
                    "file": report.file,
                    "view_name": view.name,
                    "view_type": view.view_type,
                    "filters": view.filters,
                    "sort_by": view.sort_by,
                    "sort_descending": view.sort_descending,
                    "document_path": row.document_path,
                    "properties": row.properties,
                    "formulas": row.formulas,
                })
            })
        })
        .collect()
}

fn select_fields(row: Value, fields: Option<&[String]>) -> Value {
    let Some(fields) = fields else {
        return row;
    };
    let Some(object) = row.as_object() else {
        return row;
    };
    let mut selected = Map::new();
    for field in fields {
        if let Some(value) = object.get(field) {
            selected.insert(field.clone(), value.clone());
        }
    }
    Value::Object(selected)
}

fn print_selected_human_fields(row: &Value, fields: &[String]) {
    let Some(object) = row.as_object() else {
        println!("{row}");
        return;
    };

    let rendered = fields
        .iter()
        .filter_map(|field| {
            object
                .get(field)
                .map(|value| format!("{field}={}", render_human_value(value)))
        })
        .collect::<Vec<_>>();

    println!("{}", rendered.join(" | "));
}

fn render_human_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => "null".to_string(),
        _ => value.to_string(),
    }
}

fn print_outgoing_link(link: &OutgoingLinkRecord) {
    let target = link
        .resolved_target_path
        .as_deref()
        .or(link.target_path_candidate.as_deref())
        .unwrap_or("(self)");

    if let Some(context) = link.context.as_ref() {
        println!(
            "- {} [{} {:?}] line {}: {}",
            target, link.link_kind, link.resolution_status, context.line, link.raw_text
        );
    } else {
        println!(
            "- {} [{} {:?}]: {}",
            target, link.link_kind, link.resolution_status, link.raw_text
        );
    }
}

fn print_backlink(backlink: &BacklinkRecord) {
    if let Some(context) = backlink.context.as_ref() {
        println!(
            "- {} [{}] line {}: {}",
            backlink.source_path, backlink.link_kind, context.line, backlink.raw_text
        );
    } else {
        println!(
            "- {} [{}]: {}",
            backlink.source_path, backlink.link_kind, backlink.raw_text
        );
    }
}

fn print_search_hit(hit: &SearchHit) {
    if hit.heading_path.is_empty() {
        println!("- {} [{:.3}]: {}", hit.document_path, hit.rank, hit.snippet);
    } else {
        println!(
            "- {} > {} [{:.3}]: {}",
            hit.document_path,
            hit.heading_path.join(" > "),
            hit.rank,
            hit.snippet
        );
    }
}

fn print_vector_neighbor(hit: &VectorNeighborHit) {
    if hit.heading_path.is_empty() {
        println!(
            "- {} [{:.3}]: {}",
            hit.document_path, hit.distance, hit.snippet
        );
    } else {
        println!(
            "- {} > {} [{:.3}]: {}",
            hit.document_path,
            hit.heading_path.join(" > "),
            hit.distance,
            hit.snippet
        );
    }
}

fn print_note(note: &NoteRecord) {
    println!("- {}", note.document_path);
}

fn print_bases_row(row: &Value) {
    let document_path = row
        .get("document_path")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let view_name = row
        .get("view_name")
        .and_then(Value::as_str)
        .unwrap_or("view");

    println!("- {document_path} ({view_name})");
}

fn zero_summary() -> vulcan_core::DoctorSummary {
    vulcan_core::DoctorSummary {
        unresolved_links: 0,
        ambiguous_links: 0,
        parse_failures: 0,
        stale_index_rows: 0,
        missing_index_rows: 0,
        orphan_notes: 0,
        html_links: 0,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ListOutputControls {
    fields: Option<Vec<String>>,
    limit: Option<usize>,
    offset: usize,
}

impl ListOutputControls {
    fn from_cli(cli: &Cli) -> Self {
        Self {
            fields: cli.fields.clone(),
            limit: cli.limit,
            offset: cli.offset,
        }
    }
}

fn paginated_items<'a, T>(items: &'a [T], controls: &ListOutputControls) -> &'a [T] {
    let start = controls.offset.min(items.len());
    let end = controls.limit.map_or(items.len(), |limit| {
        start.saturating_add(limit).min(items.len())
    });

    &items[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_defaults_for_doctor_command() {
        let cli = Cli::try_parse_from(["vulcan", "doctor"]).expect("cli should parse");

        assert_eq!(cli.vault, PathBuf::from("."));
        assert_eq!(cli.output, OutputFormat::Human);
        assert_eq!(cli.fields, None);
        assert_eq!(cli.limit, None);
        assert_eq!(cli.offset, 0);
        assert!(!cli.verbose);
        assert_eq!(cli.command, Command::Doctor);
    }

    #[test]
    fn parses_links_and_backlinks_commands() {
        let links = Cli::try_parse_from(["vulcan", "links", "Home"]).expect("cli should parse");
        let backlinks = Cli::try_parse_from(["vulcan", "backlinks", "Projects/Alpha"])
            .expect("cli should parse");
        let search = Cli::try_parse_from([
            "vulcan",
            "search",
            "dashboard",
            "--tag",
            "index",
            "--path-prefix",
            "People/",
            "--has-property",
            "status",
            "--context-size",
            "24",
        ])
        .expect("cli should parse");
        let notes = Cli::try_parse_from([
            "vulcan",
            "notes",
            "--where",
            "status = done",
            "--where",
            "estimate > 2",
            "--sort",
            "due",
            "--desc",
        ])
        .expect("cli should parse");
        let bases = Cli::try_parse_from(["vulcan", "bases", "eval", "release.base"])
            .expect("cli should parse");
        let vectors =
            Cli::try_parse_from(["vulcan", "vectors", "index"]).expect("cli should parse");
        let move_command = Cli::try_parse_from([
            "vulcan",
            "move",
            "Projects/Alpha.md",
            "Archive/Alpha.md",
            "--dry-run",
        ])
        .expect("cli should parse");

        assert_eq!(
            links.command,
            Command::Links {
                note: "Home".to_string()
            }
        );
        assert_eq!(
            backlinks.command,
            Command::Backlinks {
                note: "Projects/Alpha".to_string()
            }
        );
        assert_eq!(
            search.command,
            Command::Search {
                query: "dashboard".to_string(),
                mode: SearchMode::Keyword,
                tag: Some("index".to_string()),
                path_prefix: Some("People/".to_string()),
                has_property: Some("status".to_string()),
                context_size: 24,
            }
        );
        assert_eq!(
            notes.command,
            Command::Notes {
                filters: vec!["status = done".to_string(), "estimate > 2".to_string()],
                sort: Some("due".to_string()),
                desc: true,
            }
        );
        assert_eq!(
            bases.command,
            Command::Bases {
                command: BasesCommand::Eval {
                    file: "release.base".to_string(),
                },
            }
        );
        assert_eq!(
            vectors.command,
            Command::Vectors {
                command: VectorsCommand::Index,
            }
        );
        assert_eq!(
            move_command.command,
            Command::Move {
                source: "Projects/Alpha.md".to_string(),
                dest: "Archive/Alpha.md".to_string(),
                dry_run: true
            }
        );
    }

    #[test]
    fn parses_global_flags_and_scan_options() {
        let cli = Cli::try_parse_from([
            "vulcan",
            "--vault",
            "/tmp/vault",
            "--output",
            "json",
            "--fields",
            "source_path,raw_text",
            "--limit",
            "10",
            "--offset",
            "2",
            "--verbose",
            "scan",
            "--full",
        ])
        .expect("cli should parse");

        assert_eq!(cli.vault, PathBuf::from("/tmp/vault"));
        assert_eq!(cli.output, OutputFormat::Json);
        assert_eq!(
            cli.fields,
            Some(vec!["source_path".to_string(), "raw_text".to_string()])
        );
        assert_eq!(cli.limit, Some(10));
        assert_eq!(cli.offset, 2);
        assert!(cli.verbose);
        assert_eq!(cli.command, Command::Scan { full: true });
    }

    #[test]
    fn resolves_relative_vault_path_against_current_directory() {
        let current_dir = std::env::current_dir().expect("cwd should be available");
        let resolved = resolve_vault_root(&PathBuf::from("tests/fixtures/vaults/basic"))
            .expect("path resolution should succeed");

        assert_eq!(resolved, current_dir.join("tests/fixtures/vaults/basic"));
    }
}
