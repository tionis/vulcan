//! Native per-user service installation for the Vulcan daemon.

use serde::Serialize;
use std::error::Error;
use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::NamedTempFile;

pub const DAEMON_SERVICE_PLAN_VERSION: u32 = 1;
const SYSTEMD_UNIT: &str = "vulcan-daemon.service";
const WINDOWS_TASK: &str = "Vulcan Daemon";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonServicePlatform {
    SystemdUser,
    WindowsScheduledTask,
}

impl DaemonServicePlatform {
    pub fn native() -> Result<Self, DaemonServiceError> {
        #[cfg(target_os = "linux")]
        {
            Ok(Self::SystemdUser)
        }
        #[cfg(target_os = "windows")]
        {
            Ok(Self::WindowsScheduledTask)
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            Err(DaemonServiceError::UnsupportedPlatform(
                std::env::consts::OS.to_string(),
            ))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonServiceAction {
    Install,
    Uninstall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DaemonServiceCommand {
    pub program: String,
    pub arguments: Vec<String>,
    #[serde(skip)]
    tolerate_failure: bool,
}

impl DaemonServiceCommand {
    fn new(program: &str, arguments: &[&str]) -> Self {
        Self {
            program: program.to_string(),
            arguments: arguments.iter().map(|value| (*value).to_string()).collect(),
            tolerate_failure: false,
        }
    }

    fn tolerant(mut self) -> Self {
        self.tolerate_failure = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DaemonServicePlan {
    pub version: u32,
    pub action: DaemonServiceAction,
    pub platform: DaemonServicePlatform,
    pub executable: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
    pub commands: Vec<DaemonServiceCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DaemonServiceReport {
    #[serde(flatten)]
    pub plan: DaemonServicePlan,
    pub dry_run: bool,
    pub changed: bool,
}

#[derive(Debug)]
pub enum DaemonServiceError {
    UnsupportedPlatform(String),
    InvalidExecutable(PathBuf),
    InvalidConfigDirectory(PathBuf),
    UnsafeDefinitionPath(PathBuf),
    CommandFailed {
        program: String,
        exit_code: Option<i32>,
        stderr: String,
    },
    Io(std::io::Error),
}

impl Display for DaemonServiceError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedPlatform(platform) => write!(
                formatter,
                "automatic daemon service installation is unsupported on {platform}; use `vulcan daemon start` from the platform's user service manager"
            ),
            Self::InvalidExecutable(path) => write!(
                formatter,
                "daemon service executable must be an absolute file path: {}",
                path.display()
            ),
            Self::InvalidConfigDirectory(path) => write!(
                formatter,
                "Vulcan configuration directory must have a parent: {}",
                path.display()
            ),
            Self::UnsafeDefinitionPath(path) => write!(
                formatter,
                "refusing to replace symlinked daemon service definition: {}",
                path.display()
            ),
            Self::CommandFailed {
                program,
                exit_code,
                stderr,
            } => {
                write!(formatter, "daemon service command `{program}` failed")?;
                if let Some(code) = exit_code {
                    write!(formatter, " with exit code {code}")?;
                }
                if !stderr.is_empty() {
                    write!(formatter, ": {stderr}")?;
                }
                Ok(())
            }
            Self::Io(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for DaemonServiceError {}

impl From<std::io::Error> for DaemonServiceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn plan_daemon_service(
    action: DaemonServiceAction,
    platform: DaemonServicePlatform,
    executable: &Path,
    config_directory: &Path,
) -> Result<DaemonServicePlan, DaemonServiceError> {
    if !executable.is_absolute() || !executable.is_file() {
        return Err(DaemonServiceError::InvalidExecutable(
            executable.to_path_buf(),
        ));
    }
    match platform {
        DaemonServicePlatform::SystemdUser => {
            plan_systemd_service(action, executable, config_directory)
        }
        DaemonServicePlatform::WindowsScheduledTask => Ok(plan_windows_task(action, executable)),
    }
}

pub fn apply_daemon_service(
    plan: DaemonServicePlan,
    dry_run: bool,
) -> Result<DaemonServiceReport, DaemonServiceError> {
    if dry_run {
        return Ok(DaemonServiceReport {
            plan,
            dry_run,
            changed: false,
        });
    }
    let changed = match plan.action {
        DaemonServiceAction::Install => {
            if let (Some(path), Some(definition)) = (&plan.definition_path, &plan.definition) {
                write_definition(path, definition)?;
            }
            run_commands(&plan.commands)?;
            true
        }
        DaemonServiceAction::Uninstall => {
            run_commands(&plan.commands)?;
            let mut changed = false;
            if let Some(path) = &plan.definition_path {
                changed = remove_definition(path)?;
                if plan.platform == DaemonServicePlatform::SystemdUser {
                    run_commands(&[DaemonServiceCommand::new(
                        "systemctl",
                        &["--user", "daemon-reload"],
                    )])?;
                }
            }
            changed
        }
    };
    Ok(DaemonServiceReport {
        plan,
        dry_run,
        changed,
    })
}

fn plan_systemd_service(
    action: DaemonServiceAction,
    executable: &Path,
    config_directory: &Path,
) -> Result<DaemonServicePlan, DaemonServiceError> {
    let xdg_config = config_directory.parent().ok_or_else(|| {
        DaemonServiceError::InvalidConfigDirectory(config_directory.to_path_buf())
    })?;
    let definition_path = xdg_config.join("systemd/user").join(SYSTEMD_UNIT);
    let environment_path = config_directory.join("daemon.env");
    let definition = format!(
        "[Unit]\nDescription=Vulcan multi-wiki synchronization daemon\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart={} daemon start\nRestart=on-failure\nRestartSec=5s\nEnvironmentFile=-{}\n\n[Install]\nWantedBy=default.target\n",
        systemd_quote(executable),
        systemd_quote(&environment_path)
    );
    let commands = match action {
        DaemonServiceAction::Install => vec![
            DaemonServiceCommand::new("systemctl", &["--user", "daemon-reload"]),
            DaemonServiceCommand::new("systemctl", &["--user", "enable", "--now", SYSTEMD_UNIT]),
        ],
        DaemonServiceAction::Uninstall => vec![DaemonServiceCommand::new(
            "systemctl",
            &["--user", "disable", "--now", SYSTEMD_UNIT],
        )
        .tolerant()],
    };
    Ok(DaemonServicePlan {
        version: DAEMON_SERVICE_PLAN_VERSION,
        action,
        platform: DaemonServicePlatform::SystemdUser,
        executable: executable.to_path_buf(),
        definition_path: Some(definition_path),
        definition: (action == DaemonServiceAction::Install).then_some(definition),
        commands,
    })
}

fn plan_windows_task(action: DaemonServiceAction, executable: &Path) -> DaemonServicePlan {
    let command = windows_task_command(executable);
    let commands = match action {
        DaemonServiceAction::Install => vec![DaemonServiceCommand::new(
            "schtasks.exe",
            &[
                "/Create",
                "/TN",
                WINDOWS_TASK,
                "/TR",
                &command,
                "/SC",
                "ONLOGON",
                "/RL",
                "LIMITED",
                "/F",
            ],
        )],
        DaemonServiceAction::Uninstall => {
            vec![
                DaemonServiceCommand::new("schtasks.exe", &["/Delete", "/TN", WINDOWS_TASK, "/F"])
                    .tolerant(),
            ]
        }
    };
    DaemonServicePlan {
        version: DAEMON_SERVICE_PLAN_VERSION,
        action,
        platform: DaemonServicePlatform::WindowsScheduledTask,
        executable: executable.to_path_buf(),
        definition_path: None,
        definition: None,
        commands,
    }
}

fn systemd_quote(path: &Path) -> String {
    let value = path.to_string_lossy().replace('%', "%%");
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn windows_task_command(executable: &Path) -> String {
    format!("\"{}\" daemon start", executable.display())
}

fn write_definition(path: &Path, contents: &str) -> Result<(), DaemonServiceError> {
    let parent = path
        .parent()
        .ok_or_else(|| DaemonServiceError::InvalidConfigDirectory(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(DaemonServiceError::UnsafeDefinitionPath(path.to_path_buf()));
    }
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(contents.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| DaemonServiceError::Io(error.error))?;
    Ok(())
}

fn remove_definition(path: &Path) -> Result<bool, DaemonServiceError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(DaemonServiceError::UnsafeDefinitionPath(path.to_path_buf()))
        }
        Ok(_) => {
            fs::remove_file(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn run_commands(commands: &[DaemonServiceCommand]) -> Result<(), DaemonServiceError> {
    for command in commands {
        let output = Command::new(&command.program)
            .args(command.arguments.iter().map(OsString::from))
            .output()?;
        if !output.status.success() && !command.tolerate_failure {
            const MAX_STDERR: usize = 16 * 1024;
            let stderr =
                String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(MAX_STDERR)])
                    .trim()
                    .to_string();
            return Err(DaemonServiceError::CommandFailed {
                program: command.program.clone(),
                exit_code: output.status.code(),
                stderr,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn systemd_plan_is_user_scoped_restartable_and_secret_free() {
        let temporary = tempdir().expect("temporary directory");
        let executable = temporary.path().join("bin/vulcan");
        fs::create_dir_all(executable.parent().expect("binary parent")).expect("binary parent");
        fs::write(&executable, "binary").expect("binary fixture");
        let config = temporary.path().join("config/vulcan");

        let plan = plan_daemon_service(
            DaemonServiceAction::Install,
            DaemonServicePlatform::SystemdUser,
            &executable,
            &config,
        )
        .expect("systemd plan");

        assert_eq!(
            plan.definition_path,
            Some(
                temporary
                    .path()
                    .join("config/systemd/user/vulcan-daemon.service")
            )
        );
        let definition = plan.definition.expect("unit definition");
        assert!(definition.contains("ExecStart=\""));
        assert!(definition.contains(" daemon start"));
        assert!(definition.contains("Restart=on-failure"));
        assert!(definition.contains("EnvironmentFile=-"));
        assert!(!definition.contains("API_KEY="));
        assert_eq!(
            plan.commands[1].arguments,
            ["--user", "enable", "--now", SYSTEMD_UNIT]
        );
    }

    #[test]
    fn windows_plan_uses_a_per_user_logon_task_without_a_shell() {
        let temporary = tempdir().expect("temporary directory");
        let executable = temporary.path().join("Vulcan Bin/vulcan.exe");
        fs::create_dir_all(executable.parent().expect("binary parent")).expect("binary parent");
        fs::write(&executable, "binary").expect("binary fixture");

        let plan = plan_daemon_service(
            DaemonServiceAction::Install,
            DaemonServicePlatform::WindowsScheduledTask,
            &executable,
            temporary.path(),
        )
        .expect("Windows task plan");

        assert_eq!(plan.commands.len(), 1);
        assert_eq!(plan.commands[0].program, "schtasks.exe");
        assert!(plan.commands[0]
            .arguments
            .windows(2)
            .any(|pair| pair == ["/SC", "ONLOGON"]));
        let task_command = plan.commands[0]
            .arguments
            .iter()
            .skip_while(|argument| *argument != "/TR")
            .nth(1)
            .expect("task command");
        assert!(task_command.starts_with('"'));
        assert!(task_command.ends_with(" daemon start"));
    }

    #[test]
    fn dry_run_and_uninstalled_definition_are_idempotent() {
        let temporary = tempdir().expect("temporary directory");
        let executable = temporary.path().join("vulcan");
        fs::write(&executable, "binary").expect("binary fixture");
        let install = plan_daemon_service(
            DaemonServiceAction::Install,
            DaemonServicePlatform::SystemdUser,
            &executable,
            &temporary.path().join("config/vulcan"),
        )
        .expect("install plan");
        let report = apply_daemon_service(install.clone(), true).expect("dry run");
        assert!(report.dry_run);
        assert!(!report.changed);
        assert!(!install.definition_path.expect("definition path").exists());

        let missing = temporary.path().join("missing.service");
        assert!(!remove_definition(&missing).expect("missing definition"));
    }

    #[cfg(unix)]
    #[test]
    fn definition_writer_rejects_symlink_targets() {
        use std::os::unix::fs::symlink;

        let temporary = tempdir().expect("temporary directory");
        let target = temporary.path().join("target");
        let link = temporary.path().join("unit");
        fs::write(&target, "unchanged").expect("target");
        symlink(&target, &link).expect("symlink");

        assert!(matches!(
            write_definition(&link, "replacement"),
            Err(DaemonServiceError::UnsafeDefinitionPath(path)) if path == link
        ));
        assert_eq!(fs::read_to_string(target).expect("target"), "unchanged");
    }
}
