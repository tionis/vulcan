use crate::{js_repl, Cli, CliError, GitCommand, WebCommand};
use std::io::{self, IsTerminal};
use vulcan_core::{git_commit, VaultPaths};

pub(crate) fn handle_git_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &GitCommand,
) -> Result<(), CliError> {
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
            let report = git_commit(paths.vault_root(), message).map_err(CliError::operation)?;
            crate::print_git_commit_report(cli.output, &report)
        }
        GitCommand::Blame { path } => {
            let report = crate::run_git_blame_command(paths, path)?;
            crate::print_git_blame_report(cli.output, &report)
        }
    }
}

pub(crate) fn handle_run_command(
    cli: &Cli,
    paths: &VaultPaths,
    script: Option<&str>,
    script_mode: bool,
    timeout: Option<&str>,
    sandbox: Option<&str>,
) -> Result<(), CliError> {
    let timeout = crate::parse_run_timeout(timeout)?;
    let sandbox = crate::parse_run_sandbox(sandbox)?;
    if script.is_none() && io::stdin().is_terminal() {
        js_repl::run_js_repl(paths, cli.output, timeout, sandbox)
    } else {
        let result = crate::run_js_command(paths, script, script_mode, timeout, sandbox)?;
        crate::print_dataview_js_result(cli.output, &result, false)
    }
}

pub(crate) fn handle_web_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &WebCommand,
) -> Result<(), CliError> {
    match command {
        WebCommand::Search {
            query,
            backend,
            limit,
        } => {
            let report = crate::run_web_search_command(paths, query, backend.as_deref(), *limit)?;
            crate::print_web_search_report(cli.output, &report)
        }
        WebCommand::Fetch {
            url,
            mode,
            save,
            extract_article,
        } => {
            let report =
                crate::run_web_fetch_command(paths, url, *mode, save.as_ref(), *extract_article)?;
            crate::print_web_fetch_report(cli.output, &report)
        }
    }
}
