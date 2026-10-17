//! Schedule execution state and authorization (Roadmap 10.8,
//! `docs/specs/scheduled-execution.md`).
//!
//! The ledger records each occurrence before its workflow starts, so a crash
//! leaves evidence instead of a silent repeat, and keeps a per-job watermark
//! that history pruning never touches. It lives in the device state
//! directory, outside `cache.db` and outside sync. The authorizer is the one
//! synchronous boundary every scheduled or manual run passes; v1 implements
//! repository trust.

use crate::AppError;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::{Path, PathBuf};
use vulcan_core::schedules::ScheduleJob;
use vulcan_core::{resolve_permission_profile, ResolvedPermissionProfile, VaultPaths};

const LEDGER_FILE: &str = "daemon/schedules.sqlite";
const RUN_LOCK_DIR: &str = "daemon/schedule-runs";
const LEDGER_VERSION: i64 = 1;
/// History kept per job: the newest this many occurrences, and none older
/// than the retention window. Watermarks are never pruned.
pub const HISTORY_PER_JOB: i64 = 200;
pub const HISTORY_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;

/// How an occurrence ended, or that it has not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OccurrenceState {
    /// Recorded and handed to its workflow.
    Dispatched,
    Succeeded,
    Failed,
    /// Stopped by cancellation, a runtime limit, or shutdown.
    Cancelled,
    /// Found dispatched after a restart and not retried.
    Interrupted,
}

impl OccurrenceState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Dispatched => "dispatched",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "interrupted" => Self::Interrupted,
            _ => Self::Dispatched,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OccurrenceRecord {
    pub job_id: String,
    pub revision: String,
    /// The scheduled instant; for a manual run, when it was started.
    pub scheduled_ms: i64,
    pub manual: bool,
    pub attempt: i64,
    pub state: OccurrenceState,
    pub started_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// Which occurrence a ledger entry is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccurrenceKey {
    /// The vault: its registration ID, or its canonical path when it is not
    /// registered.
    pub vault: String,
    pub job_id: String,
    pub revision: String,
    pub scheduled_ms: i64,
    pub manual: bool,
}

pub struct ScheduleLedger {
    connection: Connection,
}

impl ScheduleLedger {
    #[must_use]
    pub fn path(state_root: &Path) -> PathBuf {
        state_root.join(LEDGER_FILE)
    }

    pub fn open(state_root: &Path) -> Result<Self, AppError> {
        let path = Self::path(state_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(AppError::operation)?;
        }
        let connection = Connection::open(&path).map_err(AppError::operation)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(10))
            .map_err(AppError::operation)?;
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(AppError::operation)?;
        if version > LEDGER_VERSION {
            return Err(AppError::operation(format!(
                "the schedule ledger is version {version}; this Vulcan reads version {LEDGER_VERSION}"
            )));
        }
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE IF NOT EXISTS occurrences (
                     vault TEXT NOT NULL,
                     job_id TEXT NOT NULL,
                     revision TEXT NOT NULL,
                     scheduled_ms INTEGER NOT NULL,
                     manual INTEGER NOT NULL,
                     attempt INTEGER NOT NULL,
                     state TEXT NOT NULL,
                     started_ms INTEGER NOT NULL,
                     finished_ms INTEGER,
                     outcome TEXT,
                     PRIMARY KEY (vault, job_id, revision, scheduled_ms, manual)
                 );
                 CREATE INDEX IF NOT EXISTS occurrences_by_job
                     ON occurrences (vault, job_id, started_ms);
                 CREATE TABLE IF NOT EXISTS watermarks (
                     vault TEXT NOT NULL,
                     job_id TEXT NOT NULL,
                     revision TEXT NOT NULL,
                     activated_ms INTEGER NOT NULL,
                     last_scheduled_ms INTEGER,
                     PRIMARY KEY (vault, job_id, revision)
                 );
                 CREATE TABLE IF NOT EXISTS trust_checkpoints (
                     vault TEXT PRIMARY KEY,
                     commit_oid TEXT NOT NULL,
                     signer TEXT NOT NULL,
                     policy_generation TEXT NOT NULL,
                     accepted_ms INTEGER NOT NULL,
                     how TEXT NOT NULL
                 );
                 PRAGMA user_version = 1;",
            )
            .map_err(AppError::operation)?;
        Ok(Self { connection })
    }

    /// The vault's accepted trusted-commit checkpoint, if any.
    pub fn trust_checkpoint(
        &self,
        vault: &str,
    ) -> Result<Option<crate::schedule_trust::TrustCheckpoint>, AppError> {
        self.connection
            .query_row(
                "SELECT commit_oid, signer, policy_generation, accepted_ms, how
                 FROM trust_checkpoints WHERE vault = ?1",
                params![vault],
                |row| {
                    Ok(crate::schedule_trust::TrustCheckpoint {
                        commit: row.get(0)?,
                        signer: row.get(1)?,
                        policy_generation: row.get(2)?,
                        accepted_ms: row.get(3)?,
                        how: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(AppError::operation)
    }

    pub fn set_trust_checkpoint(
        &self,
        vault: &str,
        checkpoint: &crate::schedule_trust::TrustCheckpoint,
    ) -> Result<(), AppError> {
        self.connection
            .execute(
                "INSERT INTO trust_checkpoints
                     (vault, commit_oid, signer, policy_generation, accepted_ms, how)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (vault) DO UPDATE SET commit_oid = ?2, signer = ?3,
                     policy_generation = ?4, accepted_ms = ?5, how = ?6",
                params![
                    vault,
                    checkpoint.commit,
                    checkpoint.signer,
                    checkpoint.policy_generation,
                    checkpoint.accepted_ms,
                    checkpoint.how
                ],
            )
            .map_err(AppError::operation)?;
        Ok(())
    }

    /// Forgets the vault's checkpoint, as `trust disable` does.
    pub fn clear_trust_checkpoint(&self, vault: &str) -> Result<(), AppError> {
        self.connection
            .execute(
                "DELETE FROM trust_checkpoints WHERE vault = ?1",
                params![vault],
            )
            .map_err(AppError::operation)?;
        Ok(())
    }

    /// The instant after which this revision's next occurrence is due: its
    /// latest dispatched occurrence, or when it was first seen. A revision
    /// seen for the first time is activated at `now`, so only occurrences
    /// after it run.
    pub fn watermark(&self, vault: &str, job: &ScheduleJob, now_ms: i64) -> Result<i64, AppError> {
        self.connection
            .execute(
                "INSERT OR IGNORE INTO watermarks (vault, job_id, revision, activated_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![vault, job.id, job.revision, now_ms],
            )
            .map_err(AppError::operation)?;
        let (activated, last): (i64, Option<i64>) = self
            .connection
            .query_row(
                "SELECT activated_ms, last_scheduled_ms FROM watermarks
                 WHERE vault = ?1 AND job_id = ?2 AND revision = ?3",
                params![vault, job.id, job.revision],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(AppError::operation)?;
        Ok(last.unwrap_or(activated))
    }

    /// Records `key` as dispatched and, for a scheduled occurrence, advances
    /// the watermark, in one transaction. Returns `false` when the
    /// occurrence was already recorded, so it is never dispatched twice.
    pub fn claim(&mut self, key: &OccurrenceKey, now_ms: i64) -> Result<bool, AppError> {
        let transaction = self.connection.transaction().map_err(AppError::operation)?;
        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO occurrences
                     (vault, job_id, revision, scheduled_ms, manual, attempt, state, started_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, 'dispatched', ?6)",
                params![
                    key.vault,
                    key.job_id,
                    key.revision,
                    key.scheduled_ms,
                    key.manual,
                    now_ms
                ],
            )
            .map_err(AppError::operation)?
            == 1;
        if inserted && !key.manual {
            transaction
                .execute(
                    "UPDATE watermarks SET last_scheduled_ms = ?4
                     WHERE vault = ?1 AND job_id = ?2 AND revision = ?3
                       AND (last_scheduled_ms IS NULL OR last_scheduled_ms < ?4)",
                    params![key.vault, key.job_id, key.revision, key.scheduled_ms],
                )
                .map_err(AppError::operation)?;
        }
        transaction.commit().map_err(AppError::operation)?;
        Ok(inserted)
    }

    /// Records a retry of a dispatched occurrence.
    pub fn retry(&self, key: &OccurrenceKey, now_ms: i64) -> Result<(), AppError> {
        self.connection
            .execute(
                "UPDATE occurrences SET attempt = attempt + 1, state = 'dispatched',
                     started_ms = ?6, finished_ms = NULL, outcome = NULL
                 WHERE vault = ?1 AND job_id = ?2 AND revision = ?3
                   AND scheduled_ms = ?4 AND manual = ?5",
                params![
                    key.vault,
                    key.job_id,
                    key.revision,
                    key.scheduled_ms,
                    key.manual,
                    now_ms
                ],
            )
            .map_err(AppError::operation)?;
        Ok(())
    }

    pub fn finish(
        &self,
        key: &OccurrenceKey,
        state: OccurrenceState,
        outcome: &str,
        now_ms: i64,
    ) -> Result<(), AppError> {
        let outcome = outcome.chars().take(4_000).collect::<String>();
        self.connection
            .execute(
                "UPDATE occurrences SET state = ?6, finished_ms = ?7, outcome = ?8
                 WHERE vault = ?1 AND job_id = ?2 AND revision = ?3
                   AND scheduled_ms = ?4 AND manual = ?5",
                params![
                    key.vault,
                    key.job_id,
                    key.revision,
                    key.scheduled_ms,
                    key.manual,
                    state.as_str(),
                    now_ms,
                    outcome
                ],
            )
            .map_err(AppError::operation)?;
        Ok(())
    }

    /// Occurrences of `vault` still marked dispatched, as a restart finds
    /// them: the previous run stopped before recording an outcome.
    pub fn unfinished(&self, vault: &str) -> Result<Vec<(OccurrenceKey, i64)>, AppError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT job_id, revision, scheduled_ms, manual, attempt FROM occurrences
                 WHERE vault = ?1 AND state = 'dispatched' ORDER BY started_ms",
            )
            .map_err(AppError::operation)?;
        let rows = statement
            .query_map(params![vault], |row| {
                Ok((
                    OccurrenceKey {
                        vault: vault.to_string(),
                        job_id: row.get(0)?,
                        revision: row.get(1)?,
                        scheduled_ms: row.get(2)?,
                        manual: row.get(3)?,
                    },
                    row.get(4)?,
                ))
            })
            .map_err(AppError::operation)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AppError::operation)
    }

    /// A job's newest occurrences first.
    pub fn history(
        &self,
        vault: &str,
        job_id: &str,
        limit: usize,
    ) -> Result<Vec<OccurrenceRecord>, AppError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT job_id, revision, scheduled_ms, manual, attempt, state, started_ms,
                        finished_ms, outcome
                 FROM occurrences WHERE vault = ?1 AND job_id = ?2
                 ORDER BY started_ms DESC, scheduled_ms DESC LIMIT ?3",
            )
            .map_err(AppError::operation)?;
        let rows = statement
            .query_map(
                params![vault, job_id, i64::try_from(limit).unwrap_or(i64::MAX)],
                |row| {
                    Ok(OccurrenceRecord {
                        job_id: row.get(0)?,
                        revision: row.get(1)?,
                        scheduled_ms: row.get(2)?,
                        manual: row.get(3)?,
                        attempt: row.get(4)?,
                        state: OccurrenceState::parse(&row.get::<_, String>(5)?),
                        started_ms: row.get(6)?,
                        finished_ms: row.get(7)?,
                        outcome: row.get(8)?,
                    })
                },
            )
            .map_err(AppError::operation)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AppError::operation)
    }

    /// The latest finished occurrence of each state for a job, for status.
    pub fn last_finished(
        &self,
        vault: &str,
        job_id: &str,
        succeeded: bool,
    ) -> Result<Option<OccurrenceRecord>, AppError> {
        let condition = if succeeded {
            "state = 'succeeded'"
        } else {
            "state IN ('failed', 'cancelled', 'interrupted')"
        };
        Ok(self
            .history_where(vault, job_id, condition)?
            .into_iter()
            .next())
    }

    fn history_where(
        &self,
        vault: &str,
        job_id: &str,
        condition: &str,
    ) -> Result<Vec<OccurrenceRecord>, AppError> {
        let sql = format!(
            "SELECT job_id, revision, scheduled_ms, manual, attempt, state, started_ms,
                    finished_ms, outcome
             FROM occurrences WHERE vault = ?1 AND job_id = ?2 AND {condition}
             ORDER BY started_ms DESC LIMIT 1"
        );
        let mut statement = self.connection.prepare(&sql).map_err(AppError::operation)?;
        let row = statement
            .query_row(params![vault, job_id], |row| {
                Ok(OccurrenceRecord {
                    job_id: row.get(0)?,
                    revision: row.get(1)?,
                    scheduled_ms: row.get(2)?,
                    manual: row.get(3)?,
                    attempt: row.get(4)?,
                    state: OccurrenceState::parse(&row.get::<_, String>(5)?),
                    started_ms: row.get(6)?,
                    finished_ms: row.get(7)?,
                    outcome: row.get(8)?,
                })
            })
            .optional()
            .map_err(AppError::operation)?;
        Ok(row.into_iter().collect())
    }

    /// Drops finished history beyond the newest [`HISTORY_PER_JOB`] per job
    /// and older than [`HISTORY_RETENTION_MS`]. Watermarks are untouched, so
    /// pruning never makes an old occurrence due again.
    pub fn prune(&self, now_ms: i64) -> Result<usize, AppError> {
        let removed = self
            .connection
            .execute(
                "DELETE FROM occurrences WHERE state != 'dispatched' AND (
                     started_ms < ?1 OR rowid IN (
                         SELECT rowid FROM (
                             SELECT rowid, ROW_NUMBER() OVER (
                                 PARTITION BY vault, job_id ORDER BY started_ms DESC
                             ) AS position FROM occurrences
                         ) WHERE position > ?2))",
                params![now_ms - HISTORY_RETENTION_MS, HISTORY_PER_JOB],
            )
            .map_err(AppError::operation)?;
        Ok(removed)
    }
}

/// What the authorizer allows one run to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutionAuthorization {
    /// The trust policy that decided: `repository-trust` or
    /// `trusted-commit`.
    pub policy: &'static str,
    /// Under trusted-commit, the commit whose definition was authorized.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The exact definition that was authorized.
    pub job_id: String,
    pub revision: String,
    /// The permission profile the run uses.
    pub profile: String,
}

/// Authorizes `job` to run in `paths` under repository trust: the vault's
/// definition is accepted as written, but its `authority` profile must exist
/// and stay within `ceiling`, the device's limit for this vault (the
/// registration's profile, or the caller's for a direct run). A synced change
/// can narrow what a job may do, never widen it: the ceiling is resolved by
/// [`resolve_device_ceiling`], never from the shared configuration.
pub fn authorize_execution(
    paths: &VaultPaths,
    job: &ScheduleJob,
    ceiling: Option<&str>,
) -> Result<(ExecutionAuthorization, ResolvedPermissionProfile), AppError> {
    let ceiling_name = ceiling.unwrap_or("unrestricted");
    let ceiling = resolve_device_ceiling(paths, ceiling_name)?;
    let authority =
        resolve_permission_profile(paths, Some(&job.authority)).map_err(AppError::operation)?;
    if !authority.grant.is_subset_of(&ceiling.grant) {
        return Err(AppError::operation(format!(
            "job `{}` asks for profile `{}`, which exceeds this device's ceiling `{ceiling_name}` for the vault",
            job.id, job.authority
        )));
    }
    Ok((
        ExecutionAuthorization {
            policy: "repository-trust",
            commit: None,
            job_id: job.id.clone(),
            revision: job.revision.clone(),
            profile: job.authority.clone(),
        },
        authority,
    ))
}

/// Resolves the device's permission ceiling `name` from the built-in profiles
/// and this device's local configuration only. The vault's shared
/// configuration is synced, so a profile defined (or redefined) there would
/// let a synced change widen the limit meant to cap it.
pub fn resolve_device_ceiling(
    paths: &VaultPaths,
    name: &str,
) -> Result<ResolvedPermissionProfile, AppError> {
    vulcan_core::resolve_device_permission_profile(paths, Some(name)).map_err(AppError::operation)
}

/// This device's local configuration, if it exists. An unreadable or invalid
/// file is an error rather than ignored, since it may narrow the ceiling.
fn local_config(paths: &VaultPaths) -> Result<Option<toml::Value>, AppError> {
    let path = paths.local_config_file();
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AppError::operation(format!(
                "failed to read {}: {error}",
                path.display()
            )))
        }
    };
    text.parse::<toml::Table>()
        .map(|table| Some(toml::Value::Table(table)))
        .map_err(|error| {
            AppError::operation(format!("failed to parse {}: {error}", path.display()))
        })
}

/// Held for the whole of one run of a job, scheduled or manual, by whichever
/// process runs it, so a job never overlaps itself on this device even when a
/// direct `vulcan schedule run` meets the daemon's run. The operating system
/// releases it if the process dies, so a crash never leaves it stuck.
pub struct JobRunLock {
    file: std::fs::File,
}

impl Drop for JobRunLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

/// Takes the run lock of `job_id` in `vault`, or returns `None` while
/// another run of it holds the lock.
pub fn try_lock_job_run(
    state_root: &Path,
    vault: &str,
    job_id: &str,
) -> Result<Option<JobRunLock>, AppError> {
    let directory = state_root.join(RUN_LOCK_DIR);
    std::fs::create_dir_all(&directory).map_err(AppError::operation)?;
    let name = blake3::hash(format!("{vault}\0{job_id}").as_bytes()).to_hex();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join(format!("{}.lock", &name[..32])))
        .map_err(AppError::operation)?;
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(JobRunLock { file })),
        Err(error)
            if error.kind() == std::io::ErrorKind::WouldBlock
                || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
        {
            Ok(None)
        }
        Err(error) => Err(AppError::operation(error)),
    }
}

/// What a trusted-commit run uses instead of the working tree.
#[derive(Debug, Clone)]
pub struct TrustedSnapshot {
    /// The signed `[git]` table, with this device's local overrides.
    pub git: vulcan_core::GitConfig,
}

/// Authorizes `job` under trusted-commit: `resolution` must authorize it, the
/// job must be exactly the definition at the resolved commit, and its
/// `authority` profile is resolved from the signed shared configuration (with
/// this device's local overrides), so an unsigned working-tree edit cannot
/// widen it, and the device `ceiling` by [`resolve_device_ceiling`], so not even
/// a signed one can.
pub fn authorize_trusted_execution(
    paths: &VaultPaths,
    job: &ScheduleJob,
    ceiling: Option<&str>,
    resolution: &crate::schedule_trust::TrustResolution,
) -> Result<
    (
        ExecutionAuthorization,
        ResolvedPermissionProfile,
        TrustedSnapshot,
    ),
    AppError,
> {
    resolution.authorize(&job.id).map_err(AppError::operation)?;
    let signed = resolution.schedules.job(&job.id).ok_or_else(|| {
        AppError::operation(format!(
            "trusted-commit: job `{}` does not parse at the resolved commit",
            job.id
        ))
    })?;
    if signed.revision != job.revision {
        return Err(AppError::operation(format!(
            "trusted-commit: job `{}` differs from its authorized definition",
            job.id
        )));
    }
    let shared = resolution.signed_config.as_ref();
    let local = local_config(paths)?;
    let ceiling_name = ceiling.unwrap_or("unrestricted");
    let ceiling = resolve_device_ceiling(paths, ceiling_name)?;
    let authority = vulcan_core::resolve_permission_profile_with_overrides(
        paths,
        shared,
        local.as_ref(),
        Some(&job.authority),
    )
    .map_err(AppError::operation)?;
    if !authority.grant.is_subset_of(&ceiling.grant) {
        return Err(AppError::operation(format!(
            "job `{}` asks for profile `{}`, which exceeds this device's ceiling `{ceiling_name}` for the vault",
            job.id, job.authority
        )));
    }
    let git = vulcan_core::load_vault_config_with_overrides(paths, shared, local.as_ref())
        .config
        .git;
    Ok((
        ExecutionAuthorization {
            policy: crate::schedule_trust::TRUSTED_COMMIT_POLICY,
            commit: resolution.tip.clone(),
            job_id: job.id.clone(),
            revision: job.revision.clone(),
            profile: job.authority.clone(),
        },
        authority,
        TrustedSnapshot { git },
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetJobEnabledReport {
    pub job_id: String,
    pub enabled: bool,
    /// Whether the file already said so.
    pub unchanged: bool,
    pub dry_run: bool,
    pub path: String,
}

/// Sets `enabled` on one `[[schedules.jobs]]` entry of the shared
/// `.vulcan/config.toml`, keeping the rest of the file as written. Enabling
/// is the vault's decision, so it is never written to `config.local.toml`.
pub fn set_job_enabled(
    paths: &VaultPaths,
    job_id: &str,
    enabled: bool,
    dry_run: bool,
) -> Result<SetJobEnabledReport, AppError> {
    let source = std::fs::read_to_string(paths.config_file()).map_err(|error| {
        AppError::operation(format!(
            "cannot read {}: {error}",
            paths.config_file().display()
        ))
    })?;
    let mut document = source
        .parse::<toml_edit::DocumentMut>()
        .map_err(AppError::operation)?;
    let jobs = document
        .get_mut("schedules")
        .and_then(|schedules| schedules.get_mut("jobs"))
        .and_then(toml_edit::Item::as_array_of_tables_mut)
        .ok_or_else(|| AppError::operation("config.toml has no `[[schedules.jobs]]`"))?;
    let mut matches = jobs
        .iter_mut()
        .filter(|job| job.get("id").and_then(toml_edit::Item::as_str) == Some(job_id))
        .collect::<Vec<_>>();
    let job = match matches.len() {
        0 => {
            return Err(AppError::operation(format!(
                "no scheduled job `{job_id}` in config.toml"
            )))
        }
        1 => matches.remove(0),
        _ => {
            return Err(AppError::operation(format!(
                "job ID `{job_id}` is defined more than once; fix config.toml first"
            )))
        }
    };
    let current = job
        .get("enabled")
        .and_then(toml_edit::Item::as_bool)
        .unwrap_or(true);
    let unchanged = current == enabled;
    if !unchanged {
        job["enabled"] = toml_edit::value(enabled);
        if !dry_run {
            vulcan_core::durable::replace(
                paths.config_file(),
                document.to_string().as_bytes(),
                vulcan_core::durable::Durability::Full,
            )
            .map_err(AppError::operation)?;
        }
    }
    Ok(SetJobEnabledReport {
        job_id: job_id.to_string(),
        enabled,
        unchanged,
        dry_run,
        path: ".vulcan/config.toml".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vulcan_core::schedules::load_schedules;

    fn job(paths: &VaultPaths, authority: &str) -> ScheduleJob {
        std::fs::create_dir_all(paths.vulcan_dir()).unwrap();
        std::fs::write(
            paths.config_file(),
            format!(
                "[permissions.profiles.narrow]\nread = \"all\"\nwrite = \"none\"\nindex = \"allow\"\n\n[schedules]\nversion = 1\n[[schedules.jobs]]\nid = \"rescan\"\ntarget = {{ builtin = \"scan\" }}\nnodes = [\"device-a\"]\ntrigger = {{ every = \"15m\" }}\nauthority = \"{authority}\"\n"
            ),
        )
        .unwrap();
        load_schedules(paths).job("rescan").unwrap().clone()
    }

    fn key(job: &ScheduleJob, scheduled_ms: i64, manual: bool) -> OccurrenceKey {
        OccurrenceKey {
            vault: "vault".to_string(),
            job_id: job.id.clone(),
            revision: job.revision.clone(),
            scheduled_ms,
            manual,
        }
    }

    #[test]
    fn an_occurrence_is_claimed_once_and_advances_the_watermark() {
        let state = tempfile::tempdir().unwrap();
        let vault = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(vault.path());
        let job = job(&paths, "unrestricted");
        let mut ledger = ScheduleLedger::open(state.path()).unwrap();

        assert_eq!(ledger.watermark("vault", &job, 1_000).unwrap(), 1_000);
        assert_eq!(
            ledger.watermark("vault", &job, 9_000).unwrap(),
            1_000,
            "activation is recorded once"
        );
        let occurrence = key(&job, 5_000, false);
        assert!(ledger.claim(&occurrence, 5_001).unwrap());
        assert!(!ledger.claim(&occurrence, 5_002).unwrap(), "never twice");
        assert_eq!(ledger.watermark("vault", &job, 9_000).unwrap(), 5_000);
        // An older occurrence cannot move the watermark back.
        assert!(ledger.claim(&key(&job, 4_000, false), 5_003).unwrap());
        assert_eq!(ledger.watermark("vault", &job, 9_000).unwrap(), 5_000);
        // A manual run does not move it at all.
        assert!(ledger.claim(&key(&job, 7_000, true), 7_000).unwrap());
        assert_eq!(ledger.watermark("vault", &job, 9_000).unwrap(), 5_000);

        ledger
            .finish(&occurrence, OccurrenceState::Succeeded, "ok", 6_000)
            .unwrap();
        let unfinished = ledger.unfinished("vault").unwrap();
        assert_eq!(unfinished.len(), 2, "the older and the manual run");
        let history = ledger.history("vault", &job.id, 10).unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(
            ledger
                .last_finished("vault", &job.id, true)
                .unwrap()
                .unwrap()
                .scheduled_ms,
            5_000
        );
        // Reopening keeps everything.
        drop(ledger);
        let ledger = ScheduleLedger::open(state.path()).unwrap();
        assert_eq!(ledger.watermark("vault", &job, 9_000).unwrap(), 5_000);
    }

    #[test]
    fn pruning_keeps_watermarks_and_unfinished_runs() {
        let state = tempfile::tempdir().unwrap();
        let vault = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(vault.path());
        let job = job(&paths, "unrestricted");
        let mut ledger = ScheduleLedger::open(state.path()).unwrap();
        let now = HISTORY_RETENTION_MS * 2;
        ledger.watermark("vault", &job, 0).unwrap();
        let old = key(&job, 10, false);
        ledger.claim(&old, 10).unwrap();
        ledger
            .finish(&old, OccurrenceState::Succeeded, "", 20)
            .unwrap();
        let stuck = key(&job, 30, false);
        ledger.claim(&stuck, 30).unwrap();
        for index in 0..(HISTORY_PER_JOB + 5) {
            let recent = key(&job, now - 1_000 + index, false);
            ledger.claim(&recent, now - 1_000 + index).unwrap();
            ledger
                .finish(&recent, OccurrenceState::Succeeded, "", now)
                .unwrap();
        }
        let removed = ledger.prune(now).unwrap();
        assert_eq!(removed, 6, "the expired one and the five beyond the cap");
        assert_eq!(ledger.unfinished("vault").unwrap().len(), 1);
        assert_eq!(
            ledger.watermark("vault", &job, now).unwrap(),
            now - 1_000 + HISTORY_PER_JOB + 4
        );
    }

    #[test]
    fn enabling_and_disabling_edit_only_the_job_entry() {
        let vault = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(vault.path());
        let enabled = job(&paths, "unrestricted");
        assert!(enabled.enabled);
        let before = std::fs::read_to_string(paths.config_file()).unwrap();
        let preview = set_job_enabled(&paths, "rescan", false, true).unwrap();
        assert!(!preview.unchanged && preview.dry_run);
        assert_eq!(
            std::fs::read_to_string(paths.config_file()).unwrap(),
            before
        );

        set_job_enabled(&paths, "rescan", false, false).unwrap();
        let after = std::fs::read_to_string(paths.config_file()).unwrap();
        assert!(after.contains("enabled = false"), "{after}");
        assert!(
            after.contains("[permissions.profiles.narrow]"),
            "the rest is kept"
        );
        let disabled = load_schedules(&paths).job("rescan").unwrap().clone();
        assert!(!disabled.enabled);
        assert_ne!(disabled.revision, enabled.revision);
        assert!(
            set_job_enabled(&paths, "rescan", false, false)
                .unwrap()
                .unchanged
        );
        assert!(set_job_enabled(&paths, "missing", true, false).is_err());
    }

    #[test]
    fn a_job_may_narrow_but_not_widen_the_device_ceiling() {
        let vault = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(vault.path());
        let narrow = job(&paths, "narrow");
        let (authorization, profile) = authorize_execution(&paths, &narrow, None).unwrap();
        assert_eq!(authorization.policy, "repository-trust");
        assert_eq!(profile.name, "narrow");
        let wide = job(&paths, "unrestricted");
        let error = authorize_execution(&paths, &wide, Some("readonly")).unwrap_err();
        assert!(error.to_string().contains("exceeds this device's ceiling"));
        let unknown = job(&paths, "missing-profile");
        assert!(authorize_execution(&paths, &unknown, None).is_err());
    }

    #[test]
    fn a_job_run_lock_excludes_only_other_runs_of_the_same_job() {
        let state = tempfile::tempdir().unwrap();
        let held = try_lock_job_run(state.path(), "vault", "rescan")
            .unwrap()
            .expect("free at first");
        assert!(try_lock_job_run(state.path(), "vault", "rescan")
            .unwrap()
            .is_none());
        let other = try_lock_job_run(state.path(), "vault", "nightly").unwrap();
        assert!(other.is_some(), "another job runs alongside");
        let elsewhere = try_lock_job_run(state.path(), "other-vault", "rescan").unwrap();
        assert!(elsewhere.is_some(), "the same job in another vault too");
        drop(held);
        assert!(try_lock_job_run(state.path(), "vault", "rescan")
            .unwrap()
            .is_some());
    }

    #[test]
    fn the_device_ceiling_ignores_the_shared_configuration() {
        let vault = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(vault.path());
        let wide = job(&paths, "unrestricted");
        // A synced edit can neither supply a ceiling nor redefine a built-in one.
        let error = authorize_execution(&paths, &wide, Some("narrow")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("defined only in the vault's synced configuration"),
            "{error}"
        );
        // `narrow` indexes, which the built-in `readonly` does not allow.
        let narrow = job(&paths, "narrow");
        assert!(authorize_execution(&paths, &narrow, Some("readonly")).is_err());
        let mut config = std::fs::read_to_string(paths.config_file()).unwrap();
        config.push_str("\n[permissions.profiles.readonly]\nindex = \"allow\"\n");
        std::fs::write(paths.config_file(), config).unwrap();
        // As the shared configuration now reads, `readonly` would admit it.
        let redefined = resolve_permission_profile(&paths, Some("readonly")).unwrap();
        let asked = resolve_permission_profile(&paths, Some("narrow")).unwrap();
        assert!(asked.grant.is_subset_of(&redefined.grant));
        let error = authorize_execution(&paths, &narrow, Some("readonly")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("beyond this device's definition"),
            "{error}"
        );

        // This device's local configuration may define one.
        std::fs::write(
            paths.local_config_file(),
            "[permissions.profiles.narrow]\nread = \"all\"\nwrite = \"none\"\nindex = \"allow\"\n",
        )
        .unwrap();
        authorize_execution(&paths, &narrow, Some("narrow")).unwrap();
        assert!(authorize_execution(&paths, &wide, Some("narrow"))
            .unwrap_err()
            .to_string()
            .contains("exceeds this device's ceiling"));

        // An unreadable local file fails closed instead of being skipped.
        std::fs::write(paths.local_config_file(), "not = [toml").unwrap();
        assert!(authorize_execution(&paths, &narrow, Some("narrow"))
            .unwrap_err()
            .to_string()
            .contains("failed to parse"));
    }
}
