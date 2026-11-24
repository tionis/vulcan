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
    /// Why the wiki needs attention when the state alone does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<SyncAlertReason>,
    /// For `branch_paused`: what stopped the branch, which decides the fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_pause: Option<vulcan_sync::GitBranchPause>,
}

/// A secret-free reason code; delivery turns it into guidance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncAlertReason {
    /// The checked-out branch could not take the upstream's commits: it
    /// diverged under fast-forward-only pulls, or a merge or rebase
    /// conflicted and was aborted. The branch was left as it was.
    BranchPaused,
    /// Upstream commits have waited longer than [`BRANCH_DEFERRAL_ALERT`]
    /// because uncommitted edits in the checkout overlap them.
    BranchDeferred,
    /// This device's remote safety head kept receiving history another
    /// checkout wrote: two checkouts or installations share its identity.
    SharedDeviceIdentity,
}

/// How many foreign bridges of a wiki's device backup, within one daemon
/// run, raise [`SyncAlertReason::SharedDeviceIdentity`]; a single one can be
/// an old checkout's last cycle.
pub const FOREIGN_BRIDGE_ALERT: u32 = 3;

/// How long the branch lane may defer to uncommitted edits before the
/// wiki needs someone.
pub const BRANCH_DEFERRAL_ALERT: std::time::Duration = std::time::Duration::from_mins(30);

impl SyncAlert {
    #[must_use]
    pub fn from_execution(execution: &DaemonSyncExecution) -> Option<Self> {
        let lane = execution
            .report
            .as_ref()
            .and_then(|report| report.sync.branch.as_ref())
            .filter(|branch| branch.action == vulcan_app::sync::GitBranchSyncAction::Paused);
        let mut alert = Self::from_job_and_branch(&execution.job.job, lane.is_some())?;
        if alert.reason == Some(SyncAlertReason::BranchPaused) {
            alert.branch_pause = lane.and_then(|lane| lane.pause);
        }
        Some(alert)
    }

    fn from_job_and_branch(job: &vulcan_sync::SyncJob, branch_paused: bool) -> Option<Self> {
        match Self::from_job(job) {
            Some(mut alert) => {
                if branch_paused {
                    alert.reason = Some(SyncAlertReason::BranchPaused);
                }
                Some(alert)
            }
            // With the worktree synchronized, the file lane can succeed
            // while the branch lane pauses; the branch still needs someone.
            None if branch_paused && job.state == SyncJobState::Succeeded => Some(Self {
                version: 1,
                event: "sync_branch_paused".to_string(),
                severity: AlertSeverity::Warning,
                job_id: job.id.clone(),
                wiki_id: job
                    .wiki_id
                    .clone()
                    .unwrap_or_else(|| "<unregistered>".to_string()),
                state: job.state,
                category: None,
                retryable: false,
                reason: Some(SyncAlertReason::BranchPaused),
                branch_pause: None,
            }),
            None => None,
        }
    }

    #[must_use]
    pub fn from_job(job: &vulcan_sync::SyncJob) -> Option<Self> {
        let (event, severity) = match job.state {
            SyncJobState::Failed => ("sync_failed", AlertSeverity::Error),
            SyncJobState::Conflicted => ("sync_conflicted", AlertSeverity::Warning),
            // A pause someone asked for, or a background-policy hold
            // (battery, metered network), needs nobody.
            SyncJobState::Paused
                if job.status.as_ref().is_some_and(|status| {
                    status
                        .detail
                        .as_deref()
                        .is_some_and(|detail| detail.starts_with(crate::sync::HELD_BACK_DETAIL))
                }) =>
            {
                return None;
            }
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
            reason: (job.state == SyncJobState::Paused
                && job.status.as_ref().is_some_and(|status| {
                    status
                        .detail
                        .as_deref()
                        .is_some_and(|detail| detail.starts_with("branch lane paused"))
                }))
            .then_some(SyncAlertReason::BranchPaused),
            branch_pause: None,
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
            "{}:{:?}:{:?}:{}:{:?}",
            self.event, self.state, self.category, self.retryable, self.reason
        )
    }

    pub(crate) fn desktop_title(&self) -> &'static str {
        if self.reason == Some(SyncAlertReason::BranchPaused) {
            return "Vulcan sync: branch needs you";
        }
        if self.reason == Some(SyncAlertReason::BranchDeferred) {
            return "Vulcan sync: uncommitted edits hold the branch back";
        }
        if self.reason == Some(SyncAlertReason::SharedDeviceIdentity) {
            return "Vulcan sync: two checkouts share a device identity";
        }
        match self.state {
            SyncJobState::Failed => "Vulcan sync failed",
            SyncJobState::Conflicted => "Vulcan sync needs resolution",
            SyncJobState::Paused => "Vulcan sync paused",
            _ => "Vulcan sync needs attention",
        }
    }

    pub(crate) fn desktop_body(&self) -> String {
        if self.reason == Some(SyncAlertReason::BranchPaused) {
            use vulcan_sync::GitBranchPause as Pause;
            let (why, fix) = match self.branch_pause {
                Some(Pause::SharedCommits) => (
                    "its local commits are also on another branch or worktree, which a rebase would leave behind",
                    "Merge instead (`git pull --no-rebase && git push`), or rebase that branch too",
                ),
                Some(Pause::MergeCommits) => (
                    "its local commits include merges a rebase would flatten",
                    "Merge (`git pull --no-rebase && git push`)",
                ),
                Some(Pause::InteractiveRebase) => (
                    "pull.rebase=interactive asks for a person",
                    "Run `git pull` in the wiki, finish the rebase, and `git push`",
                ),
                Some(Pause::FastForwardOnly) => (
                    "it diverged and only fast-forwards are allowed",
                    "Run `git pull --rebase && git push` in the wiki",
                ),
                Some(Pause::RebaseConflict | Pause::MergeConflict) => (
                    "combining the local commits with the upstream conflicted, and the attempt was undone",
                    "Run `git pull --rebase` in the wiki, resolve the conflicts, and `git push`",
                ),
                None => (
                    "local commits could not be combined with the upstream automatically",
                    "Run `git pull --rebase && git push` in the wiki (resolving any conflict)",
                ),
            };
            return format!(
                "Wiki `{wiki}`: the branch was left as it was because {why}. {fix}, then `vulcan sync refresh {wiki}`.",
                wiki = self.wiki_id
            );
        }
        if self.reason == Some(SyncAlertReason::SharedDeviceIdentity) {
            return format!(
                "Wiki `{wiki}`: this device's remote backup received history another checkout wrote {FOREIGN_BRIDGE_ALERT} times; both are kept each time. Another checkout of this vault on this device, or another installation with a copy of this device key, syncs under the same ID: remove the extra checkout, or run `vulcan device replace` on the other installation.",
                wiki = self.wiki_id
            );
        }
        if self.reason == Some(SyncAlertReason::BranchDeferred) {
            return format!(
                "Wiki `{wiki}`: upstream commits have waited over {minutes} minutes because uncommitted edits in the checkout overlap them. Commit or discard those edits (`git status` in the wiki); sync then continues by itself.",
                wiki = self.wiki_id,
                minutes = BRANCH_DEFERRAL_ALERT.as_secs() / 60
            );
        }
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
    /// When each wiki's branch lane started deferring, and whether that
    /// streak has been reported.
    deferred_since: BTreeMap<String, (Instant, bool)>,
    /// Foreign bridges of each wiki's device backup in this daemon run.
    foreign_bridges: BTreeMap<String, u32>,
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
        self.observe_at(execution, Instant::now())
    }

    fn observe_at(&mut self, execution: &DaemonSyncExecution, now: Instant) -> Option<SyncAlert> {
        let foreign = execution
            .report
            .as_ref()
            .and_then(|report| report.sync.device_backup.as_ref())
            .is_some_and(|backup| {
                backup.outcome == vulcan_sync::GitDeviceBackupOutcome::Bridged
                    && backup.foreign_history
            });
        let shared = self.foreign_bridge_alert(&execution.job.job, foreign);
        let deferred = self.deferral_alert(execution, now).or(shared);
        let wiki = execution
            .job
            .job
            .wiki_id
            .as_deref()
            .unwrap_or("<unregistered>")
            .to_string();
        let Some(alert) = SyncAlert::from_execution(execution) else {
            self.last_by_wiki.remove(&wiki);
            return deferred;
        };
        let fingerprint = alert.fingerprint();
        if self.last_by_wiki.get(&wiki) == Some(&fingerprint) {
            return None;
        }
        self.last_by_wiki.insert(wiki, fingerprint);
        Some(alert)
    }
}

impl SyncAlertTracker {
    /// Reports once when a wiki's branch lane has deferred to uncommitted
    /// edits for longer than [`BRANCH_DEFERRAL_ALERT`]; any other lane
    /// outcome ends the streak.
    fn deferral_alert(
        &mut self,
        execution: &DaemonSyncExecution,
        now: Instant,
    ) -> Option<SyncAlert> {
        let deferred = execution
            .report
            .as_ref()
            .and_then(|report| report.sync.branch.as_ref())
            .is_some_and(|branch| branch.action == vulcan_app::sync::GitBranchSyncAction::Deferred);
        self.deferral_alert_for(&execution.job.job, deferred, now)
    }

    /// Counts foreign bridges per wiki and reports once when they reach
    /// [`FOREIGN_BRIDGE_ALERT`].
    fn foreign_bridge_alert(
        &mut self,
        job: &vulcan_sync::SyncJob,
        foreign: bool,
    ) -> Option<SyncAlert> {
        if !foreign {
            return None;
        }
        let wiki = job
            .wiki_id
            .clone()
            .unwrap_or_else(|| "<unregistered>".to_string());
        let count = self.foreign_bridges.entry(wiki.clone()).or_default();
        *count += 1;
        (*count == FOREIGN_BRIDGE_ALERT).then(|| SyncAlert {
            version: 1,
            event: "sync_device_identity_shared".to_string(),
            severity: AlertSeverity::Warning,
            job_id: job.id.clone(),
            wiki_id: wiki,
            state: job.state,
            category: None,
            retryable: false,
            reason: Some(SyncAlertReason::SharedDeviceIdentity),
            branch_pause: None,
        })
    }

    fn deferral_alert_for(
        &mut self,
        job: &vulcan_sync::SyncJob,
        deferred: bool,
        now: Instant,
    ) -> Option<SyncAlert> {
        let wiki = job
            .wiki_id
            .clone()
            .unwrap_or_else(|| "<unregistered>".to_string());
        if !deferred {
            self.deferred_since.remove(&wiki);
            return None;
        }
        let (since, reported) = self
            .deferred_since
            .entry(wiki.clone())
            .or_insert((now, false));
        if *reported || now.duration_since(*since) < BRANCH_DEFERRAL_ALERT {
            return None;
        }
        *reported = true;
        Some(SyncAlert {
            version: 1,
            event: "sync_branch_deferred".to_string(),
            severity: AlertSeverity::Warning,
            job_id: job.id.clone(),
            wiki_id: wiki,
            state: job.state,
            category: None,
            retryable: true,
            reason: Some(SyncAlertReason::BranchDeferred),
            branch_pause: None,
        })
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
    fn a_paused_branch_alerts_with_guidance_even_when_the_files_synced() {
        let succeeded = execution(SyncJobState::Succeeded, None).job.job;
        assert!(SyncAlert::from_job_and_branch(&succeeded, false).is_none());
        let alert = SyncAlert::from_job_and_branch(&succeeded, true).expect("branch alert");
        assert_eq!(alert.event, "sync_branch_paused");
        assert_eq!(alert.severity, AlertSeverity::Warning);
        assert_eq!(alert.reason, Some(SyncAlertReason::BranchPaused));
        assert!(alert
            .desktop_body()
            .contains("git pull --rebase && git push"));
        assert!(alert.desktop_body().contains("vulcan sync refresh alpha"));

        // Committed-history-only cycles pause the job; the status says why.
        let mut paused = execution(SyncJobState::Paused, None).job.job;
        paused.status = Some(vulcan_sync::SyncStatus {
            state: vulcan_sync::SyncState::Paused,
            backend: "git".to_string(),
            vault: PathBuf::from("/vault"),
            local_revision: None,
            remote_revision: None,
            accepted_revision: None,
            unresolved_conflicts: 0,
            detail: Some("branch lane paused: branch diverged".to_string()),
        });
        let alert = SyncAlert::from_job(&paused).expect("paused alert");
        assert_eq!(alert.event, "sync_paused");
        assert_eq!(alert.reason, Some(SyncAlertReason::BranchPaused));
        assert_eq!(
            serde_json::to_value(&alert).unwrap()["reason"],
            serde_json::json!("branch_paused")
        );
    }

    #[test]
    fn branch_pause_guidance_follows_what_stopped_the_branch() {
        let succeeded = execution(SyncJobState::Succeeded, None).job.job;
        let mut alert = SyncAlert::from_job_and_branch(&succeeded, true).expect("branch alert");
        alert.branch_pause = Some(vulcan_sync::GitBranchPause::SharedCommits);
        let shared = alert.desktop_body();
        assert!(shared.contains("git pull --no-rebase"), "{shared}");
        assert!(!shared.contains("--rebase &&"), "{shared}");
        alert.branch_pause = Some(vulcan_sync::GitBranchPause::RebaseConflict);
        assert!(alert.desktop_body().contains("resolve the conflicts"));
        assert_eq!(
            serde_json::to_value(&alert).unwrap()["branch_pause"],
            serde_json::json!("rebase_conflict")
        );
    }

    #[test]
    fn repeated_foreign_bridges_alert_once() {
        let job = execution(SyncJobState::Succeeded, None).job.job;
        let mut tracker = SyncAlertTracker::default();
        assert!(tracker.foreign_bridge_alert(&job, false).is_none());
        for _ in 1..FOREIGN_BRIDGE_ALERT {
            assert!(tracker.foreign_bridge_alert(&job, true).is_none());
        }
        let alert = tracker
            .foreign_bridge_alert(&job, true)
            .expect("shared identity alert");
        assert_eq!(alert.reason, Some(SyncAlertReason::SharedDeviceIdentity));
        assert!(alert.desktop_body().contains("vulcan device replace"));
        assert!(tracker.foreign_bridge_alert(&job, true).is_none());
    }

    #[test]
    fn jobs_held_back_by_a_pause_or_the_background_policy_raise_no_alert() {
        for detail in [
            "automatic sync is paused",
            "automatic sync deferred by this device's background policy: metered network",
        ] {
            let mut job = execution(SyncJobState::Paused, None).job.job;
            job.status = Some(vulcan_sync::SyncStatus {
                state: vulcan_sync::SyncState::Paused,
                backend: "git".to_string(),
                vault: PathBuf::from("/vault"),
                local_revision: None,
                remote_revision: None,
                accepted_revision: None,
                unresolved_conflicts: 0,
                detail: Some(detail.to_string()),
            });
            assert!(SyncAlert::from_job(&job).is_none(), "{detail}");
        }
    }

    #[test]
    fn a_branch_deferred_for_half_an_hour_alerts_once_per_streak() {
        let job = execution(SyncJobState::Succeeded, None).job.job;
        let mut tracker = SyncAlertTracker::default();
        let start = Instant::now();
        assert!(tracker.deferral_alert_for(&job, true, start).is_none());
        let late = start + BRANCH_DEFERRAL_ALERT;
        let alert = tracker
            .deferral_alert_for(&job, true, late)
            .expect("deferral alert");
        assert_eq!(alert.event, "sync_branch_deferred");
        assert_eq!(alert.reason, Some(SyncAlertReason::BranchDeferred));
        assert!(alert.desktop_body().contains("uncommitted edits"));
        assert!(tracker
            .deferral_alert_for(&job, true, late + BRANCH_DEFERRAL_ALERT)
            .is_none());
        // Any other outcome ends the streak; a new one waits again.
        assert!(tracker.deferral_alert_for(&job, false, late).is_none());
        assert!(tracker.deferral_alert_for(&job, true, late).is_none());
        assert!(tracker
            .deferral_alert_for(&job, true, late + BRANCH_DEFERRAL_ALERT)
            .is_some());
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
