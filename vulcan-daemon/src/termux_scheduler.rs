//! Android/Termux adapter for finite, OS-scheduled synchronization.

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
    let job_id = options.job_id.unwrap_or_else(|| stable_job_id(wiki_id));
    if job_id == 0 || job_id > i32::MAX as u32 {
        return Err(TermuxSyncError::InvalidJobId(job_id));
    }

    let directory = state_root.join("termux-sync");
    let script_path = directory.join(format!("{wiki_id}.sh"));
    let manifest_path = directory.join(format!("{wiki_id}.json"));
    let script = (action == TermuxSyncAction::Install).then(|| render_script(executable, wiki_id));
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
            if let Some(existing) = load_termux_sync_plan_path(&plan.manifest_path, &plan.wiki_id)?
            {
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
        }
        TermuxSyncAction::Uninstall => {
            ensure_safe_existing(&plan.script_path)?;
            ensure_safe_existing(&plan.manifest_path)?;
            run(&plan.scheduler_program, &plan.scheduler_arguments)?;
            remove_managed(&plan.script_path)?;
            remove_managed(&plan.manifest_path)?;
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

fn render_script(executable: &Path, wiki_id: &str) -> String {
    format!(
        "#!/data/data/com.termux/files/usr/bin/sh\nset -eu\numask 077\nexec {} --output json sync run {}\n",
        shell_quote(&executable.to_string_lossy()),
        shell_quote(wiki_id)
    )
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
}
