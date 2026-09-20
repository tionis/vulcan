//! Native per-user scheduling for portable Vulcan self-updates.

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::Write as _;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::NamedTempFile;

pub const UPDATE_SCHEDULE_PLAN_VERSION: u32 = 1;
const SYSTEMD_SERVICE: &str = "vulcan-update.service";
const SYSTEMD_TIMER: &str = "vulcan-update.timer";
const LAUNCHD_LABEL: &str = "dev.tionis.vulcan.update";
const LAUNCHD_PLIST: &str = "dev.tionis.vulcan.update.plist";
const WINDOWS_TASK: &str = "Vulcan Self Update";
const ANDROID_JOB_ID: u32 = 1_947_853_021;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateSchedulePlatform {
    SystemdUser,
    LaunchdUser,
    WindowsScheduledTask,
    TermuxJob,
}

impl UpdateSchedulePlatform {
    pub fn native() -> Result<Self, UpdateScheduleError> {
        #[cfg(target_os = "android")]
        return Ok(Self::TermuxJob);
        #[cfg(all(target_os = "linux", not(target_os = "android")))]
        return Ok(Self::SystemdUser);
        #[cfg(target_os = "macos")]
        return Ok(Self::LaunchdUser);
        #[cfg(target_os = "windows")]
        return Ok(Self::WindowsScheduledTask);
        #[allow(unreachable_code)]
        Err(UpdateScheduleError::UnsupportedPlatform(
            std::env::consts::OS.to_string(),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateScheduleAction {
    Install,
    Uninstall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateScheduleOptions {
    /// Local wall-clock time used by desktop schedulers, in HH:MM form.
    pub daily_at: String,
    /// Android `JobScheduler` period; Android cannot promise an exact wall-clock time.
    pub android_period_hours: u32,
    pub channel: String,
    pub channel_url: Option<String>,
    pub notify_on_failure: bool,
}

impl Default for UpdateScheduleOptions {
    fn default() -> Self {
        Self {
            daily_at: "03:00".to_string(),
            android_period_hours: 24,
            channel: "stable".to_string(),
            channel_url: None,
            notify_on_failure: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateScheduleDefinition {
    pub path: PathBuf,
    pub contents: String,
    #[serde(default)]
    pub executable: bool,
    #[serde(default)]
    pub windows_utf16: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateScheduleCommand {
    pub program: String,
    pub arguments: Vec<String>,
    #[serde(default)]
    tolerate_failure: bool,
}

impl UpdateScheduleCommand {
    fn new(program: &str, arguments: Vec<String>) -> Self {
        Self {
            program: program.to_string(),
            arguments,
            tolerate_failure: false,
        }
    }

    fn tolerant(mut self) -> Self {
        self.tolerate_failure = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateSchedulePlan {
    pub version: u32,
    pub action: UpdateScheduleAction,
    pub platform: UpdateSchedulePlatform,
    pub executable: PathBuf,
    pub options: UpdateScheduleOptions,
    pub manifest_path: PathBuf,
    pub definitions: Vec<UpdateScheduleDefinition>,
    pub commands: Vec<UpdateScheduleCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateScheduleReport {
    #[serde(flatten)]
    pub plan: UpdateSchedulePlan,
    pub dry_run: bool,
    pub changed: bool,
}

#[derive(Debug)]
pub enum UpdateScheduleError {
    UnsupportedPlatform(String),
    InvalidExecutable(PathBuf),
    InvalidRoot(PathBuf),
    InvalidTime(String),
    InvalidPeriod(u32),
    InvalidChannel(String),
    InvalidChannelUrl(String),
    InvalidWindowsUserSid(String),
    UnsafeManagedPath(PathBuf),
    CommandFailed {
        program: String,
        exit_code: Option<i32>,
        stderr: String,
    },
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl Display for UpdateScheduleError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedPlatform(platform) => write!(
                formatter,
                "automatic self-update scheduling is unsupported on {platform}"
            ),
            Self::InvalidExecutable(path) => write!(
                formatter,
                "scheduled updater executable must be an absolute regular file: {}",
                path.display()
            ),
            Self::InvalidRoot(path) => write!(
                formatter,
                "scheduled updater root must be absolute: {}",
                path.display()
            ),
            Self::InvalidTime(value) => write!(
                formatter,
                "scheduled update time must use 24-hour HH:MM form, got `{value}`"
            ),
            Self::InvalidPeriod(value) => write!(
                formatter,
                "Android scheduled update period must be between 1 and 168 hours, got {value}"
            ),
            Self::InvalidChannel(value) => write!(
                formatter,
                "scheduled update channel must be `stable` or `main`, got `{value}`"
            ),
            Self::InvalidChannelUrl(value) => write!(
                formatter,
                "scheduled update channel URL must be a single HTTPS URL safe for a service definition, got `{value}`"
            ),
            Self::InvalidWindowsUserSid(value) => {
                write!(formatter, "invalid current-user Windows SID `{value}`")
            }
            Self::UnsafeManagedPath(path) => write!(
                formatter,
                "refusing unsafe scheduled updater path: {}",
                path.display()
            ),
            Self::CommandFailed {
                program,
                exit_code,
                stderr,
            } => {
                write!(formatter, "scheduled updater command `{program}` failed")?;
                if let Some(code) = exit_code {
                    write!(formatter, " with exit code {code}")?;
                }
                if !stderr.is_empty() {
                    write!(formatter, ": {stderr}")?;
                }
                Ok(())
            }
            Self::Io(error) => Display::fmt(error, formatter),
            Self::Json(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for UpdateScheduleError {}
impl From<std::io::Error> for UpdateScheduleError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<serde_json::Error> for UpdateScheduleError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn plan_update_schedule(
    action: UpdateScheduleAction,
    platform: UpdateSchedulePlatform,
    executable: &Path,
    config_directory: &Path,
    state_directory: &Path,
    home_directory: &Path,
    windows_user_sid: Option<&str>,
    user_id: u32,
    options: UpdateScheduleOptions,
) -> Result<UpdateSchedulePlan, UpdateScheduleError> {
    if !executable.is_absolute() || !executable.is_file() {
        return Err(UpdateScheduleError::InvalidExecutable(
            executable.to_path_buf(),
        ));
    }
    for root in [config_directory, state_directory, home_directory] {
        if !root.is_absolute() {
            return Err(UpdateScheduleError::InvalidRoot(root.to_path_buf()));
        }
    }
    let (hour, minute) = parse_time(&options.daily_at)?;
    if !(1..=168).contains(&options.android_period_hours) {
        return Err(UpdateScheduleError::InvalidPeriod(
            options.android_period_hours,
        ));
    }
    if !matches!(options.channel.as_str(), "stable" | "main") {
        return Err(UpdateScheduleError::InvalidChannel(options.channel.clone()));
    }
    if let Some(url) = &options.channel_url {
        if !url.starts_with("https://")
            || url.chars().any(|character| {
                character.is_whitespace() || character.is_control() || character == '"'
            })
        {
            return Err(UpdateScheduleError::InvalidChannelUrl(url.clone()));
        }
    }
    let manifest_path = state_directory.join("update-schedule.json");
    let run_arguments = updater_arguments(&options);
    let (definitions, commands) = match platform {
        UpdateSchedulePlatform::SystemdUser => {
            let config_root = config_directory
                .parent()
                .ok_or_else(|| UpdateScheduleError::InvalidRoot(config_directory.to_path_buf()))?;
            let state_root = state_directory
                .parent()
                .ok_or_else(|| UpdateScheduleError::InvalidRoot(state_directory.to_path_buf()))?;
            let directory = config_root.join("systemd/user");
            let service_path = directory.join(SYSTEMD_SERVICE);
            let timer_path = directory.join(SYSTEMD_TIMER);
            let service = format!("[Unit]\nDescription=Vulcan portable binary self-update\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=oneshot\nEnvironment={}\nEnvironment={}\nExecStart={} {}\n", systemd_word(&format!("XDG_CONFIG_HOME={}", config_root.display())), systemd_word(&format!("XDG_STATE_HOME={}", state_root.display())), systemd_quote(executable), run_arguments.iter().map(|v| systemd_word(v)).collect::<Vec<_>>().join(" "));
            let timer = format!("[Unit]\nDescription=Daily Vulcan portable binary self-update\n\n[Timer]\nOnCalendar=*-*-* {hour:02}:{minute:02}:00\nPersistent=true\nRandomizedDelaySec=15m\nUnit={SYSTEMD_SERVICE}\n\n[Install]\nWantedBy=timers.target\n");
            let commands = match action {
                UpdateScheduleAction::Install => vec![
                    UpdateScheduleCommand::new("systemctl", strings(&["--user", "daemon-reload"])),
                    UpdateScheduleCommand::new(
                        "systemctl",
                        strings(&["--user", "enable", "--now", SYSTEMD_TIMER]),
                    ),
                ],
                UpdateScheduleAction::Uninstall => vec![
                    UpdateScheduleCommand::new(
                        "systemctl",
                        strings(&["--user", "disable", "--now", SYSTEMD_TIMER]),
                    )
                    .tolerant(),
                    UpdateScheduleCommand::new("systemctl", strings(&["--user", "daemon-reload"])),
                ],
            };
            (
                vec![
                    definition(service_path, service),
                    definition(timer_path, timer),
                ],
                commands,
            )
        }
        UpdateSchedulePlatform::LaunchdUser => {
            let path = home_directory
                .join("Library/LaunchAgents")
                .join(LAUNCHD_PLIST);
            let label = format!("gui/{user_id}/{LAUNCHD_LABEL}");
            let domain = format!("gui/{user_id}");
            let mut args = format!(
                "    <string>{}</string>\n",
                xml_escape(&executable.to_string_lossy())
            );
            for argument in &run_arguments {
                writeln!(args, "    <string>{}</string>", xml_escape(argument))
                    .expect("writing to a String cannot fail");
            }
            let stdout = state_directory.join("update.log");
            let stderr = state_directory.join("update.error.log");
            let config_root = config_directory.parent().unwrap_or(config_directory);
            let state_root = state_directory.parent().unwrap_or(state_directory);
            let plist = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{LAUNCHD_LABEL}</string>\n<key>ProgramArguments</key><array>\n{args}</array>\n<key>EnvironmentVariables</key><dict><key>XDG_CONFIG_HOME</key><string>{}</string><key>XDG_STATE_HOME</key><string>{}</string></dict>\n<key>StartCalendarInterval</key><dict><key>Hour</key><integer>{hour}</integer><key>Minute</key><integer>{minute}</integer></dict>\n<key>ProcessType</key><string>Background</string>\n<key>StandardOutPath</key><string>{}</string>\n<key>StandardErrorPath</key><string>{}</string>\n</dict></plist>\n", xml_escape(&config_root.to_string_lossy()), xml_escape(&state_root.to_string_lossy()), xml_escape(&stdout.to_string_lossy()), xml_escape(&stderr.to_string_lossy()));
            let path_arg = path.to_string_lossy().into_owned();
            let commands = match action {
                UpdateScheduleAction::Install => vec![
                    UpdateScheduleCommand::new("launchctl", vec!["bootout".into(), label.clone()])
                        .tolerant(),
                    UpdateScheduleCommand::new(
                        "launchctl",
                        vec!["bootstrap".into(), domain, path_arg],
                    ),
                ],
                UpdateScheduleAction::Uninstall => {
                    vec![
                        UpdateScheduleCommand::new("launchctl", vec!["bootout".into(), label])
                            .tolerant(),
                    ]
                }
            };
            (vec![definition(path, plist)], commands)
        }
        UpdateSchedulePlatform::WindowsScheduledTask => {
            let sid = windows_user_sid
                .filter(|sid| valid_sid(sid))
                .ok_or_else(|| {
                    UpdateScheduleError::InvalidWindowsUserSid(
                        windows_user_sid.unwrap_or("missing").to_string(),
                    )
                })?;
            let path = config_directory.join("update-task.xml");
            let task_name = format!("{WINDOWS_TASK} ({sid})");
            let start = format!("2000-01-01T{hour:02}:{minute:02}:00");
            let arguments = run_arguments
                .iter()
                .map(|v| windows_argument(v))
                .collect::<Vec<_>>()
                .join(" ");
            let xml = format!("<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task version=\"1.4\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\"><RegistrationInfo><Description>Daily Vulcan portable binary self-update</Description></RegistrationInfo><Triggers><CalendarTrigger><StartBoundary>{start}</StartBoundary><Enabled>true</Enabled><ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay></CalendarTrigger></Triggers><Principals><Principal id=\"CurrentUser\"><UserId>{}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals><Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><RunOnlyIfNetworkAvailable>true</RunOnlyIfNetworkAvailable><ExecutionTimeLimit>PT1H</ExecutionTimeLimit></Settings><Actions Context=\"CurrentUser\"><Exec><Command>{}</Command><Arguments>{}</Arguments></Exec></Actions></Task>\n", xml_escape(sid), xml_escape(&executable.to_string_lossy()), xml_escape(&arguments));
            let path_arg = path.to_string_lossy().into_owned();
            let commands = match action {
                UpdateScheduleAction::Install => vec![UpdateScheduleCommand::new(
                    "schtasks.exe",
                    vec![
                        "/Create".into(),
                        "/TN".into(),
                        task_name,
                        "/XML".into(),
                        path_arg,
                        "/F".into(),
                    ],
                )],
                UpdateScheduleAction::Uninstall => vec![UpdateScheduleCommand::new(
                    "schtasks.exe",
                    vec!["/Delete".into(), "/TN".into(), task_name, "/F".into()],
                )
                .tolerant()],
            };
            let mut item = definition(path, xml);
            item.windows_utf16 = true;
            (vec![item], commands)
        }
        UpdateSchedulePlatform::TermuxJob => {
            let path = state_directory.join("update-schedule.sh");
            let command = std::iter::once(shell_quote(&executable.to_string_lossy()))
                .chain(run_arguments.iter().map(|v| shell_quote(v)))
                .collect::<Vec<_>>()
                .join(" ");
            let script = format!(
                "#!/data/data/com.termux/files/usr/bin/sh\nset -u\numask 077\nexec {command}\n"
            );
            let commands = match action {
                UpdateScheduleAction::Install => vec![UpdateScheduleCommand::new(
                    "termux-job-scheduler",
                    vec![
                        "--script".into(),
                        path.to_string_lossy().into_owned(),
                        "--job-id".into(),
                        ANDROID_JOB_ID.to_string(),
                        "--period-ms".into(),
                        (u64::from(options.android_period_hours) * 3_600_000).to_string(),
                        "--network".into(),
                        "any".into(),
                        "--battery-not-low".into(),
                        "true".into(),
                        "--storage-not-low".into(),
                        "true".into(),
                        "--persisted".into(),
                        "true".into(),
                    ],
                )],
                UpdateScheduleAction::Uninstall => vec![UpdateScheduleCommand::new(
                    "termux-job-scheduler",
                    vec![
                        "--cancel".into(),
                        "--job-id".into(),
                        ANDROID_JOB_ID.to_string(),
                    ],
                )
                .tolerant()],
            };
            let mut item = definition(path, script);
            item.executable = true;
            (vec![item], commands)
        }
    };
    Ok(UpdateSchedulePlan {
        version: UPDATE_SCHEDULE_PLAN_VERSION,
        action,
        platform,
        executable: executable.to_path_buf(),
        options,
        manifest_path,
        definitions,
        commands,
    })
}

pub fn apply_update_schedule(
    plan: UpdateSchedulePlan,
    dry_run: bool,
) -> Result<UpdateScheduleReport, UpdateScheduleError> {
    if dry_run {
        return Ok(UpdateScheduleReport {
            plan,
            dry_run,
            changed: false,
        });
    }
    apply_update_schedule_with(plan, run_command)
}

fn apply_update_schedule_with(
    plan: UpdateSchedulePlan,
    mut run: impl FnMut(&UpdateScheduleCommand) -> Result<(), UpdateScheduleError>,
) -> Result<UpdateScheduleReport, UpdateScheduleError> {
    match plan.action {
        UpdateScheduleAction::Install => {
            for item in &plan.definitions {
                write_managed(item)?;
            }
            for command in &plan.commands {
                run(command)?;
            }
            let bytes = serde_json::to_vec_pretty(&plan)?;
            write_bytes(&plan.manifest_path, &bytes, false, false)?;
        }
        UpdateScheduleAction::Uninstall => {
            let split = usize::from(plan.platform == UpdateSchedulePlatform::SystemdUser);
            for command in &plan.commands[..split.min(plan.commands.len())] {
                run(command)?;
            }
            for item in &plan.definitions {
                remove_managed(&item.path)?;
            }
            remove_managed(&plan.manifest_path)?;
            for command in &plan.commands[split.min(plan.commands.len())..] {
                run(command)?;
            }
        }
    }
    Ok(UpdateScheduleReport {
        plan,
        dry_run: false,
        changed: true,
    })
}

pub fn load_update_schedule(
    state_directory: &Path,
) -> Result<Option<UpdateSchedulePlan>, UpdateScheduleError> {
    let path = state_directory.join("update-schedule.json");
    let Some(bytes) = safe_read(&path)? else {
        return Ok(None);
    };
    let plan: UpdateSchedulePlan = serde_json::from_slice(&bytes)?;
    if plan.version != UPDATE_SCHEDULE_PLAN_VERSION
        || plan.action != UpdateScheduleAction::Install
        || plan.manifest_path != path
    {
        return Err(UpdateScheduleError::UnsafeManagedPath(path));
    }
    Ok(Some(plan))
}

fn updater_arguments(options: &UpdateScheduleOptions) -> Vec<String> {
    let mut values = vec![
        "self-update".into(),
        "run".into(),
        "--channel".into(),
        options.channel.clone(),
    ];
    if let Some(url) = &options.channel_url {
        values.extend(["--channel-url".into(), url.clone()]);
    }
    if options.notify_on_failure {
        values.push("--notify-on-failure".into());
    }
    values
}
fn parse_time(value: &str) -> Result<(u8, u8), UpdateScheduleError> {
    let (hour, minute) = value
        .split_once(':')
        .ok_or_else(|| UpdateScheduleError::InvalidTime(value.to_string()))?;
    if hour.len() != 2 || minute.len() != 2 {
        return Err(UpdateScheduleError::InvalidTime(value.to_string()));
    }
    let hour: u8 = hour
        .parse()
        .map_err(|_| UpdateScheduleError::InvalidTime(value.to_string()))?;
    let minute: u8 = minute
        .parse()
        .map_err(|_| UpdateScheduleError::InvalidTime(value.to_string()))?;
    if hour > 23 || minute > 59 {
        return Err(UpdateScheduleError::InvalidTime(value.to_string()));
    }
    Ok((hour, minute))
}
fn definition(path: PathBuf, contents: String) -> UpdateScheduleDefinition {
    UpdateScheduleDefinition {
        path,
        contents,
        executable: false,
        windows_utf16: false,
    }
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_string()).collect()
}
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn systemd_quote(path: &Path) -> String {
    systemd_word(&path.to_string_lossy())
}
fn systemd_word(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('%', "%%")
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}
fn windows_argument(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn valid_sid(value: &str) -> bool {
    value.starts_with("S-1-")
        && value.len() <= 184
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'-' || b == b'S')
        && value.split('-').skip(1).all(|p| !p.is_empty())
}

fn write_managed(item: &UpdateScheduleDefinition) -> Result<(), UpdateScheduleError> {
    write_bytes(
        &item.path,
        item.contents.as_bytes(),
        item.executable,
        item.windows_utf16,
    )
}
fn write_bytes(
    path: &Path,
    bytes: &[u8],
    executable: bool,
    windows_utf16: bool,
) -> Result<(), UpdateScheduleError> {
    let parent = path
        .parent()
        .ok_or_else(|| UpdateScheduleError::UnsafeManagedPath(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    ensure_safe(path)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    if windows_utf16 {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| UpdateScheduleError::UnsafeManagedPath(path.to_path_buf()))?;
        temporary.write_all(&[0xff, 0xfe])?;
        for unit in text.encode_utf16() {
            temporary.write_all(&unit.to_le_bytes())?;
        }
    } else {
        temporary.write_all(bytes)?;
    }
    temporary.as_file().sync_all()?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o700))?;
    }
    let _ = executable;
    temporary
        .persist(path)
        .map_err(|error| UpdateScheduleError::Io(error.error))?;
    Ok(())
}
fn safe_read(path: &Path) -> Result<Option<Vec<u8>>, UpdateScheduleError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && metadata.len() <= 1024 * 1024 => {
            Ok(Some(fs::read(path)?))
        }
        Ok(_) => Err(UpdateScheduleError::UnsafeManagedPath(path.to_path_buf())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn ensure_safe(path: &Path) -> Result<(), UpdateScheduleError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(UpdateScheduleError::UnsafeManagedPath(path.to_path_buf())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
fn remove_managed(path: &Path) -> Result<(), UpdateScheduleError> {
    ensure_safe(path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
fn run_command(command: &UpdateScheduleCommand) -> Result<(), UpdateScheduleError> {
    let output = Command::new(&command.program)
        .args(&command.arguments)
        .output()?;
    if output.status.success() || command.tolerate_failure {
        return Ok(());
    }
    Err(UpdateScheduleError::CommandFailed {
        program: command.program.clone(),
        exit_code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(16 * 1024)])
            .trim()
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(root: &Path) -> PathBuf {
        let path = root.join("vulcan binary");
        fs::write(&path, b"vulcan").unwrap();
        path
    }
    fn plan(platform: UpdateSchedulePlatform) -> UpdateSchedulePlan {
        let root = tempfile::tempdir().unwrap().keep();
        plan_update_schedule(
            UpdateScheduleAction::Install,
            platform,
            &executable(&root),
            &root.join("config/vulcan"),
            &root.join("state/vulcan"),
            &root.join("home"),
            Some("S-1-5-21-42"),
            501,
            UpdateScheduleOptions {
                notify_on_failure: true,
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn renders_each_native_scheduler_without_shelling_the_update_command() {
        let systemd = plan(UpdateSchedulePlatform::SystemdUser);
        assert!(systemd.definitions[0]
            .contents
            .contains("self-update\" \"run"));
        assert!(systemd.definitions[0]
            .contents
            .contains("Environment=\"XDG_CONFIG_HOME="));
        assert!(systemd.definitions[1]
            .contents
            .contains("OnCalendar=*-*-* 03:00:00"));
        let launchd = plan(UpdateSchedulePlatform::LaunchdUser);
        assert!(launchd.definitions[0]
            .contents
            .contains("<key>StartCalendarInterval</key>"));
        let windows = plan(UpdateSchedulePlatform::WindowsScheduledTask);
        assert!(windows.definitions[0]
            .contents
            .contains("<CalendarTrigger>"));
        assert!(windows.definitions[0].windows_utf16);
        let android = plan(UpdateSchedulePlatform::TermuxJob);
        assert!(android.commands[0]
            .arguments
            .contains(&"86400000".to_string()));
        assert!(android.definitions[0]
            .contents
            .contains("--notify-on-failure"));
    }

    #[test]
    fn rejects_invalid_schedule_values() {
        let root = tempfile::tempdir().unwrap();
        let executable = executable(root.path());
        let error = plan_update_schedule(
            UpdateScheduleAction::Install,
            UpdateSchedulePlatform::SystemdUser,
            &executable,
            &root.path().join("config"),
            &root.path().join("state"),
            &root.path().join("home"),
            None,
            0,
            UpdateScheduleOptions {
                daily_at: "24:00".into(),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(error, UpdateScheduleError::InvalidTime(_)));

        let error = plan_update_schedule(
            UpdateScheduleAction::Install,
            UpdateSchedulePlatform::SystemdUser,
            &executable,
            &root.path().join("config"),
            &root.path().join("state"),
            &root.path().join("home"),
            None,
            0,
            UpdateScheduleOptions {
                channel_url: Some("https://example.test/channel.json\nExecStart=evil".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(error, UpdateScheduleError::InvalidChannelUrl(_)));
    }

    #[test]
    fn apply_persists_a_reloadable_manifest_and_uninstall_removes_it() {
        let install = plan(UpdateSchedulePlatform::SystemdUser);
        let state = install.manifest_path.parent().unwrap().to_path_buf();
        let report = apply_update_schedule_with(install.clone(), |_| Ok(())).unwrap();
        assert!(report.changed);
        assert_eq!(load_update_schedule(&state).unwrap().unwrap(), install);
        let mut uninstall = install;
        uninstall.action = UpdateScheduleAction::Uninstall;
        apply_update_schedule_with(uninstall, |_| Ok(())).unwrap();
        assert!(load_update_schedule(&state).unwrap().is_none());
    }

    #[test]
    fn dry_run_has_no_filesystem_effects() {
        let install = plan(UpdateSchedulePlatform::SystemdUser);
        let manifest = install.manifest_path.clone();
        apply_update_schedule(install, true).unwrap();
        assert!(!manifest.exists());
    }
}
