use crate::{Cli, CliError, DataviewCommand};
use vulcan_core::{load_vault_config, VaultPaths};

pub(crate) fn handle_dataview_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &DataviewCommand,
) -> Result<(), CliError> {
    let display_result_count = load_vault_config(paths)
        .config
        .dataview
        .display_result_count;

    match command {
        DataviewCommand::Inline { file } => {
            let report = crate::run_dataview_inline_command(paths, file)?;
            crate::print_dataview_inline_report(cli.output, &report)
        }
        DataviewCommand::Query { dql } => {
            let result = crate::run_dataview_query_command(paths, dql)?;
            crate::print_dql_query_result(cli.output, &result, display_result_count)
        }
        DataviewCommand::QueryJs { js, file } => {
            let result = crate::run_dataview_query_js_command(paths, js, file.as_deref())?;
            crate::print_dataview_js_result(cli.output, &result, display_result_count)
        }
        DataviewCommand::Eval { file, block } => {
            let report = crate::run_dataview_eval_command(paths, file, *block)?;
            crate::print_dataview_eval_report(cli.output, &report, display_result_count)
        }
    }
}
