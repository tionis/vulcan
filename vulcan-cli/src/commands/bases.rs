#![allow(clippy::too_many_lines)]

use crate::bases_tui;
use crate::commit::AutoCommitPolicy;
use crate::output::{
    markdown_table_header_lines, markdown_table_row, paginated_items, print_json, print_json_lines,
    print_selected_human_fields, render_human_value, ListOutputControls,
};
use crate::{
    export_rows, print_markdown_output, warn_auto_commit_if_needed, AnsiPalette, BasesCommand,
    BasesCreateReport, Cli, CliError, OutputFormat, ResolvedExport,
};
use serde_json::Value;
use std::io::{self, IsTerminal};
use vulcan_core::{
    bases_view_add, bases_view_delete, bases_view_edit, bases_view_rename, evaluate_base_file,
    BaseViewGroupBy, BaseViewPatch, BaseViewSpec, BasesEvalReport, BasesViewEditReport, VaultPaths,
};

pub(crate) fn print_bases_report(
    output: OutputFormat,
    report: &BasesEvalReport,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_color: bool,
    export: Option<&ResolvedExport>,
) -> Result<(), CliError> {
    let rows = bases_rows(report);
    let visible_rows = paginated_items(&rows, list_controls);
    let palette = AnsiPalette::new(use_color);

    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if output == OutputFormat::Human && stdout_is_tty {
                println!(
                    "{} {}",
                    palette.cyan("Bases eval"),
                    palette.bold(&report.file)
                );
            }
            if rows.is_empty() {
                println!("No bases rows.");
            } else if let Some(fields) = list_controls.fields.as_deref() {
                for row in visible_rows {
                    print_selected_human_fields(row, fields);
                }
            } else {
                print_markdown_output(
                    output,
                    &render_bases_markdown(report, list_controls),
                    stdout_is_tty,
                    use_color,
                )?;
            }

            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            Ok(())
        }
        OutputFormat::Json => {
            export_rows(visible_rows, list_controls.fields.as_deref(), export)?;
            if list_controls.fields.is_some() {
                print_json_lines(visible_rows.to_vec(), list_controls.fields.as_deref())
            } else {
                print_json(report)
            }
        }
    }
}

pub(crate) fn print_bases_view_edit_report(
    output: OutputFormat,
    report: &BasesViewEditReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!("Dry run: {}", report.action);
            } else {
                println!("{}", report.action);
            }
            println!(
                "{} views, {} diagnostics",
                report.eval.views.len(),
                report.eval.diagnostics.len()
            );
            for diag in &report.eval.diagnostics {
                let path = diag.path.as_deref().unwrap_or("(root)");
                println!("  warning [{path}]: {}", diag.message);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

pub(crate) fn print_bases_create_report(
    output: OutputFormat,
    report: &BasesCreateReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.dry_run {
                println!("Would create {} from {}.", report.path, report.file);
            } else {
                println!("Created {} from {}.", report.path, report.file);
            }

            let view = report
                .view_name
                .as_deref()
                .map_or_else(|| format!("#{}", report.view_index + 1), ToOwned::to_owned);
            println!("View: {view}");
            println!(
                "Folder: {}",
                report.folder.as_deref().unwrap_or("<vault root>")
            );
            println!(
                "Template: {}",
                report.template.as_deref().unwrap_or("<none>")
            );

            if report.properties.is_empty() {
                println!("Properties: <none>");
            } else {
                println!("Properties:");
                for (key, value) in &report.properties {
                    println!("  {key}: {}", render_human_value(value));
                }
            }

            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

pub(crate) fn bases_rows(report: &BasesEvalReport) -> Vec<Value> {
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
                    "columns": view.columns,
                    "group_by": view.group_by,
                    "group_value": row.group_value,
                    "document_path": row.document_path,
                    "file_name": row.file_name,
                    "file_ext": row.file_ext,
                    "file_mtime": row.file_mtime,
                    "properties": row.properties,
                    "formulas": row.formulas,
                    "cells": row.cells,
                })
            })
        })
        .collect()
}

pub(crate) fn render_bases_markdown(
    report: &BasesEvalReport,
    list_controls: &ListOutputControls,
) -> String {
    let mut row_index = 0_usize;
    let mut printed_any = false;
    let end = list_controls.limit.map_or(usize::MAX, |limit| {
        list_controls.offset.saturating_add(limit)
    });
    let mut sections = Vec::new();

    for view in &report.views {
        let mut visible_rows = Vec::new();
        for row in &view.rows {
            if row_index < list_controls.offset {
                row_index += 1;
                continue;
            }
            if row_index >= end {
                break;
            }
            visible_rows.push(row);
            row_index += 1;
        }

        if !visible_rows.is_empty() {
            sections.push(render_bases_view_markdown(view, &visible_rows));
            printed_any = true;
        }

        if row_index >= end {
            break;
        }
    }

    if !printed_any {
        sections.push("No bases rows.".to_string());
    }

    if !report.diagnostics.is_empty() {
        let mut diagnostics = vec!["## Diagnostics".to_string()];
        diagnostics.extend(report.diagnostics.iter().map(|diagnostic| {
            if let Some(path) = diagnostic.path.as_deref() {
                format!("- {path}: {}", diagnostic.message)
            } else {
                format!("- {}", diagnostic.message)
            }
        }));
        sections.push(diagnostics.join("\n"));
    }

    sections.join("\n\n")
}

fn render_bases_view_markdown(
    view: &vulcan_core::BasesEvaluatedView,
    rows: &[&vulcan_core::BasesRow],
) -> String {
    let visible_rows = rows.len();
    let name = view.name.as_deref().unwrap_or("view");
    let row_summary = if visible_rows == view.rows.len() {
        format!("{} rows", view.rows.len())
    } else {
        format!("{visible_rows} of {} rows", view.rows.len())
    };
    let mut lines = vec![format!("## {name} ({row_summary})")];
    if !view.columns.is_empty() {
        let columns = view
            .columns
            .iter()
            .map(|column| column.display_name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("Columns: {columns}"));
    }
    if let Some(group_by) = view.group_by.as_ref() {
        lines.push(format!(
            "Grouped by: {}{}",
            group_by.display_name,
            if group_by.descending { " (desc)" } else { "" }
        ));
    }
    lines.push(render_bases_table_markdown(view, rows));
    lines.join("\n\n")
}

fn render_bases_table_markdown(
    view: &vulcan_core::BasesEvaluatedView,
    rows: &[&vulcan_core::BasesRow],
) -> String {
    let group_key = view
        .group_by
        .as_ref()
        .map(|group_by| group_by.property.as_str());
    let mut columns = view
        .columns
        .iter()
        .filter(|column| Some(column.key.as_str()) != group_key)
        .collect::<Vec<_>>();
    if columns.is_empty() {
        columns = view.columns.iter().collect();
    }

    let mut sections = Vec::new();
    if view.group_by.is_some() {
        let mut start = 0_usize;
        while start < rows.len() {
            let group_name = bases_group_name(rows[start]);
            let mut end = start + 1;
            while end < rows.len() && bases_group_name(rows[end]) == group_name {
                end += 1;
            }

            let group_rows = &rows[start..end];
            sections.push(format!(
                "### Group: {group_name} ({} rows)\n\n{}",
                group_rows.len(),
                render_bases_table_block_markdown(&columns, group_rows)
            ));
            start = end;
        }
    } else {
        sections.push(render_bases_table_block_markdown(&columns, rows));
    }
    sections.join("\n\n")
}

fn render_bases_table_block_markdown(
    columns: &[&vulcan_core::BasesColumn],
    rows: &[&vulcan_core::BasesRow],
) -> String {
    let headers = columns
        .iter()
        .map(|column| column.display_name.clone())
        .collect::<Vec<_>>();
    let column_count = headers.len();
    if column_count == 0 {
        return String::new();
    }

    let [header, separator] = markdown_table_header_lines(&headers, column_count);
    let mut lines = vec![header, separator];

    lines.extend(rows.iter().map(|row| {
        markdown_table_row(
            columns
                .iter()
                .map(|column| bases_cell_text(row, &column.key)),
            column_count,
        )
    }));
    lines.join("\n")
}

fn bases_group_name(row: &vulcan_core::BasesRow) -> String {
    row.group_value
        .as_ref()
        .map(render_human_value)
        .filter(|value| !value.is_empty() && value != "null")
        .unwrap_or_else(|| "Ungrouped".to_string())
}

fn bases_cell_text(row: &vulcan_core::BasesRow, key: &str) -> String {
    bases_value_for_key(row, key)
        .filter(|value| !value.is_null())
        .map(|value| render_human_value(&value))
        .filter(|value| !value.is_empty() && value != "null")
        .unwrap_or_else(|| "-".to_string())
}

fn bases_value_for_key(row: &vulcan_core::BasesRow, key: &str) -> Option<Value> {
    if let Some(value) = row.cells.get(key) {
        return Some(value.clone());
    }
    if let Some(value) = row.formulas.get(key) {
        return Some(value.clone());
    }

    match key {
        "file.path" => Some(Value::String(row.document_path.clone())),
        "file.name" => Some(Value::String(row.file_name.clone())),
        "file.ext" => Some(Value::String(row.file_ext.clone())),
        "file.mtime" => Some(Value::Number(row.file_mtime.into())),
        property => row.properties.get(property).cloned(),
    }
}

pub(crate) fn handle_bases_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &BasesCommand,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
    use_stderr_color: bool,
) -> Result<(), CliError> {
    match command {
        BasesCommand::Eval { file, export } => {
            let report = evaluate_base_file(paths, file).map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            print_bases_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )?;
            Ok(())
        }
        BasesCommand::Create {
            file,
            title,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report =
                crate::create_note_from_bases_view(paths, file, 0, title.as_deref(), *dry_run)?;
            if !*dry_run {
                crate::run_incremental_scan(paths, cli.output, use_stderr_color, cli.quiet)?;
                auto_commit
                    .commit(
                        paths,
                        "bases-create",
                        std::slice::from_ref(&report.path),
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_bases_create_report(cli.output, &report)
        }
        BasesCommand::Tui { file } => {
            let report = evaluate_base_file(paths, file).map_err(CliError::operation)?;
            if cli.output == OutputFormat::Human && stdout_is_tty && io::stdin().is_terminal() {
                bases_tui::run_bases_tui(paths, file, &report).map_err(CliError::operation)
            } else {
                print_bases_report(
                    cli.output,
                    &report,
                    list_controls,
                    stdout_is_tty,
                    use_stdout_color,
                    None,
                )
            }
        }
        BasesCommand::ViewAdd {
            file,
            name,
            filters,
            column,
            sort,
            sort_desc,
            group_by,
            group_desc,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let spec = BaseViewSpec {
                name: Some(name.clone()),
                view_type: "table".to_string(),
                filters: filters.clone(),
                sort_by: sort.clone(),
                sort_descending: *sort_desc,
                columns: column.clone(),
                group_by: group_by.as_deref().map(|property| BaseViewGroupBy {
                    property: property.to_string(),
                    descending: *group_desc,
                }),
            };
            let report =
                bases_view_add(paths, file, spec, *dry_run).map_err(CliError::operation)?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "bases-view-add",
                        std::slice::from_ref(file),
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_bases_view_edit_report(cli.output, &report)
        }
        BasesCommand::ViewDelete {
            file,
            name,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report =
                bases_view_delete(paths, file, name, *dry_run).map_err(CliError::operation)?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "bases-view-delete",
                        std::slice::from_ref(file),
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_bases_view_edit_report(cli.output, &report)
        }
        BasesCommand::ViewRename {
            file,
            old_name,
            new_name,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let report = bases_view_rename(paths, file, old_name, new_name, *dry_run)
                .map_err(CliError::operation)?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "bases-view-rename",
                        std::slice::from_ref(file),
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_bases_view_edit_report(cli.output, &report)
        }
        BasesCommand::ViewEdit {
            file,
            name,
            add_filters,
            remove_filters,
            column,
            sort,
            sort_desc,
            group_by,
            group_desc,
            dry_run,
            no_commit,
        } => {
            let auto_commit = AutoCommitPolicy::for_mutation(paths, *no_commit);
            warn_auto_commit_if_needed(&auto_commit, cli.quiet);
            let patch = BaseViewPatch {
                add_filters: add_filters.clone(),
                remove_filters: remove_filters.clone(),
                set_columns: if column.is_empty() {
                    None
                } else {
                    Some(column.clone())
                },
                set_sort: sort.as_deref().map(|value| {
                    if value.is_empty() {
                        None
                    } else {
                        Some(value.to_string())
                    }
                }),
                set_sort_descending: if sort.is_some() {
                    Some(*sort_desc)
                } else {
                    None
                },
                set_group_by: group_by.as_deref().map(|property| {
                    if property.is_empty() {
                        None
                    } else {
                        Some(BaseViewGroupBy {
                            property: property.to_string(),
                            descending: *group_desc,
                        })
                    }
                }),
                ..Default::default()
            };
            let report =
                bases_view_edit(paths, file, name, patch, *dry_run).map_err(CliError::operation)?;
            if !*dry_run {
                auto_commit
                    .commit(
                        paths,
                        "bases-view-edit",
                        std::slice::from_ref(file),
                        cli.permissions.as_deref(),
                        cli.quiet,
                    )
                    .map_err(CliError::operation)?;
            }
            print_bases_view_edit_report(cli.output, &report)
        }
    }
}
