use crate::{selected_permission_guard, Cli, CliError, DataviewCommand, PermissionGuard};
use vulcan_core::{load_vault_config, VaultPaths};

pub(crate) fn handle_dataview_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &DataviewCommand,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    let display_result_count = load_vault_config(paths)
        .config
        .dataview
        .display_result_count;

    match command {
        DataviewCommand::Inline { file } => {
            let guard = selected_permission_guard(cli, paths)?;
            let report = crate::run_dataview_inline_command(paths, file, Some(&guard))?;
            crate::print_dataview_inline_report(cli.output, &report)
        }
        DataviewCommand::Query { dql } => {
            let read_filter = selected_permission_guard(cli, paths)?.read_filter();
            let result = crate::run_dataview_query_command(paths, dql, Some(&read_filter))?;
            crate::print_dql_query_result(
                cli.output,
                &result,
                display_result_count,
                stdout_is_tty,
                use_stdout_color,
            )
        }
        DataviewCommand::QueryJs { js, file } => {
            let result = crate::run_dataview_query_js_command(
                paths,
                js,
                file.as_deref(),
                cli.permissions.as_deref(),
            )?;
            crate::print_dataview_js_result(
                cli.output,
                &result,
                display_result_count,
                stdout_is_tty,
                use_stdout_color,
            )
        }
        DataviewCommand::Eval { file, block } => {
            let guard = selected_permission_guard(cli, paths)?;
            let report = crate::run_dataview_eval_command(
                paths,
                file,
                *block,
                cli.permissions.as_deref(),
                Some(&guard),
            )?;
            crate::print_dataview_eval_report(
                cli.output,
                &report,
                display_result_count,
                stdout_is_tty,
                use_stdout_color,
            )
        }
    }
}
