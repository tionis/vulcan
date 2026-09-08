//! Secret-minimal daemon attention events and local delivery.

use crate::sync::DaemonSyncExecution;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use vulcan_sync::{SyncErrorCategory, SyncJobState};

const DESKTOP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertSeverity {
    Warning,
    Error,
}

impl AlertSeverity {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncAlert {
    pub version: u32,
    pub event: &'static str,
    pub severity: AlertSeverity,
    pub job_id: String,
    pub wiki_id: String,
    pub state: SyncJobState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<SyncErrorCategory>,
    pub retryable: bool,
}

impl SyncAlert {
    #[must_use]
    pub fn from_execution(execution: &DaemonSyncExecution) -> Option<Self> {
        let (event, severity) = match execution.job.job.state {
            SyncJobState::Failed => ("sync_failed", AlertSeverity::Error),
            SyncJobState::Conflicted => ("sync_conflicted", AlertSeverity::Warning),
            SyncJobState::Paused => ("sync_paused", AlertSeverity::Warning),
            _ => return None,
        };
        let error = execution.job.job.error.as_ref();
        Some(Self {
            version: 1,
            event,
            severity,
            job_id: execution.job.job.id.clone(),
            wiki_id: execution
                .job
                .job
                .wiki_id
                .clone()
                .unwrap_or_else(|| "<unregistered>".to_string()),
            state: execution.job.job.state,
            category: error.map(|error| error.category),
            retryable: error.is_some_and(|error| error.retryable),
        })
    }

    #[must_use]
    pub fn log_line(&self) -> String {
        let category = self.category.map_or("none".to_string(), |category| {
            format!("{category:?}").to_ascii_lowercase()
        });
        format!(
            "level={} event={} wiki={} job={} category={} retryable={}; inspect retained synchronization status for details",
            self.severity.as_str(),
            self.event,
            self.wiki_id,
            self.job_id,
            category,
            self.retryable,
        )
    }

    fn fingerprint(&self) -> String {
        format!(
            "{}:{:?}:{:?}:{}",
            self.event, self.state, self.category, self.retryable
        )
    }

    fn desktop_title(&self) -> &'static str {
        match self.state {
            SyncJobState::Failed => "Vulcan sync failed",
            SyncJobState::Conflicted => "Vulcan sync needs resolution",
            SyncJobState::Paused => "Vulcan sync paused",
            _ => "Vulcan sync needs attention",
        }
    }

    fn desktop_body(&self) -> String {
        let reason = self.category.map_or(String::new(), |category| {
            format!(" ({})", format!("{category:?}").to_ascii_lowercase())
        });
        format!(
            "Wiki `{}` needs attention{}. Run `vulcan sync status {}` for details.",
            self.wiki_id, reason, self.wiki_id
        )
    }
}

#[derive(Debug, Default)]
pub struct SyncAlertTracker {
    last_by_wiki: BTreeMap<String, String>,
}

impl SyncAlertTracker {
    /// Returns a new or changed attention event. A healthy execution clears
    /// the per-wiki fingerprint so a later recurrence is delivered again.
    pub fn observe(&mut self, execution: &DaemonSyncExecution) -> Option<SyncAlert> {
        let wiki = execution
            .job
            .job
            .wiki_id
            .as_deref()
            .unwrap_or("<unregistered>")
            .to_string();
        let Some(alert) = SyncAlert::from_execution(execution) else {
            self.last_by_wiki.remove(&wiki);
            return None;
        };
        let fingerprint = alert.fingerprint();
        if self.last_by_wiki.get(&wiki) == Some(&fingerprint) {
            return None;
        }
        self.last_by_wiki.insert(wiki, fingerprint);
        Some(alert)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Every target constructs only its native variant; tests exercise all command
// renderers on one host so release behavior does not drift unnoticed.
#[allow(dead_code)]
enum DesktopPlatform {
    Linux,
    MacOs,
    Windows,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DesktopCommand {
    program: &'static str,
    args: Vec<String>,
}

fn desktop_command(platform: DesktopPlatform, alert: &SyncAlert) -> DesktopCommand {
    let title = alert.desktop_title().to_string();
    let body = alert.desktop_body();
    match platform {
        DesktopPlatform::Linux => DesktopCommand {
            program: "notify-send",
            args: vec![
                "--app-name=Vulcan".to_string(),
                format!(
                    "--urgency={}",
                    if alert.severity == AlertSeverity::Error {
                        "critical"
                    } else {
                        "normal"
                    }
                ),
                title,
                body,
            ],
        },
        DesktopPlatform::MacOs => DesktopCommand {
            program: "osascript",
            args: vec![
                "-e".to_string(),
                "on run argv".to_string(),
                "-e".to_string(),
                "display notification (item 2 of argv) with title (item 1 of argv)".to_string(),
                "-e".to_string(),
                "end run".to_string(),
                title,
                body,
            ],
        },
        DesktopPlatform::Windows => DesktopCommand {
            program: "powershell.exe",
            args: vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                "Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.NotifyIcon]::new() | ForEach-Object { $_.Icon = [System.Drawing.SystemIcons]::Warning; $_.BalloonTipTitle = $args[0]; $_.BalloonTipText = $args[1]; $_.Visible = $true; $_.ShowBalloonTip(5000); Start-Sleep -Milliseconds 5500; $_.Dispose() }".to_string(),
                title,
                body,
            ],
        },
    }
}

/// Delivers one native notification without invoking a shell. Absence of the
/// platform helper is reported to the operational log by the caller and never
/// changes synchronization state.
pub fn deliver_desktop(alert: &SyncAlert) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    let platform = DesktopPlatform::Linux;
    #[cfg(target_os = "macos")]
    let platform = DesktopPlatform::MacOs;
    #[cfg(target_os = "windows")]
    let platform = DesktopPlatform::Windows;
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "desktop notifications are unsupported on this platform",
    ));

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    {
        let plan = desktop_command(platform, alert);
        let mut child = Command::new(plan.program)
            .args(plan.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(io::Error::other(format!(
                        "desktop notification helper exited with {status}"
                    )))
                };
            }
            if started.elapsed() >= DESKTOP_TIMEOUT {
                child.kill()?;
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "desktop notification helper timed out",
                ));
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::SupervisedSyncJob;
    use crate::sync::DaemonSyncExecution;
    use std::path::PathBuf;
    use vulcan_sync::{SyncError, SyncJob, SyncJobState, SyncJobTrigger, SYNC_CONTRACT_VERSION};

    fn execution(state: SyncJobState, error: Option<SyncError>) -> DaemonSyncExecution {
        DaemonSyncExecution {
            job: SupervisedSyncJob {
                job: SyncJob {
                    version: SYNC_CONTRACT_VERSION,
                    id: "job-1".to_string(),
                    wiki_id: Some("alpha".to_string()),
                    backend: "git".to_string(),
                    vault: PathBuf::from("/vault"),
                    trigger: SyncJobTrigger::Poll,
                    state,
                    status: None,
                    error,
                },
                triggers: vec![SyncJobTrigger::Poll],
                watch: None,
            },
            report: None,
        }
    }

    #[test]
    fn attention_states_are_structured_and_secret_minimal() {
        let failed = execution(
            SyncJobState::Failed,
            Some(SyncError::new(
                SyncErrorCategory::Authentication,
                "credential https://secret@example.test leaked",
                false,
            )),
        );
        let alert = SyncAlert::from_execution(&failed).expect("alert");
        assert_eq!(alert.severity, AlertSeverity::Error);
        assert_eq!(alert.category, Some(SyncErrorCategory::Authentication));
        assert!(!alert.log_line().contains("secret"));
        assert!(!alert.desktop_body().contains("secret"));

        for state in [SyncJobState::Conflicted, SyncJobState::Paused] {
            assert_eq!(
                SyncAlert::from_execution(&execution(state, None))
                    .expect("warning")
                    .severity,
                AlertSeverity::Warning
            );
        }
        assert!(SyncAlert::from_execution(&execution(SyncJobState::Succeeded, None)).is_none());
    }

    #[test]
    fn tracker_deduplicates_state_but_resets_after_recovery() {
        let failed = execution(
            SyncJobState::Failed,
            Some(SyncError::new(SyncErrorCategory::Network, "offline", true)),
        );
        let healthy = execution(SyncJobState::Succeeded, None);
        let mut tracker = SyncAlertTracker::default();
        assert!(tracker.observe(&failed).is_some());
        assert!(tracker.observe(&failed).is_none());
        assert!(tracker.observe(&healthy).is_none());
        assert!(tracker.observe(&failed).is_some());
    }

    #[test]
    fn desktop_plans_pass_untrusted_values_as_arguments() {
        let alert =
            SyncAlert::from_execution(&execution(SyncJobState::Conflicted, None)).expect("alert");
        for platform in [
            DesktopPlatform::Linux,
            DesktopPlatform::MacOs,
            DesktopPlatform::Windows,
        ] {
            let plan = desktop_command(platform, &alert);
            assert!(!plan.program.contains("alpha"));
            assert!(plan.args.iter().any(|argument| argument.contains("alpha")));
        }
    }
}
