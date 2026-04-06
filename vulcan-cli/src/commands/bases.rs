#![allow(clippy::too_many_lines)]

use crate::bases_tui;
use crate::commit::AutoCommitPolicy;
use crate::output::ListOutputControls;
use crate::{warn_auto_commit_if_needed, BasesCommand, Cli, CliError, OutputFormat};
use std::io::{self, IsTerminal};
use vulcan_core::{
    bases_view_add, bases_view_delete, bases_view_edit, bases_view_rename, evaluate_base_file,
    BaseViewGroupBy, BaseViewPatch, BaseViewSpec, VaultPaths,
};

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
            crate::print_bases_report(
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
                    .commit(paths, "bases-create", std::slice::from_ref(&report.path))
                    .map_err(CliError::operation)?;
            }
            crate::print_bases_create_report(cli.output, &report)
        }
        BasesCommand::Tui { file } => {
            let report = evaluate_base_file(paths, file).map_err(CliError::operation)?;
            if cli.output == OutputFormat::Human && stdout_is_tty && io::stdin().is_terminal() {
                bases_tui::run_bases_tui(paths, file, &report).map_err(CliError::operation)
            } else {
                crate::print_bases_report(
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
                    .commit(paths, "bases-view-add", std::slice::from_ref(file))
                    .map_err(CliError::operation)?;
            }
            crate::print_bases_view_edit_report(cli.output, &report)
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
                    .commit(paths, "bases-view-delete", std::slice::from_ref(file))
                    .map_err(CliError::operation)?;
            }
            crate::print_bases_view_edit_report(cli.output, &report)
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
                    .commit(paths, "bases-view-rename", std::slice::from_ref(file))
                    .map_err(CliError::operation)?;
            }
            crate::print_bases_view_edit_report(cli.output, &report)
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
                    .commit(paths, "bases-view-edit", std::slice::from_ref(file))
                    .map_err(CliError::operation)?;
            }
            crate::print_bases_view_edit_report(cli.output, &report)
        }
    }
}
