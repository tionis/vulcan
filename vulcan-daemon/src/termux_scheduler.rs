//! Android/Termux adapter for finite, OS-scheduled synchronization.

use crate::registry::NetworkNotificationMode;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::NamedTempFile;

pub const TERMUX_SYNC_PLAN_VERSION: u32 = 1;
const MINIMUM_PERIOD_MINUTES: u32 = 15;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_STDERR_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TermuxSyncAction {
    Install,
    Uninstall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TermuxNetwork {
    Any,
    Unmetered,
    Cellular,
    NotRoaming,
}

impl TermuxNetwork {
    const fn as_scheduler_value(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Unmetered => "unmetered",
            Self::Cellular => "cellular",
            Self::NotRoaming => "not_roaming",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermuxSyncPlan {
    pub version: u32,
    pub action: TermuxSyncAction,
    pub wiki_id: String,
    pub job_id: u32,
    pub period_minutes: u32,
    pub network: TermuxNetwork,
    pub battery_not_low: bool,
    pub charging: bool,
    pub persisted: bool,
    #[serde(default)]
    pub network_notification_mode: NetworkNotificationMode,
    #[serde(default = "default_network_failure_count")]
    pub network_failure_count: u32,
    #[serde(default = "default_network_failure_minutes")]
    pub network_failure_minutes: u32,
    pub executable: PathBuf,
    pub script_path: PathBuf,
    pub manifest_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    pub scheduler_program: String,
    pub scheduler_arguments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TermuxSyncInstallOptions {
    pub period_minutes: u32,
    pub network: TermuxNetwork,
    pub battery_not_low: bool,
    pub charging: bool,
    pub persisted: bool,
    pub job_id: Option<u32>,
    pub network_notification_mode: NetworkNotificationMode,
    pub network_failure_count: u32,
    pub network_failure_minutes: u32,
}

impl Default for TermuxSyncInstallOptions {
    fn default() -> Self {
        Self {
            period_minutes: 60,
            network: TermuxNetwork::Any,
            battery_not_low: true,
            charging: false,
            persisted: true,
            job_id: None,
            network_notification_mode: NetworkNotificationMode::Immediate,
            network_failure_count: default_network_failure_count(),
            network_failure_minutes: default_network_failure_minutes(),
        }
    }
}

/// Changes to an installed job; omitted fields retain their recorded values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TermuxSyncUpdate {
    pub period_minutes: Option<u32>,
    pub network: Option<TermuxNetwork>,
    pub battery_not_low: Option<bool>,
    pub charging: Option<bool>,
    pub persisted: Option<bool>,
    pub network_notification_mode: Option<NetworkNotificationMode>,
    pub network_failure_count: Option<u32>,
    pub network_failure_minutes: Option<u32>,
}

impl TermuxSyncUpdate {
    #[must_use]
    pub fn apply_to(&self, installed: &TermuxSyncPlan) -> TermuxSyncInstallOptions {
        TermuxSyncInstallOptions {
            period_minutes: self.period_minutes.unwrap_or(installed.period_minutes),
            network: self.network.unwrap_or(installed.network),
            battery_not_low: self.battery_not_low.unwrap_or(installed.battery_not_low),
            charging: self.charging.unwrap_or(installed.charging),
            persisted: self.persisted.unwrap_or(installed.persisted),
            job_id: Some(installed.job_id),
            network_notification_mode: self
                .network_notification_mode
                .unwrap_or(installed.network_notification_mode),
            network_failure_count: self
                .network_failure_count
                .unwrap_or(installed.network_failure_count),
            network_failure_minutes: self
                .network_failure_minutes
                .unwrap_or(installed.network_failure_minutes),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TermuxSyncReport {
    #[serde(flatten)]
    pub plan: TermuxSyncPlan,
    pub dry_run: bool,
    pub changed: bool,
}

#[derive(Debug)]
pub enum TermuxSyncError {
    InvalidWikiId(String),
    InvalidExecutable(PathBuf),
    InvalidStateRoot(PathBuf),
    InvalidPeriod(u32),
    InvalidJobId(u32),
    InvalidNotificationThreshold,
    JobIdChanged {
        existing: u32,
        proposed: u32,
    },
    UnsafeManagedPath(PathBuf),
    UnsupportedHost,
    CommandFailed {
        exit_code: Option<i32>,
        stderr: String,
    },
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl Display for TermuxSyncError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidWikiId(id) => write!(formatter, "invalid registered wiki ID `{id}`"),
            Self::InvalidExecutable(path) => write!(
                formatter,
                "Vulcan executable must be an absolute regular file: {}",
                path.display()
            ),
            Self::InvalidStateRoot(path) => write!(
                formatter,
                "Termux scheduler state root must be absolute: {}",
                path.display()
            ),
            Self::InvalidPeriod(minutes) => write!(
                formatter,
                "Termux periodic jobs require at least {MINIMUM_PERIOD_MINUTES} minutes, got {minutes}"
            ),
            Self::InvalidNotificationThreshold => formatter.write_str("network notification count and duration must be positive"),
            Self::InvalidJobId(id) => write!(
                formatter,
                "Termux scheduler job ID must be between 1 and 2147483647, got {id}"
            ),
            Self::JobIdChanged { existing, proposed } => write!(
                formatter,
                "wiki already owns Termux job {existing}; uninstall it before changing to job ID {proposed}"
            ),
            Self::UnsafeManagedPath(path) => write!(
                formatter,
                "refusing unsafe Termux scheduler managed path: {}",
                path.display()
            ),
            Self::UnsupportedHost => formatter.write_str(
                "Termux scheduling can only be applied from Android/Termux; use --dry-run elsewhere",
            ),
            Self::CommandFailed { exit_code, stderr } => {
                formatter.write_str("`termux-job-scheduler` failed")?;
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

impl Error for TermuxSyncError {}

impl From<std::io::Error> for TermuxSyncError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for TermuxSyncError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub fn plan_termux_sync(
    action: TermuxSyncAction,
    wiki_id: &str,
    executable: &Path,
    state_root: &Path,
    options: &TermuxSyncInstallOptions,
) -> Result<TermuxSyncPlan, TermuxSyncError> {
    validate_wiki_id(wiki_id)?;
    if !executable.is_absolute() || !executable.is_file() {
        return Err(TermuxSyncError::InvalidExecutable(executable.to_path_buf()));
    }
    if !state_root.is_absolute() {
        return Err(TermuxSyncError::InvalidStateRoot(state_root.to_path_buf()));
    }
    if options.period_minutes < MINIMUM_PERIOD_MINUTES {
        return Err(TermuxSyncError::InvalidPeriod(options.period_minutes));
    }
    if options.network_failure_count == 0 || options.network_failure_minutes == 0 {
        return Err(TermuxSyncError::InvalidNotificationThreshold);
    }
    let job_id = options.job_id.unwrap_or_else(|| stable_job_id(wiki_id));
    if job_id == 0 || job_id > i32::MAX as u32 {
        return Err(TermuxSyncError::InvalidJobId(job_id));
    }

    let directory = state_root.join("termux-sync");
    let script_path = directory.join(format!("{wiki_id}.sh"));
    let manifest_path = directory.join(format!("{wiki_id}.json"));
    let notification_state_path = directory.join(format!("{wiki_id}.notification-state"));
    let script = (action == TermuxSyncAction::Install).then(|| {
        render_script(
            executable,
            wiki_id,
            job_id,
            &notification_state_path,
            options,
        )
    });
    let scheduler_arguments = match action {
        TermuxSyncAction::Install => vec![
            "--script".to_string(),
            script_path.to_string_lossy().into_owned(),
            "--job-id".to_string(),
            job_id.to_string(),
            "--period-ms".to_string(),
            (u64::from(options.period_minutes) * 60_000).to_string(),
            "--network".to_string(),
            options.network.as_scheduler_value().to_string(),
            "--battery-not-low".to_string(),
            options.battery_not_low.to_string(),
            "--storage-not-low".to_string(),
            "true".to_string(),
            "--charging".to_string(),
            options.charging.to_string(),
            "--persisted".to_string(),
            options.persisted.to_string(),
        ],
        TermuxSyncAction::Uninstall => vec![
            "--cancel".to_string(),
            "--job-id".to_string(),
            job_id.to_string(),
        ],
    };
    Ok(TermuxSyncPlan {
        version: TERMUX_SYNC_PLAN_VERSION,
        action,
        wiki_id: wiki_id.to_string(),
        job_id,
        period_minutes: options.period_minutes,
        network: options.network,
        battery_not_low: options.battery_not_low,
        charging: options.charging,
        persisted: options.persisted,
        network_notification_mode: options.network_notification_mode,
        network_failure_count: options.network_failure_count,
        network_failure_minutes: options.network_failure_minutes,
        executable: executable.to_path_buf(),
        script_path,
        manifest_path,
        script,
        scheduler_program: "termux-job-scheduler".to_string(),
        scheduler_arguments,
    })
}

pub fn load_termux_sync_plan(
    state_root: &Path,
    wiki_id: &str,
) -> Result<Option<TermuxSyncPlan>, TermuxSyncError> {
    validate_wiki_id(wiki_id)?;
    let path = state_root
        .join("termux-sync")
        .join(format!("{wiki_id}.json"));
    load_termux_sync_plan_path(&path, wiki_id)
}

fn load_termux_sync_plan_path(
    path: &Path,
    wiki_id: &str,
) -> Result<Option<TermuxSyncPlan>, TermuxSyncError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_MANIFEST_BYTES {
        return Err(TermuxSyncError::UnsafeManagedPath(path.to_path_buf()));
    }
    let plan: TermuxSyncPlan = serde_json::from_slice(&fs::read(path)?)?;
    if plan.version != TERMUX_SYNC_PLAN_VERSION
        || plan.action != TermuxSyncAction::Install
        || plan.wiki_id != wiki_id
        || plan.manifest_path != path
    {
        return Err(TermuxSyncError::UnsafeManagedPath(path.to_path_buf()));
    }
    Ok(Some(plan))
}

pub fn apply_termux_sync(
    plan: TermuxSyncPlan,
    dry_run: bool,
) -> Result<TermuxSyncReport, TermuxSyncError> {
    if dry_run {
        return Ok(TermuxSyncReport {
            plan,
            dry_run,
            changed: false,
        });
    }
    if !cfg!(target_os = "android") || std::env::var_os("PREFIX").is_none() {
        return Err(TermuxSyncError::UnsupportedHost);
    }
    apply_termux_sync_with(plan, run_scheduler)
}

fn apply_termux_sync_with(
    plan: TermuxSyncPlan,
    run: impl FnOnce(&str, &[String]) -> Result<(), TermuxSyncError>,
) -> Result<TermuxSyncReport, TermuxSyncError> {
    match plan.action {
        TermuxSyncAction::Install => {
            let existing = load_termux_sync_plan_path(&plan.manifest_path, &plan.wiki_id)?;
            let reset_notification_state = existing.as_ref().is_some_and(|existing| {
                existing.network_notification_mode != plan.network_notification_mode
                    || existing.network_failure_count != plan.network_failure_count
                    || existing.network_failure_minutes != plan.network_failure_minutes
            });
            if let Some(existing) = existing {
                if existing.job_id != plan.job_id {
                    return Err(TermuxSyncError::JobIdChanged {
                        existing: existing.job_id,
                        proposed: plan.job_id,
                    });
                }
            }
            let script = plan
                .script
                .as_deref()
                .ok_or_else(|| TermuxSyncError::UnsafeManagedPath(plan.script_path.clone()))?;
            ensure_safe_existing(&plan.script_path)?;
            ensure_safe_existing(&plan.manifest_path)?;
            let previous_script = fs::read(&plan.script_path).ok();
            let previous_manifest = fs::read(&plan.manifest_path).ok();
            write_managed(&plan.script_path, script.as_bytes(), true)?;
            let manifest = serde_json::to_vec_pretty(&plan)?;
            write_managed(&plan.manifest_path, &manifest, false)?;
            if let Err(error) = run(&plan.scheduler_program, &plan.scheduler_arguments) {
                restore_managed(&plan.script_path, previous_script.as_deref(), true);
                restore_managed(&plan.manifest_path, previous_manifest.as_deref(), false);
                return Err(error);
            }
            if reset_notification_state {
                remove_managed(&plan.script_path.with_extension("notification-state"))?;
            }
        }
        TermuxSyncAction::Uninstall => {
            ensure_safe_existing(&plan.script_path)?;
            ensure_safe_existing(&plan.manifest_path)?;
            run(&plan.scheduler_program, &plan.scheduler_arguments)?;
            remove_managed(&plan.script_path)?;
            remove_managed(&plan.manifest_path)?;
            remove_managed(&plan.script_path.with_extension("notification-state"))?;
        }
    }
    Ok(TermuxSyncReport {
        plan,
        dry_run: false,
        changed: true,
    })
}

fn restore_managed(path: &Path, previous: Option<&[u8]>, executable: bool) {
    if let Some(bytes) = previous {
        let _ = write_managed(path, bytes, executable);
    } else {
        let _ = remove_managed(path);
    }
}

fn run_scheduler(program: &str, arguments: &[String]) -> Result<(), TermuxSyncError> {
    let output = Command::new(program).args(arguments).output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(TermuxSyncError::CommandFailed {
        exit_code: output.status.code(),
        stderr: String::from_utf8_lossy(
            &output.stderr[..output.stderr.len().min(MAX_STDERR_BYTES)],
        )
        .trim()
        .to_string(),
    })
}

fn write_managed(path: &Path, bytes: &[u8], executable: bool) -> Result<(), TermuxSyncError> {
    let parent = path
        .parent()
        .ok_or_else(|| TermuxSyncError::UnsafeManagedPath(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    ensure_safe_existing(path)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
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
        .map_err(|error| TermuxSyncError::Io(error.error))?;
    Ok(())
}

fn ensure_safe_existing(path: &Path) -> Result<(), TermuxSyncError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            Err(TermuxSyncError::UnsafeManagedPath(path.to_path_buf()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remove_managed(path: &Path) -> Result<(), TermuxSyncError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

const fn default_network_failure_count() -> u32 {
    3
}
const fn default_network_failure_minutes() -> u32 {
    15
}

fn render_script(
    executable: &Path,
    wiki_id: &str,
    notification_id: u32,
    state_path: &Path,
    options: &TermuxSyncInstallOptions,
) -> String {
    use std::fmt::Write as _;
    let mode = match options.network_notification_mode {
        NetworkNotificationMode::Immediate => "immediate",
        NetworkNotificationMode::Ignore => "ignore",
        NetworkNotificationMode::Count => "count",
        NetworkNotificationMode::Duration => "duration",
    };
    let mut script = format!(
        "#!/data/data/com.termux/files/usr/bin/sh\nset -u\numask 077\nstate={}\nmode={}\nlimit_count={}\nlimit_seconds={}\nnotification_id={}\nwiki={}\noutput=$(mktemp \"$state.output.XXXXXX\") || exit 1\n",
        shell_quote(&state_path.to_string_lossy()),
        shell_quote(mode),
        options.network_failure_count,
        u64::from(options.network_failure_minutes) * 60,
        notification_id,
        shell_quote(wiki_id),
    );
    write!(
        script,
        "if {} --output json sync run {} > \"$output\"; then\n  cat \"$output\"\n  rm -f \"$output\" \"$state\"\n  termux-notification-remove \"$notification_id\" >/dev/null 2>&1 || :\n  exit 0\nelse\n  status=$?\nfi\n",
        shell_quote(&executable.to_string_lossy()), shell_quote(wiki_id),
    ).expect("write shell script");
    script.push_str(r#"cat "$output"
if grep -F -q '"error_category": "network"' "$output"; then
  rm -f "$output"
  count=0
  first=0
  notified=0
  if [ -f "$state" ]; then
    read -r count first notified < "$state" || :
  fi
  case "$count:$first:$notified" in
    *[!0-9:]*|:*|*::*|*:) count=0; first=0; notified=0 ;;
  esac
  now=$(date +%s)
  if [ "$first" -eq 0 ] || [ "$now" -lt "$first" ]; then first=$now; count=0; notified=0; fi
  count=$((count + 1))
  alert=0
  case "$mode" in
    immediate) alert=1 ;;
    count) if [ "$count" -ge "$limit_count" ] && [ "$notified" -eq 0 ]; then alert=1; fi ;;
    duration) if [ $((now - first)) -ge "$limit_seconds" ] && [ "$notified" -eq 0 ]; then alert=1; fi ;;
  esac
  if [ "$alert" -eq 1 ]; then
    elapsed_minutes=$(((now - first) / 60))
    termux-notification --id "$notification_id" --group vulcan-sync --priority high --title 'Vulcan sync: network unavailable' --content "Wiki $wiki: $count network failure(s) over $elapsed_minutes minute(s). Run vulcan sync status $wiki for details." >/dev/null 2>&1 || :
    notified=1
  elif [ "$notified" -eq 0 ]; then
    termux-notification-remove "$notification_id" >/dev/null 2>&1 || :
  fi
  printf '%s %s %s\n' "$count" "$first" "$notified" > "$state.tmp.$$" && mv -f "$state.tmp.$$" "$state"
else
  reason='sync failure'
  for category in authentication configuration repository conflict busy io observer invariant unsupported cancelled unknown; do
    if grep -F -q "\"error_category\": \"$category\"" "$output"; then reason="$category failure"; break; fi
  done
  if grep -F -q '"conflicted": 1' "$output"; then reason='sync conflict'; fi
  if grep -F -q '"incomplete": 1' "$output"; then reason='incomplete sync'; fi
  rm -f "$output" "$state"
  termux-notification --id "$notification_id" --group vulcan-sync --priority high --title "Vulcan sync: $reason" --content "Wiki $wiki: $reason. Run vulcan sync status $wiki for details." >/dev/null 2>&1 || :
fi
exit "$status"
"#);
    script
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn stable_job_id(wiki_id: &str) -> u32 {
    let hash = wiki_id.bytes().fold(2_166_136_261_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    });
    1_000_000_000 + hash % 1_000_000_000
}

fn validate_wiki_id(id: &str) -> Result<(), TermuxSyncError> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && id.bytes().enumerate().all(|(index, byte)| match byte {
            b'a'..=b'z' | b'0'..=b'9' => true,
            b'-' | b'_' => index > 0,
            _ => false,
        });
    if valid {
        Ok(())
    } else {
        Err(TermuxSyncError::InvalidWikiId(id.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(directory: &Path) -> PathBuf {
        let path = directory.join("vulcan's binary");
        fs::write(&path, b"binary").expect("executable fixture");
        path
    }

    #[test]
    fn install_plan_is_persisted_battery_aware_and_shell_safe() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let executable = executable(temporary.path());
        let options = TermuxSyncInstallOptions::default();
        let plan = plan_termux_sync(
            TermuxSyncAction::Install,
            "personal",
            &executable,
            &temporary.path().join("state"),
            &options,
        )
        .expect("install plan");

        assert_eq!(plan.period_minutes, 60);
        assert!(plan.battery_not_low);
        assert!(plan.persisted);
        assert!(plan
            .scheduler_arguments
            .windows(2)
            .any(|pair| pair == ["--storage-not-low", "true"]));
        assert!(plan
            .scheduler_arguments
            .windows(2)
            .any(|pair| pair == ["--period-ms", "3600000"]));
        let script = plan.script.expect("script");
        assert!(script.contains("vulcan'\\''s binary'"));
        assert!(script.contains("--output json sync run 'personal'"));
        assert!(script.contains("termux-notification --id"));
        assert!(script.contains("--group vulcan-sync --priority high"));
        assert!(script.contains("termux-notification-remove"));
        assert!(script.contains("exit \"$status\""));
        assert!(!script.contains("eval"));
    }

    #[test]
    fn older_manifests_default_to_immediate_notifications() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let plan = plan_termux_sync(
            TermuxSyncAction::Install,
            "personal",
            &executable(temporary.path()),
            &temporary.path().join("state"),
            &TermuxSyncInstallOptions::default(),
        )
        .expect("plan");
        let mut value = serde_json::to_value(&plan).expect("plan JSON");
        let object = value.as_object_mut().expect("object");
        object.remove("network_notification_mode");
        object.remove("network_failure_count");
        object.remove("network_failure_minutes");
        let restored: TermuxSyncPlan = serde_json::from_value(value).expect("old manifest");
        assert_eq!(
            restored.network_notification_mode,
            NetworkNotificationMode::Immediate
        );
        assert_eq!(restored.network_failure_count, 3);
        assert_eq!(restored.network_failure_minutes, 15);
    }

    #[test]
    fn dry_run_never_writes_or_invokes_android_tools() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let plan = plan_termux_sync(
            TermuxSyncAction::Install,
            "personal",
            &executable(temporary.path()),
            &temporary.path().join("state"),
            &TermuxSyncInstallOptions::default(),
        )
        .expect("install plan");
        let script_path = plan.script_path.clone();

        let report = apply_termux_sync(plan, true).expect("dry run");
        assert!(!report.changed);
        assert!(!script_path.exists());
    }

    #[test]
    fn adapter_writes_and_removes_only_managed_regular_files() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let executable = executable(temporary.path());
        let options = TermuxSyncInstallOptions::default();
        let install = plan_termux_sync(
            TermuxSyncAction::Install,
            "personal",
            &executable,
            &temporary.path().join("state"),
            &options,
        )
        .expect("install plan");
        let script_path = install.script_path.clone();
        let manifest_path = install.manifest_path.clone();
        let report = apply_termux_sync_with(install, |program, arguments| {
            assert_eq!(program, "termux-job-scheduler");
            assert!(arguments.contains(&"--period-ms".to_string()));
            Ok(())
        })
        .expect("install");
        assert!(report.changed);
        assert!(script_path.is_file());
        assert!(manifest_path.is_file());
        let notification_state = script_path.with_extension("notification-state");
        fs::write(&notification_state, "2 100 0\n").expect("notification state");
        let loaded = load_termux_sync_plan(&temporary.path().join("state"), "personal")
            .expect("load manifest")
            .expect("installed manifest");
        assert_eq!(loaded.job_id, report.plan.job_id);

        let replacement = plan_termux_sync(
            TermuxSyncAction::Install,
            "personal",
            &executable,
            &temporary.path().join("state"),
            &TermuxSyncInstallOptions {
                job_id: Some(42),
                ..TermuxSyncInstallOptions::default()
            },
        )
        .expect("replacement plan");
        assert!(matches!(
            apply_termux_sync_with(replacement, |_, _| panic!("must not reschedule")),
            Err(TermuxSyncError::JobIdChanged { .. })
        ));

        let uninstall = plan_termux_sync(
            TermuxSyncAction::Uninstall,
            "personal",
            &executable,
            &temporary.path().join("state"),
            &options,
        )
        .expect("uninstall plan");
        apply_termux_sync_with(uninstall, |_, arguments| {
            assert_eq!(arguments.first().map(String::as_str), Some("--cancel"));
            Ok(())
        })
        .expect("uninstall");
        assert!(!script_path.exists());
        assert!(!manifest_path.exists());
        assert!(!notification_state.exists());
    }

    #[test]
    fn planner_rejects_android_and_scheduler_hazards() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let executable = executable(temporary.path());
        let state = temporary.path().join("state");
        let options = TermuxSyncInstallOptions {
            period_minutes: 14,
            ..TermuxSyncInstallOptions::default()
        };
        assert!(matches!(
            plan_termux_sync(
                TermuxSyncAction::Install,
                "personal",
                &executable,
                &state,
                &options
            ),
            Err(TermuxSyncError::InvalidPeriod(14))
        ));
        let options = TermuxSyncInstallOptions {
            period_minutes: 15,
            job_id: Some(0),
            ..TermuxSyncInstallOptions::default()
        };
        assert!(matches!(
            plan_termux_sync(
                TermuxSyncAction::Install,
                "personal",
                &executable,
                &state,
                &options
            ),
            Err(TermuxSyncError::InvalidJobId(0))
        ));
        let options = TermuxSyncInstallOptions {
            network_failure_count: 0,
            ..TermuxSyncInstallOptions::default()
        };
        assert!(matches!(
            plan_termux_sync(
                TermuxSyncAction::Install,
                "personal",
                &executable,
                &state,
                &options
            ),
            Err(TermuxSyncError::InvalidNotificationThreshold)
        ));
        let options = TermuxSyncInstallOptions {
            network_failure_minutes: 0,
            ..TermuxSyncInstallOptions::default()
        };
        assert!(matches!(
            plan_termux_sync(
                TermuxSyncAction::Install,
                "personal",
                &executable,
                &state,
                &options
            ),
            Err(TermuxSyncError::InvalidNotificationThreshold)
        ));
        assert!(matches!(
            plan_termux_sync(
                TermuxSyncAction::Install,
                "Bad ID",
                &executable,
                &state,
                &TermuxSyncInstallOptions::default()
            ),
            Err(TermuxSyncError::InvalidWikiId(_))
        ));
    }

    #[test]
    fn failed_scheduler_install_rolls_back_managed_files() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let plan = plan_termux_sync(
            TermuxSyncAction::Install,
            "personal",
            &executable(temporary.path()),
            &temporary.path().join("state"),
            &TermuxSyncInstallOptions::default(),
        )
        .expect("install plan");
        let script = plan.script_path.clone();
        let manifest = plan.manifest_path.clone();

        let error = apply_termux_sync_with(plan, |_, _| {
            Err(TermuxSyncError::CommandFailed {
                exit_code: Some(1),
                stderr: "fixture failure".to_string(),
            })
        })
        .expect_err("scheduler failure");
        assert!(matches!(error, TermuxSyncError::CommandFailed { .. }));
        assert!(!script.exists());
        assert!(!manifest.exists());
    }

    #[test]
    fn schedule_updates_preserve_settings_and_replace_the_same_job() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let executable = executable(temporary.path());
        let state = temporary.path().join("state");
        let plan = plan_termux_sync(
            TermuxSyncAction::Install,
            "personal",
            &executable,
            &state,
            &TermuxSyncInstallOptions {
                period_minutes: 60,
                network: TermuxNetwork::Unmetered,
                charging: true,
                battery_not_low: true,
                persisted: true,
                job_id: Some(42),
                network_notification_mode: NetworkNotificationMode::Count,
                network_failure_count: 4,
                network_failure_minutes: 20,
            },
        )
        .expect("plan");
        apply_termux_sync_with(plan.clone(), |_, _| Ok(())).expect("install");
        let notification_state = plan.script_path.with_extension("notification-state");
        fs::write(&notification_state, "2 100 0\n").expect("notification state");
        let updated = TermuxSyncUpdate {
            period_minutes: Some(30),
            ..Default::default()
        }
        .apply_to(&plan);
        assert_eq!(
            updated,
            TermuxSyncInstallOptions {
                period_minutes: 30,
                network: TermuxNetwork::Unmetered,
                charging: true,
                battery_not_low: true,
                persisted: true,
                job_id: Some(42),
                network_notification_mode: NetworkNotificationMode::Count,
                network_failure_count: 4,
                network_failure_minutes: 20,
            }
        );
        let replacement = plan_termux_sync(
            TermuxSyncAction::Install,
            "personal",
            &executable,
            &state,
            &updated,
        )
        .expect("replacement");
        apply_termux_sync_with(replacement, |_, arguments| {
            assert!(arguments.windows(2).any(|pair| pair == ["--job-id", "42"]));
            assert!(arguments
                .windows(2)
                .any(|pair| pair == ["--period-ms", "1800000"]));
            Ok(())
        })
        .expect("reschedule same job");
        assert!(
            notification_state.exists(),
            "schedule-only update preserves failure state"
        );
        let installed = load_termux_sync_plan(&state, "personal")
            .expect("load")
            .expect("installed");
        assert_eq!(installed.period_minutes, 30);
        let reset = TermuxSyncUpdate {
            charging: Some(false),
            battery_not_low: Some(false),
            persisted: Some(false),
            network: Some(TermuxNetwork::Any),
            ..Default::default()
        }
        .apply_to(&installed);
        assert!(!reset.charging && !reset.battery_not_low && !reset.persisted);
        assert_eq!(reset.network, TermuxNetwork::Any);
        assert_eq!(reset.period_minutes, 30);
        assert_eq!(reset.job_id, Some(42));
        let policy_change = TermuxSyncUpdate {
            network_notification_mode: Some(NetworkNotificationMode::Ignore),
            ..Default::default()
        }
        .apply_to(&installed);
        let policy_plan = plan_termux_sync(
            TermuxSyncAction::Install,
            "personal",
            &executable,
            &state,
            &policy_change,
        )
        .expect("policy plan");
        apply_termux_sync_with(policy_plan, |_, _| Ok(())).expect("policy update");
        assert!(
            !notification_state.exists(),
            "policy change resets failure state"
        );
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::too_many_lines)] // Exercises one installed wrapper across the full outage lifecycle.
    fn wrapper_filters_network_failures_and_resets_after_recovery() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().expect("temporary directory");
        let bin = temporary.path().join("bin");
        fs::create_dir(&bin).expect("bin");
        let executable = bin.join("vulcan");
        fs::write(
            &executable,
            "#!/bin/sh\ncat \"$SYNC_REPORT\"\nexit \"$SYNC_EXIT\"\n",
        )
        .expect("vulcan stub");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).expect("executable");
        for helper in ["termux-notification", "termux-notification-remove"] {
            let path = bin.join(helper);
            fs::write(
                &path,
                format!("#!/bin/sh\nprintf '%s\\n' '{helper}' >> \"$NOTICE_LOG\"\nprintf '%s\\n' \"$*\" >> \"$NOTICE_DETAIL\"\n"),
            )
            .expect("notification stub");
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("executable");
        }
        let report = temporary.path().join("report.json");
        let log = temporary.path().join("notices.log");
        let detail = temporary.path().join("notice-detail.log");
        let state_root = temporary.path().join("state");
        let options = TermuxSyncInstallOptions {
            network_notification_mode: NetworkNotificationMode::Count,
            network_failure_count: 2,
            ..Default::default()
        };
        let plan = plan_termux_sync(
            TermuxSyncAction::Install,
            "personal",
            &executable,
            &state_root,
            &options,
        )
        .expect("plan");
        fs::create_dir_all(plan.script_path.parent().expect("parent")).expect("state root");
        fs::write(&plan.script_path, plan.script.as_deref().expect("script")).expect("script");
        let run = |code: &str| {
            let output = Command::new("sh")
                .arg(&plan.script_path)
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        bin.display(),
                        std::env::var("PATH").unwrap_or_default()
                    ),
                )
                .env("SYNC_REPORT", &report)
                .env("SYNC_EXIT", code)
                .env("NOTICE_LOG", &log)
                .env("NOTICE_DETAIL", &detail)
                .output()
                .expect("wrapper run");
            assert_eq!(
                output.status.code(),
                Some(code.parse::<i32>().expect("code"))
            );
        };
        fs::write(&report, "{\"items\":[{\"error_category\": \"network\"}]}").expect("report");
        run("7");
        assert!(!fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("termux-notification\n"));
        run("7");
        assert_eq!(
            fs::read_to_string(&log)
                .expect("log")
                .matches("termux-notification\n")
                .count(),
            1
        );
        run("7");
        assert_eq!(
            fs::read_to_string(&log)
                .expect("log")
                .matches("termux-notification\n")
                .count(),
            1
        );
        fs::write(&report, "{\"items\":[]}").expect("report");
        run("0");
        fs::write(&report, "{\"items\":[{\"error_category\": \"network\"}]}").expect("report");
        run("7");
        assert_eq!(
            fs::read_to_string(&log)
                .expect("log")
                .matches("termux-notification\n")
                .count(),
            1
        );
        fs::write(
            &report,
            "{\"items\":[{\"error_category\": \"authentication\"}]}",
        )
        .expect("report");
        run("7");
        assert_eq!(
            fs::read_to_string(&log)
                .expect("log")
                .matches("termux-notification\n")
                .count(),
            2
        );
        let messages = fs::read_to_string(&detail).expect("notice detail");
        assert!(messages.contains("network unavailable"));
        assert!(messages.contains("authentication failure"));
        let notification_state = state_root.join("termux-sync/personal.notification-state");
        assert!(!notification_state.exists());

        let ignore = plan_termux_sync(
            TermuxSyncAction::Install,
            "personal",
            &executable,
            &state_root,
            &TermuxSyncInstallOptions {
                network_notification_mode: NetworkNotificationMode::Ignore,
                ..Default::default()
            },
        )
        .expect("ignore plan");
        fs::write(&plan.script_path, ignore.script.expect("ignore script")).expect("script");
        fs::write(&report, "{\"items\":[{\"error_category\": \"network\"}]}").expect("report");
        run("7");
        assert_eq!(
            fs::read_to_string(&log)
                .expect("log")
                .matches("termux-notification\n")
                .count(),
            2
        );

        let duration = plan_termux_sync(
            TermuxSyncAction::Install,
            "personal",
            &executable,
            &state_root,
            &TermuxSyncInstallOptions {
                network_notification_mode: NetworkNotificationMode::Duration,
                network_failure_minutes: 1,
                ..Default::default()
            },
        )
        .expect("duration plan");
        fs::write(&plan.script_path, duration.script.expect("duration script")).expect("script");
        fs::remove_file(&notification_state).expect("reset state");
        run("7");
        assert_eq!(
            fs::read_to_string(&log)
                .expect("log")
                .matches("termux-notification\n")
                .count(),
            2
        );
        let first = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_secs()
            - 120;
        fs::write(&notification_state, format!("1 {first} 0\n")).expect("aged state");
        run("7");
        assert_eq!(
            fs::read_to_string(&log)
                .expect("log")
                .matches("termux-notification\n")
                .count(),
            3
        );
    }
}
