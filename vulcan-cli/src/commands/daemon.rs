use crate::output::print_json;
use crate::{Cli, CliError, DaemonAgentKindArg, DaemonCommand, DaemonConfigCommand, OutputFormat};
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use vulcan_daemon::credentials::CompanionCredentialStore;
use vulcan_daemon::process::{
    daemon_status, request_daemon_shutdown, run_daemon_foreground, DaemonProcessContext,
    DaemonStatusReport,
};
use vulcan_daemon::registry::{
    DaemonAgentConfig, DaemonAgentKind, DaemonConfig, DaemonSemanticWorkerConfig, WikiId,
};
use vulcan_daemon::semantic_worker::{load_semantic_worker_status, SemanticWorkerStatus};
use vulcan_daemon::service::{
    apply_daemon_service, inspect_daemon_service, plan_daemon_service, DaemonServiceAction,
    DaemonServicePlan, DaemonServicePlatform, DaemonServiceReport,
};

#[derive(Debug, Serialize)]
struct DaemonStartReport<'a> {
    version: u32,
    detached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    child_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    log_path: Option<&'a std::path::Path>,
    status: &'a DaemonStatusReport,
}

#[derive(Debug, Serialize)]
struct DaemonCompanionReport {
    version: u32,
    base_url: String,
    credential_id: String,
    allowed_origins: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
}

pub(crate) fn handle_daemon_command(cli: &Cli, command: &DaemonCommand) -> Result<(), CliError> {
    let context = DaemonProcessContext::user_default().map_err(CliError::operation)?;
    match command {
        DaemonCommand::Install { dry_run } => {
            manage_service(cli.output, &context, DaemonServiceAction::Install, *dry_run)
        }
        DaemonCommand::Uninstall { dry_run } => manage_service(
            cli.output,
            &context,
            DaemonServiceAction::Uninstall,
            *dry_run,
        ),
        DaemonCommand::Start { detach, child } => {
            let mut context = context.clone();
            context.verbose = cli.verbose;
            if *detach {
                start_detached(cli, &context)
            } else {
                start_foreground(cli, &context, *child)
            }
        }
        DaemonCommand::Status => {
            let mut status = daemon_status(&context).map_err(CliError::operation)?;
            if DaemonServicePlatform::native().is_ok() {
                let plan = native_service_plan(&context, DaemonServiceAction::Install)?;
                status.service = inspect_daemon_service(&plan).map_err(CliError::operation)?;
            }
            print_status(cli.output, &status)
        }
        DaemonCommand::SemanticStatus => {
            let status = load_semantic_worker_status(&context.state_root)
                .map_err(CliError::operation)?
                .ok_or_else(|| {
                    CliError::operation("the semantic worker has not completed a pass")
                })?;
            print_semantic_worker_status(cli.output, &status)
        }
        DaemonCommand::Stop => {
            let status = request_daemon_shutdown(&context).map_err(CliError::operation)?;
            print_status(cli.output, &status)
        }
        DaemonCommand::Companion { reveal_token } => {
            print_companion(cli.output, &context, *reveal_token)
        }
        DaemonCommand::Config { command } => handle_config(cli.output, &context, command),
    }
}

fn print_semantic_worker_status(
    output: OutputFormat,
    status: &SemanticWorkerStatus,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(status);
    }
    println!("Semantic worker checked at {} ms", status.checked_unix_ms);
    for entry in &status.entries {
        if let Some(report) = &entry.report {
            println!("{}: {:?}", entry.wiki_id, report.outcome);
        } else if let Some(detail) = &entry.skipped {
            println!("{}: skipped ({detail})", entry.wiki_id);
        } else if let Some(error) = &entry.error {
            println!("{}: error ({error})", entry.wiki_id);
        }
    }
    Ok(())
}

fn manage_service(
    output: OutputFormat,
    context: &DaemonProcessContext,
    action: DaemonServiceAction,
    dry_run: bool,
) -> Result<(), CliError> {
    let plan = native_service_plan(context, action)?;
    let report = apply_daemon_service(plan, dry_run).map_err(CliError::operation)?;
    print_service_report(output, &report)
}

fn native_service_plan(
    context: &DaemonProcessContext,
    action: DaemonServiceAction,
) -> Result<DaemonServicePlan, CliError> {
    let executable = std::env::current_exe().map_err(CliError::operation)?;
    let config_directory = context.registry.path().parent().ok_or_else(|| {
        CliError::operation("daemon registry path has no configuration directory")
    })?;
    let home_directory = service_home_directory()?;
    plan_daemon_service(
        action,
        DaemonServicePlatform::native().map_err(CliError::operation)?,
        &executable,
        config_directory,
        &context.state_root,
        &home_directory,
        service_user_id()?,
    )
    .map_err(CliError::operation)
}

fn service_home_directory() -> Result<PathBuf, CliError> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| CliError::operation("cannot determine the user home directory"))
}

#[cfg(target_os = "macos")]
fn service_user_id() -> Result<u32, CliError> {
    let output = Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .map_err(CliError::operation)?;
    if !output.status.success() {
        return Err(CliError::operation(
            "failed to determine the logged-in macOS user id with `/usr/bin/id -u`",
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(CliError::operation)?
        .trim()
        .parse()
        .map_err(CliError::operation)
}

#[cfg(not(target_os = "macos"))]
// Keep one fallible call shape at the platform-neutral planner boundary.
#[allow(clippy::unnecessary_wraps)]
fn service_user_id() -> Result<u32, CliError> {
    Ok(0)
}

fn print_service_report(
    output: OutputFormat,
    report: &DaemonServiceReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(report);
    }
    let action = match report.plan.action {
        DaemonServiceAction::Install => "install",
        DaemonServiceAction::Uninstall => "uninstall",
    };
    if report.dry_run {
        println!(
            "Would {action} the {:?} per-user daemon service",
            report.plan.platform
        );
    } else if report.changed {
        println!(
            "Vulcan {:?} per-user daemon service {action} completed",
            report.plan.platform
        );
    } else {
        println!("Vulcan daemon service was already uninstalled");
    }
    if let Some(path) = &report.plan.definition_path {
        println!("Definition: {}", path.display());
    }
    Ok(())
}

fn print_companion(
    output: OutputFormat,
    context: &DaemonProcessContext,
    reveal_token: bool,
) -> Result<(), CliError> {
    let status = daemon_status(context).map_err(CliError::operation)?;
    let runtime = status.runtime.filter(|_| status.running).ok_or_else(|| {
        CliError::operation("the Vulcan daemon must be running to provision a companion client")
    })?;
    let credential = CompanionCredentialStore::at(&context.state_root)
        .load()
        .map_err(CliError::operation)?;
    if credential.id != runtime.credential_id {
        return Err(CliError::operation(
            "daemon runtime credential identity does not match device state",
        ));
    }
    let report = DaemonCompanionReport {
        version: 1,
        base_url: format!("http://{}", runtime.bind),
        credential_id: credential.id,
        allowed_origins: credential.allowed_origins,
        token: reveal_token.then_some(credential.token),
    };
    if output == OutputFormat::Json {
        return print_json(&report);
    }
    println!("Companion endpoint: {}", report.base_url);
    println!("Credential ID: {}", report.credential_id);
    println!("Allowed origins: {}", report.allowed_origins.join(", "));
    if let Some(token) = report.token {
        println!("Bearer token: {token}");
        println!("Store this token only in device-local client storage.");
    } else {
        println!("Bearer token: [REDACTED] (pass --reveal-token for explicit transfer)");
    }
    Ok(())
}

fn handle_config(
    output: OutputFormat,
    context: &DaemonProcessContext,
    command: &DaemonConfigCommand,
) -> Result<(), CliError> {
    let config = match command {
        DaemonConfigCommand::Show => context.registry.load().map_err(CliError::operation)?,
        DaemonConfigCommand::SetBind { bind, dry_run } => context
            .registry
            .set_bind(bind, *dry_run)
            .map_err(CliError::operation)?,
        DaemonConfigCommand::SetAgent {
            kind,
            base_url,
            model,
            api_key_env,
            dry_run,
        } => context
            .registry
            .set_agent(
                daemon_agent_kind(*kind),
                DaemonAgentConfig {
                    base_url: base_url.clone(),
                    model: model.clone(),
                    api_key_env: api_key_env.clone(),
                },
                *dry_run,
            )
            .map_err(CliError::operation)?,
        DaemonConfigCommand::ClearAgent { kind, dry_run } => context
            .registry
            .clear_agent(daemon_agent_kind(*kind), *dry_run)
            .map_err(CliError::operation)?,
        DaemonConfigCommand::SetSemanticWorker {
            wiki,
            semantic_ref,
            remote,
            live_ref,
            quiet_seconds,
            maximum_wait_seconds,
            poll_seconds,
            no_publish,
            dry_run,
        } => context
            .registry
            .set_semantic_worker(
                DaemonSemanticWorkerConfig {
                    wikis: wiki
                        .iter()
                        .map(|id| WikiId::parse(id.clone()))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(CliError::operation)?,
                    semantic_ref: semantic_ref.clone(),
                    remote: remote.clone(),
                    live_ref: live_ref.clone(),
                    publish: !*no_publish,
                    quiet_seconds: *quiet_seconds,
                    maximum_wait_seconds: *maximum_wait_seconds,
                    poll_seconds: *poll_seconds,
                },
                *dry_run,
            )
            .map_err(CliError::operation)?,
        DaemonConfigCommand::ClearSemanticWorker { dry_run } => context
            .registry
            .clear_semantic_worker(*dry_run)
            .map_err(CliError::operation)?,
    };
    print_config(output, &config)
}

const fn daemon_agent_kind(kind: DaemonAgentKindArg) -> DaemonAgentKind {
    match kind {
        DaemonAgentKindArg::Resolution => DaemonAgentKind::Resolution,
        DaemonAgentKindArg::Semantic => DaemonAgentKind::Semantic,
    }
}

fn print_config(output: OutputFormat, config: &DaemonConfig) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(config);
    }
    print!(
        "{}",
        toml::to_string_pretty(config).map_err(CliError::operation)?
    );
    Ok(())
}

fn start_foreground(
    cli: &Cli,
    context: &DaemonProcessContext,
    child: bool,
) -> Result<(), CliError> {
    let daemon_context = context.clone();
    let (sender, receiver) = mpsc::channel();
    let daemon = thread::spawn(move || {
        let result = run_daemon_foreground(&daemon_context);
        let _ = sender.send(result);
    });
    let status = wait_until_ready(context, &receiver)?;
    if !child {
        print_start(cli.output, false, None, None, &status)?;
    }
    daemon
        .join()
        .map_err(|_| CliError::operation("daemon process runtime panicked"))?;
    receiver
        .recv()
        .map_err(CliError::operation)?
        .map_err(CliError::operation)
}

fn start_detached(cli: &Cli, context: &DaemonProcessContext) -> Result<(), CliError> {
    let current = daemon_status(context).map_err(CliError::operation)?;
    if current.running {
        return Err(CliError::operation("the Vulcan daemon is already running"));
    }
    let daemon_dir = context.state_root.join("daemon");
    fs::create_dir_all(&daemon_dir).map_err(CliError::operation)?;
    let log_path = daemon_dir.join("daemon.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(CliError::operation)?;
    let error_log = log.try_clone().map_err(CliError::operation)?;
    let executable = std::env::current_exe().map_err(CliError::operation)?;
    let mut command = Command::new(executable);
    command
        .args(detached_child_args(context.verbose))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(error_log));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        // Keep the long-running child independent of the invoking console and
        // its captured process group. These are the Win32 DETACHED_PROCESS and
        // CREATE_NEW_PROCESS_GROUP flags, respectively.
        command.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    let mut child = command.spawn().map_err(CliError::operation)?;
    let status = wait_until_ready_child(context, &mut child)?;
    print_start(cli.output, true, Some(child.id()), Some(&log_path), &status)
}

fn wait_until_ready(
    context: &DaemonProcessContext,
    result: &mpsc::Receiver<Result<(), vulcan_daemon::process::DaemonProcessError>>,
) -> Result<DaemonStatusReport, CliError> {
    for _ in 0..100 {
        if let Ok(result) = result.try_recv() {
            return result
                .map(|()| unreachable!("daemon stopped before readiness"))
                .map_err(CliError::operation);
        }
        if let Ok(status) = daemon_status(context) {
            if status.running {
                return Ok(status);
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(CliError::operation(
        "daemon did not become ready within five seconds",
    ))
}

fn wait_until_ready_child(
    context: &DaemonProcessContext,
    child: &mut std::process::Child,
) -> Result<DaemonStatusReport, CliError> {
    for _ in 0..100 {
        if let Some(status) = child.try_wait().map_err(CliError::operation)? {
            return Err(CliError::operation(format!(
                "detached daemon exited before readiness with {status}"
            )));
        }
        if let Ok(status) = daemon_status(context) {
            if status.running {
                return Ok(status);
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(CliError::operation(
        "detached daemon did not become ready within five seconds",
    ))
}

fn print_start(
    output: OutputFormat,
    detached: bool,
    child_pid: Option<u32>,
    log_path: Option<&std::path::Path>,
    status: &DaemonStatusReport,
) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(&DaemonStartReport {
            version: status.version,
            detached,
            child_pid,
            log_path,
            status,
        });
    }
    let runtime = status
        .runtime
        .as_ref()
        .ok_or_else(|| CliError::operation("daemon became ready without runtime metadata"))?;
    if detached {
        println!(
            "Vulcan daemon started in the background on {}",
            runtime.bind
        );
        if let Some(path) = log_path {
            println!("Log: {}", path.display());
        }
    } else {
        println!(
            "Vulcan daemon running on {} (press Ctrl-C or use `vulcan daemon stop`)",
            runtime.bind
        );
    }
    Ok(())
}

fn print_status(output: OutputFormat, status: &DaemonStatusReport) -> Result<(), CliError> {
    if output == OutputFormat::Json {
        return print_json(status);
    }
    if status.running {
        let runtime = status
            .runtime
            .as_ref()
            .ok_or_else(|| CliError::operation("running daemon has no runtime metadata"))?;
        println!(
            "Vulcan daemon is running on {} (pid {}, {} registered wikis)",
            runtime.bind,
            runtime.pid,
            status.registered_wikis.len()
        );
    } else {
        println!("Vulcan daemon is stopped");
    }
    if let Some(service) = &status.service {
        if let Some(repair) = &service.repair_command {
            println!(
                "Daemon service definition is missing or stale at {}; repair with `{repair}`",
                service.definition_path.display()
            );
        }
    }
    Ok(())
}

/// Arguments for the detached daemon child. The global `--verbose` flag must
/// precede the subcommand so the child enables the same operational logging.
fn detached_child_args(verbose: bool) -> Vec<&'static str> {
    if verbose {
        vec!["--verbose", "daemon", "start", "--child"]
    } else {
        vec!["daemon", "start", "--child"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detached_child_preserves_verbose_logging() {
        assert_eq!(detached_child_args(false), ["daemon", "start", "--child"]);
        assert_eq!(
            detached_child_args(true),
            ["--verbose", "daemon", "start", "--child"]
        );
    }
}
