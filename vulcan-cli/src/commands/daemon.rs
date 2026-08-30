use crate::output::print_json;
use crate::{Cli, CliError, DaemonCommand, OutputFormat};
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use vulcan_daemon::process::{
    daemon_status, request_daemon_shutdown, run_daemon_foreground, DaemonProcessContext,
    DaemonStatusReport,
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

pub(crate) fn handle_daemon_command(cli: &Cli, command: &DaemonCommand) -> Result<(), CliError> {
    let context = DaemonProcessContext::user_default().map_err(CliError::operation)?;
    match command {
        DaemonCommand::Start { detach, child } => {
            if *detach {
                start_detached(cli, &context)
            } else {
                start_foreground(cli, &context, *child)
            }
        }
        DaemonCommand::Status => {
            let status = daemon_status(&context).map_err(CliError::operation)?;
            print_status(cli.output, &status)
        }
        DaemonCommand::Stop => {
            let status = request_daemon_shutdown(&context).map_err(CliError::operation)?;
            print_status(cli.output, &status)
        }
    }
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
    let mut child = Command::new(executable)
        .args(["daemon", "start", "--child"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(error_log))
        .spawn()
        .map_err(CliError::operation)?;
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
    Ok(())
}
