#![allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]

use crate::commit::AutoCommitPolicy;
use crate::output::ListOutputControls;
use crate::resolve::resolve_note_argument;
use crate::{warn_auto_commit_if_needed, Cli, CliError, QueryEngineArg};
use vulcan_core::{
    bulk_set_property, evaluate_dql, execute_query_report, load_vault_config, query_backlinks,
    query_links, query_notes, search_vault, NoteQuery, QueryAst, QueryReport, SearchQuery,
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
    let report = query_backlinks(paths, &note).map_err(CliError::operation)?;
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
    let report = query_links(paths, &note).map_err(CliError::operation)?;
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

pub(crate) fn handle_query_command(
    cli: &Cli,
    paths: &VaultPaths,
    dsl: Option<&str>,
    json: Option<&str>,
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
    let use_dql = match engine {
        QueryEngineArg::Dql => true,
        QueryEngineArg::Dsl => false,
        QueryEngineArg::Auto => dsl.map_or(false, looks_like_dql),
    };

    if use_dql {
        let dql = dsl.ok_or_else(|| {
            CliError::operation("DQL engine requires a positional query string, not --json")
        })?;
        let result = evaluate_dql(paths, dql, None).map_err(CliError::operation)?;
        let display_result_count = load_vault_config(paths)
            .config
            .dataview
            .display_result_count;
        return crate::print_dql_query_result(cli.output, &result, display_result_count);
    }

    let ast = match (dsl, json) {
        (Some(_), Some(_)) => {
            return Err(CliError::operation(
                "provide either a DSL argument or --json, not both",
            ));
        }
        (Some(dsl), None) => QueryAst::from_dsl(dsl).map_err(CliError::operation)?,
        (None, Some(json)) => QueryAst::from_json(json).map_err(CliError::operation)?,
        (None, None) => {
            return Err(CliError::operation(
                "provide a DSL query argument or --json payload",
            ));
        }
    };
    let report = execute_query_report(paths, ast).map_err(CliError::operation)?;
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
    let notes_report = query_notes(paths, &note_query).map_err(CliError::operation)?;
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

pub(crate) fn handle_update_command(
    cli: &Cli,
    paths: &VaultPaths,
    filters: &[String],
    key: &str,
    value: &str,
    dry_run: bool,
    no_commit: bool,
) -> Result<(), CliError> {
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit, cli.quiet);
    let report = bulk_set_property(paths, filters, key, Some(value), dry_run)
        .map_err(CliError::operation)?;
    if !dry_run {
        auto_commit
            .commit(
                paths,
                "update",
                &crate::bulk_mutation_changed_files(&report),
            )
            .map_err(CliError::operation)?;
    }
    crate::print_bulk_mutation_report(cli.output, &report)
}

pub(crate) fn handle_unset_command(
    cli: &Cli,
    paths: &VaultPaths,
    filters: &[String],
    key: &str,
    dry_run: bool,
    no_commit: bool,
) -> Result<(), CliError> {
    let auto_commit = AutoCommitPolicy::for_mutation(paths, no_commit);
    warn_auto_commit_if_needed(&auto_commit, cli.quiet);
    let report =
        bulk_set_property(paths, filters, key, None, dry_run).map_err(CliError::operation)?;
    if !dry_run {
        auto_commit
            .commit(paths, "unset", &crate::bulk_mutation_changed_files(&report))
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
    let report = query_notes(
        paths,
        &NoteQuery {
            filters: filters.to_vec(),
            sort_by: sort.cloned(),
            sort_descending: desc,
        },
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
    let report = search_vault(
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

