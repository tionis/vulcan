use crate::output::ListOutputControls;
use crate::resolve::resolve_note_argument;
use crate::{Cli, CliError, GraphCommand};
use vulcan_core::{
    query_graph_analytics, query_graph_components, query_graph_dead_ends, query_graph_hubs,
    query_graph_moc_candidates, query_graph_path, query_graph_trends, VaultPaths,
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
            let from = resolve_note_argument(
                paths,
                from.as_deref(),
                interactive_note_selection,
                "from note",
            )?;
            let to =
                resolve_note_argument(paths, to.as_deref(), interactive_note_selection, "to note")?;
            let report = query_graph_path(paths, &from, &to).map_err(CliError::operation)?;
            crate::print_graph_path_report(cli.output, &report)
        }
        GraphCommand::Hubs { export } => {
            let report = query_graph_hubs(paths).map_err(CliError::operation)?;
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
            let report = query_graph_moc_candidates(paths).map_err(CliError::operation)?;
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
            let report = query_graph_dead_ends(paths).map_err(CliError::operation)?;
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
            let report = query_graph_components(paths).map_err(CliError::operation)?;
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
            let report = query_graph_analytics(paths).map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            crate::print_graph_analytics_report(cli.output, &report, export.as_ref())
        }
        GraphCommand::Trends { limit, export } => {
            let report = query_graph_trends(paths, *limit).map_err(CliError::operation)?;
            let export = crate::resolve_cli_export(export)?;
            crate::print_graph_trends_report(cli.output, &report, list_controls, export.as_ref())
        }
    }
}
