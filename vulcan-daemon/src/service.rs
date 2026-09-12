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
const LAUNCHD_LABEL: &str = "dev.tionis.vulcan.daemon";
const LAUNCHD_PLIST: &str = "dev.tionis.vulcan.daemon.plist";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonServicePlatform {
    SystemdUser,
    LaunchdUser,
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
        #[cfg(target_os = "macos")]
        {
            Ok(Self::LaunchdUser)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonServiceUser {
    unix_id: u32,
    windows_sid: Option<String>,
}

impl DaemonServiceUser {
    #[must_use]
    pub fn unix(user_id: u32) -> Self {
        Self {
            unix_id: user_id,
            windows_sid: None,
        }
    }

    #[must_use]
    pub fn windows(user_sid: impl Into<String>) -> Self {
        Self {
            unix_id: 0,
            windows_sid: Some(user_sid.into()),
        }
    }
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub directories: Vec<PathBuf>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DaemonServiceDiagnostic {
    pub platform: DaemonServicePlatform,
    pub installed: bool,
    pub definition_path: PathBuf,
    pub definition_current: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_executable: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured_executable_exists: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair_command: Option<String>,
}

#[derive(Debug)]
pub enum DaemonServiceError {
    UnsupportedPlatform(String),
    InvalidExecutable(PathBuf),
    InvalidConfigDirectory(PathBuf),
    InvalidWindowsUserSid(String),
    UnsafeDefinitionPath(PathBuf),
    UnsafeDirectoryPath(PathBuf),
    CommandFailed {
        program: String,
        arguments: Vec<String>,
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
            Self::InvalidWindowsUserSid(sid) => write!(
                formatter,
                "cannot install the per-user Windows daemon task with invalid user SID {sid:?}"
            ),
            Self::UnsafeDefinitionPath(path) => write!(
                formatter,
                "refusing to replace symlinked daemon service definition: {}",
                path.display()
            ),
            Self::UnsafeDirectoryPath(path) => write!(
                formatter,
                "refusing to use symlinked or non-directory daemon service path: {}",
                path.display()
            ),
            Self::CommandFailed {
                program,
                arguments,
                exit_code,
                stderr,
            } => {
                write!(
                    formatter,
                    "daemon service command `{}` failed",
                    render_command(program, arguments)
                )?;
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
    state_directory: &Path,
    home_directory: &Path,
    user: &DaemonServiceUser,
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
        DaemonServicePlatform::LaunchdUser => Ok(plan_launchd_service(
            action,
            executable,
            config_directory,
            state_directory,
            home_directory,
            user.unix_id,
        )),
        DaemonServicePlatform::WindowsScheduledTask => plan_windows_task(
            action,
            executable,
            config_directory,
            user.windows_sid.as_deref().ok_or_else(|| {
                DaemonServiceError::InvalidWindowsUserSid("missing current-user SID".to_string())
            })?,
        ),
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
            for directory in &plan.directories {
                ensure_directory(directory)?;
            }
            if let (Some(path), Some(definition)) = (&plan.definition_path, &plan.definition) {
                write_definition(path, definition, plan.platform)?;
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

pub fn inspect_daemon_service(
    plan: &DaemonServicePlan,
) -> Result<Option<DaemonServiceDiagnostic>, DaemonServiceError> {
    let (Some(path), Some(expected)) = (&plan.definition_path, &plan.definition) else {
        return Ok(None);
    };
    let actual = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(DaemonServiceError::UnsafeDefinitionPath(path.clone()));
        }
        Ok(metadata) if metadata.is_file() => Some(read_definition(path, plan.platform)?),
        Ok(_) => return Err(DaemonServiceError::UnsafeDefinitionPath(path.clone())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let configured_executable = actual
        .as_deref()
        .and_then(|definition| configured_service_executable(plan.platform, definition));
    let configured_executable_exists = configured_executable
        .as_ref()
        .map(|executable| executable.is_file());
    let definition_current = actual.as_deref() == Some(expected.as_str());
    let healthy = definition_current && configured_executable_exists.unwrap_or(true);
    Ok(Some(DaemonServiceDiagnostic {
        platform: plan.platform,
        installed: actual.is_some(),
        definition_path: path.clone(),
        definition_current,
        configured_executable,
        configured_executable_exists,
        repair_command: (!healthy).then(|| "vulcan daemon install".to_string()),
    }))
}

fn configured_service_executable(
    platform: DaemonServicePlatform,
    definition: &str,
) -> Option<PathBuf> {
    match platform {
        DaemonServicePlatform::LaunchdUser => {
            let arguments = definition.split_once("<key>ProgramArguments</key>")?.1;
            let value = arguments
                .split_once("<string>")?
                .1
                .split_once("</string>")?
                .0;
            Some(PathBuf::from(xml_unescape(value)))
        }
        DaemonServicePlatform::SystemdUser => definition.lines().find_map(|line| {
            line.strip_prefix("ExecStart=\"")
                .and_then(|line| line.strip_suffix(" daemon start"))
                .and_then(|line| line.strip_suffix('"'))
                .map(|line| PathBuf::from(line.replace("\\\"", "\"").replace("\\\\", "\\")))
        }),
        DaemonServicePlatform::WindowsScheduledTask => None,
    }
}

fn plan_launchd_service(
    action: DaemonServiceAction,
    executable: &Path,
    config_directory: &Path,
    state_directory: &Path,
    home_directory: &Path,
    user_id: u32,
) -> DaemonServicePlan {
    let definition_path = home_directory
        .join("Library/LaunchAgents")
        .join(LAUNCHD_PLIST);
    let daemon_state = state_directory.join("daemon");
    let standard_out = daemon_state.join("daemon.log");
    let standard_error = daemon_state.join("daemon.error.log");
    let definition = render_launchd_plist(
        executable,
        config_directory,
        state_directory,
        &standard_out,
        &standard_error,
    );
    let domain = format!("gui/{user_id}");
    let service = format!("{domain}/{LAUNCHD_LABEL}");
    let definition_argument = definition_path.to_string_lossy().into_owned();
    let commands = match action {
        DaemonServiceAction::Install => vec![
            DaemonServiceCommand::new("launchctl", &["bootout", &service]).tolerant(),
            DaemonServiceCommand::new("launchctl", &["bootstrap", &domain, &definition_argument]),
            DaemonServiceCommand::new("launchctl", &["kickstart", "-k", &service]),
            DaemonServiceCommand::new("launchctl", &["print", &service]),
        ],
        DaemonServiceAction::Uninstall => {
            vec![DaemonServiceCommand::new("launchctl", &["bootout", &service]).tolerant()]
        }
    };
    DaemonServicePlan {
        version: DAEMON_SERVICE_PLAN_VERSION,
        action,
        platform: DaemonServicePlatform::LaunchdUser,
        executable: executable.to_path_buf(),
        directories: vec![daemon_state],
        definition_path: Some(definition_path),
        definition: (action == DaemonServiceAction::Install).then_some(definition),
        commands,
    }
}

fn render_launchd_plist(
    executable: &Path,
    config_directory: &Path,
    state_directory: &Path,
    standard_out: &Path,
    standard_error: &Path,
) -> String {
    let executable = xml_escape(&executable.to_string_lossy());
    let config_root = xml_escape(
        &config_directory
            .parent()
            .unwrap_or(config_directory)
            .to_string_lossy(),
    );
    let state_root = xml_escape(
        &state_directory
            .parent()
            .unwrap_or(state_directory)
            .to_string_lossy(),
    );
    let standard_out = xml_escape(&standard_out.to_string_lossy());
    let standard_error = xml_escape(&standard_error.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{executable}</string>
    <string>daemon</string>
    <string>start</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>EnvironmentVariables</key>
  <dict>
    <key>XDG_CONFIG_HOME</key>
    <string>{config_root}</string>
    <key>XDG_STATE_HOME</key>
    <string>{state_root}</string>
  </dict>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ThrottleInterval</key>
  <integer>5</integer>
  <key>ProcessType</key>
  <string>Background</string>
  <key>StandardOutPath</key>
  <string>{standard_out}</string>
  <key>StandardErrorPath</key>
  <string>{standard_error}</string>
</dict>
</plist>
"#
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
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
        directories: Vec::new(),
        definition_path: Some(definition_path),
        definition: (action == DaemonServiceAction::Install).then_some(definition),
        commands,
    })
}

fn plan_windows_task(
    action: DaemonServiceAction,
    executable: &Path,
    config_directory: &Path,
    user_sid: &str,
) -> Result<DaemonServicePlan, DaemonServiceError> {
    if !is_windows_user_sid(user_sid) {
        return Err(DaemonServiceError::InvalidWindowsUserSid(
            user_sid.to_string(),
        ));
    }
    let task_name = format!("{WINDOWS_TASK} ({user_sid})");
    let definition_path = config_directory.join("daemon-task.xml");
    let definition_argument = definition_path.to_string_lossy().into_owned();
    let definition = render_windows_task_xml(executable, user_sid);
    let commands = match action {
        DaemonServiceAction::Install => vec![
            DaemonServiceCommand::new(
                "schtasks.exe",
                &[
                    "/Create",
                    "/TN",
                    &task_name,
                    "/XML",
                    &definition_argument,
                    "/F",
                ],
            ),
            DaemonServiceCommand::new("schtasks.exe", &["/Run", "/TN", &task_name]),
        ],
        DaemonServiceAction::Uninstall => {
            vec![
                DaemonServiceCommand::new("schtasks.exe", &["/Delete", "/TN", &task_name, "/F"])
                    .tolerant(),
            ]
        }
    };
    Ok(DaemonServicePlan {
        version: DAEMON_SERVICE_PLAN_VERSION,
        action,
        platform: DaemonServicePlatform::WindowsScheduledTask,
        executable: executable.to_path_buf(),
        directories: Vec::new(),
        definition_path: Some(definition_path),
        definition: (action == DaemonServiceAction::Install).then_some(definition),
        commands,
    })
}

fn systemd_quote(path: &Path) -> String {
    let value = path.to_string_lossy().replace('%', "%%");
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn is_windows_user_sid(value: &str) -> bool {
    value.starts_with("S-1-")
        && value.len() <= 184
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-' || byte == b'S')
        && value.split('-').skip(1).all(|part| !part.is_empty())
}

fn render_windows_task_xml(executable: &Path, user_sid: &str) -> String {
    let executable = xml_escape(&executable.to_string_lossy());
    let user_sid = xml_escape(user_sid);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Vulcan multi-wiki synchronization daemon</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user_sid}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="CurrentUser">
      <UserId>{user_sid}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="CurrentUser">
    <Exec>
      <Command>{executable}</Command>
      <Arguments>daemon start --detach</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

fn write_definition(
    path: &Path,
    contents: &str,
    platform: DaemonServicePlatform,
) -> Result<(), DaemonServiceError> {
    let parent = path
        .parent()
        .ok_or_else(|| DaemonServiceError::InvalidConfigDirectory(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(DaemonServiceError::UnsafeDefinitionPath(path.to_path_buf()));
    }
    let mut temporary = NamedTempFile::new_in(parent)?;
    if platform == DaemonServicePlatform::WindowsScheduledTask {
        let mut encoded = Vec::with_capacity(2 + contents.len() * 2);
        encoded.extend_from_slice(&[0xff, 0xfe]);
        for code_unit in contents.encode_utf16() {
            encoded.extend_from_slice(&code_unit.to_le_bytes());
        }
        temporary.write_all(&encoded)?;
    } else {
        temporary.write_all(contents.as_bytes())?;
    }
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| DaemonServiceError::Io(error.error))?;
    Ok(())
}

fn read_definition(
    path: &Path,
    platform: DaemonServicePlatform,
) -> Result<String, DaemonServiceError> {
    if platform != DaemonServicePlatform::WindowsScheduledTask {
        return Ok(fs::read_to_string(path)?);
    }
    let bytes = fs::read(path)?;
    if !bytes.starts_with(&[0xff, 0xfe]) || (bytes.len() - 2) % 2 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Windows daemon task definition is not BOM-prefixed UTF-16LE",
        )
        .into());
    }
    let code_units = bytes[2..]
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    String::from_utf16(&code_units).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Windows daemon task definition is invalid UTF-16LE: {error}"),
        )
        .into()
    })
}

fn ensure_directory(path: &Path) -> Result<(), DaemonServiceError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(DaemonServiceError::UnsafeDirectoryPath(path.to_path_buf()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
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
                arguments: command.arguments.clone(),
                exit_code: output.status.code(),
                stderr,
            });
        }
    }
    Ok(())
}

fn render_command(program: &str, arguments: &[String]) -> String {
    std::iter::once(program)
        .chain(arguments.iter().map(String::as_str))
        .map(|argument| {
            if argument
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._/:".contains(&byte))
            {
                argument.to_string()
            } else {
                format!("{argument:?}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
            &temporary.path().join("state/vulcan"),
            temporary.path(),
            &DaemonServiceUser::unix(1000),
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
            &temporary.path().join("state/vulcan"),
            temporary.path(),
            &DaemonServiceUser::windows("S-1-5-21-111-222-333-1001"),
        )
        .expect("Windows task plan");

        assert_eq!(plan.commands.len(), 2);
        assert_eq!(plan.commands[0].program, "schtasks.exe");
        assert!(plan.commands[0]
            .arguments
            .windows(2)
            .any(|pair| pair == ["/TN", "Vulcan Daemon (S-1-5-21-111-222-333-1001)"]));
        assert!(plan.commands[0]
            .arguments
            .windows(2)
            .any(|pair| pair[0] == "/XML" && pair[1].ends_with("daemon-task.xml")));
        assert!(!plan.commands[0]
            .arguments
            .iter()
            .any(|value| value == "/TR"));
        let definition = plan.definition.expect("Windows task definition");
        assert!(definition.contains(
            "<LogonTrigger>\n      <Enabled>true</Enabled>\n      <UserId>S-1-5-21-111-222-333-1001</UserId>"
        ));
        assert!(definition.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(definition.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(definition.contains("Vulcan Bin/vulcan.exe</Command>"));
        assert!(definition.contains("<Arguments>daemon start --detach</Arguments>"));
        assert!(!definition.contains("<Password>"));
        assert_eq!(plan.commands[1].program, "schtasks.exe");
        assert_eq!(
            plan.commands[1].arguments,
            ["/Run", "/TN", "Vulcan Daemon (S-1-5-21-111-222-333-1001)"]
        );
    }

    #[test]
    fn windows_plan_rejects_missing_or_malformed_user_sids() {
        let temporary = tempdir().expect("temporary directory");
        let executable = temporary.path().join("vulcan.exe");
        fs::write(&executable, "binary").expect("binary fixture");

        for sid in [None, Some("CURRENT_USER"), Some("S-1-5-21-&lt;unsafe&gt;")] {
            assert!(matches!(
                plan_daemon_service(
                    DaemonServiceAction::Install,
                    DaemonServicePlatform::WindowsScheduledTask,
                    &executable,
                    temporary.path(),
                    temporary.path(),
                    temporary.path(),
                    &match sid {
                        Some(sid) => DaemonServiceUser::windows(sid),
                        None => DaemonServiceUser::unix(0),
                    },
                ),
                Err(DaemonServiceError::InvalidWindowsUserSid(_))
            ));
        }
    }

    #[test]
    fn windows_task_definition_is_written_as_bom_prefixed_utf16le() {
        let temporary = tempdir().expect("temporary directory");
        let path = temporary.path().join("daemon-task.xml");
        let definition = render_windows_task_xml(
            Path::new(r"C:\Users\alice\Vulcan & Tools\vulcan.exe"),
            "S-1-5-21-111-222-333-1001",
        );

        write_definition(
            &path,
            &definition,
            DaemonServicePlatform::WindowsScheduledTask,
        )
        .expect("write Windows task definition");

        let bytes = fs::read(path).expect("read Windows task definition");
        assert_eq!(&bytes[..2], &[0xff, 0xfe]);
        assert_eq!((bytes.len() - 2) % 2, 0);
        let decoded = read_definition(
            &temporary.path().join("daemon-task.xml"),
            DaemonServicePlatform::WindowsScheduledTask,
        )
        .expect("decode UTF-16LE definition");
        assert_eq!(decoded, definition);
        assert!(decoded.starts_with("<?xml version=\"1.0\" encoding=\"UTF-16\"?>"));
        assert!(decoded.contains("Vulcan &amp; Tools"));
    }

    #[test]
    fn windows_uninstall_targets_only_the_current_users_task() {
        let temporary = tempdir().expect("temporary directory");
        let executable = temporary.path().join("vulcan.exe");
        fs::write(&executable, "binary").expect("binary fixture");

        let plan = plan_windows_task(
            DaemonServiceAction::Uninstall,
            &executable,
            temporary.path(),
            "S-1-5-21-111-222-333-1002",
        )
        .expect("Windows uninstall plan");

        assert!(plan.definition.is_none());
        assert_eq!(
            plan.commands[0].arguments,
            [
                "/Delete",
                "/TN",
                "Vulcan Daemon (S-1-5-21-111-222-333-1002)",
                "/F"
            ]
        );
        assert!(plan.commands[0].tolerate_failure);
    }

    #[test]
    fn launchd_plan_is_user_scoped_restartable_and_secret_free() {
        let temporary = tempdir().expect("temporary directory");
        let executable = temporary.path().join("Applications/Vulcan & Tools/vulcan");
        fs::create_dir_all(executable.parent().expect("binary parent")).expect("binary parent");
        fs::write(&executable, "binary").expect("binary fixture");
        let state = temporary.path().join("Library/Application Support/Vulcan");

        let plan = plan_daemon_service(
            DaemonServiceAction::Install,
            DaemonServicePlatform::LaunchdUser,
            &executable,
            &temporary.path().join("config/vulcan"),
            &state,
            temporary.path(),
            &DaemonServiceUser::unix(501),
        )
        .expect("launchd plan");

        assert_eq!(
            plan.definition_path,
            Some(
                temporary
                    .path()
                    .join("Library/LaunchAgents/dev.tionis.vulcan.daemon.plist")
            )
        );
        let definition = plan.definition.expect("plist definition");
        assert!(definition.contains("<string>dev.tionis.vulcan.daemon</string>"));
        assert!(definition.contains("Vulcan &amp; Tools/vulcan</string>"));
        assert!(definition.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(definition.contains("<key>XDG_CONFIG_HOME</key>"));
        assert!(definition.contains("config</string>"));
        assert!(definition.contains("<key>XDG_STATE_HOME</key>"));
        assert!(definition.contains("Library/Application Support</string>"));
        assert!(definition.contains("<key>SuccessfulExit</key>\n    <false/>"));
        assert!(definition.contains("<key>ThrottleInterval</key>\n  <integer>5</integer>"));
        assert!(definition.contains("<string>Background</string>"));
        assert!(definition.contains("daemon.error.log</string>"));
        assert!(!definition.contains("API_KEY"));
        assert_eq!(plan.commands.len(), 4);
        assert_eq!(plan.directories, [state.join("daemon")]);
        assert!(plan
            .commands
            .iter()
            .all(|command| command.program == "launchctl"));
        assert_eq!(
            plan.commands[0].arguments,
            ["bootout", "gui/501/dev.tionis.vulcan.daemon"]
        );
        assert_eq!(plan.commands[1].arguments[0..2], ["bootstrap", "gui/501"]);
        assert!(plan.commands[1].arguments[2].ends_with(LAUNCHD_PLIST));
        assert_eq!(
            plan.commands[2].arguments,
            ["kickstart", "-k", "gui/501/dev.tionis.vulcan.daemon"]
        );
        assert_eq!(
            plan.commands[3].arguments,
            ["print", "gui/501/dev.tionis.vulcan.daemon"]
        );
    }

    #[test]
    fn launchd_uninstall_boots_out_before_removing_definition() {
        let temporary = tempdir().expect("temporary directory");
        let executable = temporary.path().join("bin/vulcan");
        fs::create_dir_all(executable.parent().expect("binary parent")).expect("binary parent");
        fs::write(&executable, "binary").expect("binary fixture");

        let plan = plan_daemon_service(
            DaemonServiceAction::Uninstall,
            DaemonServicePlatform::LaunchdUser,
            &executable,
            temporary.path(),
            &temporary.path().join("state"),
            temporary.path(),
            &DaemonServiceUser::unix(502),
        )
        .expect("launchd uninstall plan");

        assert!(plan.definition.is_none());
        assert_eq!(plan.commands.len(), 1);
        assert_eq!(
            plan.commands[0].arguments,
            ["bootout", "gui/502/dev.tionis.vulcan.daemon"]
        );
        assert!(plan.commands[0].tolerate_failure);
    }

    #[test]
    fn command_failures_include_an_actionable_argument_vector() {
        let error = DaemonServiceError::CommandFailed {
            program: "launchctl".to_string(),
            arguments: vec![
                "bootstrap".to_string(),
                "gui/501".to_string(),
                "/Users/Test User/Library/LaunchAgents/vulcan.plist".to_string(),
            ],
            exit_code: Some(5),
            stderr: "input/output error".to_string(),
        };

        assert_eq!(
            error.to_string(),
            "daemon service command `launchctl bootstrap gui/501 \"/Users/Test User/Library/LaunchAgents/vulcan.plist\"` failed with exit code 5: input/output error"
        );
    }

    #[cfg(unix)]
    #[test]
    fn launchd_log_directory_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let temporary = tempdir().expect("temporary directory");
        let target = temporary.path().join("target");
        let link = temporary.path().join("daemon");
        fs::create_dir(&target).expect("target");
        symlink(&target, &link).expect("symlink");

        assert!(matches!(
            ensure_directory(&link),
            Err(DaemonServiceError::UnsafeDirectoryPath(path)) if path == link
        ));
    }

    #[test]
    fn service_diagnostic_reports_missing_stale_and_current_definitions() {
        let temporary = tempdir().expect("temporary directory");
        let executable = temporary.path().join("bin/vulcan");
        fs::create_dir_all(executable.parent().expect("binary parent")).expect("binary parent");
        fs::write(&executable, "binary").expect("binary fixture");
        let plan = plan_daemon_service(
            DaemonServiceAction::Install,
            DaemonServicePlatform::LaunchdUser,
            &executable,
            &temporary.path().join("config/vulcan"),
            &temporary.path().join("state/vulcan"),
            temporary.path(),
            &DaemonServiceUser::unix(501),
        )
        .expect("launchd plan");
        let path = plan.definition_path.as_ref().expect("definition path");

        let missing = inspect_daemon_service(&plan)
            .expect("missing diagnostic")
            .expect("definition-backed service");
        assert!(!missing.installed);
        assert_eq!(
            missing.repair_command.as_deref(),
            Some("vulcan daemon install")
        );

        write_definition(
            path,
            plan.definition.as_deref().expect("definition"),
            DaemonServicePlatform::LaunchdUser,
        )
        .expect("write definition");
        let current = inspect_daemon_service(&plan)
            .expect("current diagnostic")
            .expect("definition-backed service");
        assert!(current.installed);
        assert!(current.definition_current);
        assert_eq!(
            current.configured_executable.as_deref(),
            Some(executable.as_path())
        );
        assert_eq!(current.configured_executable_exists, Some(true));
        assert!(current.repair_command.is_none());

        let stale_definition = plan.definition.as_deref().expect("definition").replace(
            &xml_escape(&executable.to_string_lossy()),
            "/missing/versioned/vulcan",
        );
        write_definition(path, &stale_definition, DaemonServicePlatform::LaunchdUser)
            .expect("write stale definition");
        let stale = inspect_daemon_service(&plan)
            .expect("stale diagnostic")
            .expect("definition-backed service");
        assert!(!stale.definition_current);
        assert_eq!(stale.configured_executable_exists, Some(false));
        assert_eq!(
            stale.repair_command.as_deref(),
            Some("vulcan daemon install")
        );
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
            &temporary.path().join("state/vulcan"),
            temporary.path(),
            &DaemonServiceUser::unix(1000),
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
            write_definition(&link, "replacement", DaemonServicePlatform::SystemdUser),
            Err(DaemonServiceError::UnsafeDefinitionPath(path)) if path == link
        ));
        assert_eq!(fs::read_to_string(target).expect("target"), "unchanged");
    }
}
