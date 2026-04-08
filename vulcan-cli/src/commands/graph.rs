use crate::output::ListOutputControls;
use crate::resolve::resolve_note_argument;
use crate::{selected_read_permission_filter, Cli, CliError, GraphCommand};
use vulcan_core::{
    export_graph_with_filter, query_graph_analytics_with_filter,
    query_graph_components_with_filter, query_graph_dead_ends_with_filter,
    query_graph_hubs_with_filter, query_graph_moc_candidates_with_filter,
    query_graph_path_with_filter, query_graph_trends, VaultPaths,
};

pub(crate) fn handle_graph_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &GraphCommand,
    interactive_note_selection: bool,
    list_controls: &ListOutputControls,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    match command {
        GraphCommand::Path { from, to } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let from = resolve_note_argument(
                paths,
                from.as_deref(),
                interactive_note_selection,
                "from note",
            )?;
            let to =
                resolve_note_argument(paths, to.as_deref(), interactive_note_selection, "to note")?;
            let report = query_graph_path_with_filter(paths, &from, &to, read_filter.as_ref())
                .map_err(CliError::operation)?;
            crate::print_graph_path_report(cli.output, &report)
        }
        GraphCommand::Hubs { export } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let report = query_graph_hubs_with_filter(paths, read_filter.as_ref())
                .map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            crate::print_graph_hubs_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )
        }
        GraphCommand::Moc { export } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let report = query_graph_moc_candidates_with_filter(paths, read_filter.as_ref())
                .map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            crate::print_graph_moc_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )
        }
        GraphCommand::DeadEnds { export } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let report = query_graph_dead_ends_with_filter(paths, read_filter.as_ref())
                .map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            crate::print_graph_dead_ends_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )
        }
        GraphCommand::Components { export } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let report = query_graph_components_with_filter(paths, read_filter.as_ref())
                .map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            crate::print_graph_components_report(
                cli.output,
                &report,
                list_controls,
                stdout_is_tty,
                use_stdout_color,
                export.as_ref(),
            )
        }
        GraphCommand::Stats { export } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let report = query_graph_analytics_with_filter(paths, read_filter.as_ref())
                .map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            crate::print_graph_analytics_report(cli.output, &report, export.as_ref())
        }
        GraphCommand::Trends { limit, export } => {
            let report = query_graph_trends(paths, *limit).map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            crate::print_graph_trends_report(cli.output, &report, list_controls, export.as_ref())
        }
        GraphCommand::Export { format } => {
            let read_filter = selected_read_permission_filter(cli, paths)?;
            let report = export_graph_with_filter(paths, read_filter.as_ref())
                .map_err(CliError::operation)?;
            crate::print_graph_export_report(cli.output, &report, *format)
        }
    }
}
