//! Secret-minimal daemon attention events and local delivery.

use crate::registry::{DaemonNotificationConfig, NetworkNotificationMode};
use crate::sync::DaemonSyncExecution;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use vulcan_sync::{SyncErrorCategory, SyncJob, SyncJobState};

const DESKTOP_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_STREAK_JOB_IDS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncAlert {
    pub version: u32,
    pub event: String,
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
        Self::from_job(&execution.job.job)
    }

    #[must_use]
    pub fn from_job(job: &vulcan_sync::SyncJob) -> Option<Self> {
        let (event, severity) = match job.state {
            SyncJobState::Failed => ("sync_failed", AlertSeverity::Error),
            SyncJobState::Conflicted => ("sync_conflicted", AlertSeverity::Warning),
            SyncJobState::Paused => ("sync_paused", AlertSeverity::Warning),
            _ => return None,
        };
        let error = job.error.as_ref();
        Some(Self {
            version: 1,
            event: event.to_string(),
            severity,
            job_id: job.id.clone(),
            wiki_id: job
                .wiki_id
                .clone()
                .unwrap_or_else(|| "<unregistered>".to_string()),
            state: job.state,
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

    pub(crate) fn desktop_title(&self) -> &'static str {
        match self.state {
            SyncJobState::Failed => "Vulcan sync failed",
            SyncJobState::Conflicted => "Vulcan sync needs resolution",
            SyncJobState::Paused => "Vulcan sync paused",
            _ => "Vulcan sync needs attention",
        }
    }

    pub(crate) fn desktop_body(&self) -> String {
        let reason = self.category.map_or(String::new(), |category| {
            format!(" ({})", format!("{category:?}").to_ascii_lowercase())
        });
        let retry = if self.retryable { " Retryable." } else { "" };
        format!(
            "Wiki `{}` needs attention{}.{retry} Job `{}`. Run `vulcan sync status {}` for details.",
            self.wiki_id, reason, self.job_id, self.wiki_id
        )
    }
}

#[derive(Debug, Default)]
pub struct SyncAlertTracker {
    last_by_wiki: BTreeMap<String, String>,
}

impl SyncAlertTracker {
    #[must_use]
    pub fn from_retained_jobs(jobs: &[crate::supervisor::SupervisedSyncJob]) -> Self {
        let mut tracker = Self::default();
        for retained in jobs {
            tracker.prime(&retained.job);
        }
        tracker
    }

    fn prime(&mut self, job: &vulcan_sync::SyncJob) {
        let wiki = job
            .wiki_id
            .as_deref()
            .unwrap_or("<unregistered>")
            .to_string();
        if let Some(alert) = SyncAlert::from_job(job) {
            self.last_by_wiki.insert(wiki, alert.fingerprint());
        } else {
            self.last_by_wiki.remove(&wiki);
        }
    }

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

#[derive(Debug)]
struct NetworkStreak {
    count: u32,
    job_ids: BTreeSet<String>,
    first_observed: Instant,
    latest: SyncAlert,
    notified: bool,
    notified_job_id: Option<String>,
}

/// Native desktop policy only. Logging and remote sinks retain their own
/// deduplication and are never suppressed by a local preference.
#[derive(Debug)]
pub(crate) struct DesktopNetworkPolicy {
    config: DaemonNotificationConfig,
    streaks: BTreeMap<String, NetworkStreak>,
}

impl DesktopNetworkPolicy {
    pub(crate) fn new(config: DaemonNotificationConfig) -> Self {
        Self {
            config,
            streaks: BTreeMap::new(),
        }
    }

    pub(crate) fn prime(&mut self, jobs: &[crate::supervisor::SupervisedSyncJob]) {
        for retained in jobs {
            if matches!(
                retained.job.state,
                SyncJobState::Queued | SyncJobState::Running
            ) {
                continue;
            }
            let _ = self.observe(&retained.job, None);
        }
    }

    pub(crate) fn observe(
        &mut self,
        job: &SyncJob,
        reported: Option<&SyncAlert>,
    ) -> Option<SyncAlert> {
        self.observe_at(job, reported, Instant::now())
    }

    fn observe_at(
        &mut self,
        job: &SyncJob,
        reported: Option<&SyncAlert>,
        now: Instant,
    ) -> Option<SyncAlert> {
        let wiki = job.wiki_id.as_deref().unwrap_or("<unregistered>");
        if job.state != SyncJobState::Failed
            || job.error.as_ref().map(|error| error.category) != Some(SyncErrorCategory::Network)
        {
            self.streaks.remove(wiki);
            return reported.cloned();
        }
        if self.config.network_mode == NetworkNotificationMode::Immediate {
            return reported.cloned();
        }
        if self.config.network_mode == NetworkNotificationMode::Ignore {
            return None;
        }
        let alert = SyncAlert::from_job(job)?;
        let streak = self
            .streaks
            .entry(wiki.to_string())
            .or_insert_with(|| NetworkStreak {
                count: 0,
                job_ids: BTreeSet::new(),
                first_observed: now,
                latest: alert.clone(),
                notified: false,
                notified_job_id: None,
            });
        if streak.job_ids.insert(alert.job_id.clone()) {
            if streak.job_ids.len() > MAX_STREAK_JOB_IDS {
                streak.job_ids.pop_first();
            }
            streak.count = streak.count.saturating_add(1);
            streak.latest = alert;
        }
        if self.config.network_mode == NetworkNotificationMode::Count
            && !streak.notified
            && streak.count >= self.config.network_failure_count
        {
            streak.notified = true;
            streak.notified_job_id = Some(streak.latest.job_id.clone());
            return Some(streak.latest.clone());
        }
        None
    }

    pub(crate) fn due(&mut self) -> Vec<SyncAlert> {
        self.due_at(Instant::now())
    }

    fn due_at(&mut self, now: Instant) -> Vec<SyncAlert> {
        if self.config.network_mode != NetworkNotificationMode::Duration {
            return Vec::new();
        }
        let deadline = Duration::from_secs(u64::from(self.config.network_failure_minutes) * 60);
        self.streaks
            .values_mut()
            .filter_map(|streak| {
                if streak.notified || now.duration_since(streak.first_observed) < deadline {
                    return None;
                }
                streak.notified = true;
                streak.notified_job_id = Some(streak.latest.job_id.clone());
                Some(streak.latest.clone())
            })
            .collect()
    }

    pub(crate) fn acknowledge_existing(&mut self, alert: &SyncAlert) {
        if let Some(streak) = self.streaks.get_mut(&alert.wiki_id) {
            if streak.job_ids.contains(&alert.job_id) {
                streak.notified = true;
                streak.notified_job_id = Some(alert.job_id.clone());
            }
        }
    }

    pub(crate) fn rearm(&mut self, alert: &SyncAlert) {
        if let Some(streak) = self.streaks.get_mut(&alert.wiki_id) {
            if streak.latest.job_id == alert.job_id {
                streak.notified = false;
                streak.notified_job_id = None;
            }
        }
    }

    pub(crate) fn reconcile_desktop(&self, alert: &SyncAlert) -> bool {
        if alert.category != Some(SyncErrorCategory::Network) {
            return true;
        }
        match self.config.network_mode {
            NetworkNotificationMode::Immediate => true,
            NetworkNotificationMode::Ignore | NetworkNotificationMode::Duration => false,
            NetworkNotificationMode::Count => {
                self.streaks.get(&alert.wiki_id).is_some_and(|streak| {
                    streak.notified_job_id.as_deref() == Some(alert.job_id.as_str())
                })
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Every target constructs only its native variant; tests exercise all command
// renderers on one host so release behavior does not drift unnoticed.
#[allow(dead_code)]
enum DesktopPlatform {
    Android,
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
        DesktopPlatform::Android => DesktopCommand {
            program: "termux-notification",
            args: vec![
                "--title".to_string(),
                title,
                "--content".to_string(),
                body,
                "--group".to_string(),
                "vulcan-sync".to_string(),
                "--priority".to_string(),
                if alert.severity == AlertSeverity::Error {
                    "high"
                } else {
                    "default"
                }
                .to_string(),
            ],
        },
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
    #[cfg(target_os = "android")]
    let platform = DesktopPlatform::Android;
    #[cfg(target_os = "linux")]
    let platform = DesktopPlatform::Linux;
    #[cfg(target_os = "macos")]
    let platform = DesktopPlatform::MacOs;
    #[cfg(target_os = "windows")]
    let platform = DesktopPlatform::Windows;
    #[cfg(not(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    )))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "desktop notifications are unsupported on this platform",
    ));

    #[cfg(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    ))]
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

        let primed = SyncAlertTracker::from_retained_jobs(std::slice::from_ref(&failed.job));
        let mut primed = primed;
        assert!(
            primed.observe(&failed).is_none(),
            "daemon restart must not repeat the retained state"
        );
    }

    fn network_job(id: &str) -> SyncJob {
        let mut job = execution(
            SyncJobState::Failed,
            Some(SyncError::new(SyncErrorCategory::Network, "offline", true)),
        )
        .job
        .job;
        job.id = id.to_string();
        job
    }

    #[test]
    fn native_network_count_notifies_once_after_distinct_failures_and_recovery_resets() {
        let config = DaemonNotificationConfig {
            network_mode: NetworkNotificationMode::Count,
            network_failure_count: 3,
            ..DaemonNotificationConfig::default()
        };
        let mut policy = DesktopNetworkPolicy::new(config);
        let now = Instant::now();
        assert!(policy.observe_at(&network_job("one"), None, now).is_none());
        assert!(policy.observe_at(&network_job("one"), None, now).is_none());
        assert!(policy.observe_at(&network_job("two"), None, now).is_none());
        let third = policy
            .observe_at(&network_job("three"), None, now)
            .expect("threshold");
        assert_eq!(third.job_id, "three");
        assert!(policy.reconcile_desktop(&third));
        assert!(policy.observe_at(&network_job("four"), None, now).is_none());
        let healthy = execution(SyncJobState::Succeeded, None).job.job;
        assert!(policy.observe_at(&healthy, None, now).is_none());
        assert!(policy.observe_at(&network_job("five"), None, now).is_none());
    }

    #[test]
    fn native_network_duration_fires_while_failure_remains_current() {
        let config = DaemonNotificationConfig {
            network_mode: NetworkNotificationMode::Duration,
            network_failure_minutes: 2,
            ..DaemonNotificationConfig::default()
        };
        let mut policy = DesktopNetworkPolicy::new(config);
        let now = Instant::now();
        assert!(policy.observe_at(&network_job("one"), None, now).is_none());
        assert!(policy.due_at(now + Duration::from_secs(119)).is_empty());
        assert_eq!(
            policy.due_at(now + Duration::from_secs(120))[0].job_id,
            "one"
        );
        assert!(policy.due_at(now + Duration::from_secs(121)).is_empty());
    }

    #[test]
    fn pending_recovery_jobs_do_not_erase_retained_network_streaks() {
        let config = DaemonNotificationConfig {
            network_mode: NetworkNotificationMode::Count,
            network_failure_count: 2,
            ..DaemonNotificationConfig::default()
        };
        let failed = SupervisedSyncJob {
            job: network_job("first"),
            triggers: vec![SyncJobTrigger::Poll],
            watch: None,
        };
        let mut queued = failed.clone();
        queued.job.id = "recovery".to_string();
        queued.job.state = SyncJobState::Queued;
        queued.job.error = None;
        let mut policy = DesktopNetworkPolicy::new(config);
        policy.prime(&[failed, queued]);
        assert_eq!(
            policy
                .observe(&network_job("second"), None)
                .expect("threshold")
                .job_id,
            "second"
        );
    }

    #[test]
    fn retained_desktop_delivery_suppresses_restart_replay_for_same_outage() {
        let config = DaemonNotificationConfig {
            network_mode: NetworkNotificationMode::Duration,
            network_failure_minutes: 1,
            ..DaemonNotificationConfig::default()
        };
        let now = Instant::now();
        let mut restarted = DesktopNetworkPolicy::new(config);
        let first = network_job("one");
        let next = network_job("two");
        restarted.observe_at(&first, None, now);
        restarted.observe_at(&next, None, now);
        restarted.acknowledge_existing(&SyncAlert::from_job(&first).expect("delivered alert"));
        assert!(restarted.due_at(now + Duration::from_secs(120)).is_empty());
        assert!(restarted
            .observe_at(&network_job("three"), None, now)
            .is_none());
    }

    #[test]
    fn ignored_network_failures_leave_other_native_alerts_visible() {
        let config = DaemonNotificationConfig {
            network_mode: NetworkNotificationMode::Ignore,
            ..DaemonNotificationConfig::default()
        };
        let mut policy = DesktopNetworkPolicy::new(config);
        let network = network_job("one");
        let reported = SyncAlert::from_job(&network).expect("network alert");
        assert!(policy.observe(&network, Some(&reported)).is_none());
        assert!(!policy.reconcile_desktop(&reported));
        let repository = execution(
            SyncJobState::Failed,
            Some(SyncError::new(
                SyncErrorCategory::Repository,
                "bad ref",
                false,
            )),
        )
        .job
        .job;
        let reported = SyncAlert::from_job(&repository).expect("repository alert");
        assert_eq!(policy.observe(&repository, Some(&reported)), Some(reported));
    }

    #[test]
    fn desktop_plans_pass_untrusted_values_as_arguments() {
        let alert =
            SyncAlert::from_execution(&execution(SyncJobState::Conflicted, None)).expect("alert");
        for platform in [
            DesktopPlatform::Android,
            DesktopPlatform::Linux,
            DesktopPlatform::MacOs,
            DesktopPlatform::Windows,
        ] {
            let plan = desktop_command(platform, &alert);
            assert!(!plan.program.contains("alpha"));
            assert!(plan.args.iter().any(|argument| argument.contains("alpha")));
        }
    }

    #[test]
    fn termux_plan_uses_android_notification_helper() {
        let alert = SyncAlert::from_execution(&execution(
            SyncJobState::Failed,
            Some(SyncError::new(SyncErrorCategory::Network, "offline", true)),
        ))
        .expect("alert");
        let plan = desktop_command(DesktopPlatform::Android, &alert);

        assert_eq!(plan.program, "termux-notification");
        assert_eq!(
            plan.args,
            [
                "--title",
                "Vulcan sync failed",
                "--content",
                "Wiki `alpha` needs attention (network). Retryable. Job `job-1`. Run `vulcan sync status alpha` for details.",
                "--group",
                "vulcan-sync",
                "--priority",
                "high",
            ]
        );
    }
}
