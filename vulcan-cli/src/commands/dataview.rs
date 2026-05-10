use crate::output::{print_json, render_dataview_inline_value};
use crate::{
    print_markdown_output, render_dataview_eval_markdown, selected_permission_guard, Cli, CliError,
    DataviewCommand, OutputFormat, PermissionGuard,
};
use vulcan_app::browse::{
    build_dataview_eval_report, build_dataview_inline_report, build_dataview_query_js_report,
    build_dataview_query_report, DataviewEvalReport, DataviewInlineReport,
};
use vulcan_core::{
    load_vault_config, DataviewJsResult, DqlQueryResult, PermissionFilter, ProfilePermissionGuard,
    VaultPaths,
};

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
            let report = run_dataview_inline_command(paths, file, Some(&guard))?;
            print_dataview_inline_report(cli.output, &report)
        }
        DataviewCommand::Query { dql } => {
            let read_filter = selected_permission_guard(cli, paths)?.read_filter();
            let result = run_dataview_query_command(paths, dql, Some(&read_filter))?;
            crate::print_dql_query_result(
                cli.output,
                &result,
                display_result_count,
                stdout_is_tty,
                use_stdout_color,
            )
        }
        DataviewCommand::QueryJs { js, file } => {
            let result = run_dataview_query_js_command(
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
            let report = run_dataview_eval_command(
                paths,
                file,
                *block,
                cli.permissions.as_deref(),
                Some(&guard),
            )?;
            print_dataview_eval_report(
                cli.output,
                &report,
                display_result_count,
                stdout_is_tty,
                use_stdout_color,
            )
        }
    }
}

fn run_dataview_inline_command(
    paths: &VaultPaths,
    file: &str,
    permissions: Option<&ProfilePermissionGuard>,
) -> Result<DataviewInlineReport, CliError> {
    build_dataview_inline_report(paths, file, permissions).map_err(CliError::operation)
}

fn run_dataview_query_command(
    paths: &VaultPaths,
    dql: &str,
    filter: Option<&PermissionFilter>,
) -> Result<DqlQueryResult, CliError> {
    build_dataview_query_report(paths, dql, None, filter).map_err(CliError::operation)
}

pub(crate) fn run_dataview_query_js_command(
    paths: &VaultPaths,
    js: &str,
    file: Option<&str>,
    permission_profile: Option<&str>,
) -> Result<DataviewJsResult, CliError> {
    build_dataview_query_js_report(paths, js, file, permission_profile).map_err(CliError::operation)
}

fn run_dataview_eval_command(
    paths: &VaultPaths,
    file: &str,
    block: Option<usize>,
    permission_profile: Option<&str>,
    permissions: Option<&ProfilePermissionGuard>,
) -> Result<DataviewEvalReport, CliError> {
    build_dataview_eval_report(paths, file, block, permission_profile, permissions)
        .map_err(CliError::operation)
}

fn print_dataview_inline_report(
    output: OutputFormat,
    report: &DataviewInlineReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.results.is_empty() {
                println!("No inline expressions in {}", report.file);
                return Ok(());
            }
            println!("Dataview inline expressions for {}", report.file);
            for result in &report.results {
                if let Some(error) = &result.error {
                    println!("- {} => error: {}", result.expression, error);
                } else {
                    println!(
                        "- {} => {}",
                        result.expression,
                        render_dataview_inline_value(&result.value)
                    );
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_dataview_eval_report(
    output: OutputFormat,
    report: &DataviewEvalReport,
    show_result_count: bool,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => print_markdown_output(
            output,
            &render_dataview_eval_markdown(report, show_result_count),
            stdout_is_tty,
            use_color,
        ),
        OutputFormat::Json => print_json(report),
    }
}
