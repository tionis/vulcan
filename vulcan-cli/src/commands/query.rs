#![allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]

use crate::commit::AutoCommitPolicy;
use crate::output::{
    paginated_items, print_json_lines, print_selected_human_fields, render_human_value,
    ListOutputControls,
};
use crate::resolve::resolve_note_argument;
use crate::{
    resolve_bulk_note_selection, selected_permission_guard, selected_read_permission_filter,
    warn_auto_commit_if_needed, BulkNoteSelection, Cli, CliError, QueryEngineArg,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use vulcan_core::{
    bulk_set_property_on_paths, evaluate_dql_with_filter, execute_query_report_with_filter,
    list_properties, list_query_fields, load_vault_config, query_backlinks_with_filter,
    query_links_with_filter, query_notes_with_filter, search_vault_with_filter, NamedCount,
    NoteQuery, PermissionGuard, PropertyCatalogEntry, QueryAst, QueryReport, SearchQuery,
    VaultPaths,
};

pub(crate) fn handle_backlinks_command(
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
    let report = query_backlinks_with_filter(paths, &note, read_filter.as_ref())
        .map_err(CliError::operation)?;
    let export = crate::resolve_cli_export(export)?;
    crate::print_backlinks_report(
        cli.output,
        &report,
        list_controls,
        stdout_is_tty,
        use_stdout_color,
        export.as_ref(),
    )?;
    Ok(())
}

pub(crate) fn handle_links_command(
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
    let report =
        query_links_with_filter(paths, &note, read_filter.as_ref()).map_err(CliError::operation)?;
    let export = crate::resolve_cli_export(export)?;
    crate::print_links_report(
        cli.output,
        &report,
        list_controls,
        stdout_is_tty,
        use_stdout_color,
        export.as_ref(),
    )?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub(crate) fn handle_query_command(
    cli: &Cli,
    paths: &VaultPaths,
    dsl: Option<&str>,
    json: Option<&str>,
    filters: &[String],
    sort: Option<&String>,
    desc: bool,
    list_fields: bool,
    engine: QueryEngineArg,
    format: crate::QueryFormatArg,
    glob: Option<&str>,
    explain: bool,
    exit_code: bool,
    export: &crate::ExportArgs,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    if list_fields {
        if dsl.is_some() || json.is_some() {
            return Err(CliError::operation(
                "`query --list-fields` does not accept a query string or --json payload",
            ));
        }
        return print_query_field_catalog(cli, paths, list_controls);
    }

    let use_dql = match engine {
        QueryEngineArg::Dql => true,
        QueryEngineArg::Dsl => false,
        QueryEngineArg::Auto => dsl.is_some_and(looks_like_dql),
    };

    if use_dql {
        let dql = dsl.ok_or_else(|| {
            CliError::operation("DQL engine requires a positional query string, not --json")
        })?;
        if !filters.is_empty() || sort.is_some() || desc {
            return Err(CliError::operation(
                "`query --where/--sort/--desc` cannot be combined with DQL mode",
            ));
        }
        if cli.output == crate::OutputFormat::Human
            && !cli.quiet
            && matches!(engine, QueryEngineArg::Auto)
        {
            eprintln!("(detected as Dataview query)");
        }
        let read_filter = selected_read_permission_filter(cli, paths)?;
        let result = evaluate_dql_with_filter(paths, dql, None, read_filter.as_ref())
            .map_err(CliError::operation)?;
        let display_result_count = load_vault_config(paths)
            .config
            .dataview
            .display_result_count;
        return crate::print_dql_query_result(
            cli.output,
            &result,
            display_result_count,
            stdout_is_tty,
            use_stdout_color,
        );
    }

    let read_filter = selected_read_permission_filter(cli, paths)?;
    let ast = match (dsl, json) {
        (Some(_), Some(_)) => {
            return Err(CliError::operation(
                "provide either a DSL argument or --json, not both",
            ));
        }
        (Some(dsl), None) => {
            if !filters.is_empty() || sort.is_some() || desc {
                return Err(CliError::operation(
                    "`query --where/--sort/--desc` cannot be combined with a DSL string or --json",
                ));
            }
            QueryAst::from_dsl(dsl).map_err(CliError::operation)?
        }
        (None, Some(json)) => {
            if !filters.is_empty() || sort.is_some() || desc {
                return Err(CliError::operation(
                    "`query --where/--sort/--desc` cannot be combined with a DSL string or --json",
                ));
            }
            QueryAst::from_json(json).map_err(CliError::operation)?
        }
        (None, None) if !filters.is_empty() || sort.is_some() || desc => {
            QueryAst::from_note_query(&NoteQuery {
                filters: filters.to_vec(),
                sort_by: sort.cloned(),
                sort_descending: desc,
            })
            .map_err(CliError::operation)?
        }
        (None, None) => QueryAst::from_note_query(&NoteQuery {
            filters: Vec::new(),
            sort_by: None,
            sort_descending: false,
        })
        .map_err(CliError::operation)?,
    };
    let report = execute_query_report_with_filter(paths, ast, read_filter.as_ref())
        .map_err(CliError::operation)?;
    let effective_controls = ListOutputControls {
        limit: list_controls.limit.or(report.query.limit),
        offset: if list_controls.offset > 0 {
            list_controls.offset
        } else {
            report.query.offset
        },
        fields: list_controls.fields.clone(),
    };
    let export = crate::resolve_cli_export(export)?;
    crate::print_query_report(
        paths,
        cli.output,
        &report,
        &effective_controls,
        crate::QueryReportRenderOptions {
            format,
            glob,
            explain,
            stdout_is_tty,
            use_color: use_stdout_color,
            no_header: cli.no_header,
            export: export.as_ref(),
        },
    )?;
    if exit_code && report.notes.is_empty() {
        return Err(CliError::no_results());
    }
    Ok(())
}

pub(crate) fn handle_ls_command(
    cli: &Cli,
    paths: &VaultPaths,
    filters: &[String],
    glob: Option<&str>,
    tag: Option<&str>,
    format: crate::QueryFormatArg,
    export: &crate::ExportArgs,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    let mut query_filters = filters.to_vec();
    if let Some(tag) = tag {
        query_filters.push(format!("file.tags has_tag {tag}"));
    }
    let note_query = NoteQuery {
        filters: query_filters,
        sort_by: Some("file.path".to_string()),
        sort_descending: false,
    };
    let read_filter = selected_read_permission_filter(cli, paths)?;
    let notes_report = query_notes_with_filter(paths, &note_query, read_filter.as_ref())
        .map_err(CliError::operation)?;
    let ast = QueryAst::from_note_query(&note_query).map_err(CliError::operation)?;
    let export = crate::resolve_cli_export(export)?;
    crate::print_query_report(
        paths,
        cli.output,
        &QueryReport {
            query: ast,
            notes: notes_report.notes,
        },
        list_controls,
        crate::QueryReportRenderOptions {
            format,
            glob,
            explain: false,
            stdout_is_tty,
            use_color: use_stdout_color,
            no_header: cli.no_header,
            export: export.as_ref(),
        },
    )?;
    Ok(())
}

pub(crate) fn handle_tags_command(
    cli: &Cli,
    paths: &VaultPaths,
    filters: &[String],
    sort: crate::TagSortArg,
    show_count: bool,
    list_controls: &ListOutputControls,
) -> Result<(), CliError> {
    let read_filter = selected_read_permission_filter(cli, paths)?;
    let report = query_notes_with_filter(
        paths,
        &NoteQuery {
            filters: filters.to_vec(),
            sort_by: None,
            sort_descending: false,
        },
        read_filter.as_ref(),
    )
    .map_err(CliError::operation)?;

    let mut counts = BTreeMap::<String, usize>::new();
    for note in report.notes {
        let mut seen = BTreeSet::new();
        for tag in note.tags {
            if seen.insert(tag.clone()) {
                *counts.entry(tag).or_default() += 1;
            }
        }
    }

    let mut tags = counts
        .into_iter()
        .map(|(name, count)| NamedCount { name, count })
        .collect::<Vec<_>>();
    sort_tag_counts(&mut tags, sort);
    print_tag_counts(cli.output, &tags, show_count, list_controls)
}

pub(crate) fn handle_properties_command(
    cli: &Cli,
    paths: &VaultPaths,
    sort: crate::PropertySortArg,
    show_count: bool,
    show_types: bool,
    list_controls: &ListOutputControls,
) -> Result<(), CliError> {
    let mut properties = list_properties(paths).map_err(CliError::operation)?;
    sort_property_catalog(&mut properties, sort);
    print_property_catalog(
        cli.output,
        &properties,
        show_count,
        show_types,
        list_controls,
    )
}

pub(crate) fn handle_update_command(
    cli: &Cli,
    paths: &VaultPaths,
    filters: &[String],
    stdin: bool,
    key: &str,
    value: &str,
    dry_run: bool,
    no_commit: bool,
) -> Result<(), CliError> {
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit, cli.quiet);
    let guard = selected_permission_guard(cli, paths)?;
    let selection = resolve_bulk_note_selection(filters, stdin)?;
    let note_paths = match &selection {
        BulkNoteSelection::Filters(filters) => query_notes_with_filter(
            paths,
            &NoteQuery {
                filters: filters.clone(),
                sort_by: Some("file.path".to_string()),
                sort_descending: false,
            },
            Some(&guard.read_filter()),
        )
        .map_err(CliError::operation)?
        .notes
        .into_iter()
        .map(|note| note.document_path)
        .collect::<Vec<_>>(),
        BulkNoteSelection::Paths(note_paths) => note_paths.clone(),
    };
    for path in &note_paths {
        guard.check_write_path(path).map_err(CliError::operation)?;
    }
    let report = bulk_set_property_on_paths(paths, &note_paths, key, Some(value), dry_run)
        .map_err(CliError::operation)?;
    if !dry_run {
        auto_commit
            .commit(
                paths,
                "update",
                &crate::bulk_mutation_changed_files(&report),
                cli.permissions.as_deref(),
                cli.quiet,
            )
            .map_err(CliError::operation)?;
    }
    crate::print_bulk_mutation_report(cli.output, &report)
}

pub(crate) fn handle_unset_command(
    cli: &Cli,
    paths: &VaultPaths,
    filters: &[String],
    stdin: bool,
    key: &str,
    dry_run: bool,
    no_commit: bool,
) -> Result<(), CliError> {
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit, cli.quiet);
    let guard = selected_permission_guard(cli, paths)?;
    let selection = resolve_bulk_note_selection(filters, stdin)?;
    let note_paths = match &selection {
        BulkNoteSelection::Filters(filters) => query_notes_with_filter(
            paths,
            &NoteQuery {
                filters: filters.clone(),
                sort_by: Some("file.path".to_string()),
                sort_descending: false,
            },
            Some(&guard.read_filter()),
        )
        .map_err(CliError::operation)?
        .notes
        .into_iter()
        .map(|note| note.document_path)
        .collect::<Vec<_>>(),
        BulkNoteSelection::Paths(note_paths) => note_paths.clone(),
    };
    for path in &note_paths {
        guard.check_write_path(path).map_err(CliError::operation)?;
    }
    let report = bulk_set_property_on_paths(paths, &note_paths, key, None, dry_run)
        .map_err(CliError::operation)?;
    if !dry_run {
        auto_commit
            .commit(
                paths,
                "unset",
                &crate::bulk_mutation_changed_files(&report),
                cli.permissions.as_deref(),
                cli.quiet,
            )
            .map_err(CliError::operation)?;
    }
    crate::print_bulk_mutation_report(cli.output, &report)
}

pub(crate) fn handle_notes_command(
    cli: &Cli,
    paths: &VaultPaths,
    filters: &[String],
    sort: Option<&String>,
    desc: bool,
    export: &crate::ExportArgs,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    let read_filter = selected_read_permission_filter(cli, paths)?;
    let report = query_notes_with_filter(
        paths,
        &NoteQuery {
            filters: filters.to_vec(),
            sort_by: sort.cloned(),
            sort_descending: desc,
        },
        read_filter.as_ref(),
    )
    .map_err(CliError::operation)?;
    let export = crate::resolve_cli_export(export)?;
    crate::print_notes_report(
        cli.output,
        &report,
        list_controls,
        stdout_is_tty,
        use_stdout_color,
        export.as_ref(),
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_search_command(
    cli: &Cli,
    paths: &VaultPaths,
    query: Option<&str>,
    regex: Option<&str>,
    filters: &[String],
    mode: crate::SearchMode,
    tag: Option<&str>,
    path_prefix: Option<&str>,
    has_property: Option<&str>,
    sort: Option<crate::SearchSortArg>,
    match_case: bool,
    context_size: usize,
    raw_query: bool,
    fuzzy: bool,
    explain: bool,
    exit_code: bool,
    export: &crate::ExportArgs,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    let effective_query = match (query, regex) {
        (Some(_), Some(_)) => {
            return Err(CliError::operation(
                "provide either a query string or --regex, not both",
            ));
        }
        (Some(query), None) => query.to_string(),
        (None, Some(regex)) => format!("/{regex}/"),
        (None, None) => {
            return Err(CliError::operation(
                "provide a search query or --regex pattern",
            ));
        }
    };
    let read_filter = selected_read_permission_filter(cli, paths)?;
    let report = search_vault_with_filter(
        paths,
        &SearchQuery {
            text: effective_query,
            tag: tag.map(ToString::to_string),
            path_prefix: path_prefix.map(ToString::to_string),
            has_property: has_property.map(ToString::to_string),
            filters: filters.to_vec(),
            provider: cli.provider.clone(),
            mode: crate::cli_search_mode(mode),
            sort: sort.map(crate::cli_search_sort),
            match_case: match_case.then_some(true),
            limit: cli.limit.map(|limit| limit.saturating_add(cli.offset)),
            context_size,
            raw_query,
            fuzzy,
            explain,
        },
        read_filter.as_ref(),
    )
    .map_err(CliError::operation)?;
    let export = crate::resolve_cli_export(export)?;
    crate::print_search_report(
        cli.output,
        &report,
        list_controls,
        stdout_is_tty,
        use_stdout_color,
        export.as_ref(),
    )?;
    if exit_code && report.hits.is_empty() {
        return Err(CliError::no_results());
    }
    Ok(())
}

/// Returns `true` when `input` looks like a Dataview Query Language (DQL) query rather than
/// vulcan's native query DSL.  DQL queries start with TABLE, LIST, TASK, or CALENDAR.
fn looks_like_dql(input: &str) -> bool {
    let first_word = input.split_whitespace().next().unwrap_or("");
    matches!(
        first_word.to_ascii_uppercase().as_str(),
        "TABLE" | "LIST" | "TASK" | "CALENDAR"
    )
}

fn sort_tag_counts(tags: &mut [NamedCount], sort: crate::TagSortArg) {
    match sort {
        crate::TagSortArg::Count => tags.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.name.cmp(&right.name))
        }),
        crate::TagSortArg::Name => tags.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| right.count.cmp(&left.count))
        }),
    }
}

fn print_tag_counts(
    output: crate::OutputFormat,
    tags: &[NamedCount],
    show_count: bool,
    list_controls: &ListOutputControls,
) -> Result<(), CliError> {
    let visible = paginated_items(tags, list_controls);
    let rows = tag_rows(visible);
    match output {
        crate::OutputFormat::Human | crate::OutputFormat::Markdown => {
            if visible.is_empty() {
                println!("No tags found.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
                return Ok(());
            }
            for tag in visible {
                if show_count {
                    println!("{} ({})", tag.name, tag.count);
                } else {
                    println!("{}", tag.name);
                }
            }
            Ok(())
        }
        crate::OutputFormat::Json => print_json_lines(rows, list_controls.fields.as_deref()),
    }
}

fn tag_rows(tags: &[NamedCount]) -> Vec<Value> {
    tags.iter()
        .map(|tag| json!({ "tag": tag.name, "count": tag.count }))
        .collect()
}

fn sort_property_catalog(properties: &mut [PropertyCatalogEntry], sort: crate::PropertySortArg) {
    match sort {
        crate::PropertySortArg::Count => properties.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.key.cmp(&right.key))
        }),
        crate::PropertySortArg::Name => properties.sort_by(|left, right| {
            left.key
                .cmp(&right.key)
                .then_with(|| right.count.cmp(&left.count))
        }),
    }
}

fn print_property_catalog(
    output: crate::OutputFormat,
    properties: &[PropertyCatalogEntry],
    show_count: bool,
    show_types: bool,
    list_controls: &ListOutputControls,
) -> Result<(), CliError> {
    let visible = paginated_items(properties, list_controls);
    let rows = property_catalog_rows(visible);
    match output {
        crate::OutputFormat::Human | crate::OutputFormat::Markdown => {
            if visible.is_empty() {
                println!("No properties found.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in &rows {
                    print_selected_human_fields(row, fields);
                }
                return Ok(());
            }
            for property in visible {
                match (show_count, show_types) {
                    (true, true) => {
                        println!(
                            "{} ({}) [{}]",
                            property.key,
                            property.count,
                            property.types.join(", ")
                        );
                    }
                    (true, false) => println!("{} ({})", property.key, property.count),
                    (false, true) => println!("{} [{}]", property.key, property.types.join(", ")),
                    (false, false) => println!("{}", property.key),
                }
            }
            Ok(())
        }
        crate::OutputFormat::Json => print_json_lines(rows, list_controls.fields.as_deref()),
    }
}

fn print_query_field_catalog(
    cli: &Cli,
    paths: &VaultPaths,
    list_controls: &ListOutputControls,
) -> Result<(), CliError> {
    let rows = list_query_fields(paths)
        .map_err(CliError::operation)?
        .into_iter()
        .map(|entry| {
            json!({
                "field": entry.field,
                "kind": entry.kind,
                "supports": entry.supports,
                "types": entry.types,
                "example": entry.example,
            })
        })
        .collect::<Vec<_>>();
    let visible_rows = paginated_items(&rows, list_controls);

    match cli.output {
        crate::OutputFormat::Human | crate::OutputFormat::Markdown => {
            if visible_rows.is_empty() {
                println!("No query fields discovered.");
                return Ok(());
            }
            if let Some(fields) = list_controls.fields.as_deref() {
                for row in visible_rows {
                    print_selected_human_fields(row, fields);
                }
                return Ok(());
            }
            for row in visible_rows {
                let supports = row["supports"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",");
                let types = row["types"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",");
                println!(
                    "- {} [{}] supports={} types={} example={}",
                    row["field"].as_str().unwrap_or_default(),
                    row["kind"].as_str().unwrap_or_default(),
                    supports,
                    types,
                    render_human_value(&row["example"])
                );
            }
            Ok(())
        }
        crate::OutputFormat::Json => {
            print_json_lines(visible_rows.to_vec(), list_controls.fields.as_deref())
        }
    }
}

fn property_catalog_rows(properties: &[PropertyCatalogEntry]) -> Vec<Value> {
    properties
        .iter()
        .map(|property| {
            json!({
                "property": property.key,
                "count": property.count,
                "types": property.types,
            })
        })
        .collect()
}
