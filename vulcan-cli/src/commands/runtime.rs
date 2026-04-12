use crate::{
    js_repl, selected_permission_guard, Cli, CliError, GitCommand, PermissionGuard, WebCommand,
};
use std::io::{self, IsTerminal};
use vulcan_core::{git_commit, PluginEvent, VaultPaths};

pub(crate) fn handle_git_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &GitCommand,
) -> Result<(), CliError> {
    selected_permission_guard(cli, paths)?
        .check_git()
        .map_err(CliError::operation)?;
    match command {
        GitCommand::Status => {
            let mut report = crate::git_status(paths.vault_root()).map_err(CliError::operation)?;
            report.staged = crate::filter_vault_git_paths(report.staged);
            report.unstaged = crate::filter_vault_git_paths(report.unstaged);
            report.untracked = crate::filter_vault_git_paths(report.untracked);
            report.clean = report.staged.is_empty()
                && report.unstaged.is_empty()
                && report.untracked.is_empty();
            crate::print_git_status_report(cli.output, &report)
        }
        GitCommand::Log { limit } => {
            let report = crate::run_git_log_command(paths, *limit)?;
            crate::print_git_log_report(cli.output, &report)
        }
        GitCommand::Diff { path } => {
            let report = crate::run_git_diff_group_command(paths, path.as_deref())?;
            crate::print_git_diff_group_report(cli.output, &report)
        }
        GitCommand::Commit { message } => {
            crate::plugins::dispatch_plugin_event(
                paths,
                cli.permissions.as_deref(),
                PluginEvent::OnPreCommit,
                &serde_json::json!({
                    "kind": PluginEvent::OnPreCommit,
                    "action": "git-commit",
                    "message": message,
                }),
                cli.quiet,
            )?;
            let report = git_commit(paths.vault_root(), message).map_err(CliError::operation)?;
            if report.committed {
                let _ = crate::plugins::dispatch_plugin_event(
                    paths,
                    cli.permissions.as_deref(),
                    PluginEvent::OnPostCommit,
                    &serde_json::json!({
                        "kind": PluginEvent::OnPostCommit,
                        "action": "git-commit",
                        "message": report.message,
                        "files": report.files,
                        "sha": report.sha,
                    }),
                    cli.quiet,
                );
            }
            crate::print_git_commit_report(cli.output, &report)
        }
        GitCommand::Blame { path } => {
            let report = crate::run_git_blame_command(paths, path)?;
            crate::print_git_blame_report(cli.output, &report)
        }
    }
}

pub(crate) struct RunArgs<'a> {
    pub script: Option<&'a str>,
    pub script_mode: bool,
    pub eval: &'a [String],
    pub eval_file: Option<&'a str>,
    pub timeout: Option<&'a str>,
    pub sandbox: Option<&'a str>,
    pub no_startup: bool,
}

pub(crate) fn handle_run_command(
    cli: &Cli,
    paths: &VaultPaths,
    args: &RunArgs<'_>,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    let timeout = crate::parse_run_timeout(args.timeout)?;
    let sandbox = crate::parse_run_sandbox(args.sandbox)?;
    selected_permission_guard(cli, paths)?
        .check_execute()
        .map_err(CliError::operation)?;

    // --eval/-e: evaluate one or more code snippets sequentially and print each result.
    if !args.eval.is_empty() {
        for code in args.eval {
            let result =
                crate::run_js_eval(paths, code, timeout, sandbox, cli.permissions.as_deref())?;
            crate::print_dataview_js_result(
                cli.output,
                &result,
                false,
                stdout_is_tty,
                use_stdout_color,
            )?;
        }
        return Ok(());
    }

    // --eval-file: load a file into the JS context, then start the REPL.
    if let Some(path) = args.eval_file {
        js_repl::run_js_repl_with_preload(
            paths,
            cli.output,
            timeout,
            sandbox,
            cli.permissions.as_deref(),
            path,
        )
    } else if args.script.is_none() && io::stdin().is_terminal() {
        js_repl::run_js_repl(
            paths,
            cli.output,
            timeout,
            sandbox,
            cli.permissions.as_deref(),
            args.no_startup,
        )
    } else {
        let result = crate::run_js_command(
            paths,
            args.script,
            args.script_mode,
            timeout,
            sandbox,
            cli.permissions.as_deref(),
        )?;
        crate::print_dataview_js_result(cli.output, &result, false, stdout_is_tty, use_stdout_color)
    }
}

pub(crate) fn handle_web_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &WebCommand,
    stdout_is_tty: bool,
    use_stdout_color: bool,
) -> Result<(), CliError> {
    let guard = selected_permission_guard(cli, paths)?;
    match command {
        WebCommand::Search {
            query,
            backend,
            limit,
        } => {
            let report =
                crate::run_web_search_command(paths, query, *backend, *limit, Some(&guard))?;
            crate::print_web_search_report(cli.output, &report)
        }
        WebCommand::Fetch { url, mode, save } => {
            let report =
                crate::run_web_fetch_command(paths, url, *mode, save.as_ref(), Some(&guard))?;
            crate::print_web_fetch_report(cli.output, &report, stdout_is_tty, use_stdout_color)
        }
    }
}
