use crate::output::print_json;
use crate::{
    js_repl, selected_permission_guard, tools, trust, Cli, CliError, GitCommand, OutputFormat,
    PermissionGuard, SearchBackendArg, TrustCommand, WebCommand, WebFetchMode,
};
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::time::Duration;
use vulcan_app::web::{
    apply_web_fetch_report, execute_web_search, prepare_web_search,
    WebFetchMode as AppWebFetchMode, WebFetchReport, WebFetchRequest, WebSearchReport,
    WebSearchRequest,
};
use vulcan_core::{
    evaluate_dataview_js_with_options, git_commit, load_vault_config, DataviewJsEvalOptions,
    DataviewJsResult, JsRuntimeSandbox, PluginEvent, ProfilePermissionGuard, SearchBackendKind,
    VaultPaths,
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
    let timeout = parse_run_timeout(args.timeout)?;
    let sandbox = parse_run_sandbox(args.sandbox)?;
    selected_permission_guard(cli, paths)?
        .check_execute()
        .map_err(CliError::operation)?;

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

fn app_web_fetch_mode(mode: WebFetchMode) -> AppWebFetchMode {
    match mode {
        WebFetchMode::Markdown => AppWebFetchMode::Markdown,
        WebFetchMode::Html => AppWebFetchMode::Html,
        WebFetchMode::Raw => AppWebFetchMode::Raw,
    }
}

pub(crate) fn run_web_search_command(
    paths: &VaultPaths,
    query: &str,
    backend_override: Option<SearchBackendArg>,
    limit: usize,
    permissions: Option<&ProfilePermissionGuard>,
) -> Result<WebSearchReport, CliError> {
    let prepared = prepare_web_search(
        paths,
        &WebSearchRequest {
            query: query.to_string(),
            backend: backend_override.map(search_backend_kind_from_arg),
            limit,
        },
    )
    .map_err(CliError::operation)?;
    if let Some(permissions) = permissions {
        permissions
            .check_network(&prepared.base_url)
            .map_err(CliError::operation)?;
    }
    execute_web_search(&prepared).map_err(CliError::operation)
}

pub(crate) fn run_web_fetch_command(
    paths: &VaultPaths,
    url: &str,
    mode: WebFetchMode,
    save: Option<&PathBuf>,
    permissions: Option<&ProfilePermissionGuard>,
) -> Result<WebFetchReport, CliError> {
    if let Some(permissions) = permissions {
        permissions
            .check_network(url)
            .map_err(CliError::operation)?;
    }
    apply_web_fetch_report(
        paths,
        &WebFetchRequest {
            url: url.to_string(),
            mode: app_web_fetch_mode(mode),
            save: save.cloned(),
        },
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
            tool_registry: Some(tool_registry),
            ..DataviewJsEvalOptions::default()
        },
    )
    .map_err(CliError::operation)
}

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
