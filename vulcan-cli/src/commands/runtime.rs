use crate::output::print_json;
use crate::{
    js_repl, selected_permission_guard, tools, trust, Cli, CliError, GitChangesetCommand,
    GitCommand, OutputFormat, PermissionGuard, TrustCommand, WebCommand,
};
#[cfg(feature = "web")]
use crate::{SearchBackendArg, WebFetchMode};
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::time::Duration;
use vulcan_app::git_ops::{self, GitBlameReport, GitDiffReport, GitLogReport};
use vulcan_app::mdbase::mdbase_js_mutation_committer;
#[cfg(feature = "web")]
use vulcan_app::web::{
    apply_web_fetch_report_with_permissions, build_web_search_report_with_permissions,
    WebFetchMode as AppWebFetchMode, WebFetchReport, WebFetchRequest, WebSearchReport,
    WebSearchRequest,
};
#[cfg(feature = "web")]
use vulcan_core::ProfilePermissionGuard;
#[cfg(feature = "web")]
use vulcan_core::SearchBackendKind;
use vulcan_core::Verbosity;
use vulcan_core::{
    evaluate_dataview_js_with_options, load_vault_config, DataviewJsEvalOptions, DataviewJsResult,
    GitCommitReport, GitStatusReport, JsRuntimeSandbox, VaultPaths,
};

pub(crate) fn handle_trust_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: Option<&TrustCommand>,
) -> Result<(), CliError> {
    match command.unwrap_or(&TrustCommand::Add) {
        TrustCommand::Add => {
            let added = trust::add_trust(paths.vault_root())?;
            if cli.output == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::json!({
                        "trusted": true,
                        "vault": paths.vault_root().display().to_string(),
                        "newly_added": added,
                    })
                );
            } else if added {
                println!("Vault marked as trusted: {}", paths.vault_root().display());
            } else {
                println!("Vault is already trusted: {}", paths.vault_root().display());
            }
        }
        TrustCommand::Revoke => {
            let removed = trust::revoke_trust(paths.vault_root())?;
            if cli.output == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::json!({
                        "trusted": false,
                        "vault": paths.vault_root().display().to_string(),
                        "was_trusted": removed,
                    })
                );
            } else if removed {
                println!("Trust removed from vault: {}", paths.vault_root().display());
            } else {
                println!("Vault was not trusted: {}", paths.vault_root().display());
            }
        }
        TrustCommand::List => {
            let vaults = trust::list_trusted()?;
            if cli.output == OutputFormat::Json {
                let paths_json: Vec<_> = vaults.iter().map(|p| p.display().to_string()).collect();
                println!("{}", serde_json::json!({ "trusted_vaults": paths_json }));
            } else if vaults.is_empty() {
                println!("No trusted vaults.");
            } else {
                println!("Trusted vaults:");
                for vault in &vaults {
                    println!("  {}", vault.display());
                }
            }
        }
    }
    Ok(())
}

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
            let report = git_ops::status_report(paths).map_err(CliError::operation)?;
            print_git_status_report(cli.output, &report)
        }
        GitCommand::Log { limit } => {
            let report = git_ops::log_report(paths, *limit).map_err(CliError::operation)?;
            print_git_log_report(cli.output, &report)
        }
        GitCommand::Diff { path } => {
            let report =
                git_ops::diff_report(paths, path.as_deref()).map_err(CliError::operation)?;
            print_git_diff_group_report(cli.output, &report)
        }
        GitCommand::Commit { message } => {
            let report = git_ops::commit_with_hooks(
                paths,
                cli.permissions.as_deref(),
                message,
                cli.verbosity(),
            )
            .map_err(CliError::operation)?;
            print_git_commit_report(cli.output, &report)
        }
        GitCommand::Blame { path } => {
            let report = git_ops::blame_report(paths, path).map_err(CliError::operation)?;
            print_git_blame_report(cli.output, &report)
        }
        GitCommand::BatchCommit { dry_run } => {
            let registry = vulcan_daemon::registry::WikiRegistry::user_default()
                .map_err(CliError::operation)?;
            let report = vulcan_daemon::auto_commit_worker::run_direct_batch(
                &registry,
                paths,
                cli.permissions.as_deref(),
                *dry_run,
                cli.verbosity(),
            )
            .map_err(CliError::operation)?;
            print_batched_commit_report(cli.output, &report)
        }
        GitCommand::Changeset { command } => handle_changeset_command(cli, paths, command),
    }
}

fn print_changeset_line(changeset: &vulcan_app::changesets::ChangesetInfo) {
    let idle_hours = changeset.idle_seconds / 3600;
    println!(
        "{}  {} commit(s), {} uncommitted, idle {}h{}",
        changeset.id,
        changeset.ahead,
        changeset.uncommitted.len(),
        idle_hours,
        changeset
            .title
            .as_deref()
            .map_or_default(|title| format!("  {title}"))
    );
}

#[allow(clippy::too_many_lines)]
fn handle_changeset_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &GitChangesetCommand,
) -> Result<(), CliError> {
    use vulcan_app::changesets;
    let json = cli.output == OutputFormat::Json;
    match command {
        GitChangesetCommand::List => {
            let pruned = changesets::prune_empty_changesets(paths);
            let list = changesets::list_changesets(paths).map_err(CliError::operation)?;
            if json {
                return crate::output::print_json(&serde_json::json!({
                    "changesets": list,
                    "pruned": pruned.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
                }));
            }
            if list.is_empty() {
                println!("No changesets.");
            }
            for changeset in &list {
                print_changeset_line(changeset);
            }
            Ok(())
        }
        GitChangesetCommand::Show { id } => {
            let preview = changesets::preview_land(paths, id).map_err(CliError::operation)?;
            if json {
                return crate::output::print_json(&preview);
            }
            print_changeset_line(&preview.changeset);
            println!("worktree: {}", preview.changeset.worktree.display());
            for path in &preview.changeset.uncommitted {
                println!("  uncommitted: {path}");
            }
            println!(
                "landing on {}: {}{}",
                preview.branch,
                if preview.needs_rebase {
                    "rebase, then fast-forward"
                } else {
                    "fast-forward"
                },
                if preview.needs_message {
                    "; uncommitted changes need --message"
                } else {
                    ""
                }
            );
            Ok(())
        }
        GitChangesetCommand::Land {
            id,
            message,
            dry_run,
        } => {
            if *dry_run {
                let preview = changesets::preview_land(paths, id).map_err(CliError::operation)?;
                if json {
                    return crate::output::print_json(&preview);
                }
                println!(
                    "Would land {} commit(s) of `{id}` on {}{}",
                    preview.changeset.ahead,
                    preview.branch,
                    if preview.needs_rebase {
                        " after rebasing"
                    } else {
                        ""
                    }
                );
                return Ok(());
            }
            let commit = message
                .as_deref()
                .map(|message| changesets::ChangesetLandCommit {
                    message,
                    trailers: &[],
                    permission_profile: cli.permissions.as_deref(),
                });
            let report = changesets::land_changeset(paths, id, commit.as_ref())
                .map_err(CliError::operation)?;
            if json {
                return crate::output::print_json(&report);
            }
            if report.landed {
                println!(
                    "Landed {} commit(s) of `{id}` on {}{}",
                    report.commits,
                    report.branch,
                    if report.rebased { " (rebased)" } else { "" }
                );
            } else {
                println!("`{id}` had nothing to land; removed it");
            }
            Ok(())
        }
        GitChangesetCommand::Abandon { id, dry_run } => {
            let changeset = if *dry_run {
                changesets::find_changeset(paths, id)
            } else {
                changesets::abandon_changeset(paths, id)
            }
            .map_err(CliError::operation)?;
            if json {
                return crate::output::print_json(&serde_json::json!({
                    "abandoned": changeset,
                    "dry_run": dry_run,
                }));
            }
            let verb = if *dry_run {
                "Would abandon"
            } else {
                "Abandoned"
            };
            println!(
                "{verb} `{id}` ({} commit(s), {} uncommitted)",
                changeset.ahead,
                changeset.uncommitted.len()
            );
            Ok(())
        }
        GitChangesetCommand::Prune {
            older_than,
            include_work,
            dry_run,
        } => {
            let seconds = older_than.saturating_mul(86_400);
            let pruned = changesets::prune_changesets(paths, seconds, *include_work, *dry_run)
                .map_err(CliError::operation)?;
            if json {
                return crate::output::print_json(&serde_json::json!({
                    "pruned": pruned,
                    "dry_run": dry_run,
                }));
            }
            let verb = if *dry_run { "Would remove" } else { "Removed" };
            if pruned.is_empty() {
                println!("No idle changesets to remove.");
            }
            for changeset in &pruned {
                print!("{verb} ");
                print_changeset_line(changeset);
            }
            Ok(())
        }
    }
}

fn print_batched_commit_report(
    output: OutputFormat,
    report: &vulcan_app::batch_commit::BatchedCommitReport,
) -> Result<(), CliError> {
    use vulcan_app::batch_commit::BatchedCommitStatus;

    if output == OutputFormat::Json {
        return crate::output::print_json(report);
    }
    match report.status {
        BatchedCommitStatus::Committed => {
            for commit in &report.commits {
                let short = commit.sha.get(..7).unwrap_or(&commit.sha);
                println!("{short} {} ({} files)", commit.message, commit.files.len());
            }
        }
        BatchedCommitStatus::Planned => {
            for group in &report.planned {
                println!(
                    "would commit: {} ({} files)",
                    group.message,
                    group.paths.len()
                );
            }
        }
        BatchedCommitStatus::Clean => println!("Nothing to commit."),
        BatchedCommitStatus::Disabled => {
            println!("This vault does not use batched commits (`[git] strategy = \"batched\"`).");
        }
        BatchedCommitStatus::Blocked => println!(
            "No commit: {}",
            report.detail.as_deref().unwrap_or("the batch is blocked")
        ),
    }
    if let Some(reason) = &report.fallback {
        println!("Used deterministic groups: {reason}");
    }
    if !report.left_staged.is_empty() {
        println!("Left staged by hand: {}", report.left_staged.join(", "));
    }
    Ok(())
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
    let timeout = parse_run_timeout(args.timeout)?;
    let sandbox = parse_run_sandbox(args.sandbox)?;
    // Only the unsandboxed tier needs `execute` up front; the runtime
    // enforces the same rule for the configured default sandbox.
    if sandbox == Some(vulcan_core::JsRuntimeSandbox::None) {
        selected_permission_guard(cli, paths)?
            .check_execute()
            .map_err(CliError::operation)?;
    }

    // --eval/-e: evaluate one or more code snippets sequentially and print each result.
    if !args.eval.is_empty() {
        for code in args.eval {
            let result = run_js_eval(paths, code, timeout, sandbox, cli.permissions.as_deref())?;
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
        let result = run_js_command(
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
    #[cfg(not(feature = "web"))]
    {
        let _ = (cli, paths, command, stdout_is_tty, use_stdout_color);
        Err(CliError::operation(
            "the `web` command requires a build with the `web` feature enabled",
        ))
    }

    #[cfg(feature = "web")]
    {
        let guard = selected_permission_guard(cli, paths)?;
        match command {
            WebCommand::Search {
                query,
                backend,
                limit,
            } => {
                let report = run_web_search_command(paths, query, *backend, *limit, Some(&guard))?;
                print_web_search_report(cli.output, &report)
            }
            WebCommand::Fetch { url, mode, save } => {
                let report = run_web_fetch_command(paths, url, *mode, save.as_ref(), Some(&guard))?;
                print_web_fetch_report(cli.output, &report, stdout_is_tty, use_stdout_color)
            }
        }
    }
}

#[cfg(feature = "web")]
fn search_backend_kind_from_arg(arg: SearchBackendArg) -> SearchBackendKind {
    match arg {
        SearchBackendArg::Disabled => SearchBackendKind::Disabled,
        SearchBackendArg::Auto => SearchBackendKind::Auto,
        SearchBackendArg::Duckduckgo => SearchBackendKind::Duckduckgo,
        SearchBackendArg::Kagi => SearchBackendKind::Kagi,
        SearchBackendArg::Exa => SearchBackendKind::Exa,
        SearchBackendArg::Tavily => SearchBackendKind::Tavily,
        SearchBackendArg::Brave => SearchBackendKind::Brave,
        SearchBackendArg::Ollama => SearchBackendKind::Ollama,
    }
}

#[cfg(feature = "web")]
fn app_web_fetch_mode(mode: WebFetchMode) -> AppWebFetchMode {
    match mode {
        WebFetchMode::Markdown => AppWebFetchMode::Markdown,
        WebFetchMode::Html => AppWebFetchMode::Html,
        WebFetchMode::Raw => AppWebFetchMode::Raw,
    }
}

#[cfg(feature = "web")]
pub(crate) fn run_web_search_command(
    paths: &VaultPaths,
    query: &str,
    backend_override: Option<SearchBackendArg>,
    limit: usize,
    permissions: Option<&ProfilePermissionGuard>,
) -> Result<WebSearchReport, CliError> {
    build_web_search_report_with_permissions(
        paths,
        &WebSearchRequest {
            query: query.to_string(),
            backend: backend_override.map(search_backend_kind_from_arg),
            limit,
        },
        permissions,
    )
    .map_err(CliError::operation)
}

#[cfg(feature = "web")]
pub(crate) fn run_web_fetch_command(
    paths: &VaultPaths,
    url: &str,
    mode: WebFetchMode,
    save: Option<&PathBuf>,
    permissions: Option<&ProfilePermissionGuard>,
) -> Result<WebFetchReport, CliError> {
    apply_web_fetch_report_with_permissions(
        paths,
        &WebFetchRequest {
            url: url.to_string(),
            mode: app_web_fetch_mode(mode),
            save: save.cloned(),
        },
        permissions,
    )
    .map_err(CliError::operation)
}

fn strip_shebang_line(source: &str) -> &str {
    if let Some(stripped) = source.strip_prefix("#!") {
        stripped
            .split_once('\n')
            .map_or("", |(_, remainder)| remainder)
    } else {
        source
    }
}

fn resolve_named_run_script_path(paths: &VaultPaths, script: &str) -> Option<PathBuf> {
    let scripts_root = resolve_run_scripts_root(paths);
    [PathBuf::from(script), PathBuf::from(format!("{script}.js"))]
        .into_iter()
        .map(|candidate| scripts_root.join(candidate))
        .find(|candidate| candidate.is_file())
}

fn resolve_run_scripts_root(paths: &VaultPaths) -> PathBuf {
    let configured = load_vault_config(paths).config.js_runtime.scripts_folder;
    if configured.is_absolute() {
        configured
    } else {
        paths.vault_root().join(configured)
    }
}

fn load_run_script_source(
    paths: &VaultPaths,
    script: Option<&str>,
    script_mode: bool,
) -> Result<String, CliError> {
    if let Some(script) = script {
        let direct = PathBuf::from(script);
        let path = if script_mode || direct.is_file() {
            direct
        } else if let Some(named) = resolve_named_run_script_path(paths, script) {
            named
        } else {
            return Err(CliError::operation(format!(
                "script not found: {script}; expected a file path or .vulcan/scripts entry"
            )));
        };
        return fs::read_to_string(path).map_err(CliError::operation);
    }

    if io::stdin().is_terminal() {
        return Err(CliError::operation(
            "`vulcan run` requires a script path, stdin, or an interactive terminal session",
        ));
    }

    let mut buffer = String::new();
    io::stdin()
        .read_to_string(&mut buffer)
        .map_err(CliError::operation)?;
    Ok(buffer)
}

pub(crate) fn parse_run_timeout(timeout: Option<&str>) -> Result<Option<Duration>, CliError> {
    let Some(timeout) = timeout else {
        return Ok(None);
    };

    let millis =
        vulcan_core::expression::functions::parse_duration_string(timeout).ok_or_else(|| {
            CliError::operation(format!(
                "invalid timeout duration `{timeout}`; expected values like 500ms, 30s, or 2m"
            ))
        })?;
    if millis <= 0 {
        return Err(CliError::operation("run timeout must be greater than 0ms"));
    }
    let millis = u64::try_from(millis)
        .map_err(|_| CliError::operation("run timeout must be greater than 0ms"))?;
    Ok(Some(Duration::from_millis(millis)))
}

fn parse_run_sandbox(sandbox: Option<&str>) -> Result<Option<JsRuntimeSandbox>, CliError> {
    match sandbox {
        None => Ok(None),
        Some("strict") => Ok(Some(JsRuntimeSandbox::Strict)),
        Some("fs") => Ok(Some(JsRuntimeSandbox::Fs)),
        Some("net") => Ok(Some(JsRuntimeSandbox::Net)),
        Some("none") => Ok(Some(JsRuntimeSandbox::None)),
        Some(other) => Err(CliError::operation(format!(
            "invalid sandbox level `{other}`; expected strict, fs, net, or none"
        ))),
    }
}

fn run_js_command(
    paths: &VaultPaths,
    script: Option<&str>,
    script_mode: bool,
    timeout: Option<Duration>,
    sandbox: Option<JsRuntimeSandbox>,
    permission_profile: Option<&str>,
) -> Result<DataviewJsResult, CliError> {
    let source = load_run_script_source(paths, script, script_mode)?;
    let tool_registry = tools::runtime_tool_registry(paths, permission_profile, "run");
    evaluate_dataview_js_with_options(
        paths,
        strip_shebang_line(&source),
        None,
        DataviewJsEvalOptions {
            timeout,
            sandbox,
            permission_profile: permission_profile.map(ToOwned::to_owned),
            mutation_committer: Some(mdbase_js_mutation_committer(
                paths,
                permission_profile,
                Verbosity::Quiet,
            )),
            tool_registry: Some(tool_registry),
            ..DataviewJsEvalOptions::default()
        },
    )
    .map_err(CliError::operation)
}

fn run_js_eval(
    paths: &VaultPaths,
    code: &str,
    timeout: Option<Duration>,
    sandbox: Option<JsRuntimeSandbox>,
    permission_profile: Option<&str>,
) -> Result<DataviewJsResult, CliError> {
    let tool_registry = tools::runtime_tool_registry(paths, permission_profile, "run");
    evaluate_dataview_js_with_options(
        paths,
        code,
        None,
        DataviewJsEvalOptions {
            timeout,
            sandbox,
            permission_profile: permission_profile.map(ToOwned::to_owned),
            mutation_committer: Some(mdbase_js_mutation_committer(
                paths,
                permission_profile,
                Verbosity::Quiet,
            )),
            tool_registry: Some(tool_registry),
            ..DataviewJsEvalOptions::default()
        },
    )
    .map_err(CliError::operation)
}

#[cfg(feature = "web")]
fn print_web_search_report(output: OutputFormat, report: &WebSearchReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.results.is_empty() {
                println!("No web results.");
                return Ok(());
            }
            for result in &report.results {
                println!("- {} [{}]", result.title, result.url);
                if !result.snippet.is_empty() {
                    println!("  {}", result.snippet);
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

#[cfg(feature = "web")]
fn print_web_fetch_report(
    output: OutputFormat,
    report: &WebFetchReport,
    stdout_is_tty: bool,
    use_color: bool,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human => {
            if let Some(saved) = &report.saved {
                println!(
                    "Fetched {} [{} {}] -> {}",
                    report.url, report.status, report.content_type, saved
                );
            } else if report.mode == "markdown" {
                crate::print_markdown_output(output, &report.content, stdout_is_tty, use_color)?;
            } else {
                print!("{}", report.content);
                if !report.content.ends_with('\n') {
                    println!();
                }
            }
            Ok(())
        }
        OutputFormat::Markdown => {
            if report.mode == "markdown" && report.saved.is_none() {
                crate::print_markdown_output(output, &report.content, stdout_is_tty, use_color)
            } else {
                if let Some(saved) = &report.saved {
                    println!(
                        "Fetched {} [{} {}] -> {}",
                        report.url, report.status, report.content_type, saved
                    );
                } else {
                    print!("{}", report.content);
                    if !report.content.ends_with('\n') {
                        println!();
                    }
                }
                Ok(())
            }
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_git_status_report(output: OutputFormat, report: &GitStatusReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.clean {
                println!("Working tree clean.");
                return Ok(());
            }
            if !report.staged.is_empty() {
                println!("Staged:");
                for path in &report.staged {
                    println!("- {path}");
                }
            }
            if !report.unstaged.is_empty() {
                println!("Unstaged:");
                for path in &report.unstaged {
                    println!("- {path}");
                }
            }
            if !report.untracked.is_empty() {
                println!("Untracked:");
                for path in &report.untracked {
                    println!("- {path}");
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_git_log_report(output: OutputFormat, report: &GitLogReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.entries.is_empty() {
                println!("No commits.");
                return Ok(());
            }
            for entry in &report.entries {
                println!(
                    "- {} {} ({}, {})",
                    entry.commit.chars().take(8).collect::<String>(),
                    entry.summary,
                    entry.author_name,
                    entry.committed_at
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_git_diff_group_report(
    output: OutputFormat,
    report: &GitDiffReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.diff.trim().is_empty() {
                if let Some(path) = &report.path {
                    println!("No changes in {path}.");
                } else {
                    println!("Working tree clean.");
                }
            } else {
                print!("{}", report.diff);
                if !report.diff.ends_with('\n') {
                    println!();
                }
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_git_commit_report(output: OutputFormat, report: &GitCommitReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.committed {
                let sha = report.sha.as_deref().unwrap_or_default();
                println!(
                    "Committed {} file(s) as {}: {}",
                    report.files.len(),
                    sha.chars().take(8).collect::<String>(),
                    report.message
                );
            } else {
                println!("{}", report.message);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}

fn print_git_blame_report(output: OutputFormat, report: &GitBlameReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Human | OutputFormat::Markdown => {
            for line in &report.lines {
                println!(
                    "{:>4} {} {:<16} | {}",
                    line.line_number,
                    line.commit.chars().take(8).collect::<String>(),
                    line.author_name,
                    line.line
                );
            }
            Ok(())
        }
        OutputFormat::Json => print_json(report),
    }
}
