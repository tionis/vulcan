//! Bounded in-process coordination for hosted vault work.
//!
//! These guards reduce avoidable contention inside one daemon. They do not
//! replace the vault and repository filesystem locks used across processes.

use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{
    Mutex, OwnedMutexGuard, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, OwnedSemaphorePermit,
    RwLock, Semaphore,
};
use vulcan_app::execution::{ExecutionContext, ExecutionContextError};

const CANCELLATION_POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutationSchedulerConfig {
    pub max_in_flight: usize,
    pub max_queued: usize,
    pub max_reads_per_vault: usize,
}

impl Default for MutationSchedulerConfig {
    fn default() -> Self {
        Self {
            max_in_flight: 32,
            max_queued: 128,
            max_reads_per_vault: 8,
        }
    }
}

impl MutationSchedulerConfig {
    pub fn validate(self) -> Result<Self, MutationScheduleError> {
        if self.max_in_flight == 0 || self.max_queued == 0 || self.max_reads_per_vault == 0 {
            Err(MutationScheduleError::InvalidConfiguration)
        } else {
            Ok(self)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduledOperation {
    Read,
    Mutation,
}

#[derive(Debug)]
pub struct MutationScheduler {
    config: MutationSchedulerConfig,
    in_flight: Arc<Semaphore>,
    queued: std::sync::atomic::AtomicUsize,
    vaults: StdMutex<HashMap<PathBuf, Weak<VaultLane>>>,
    repositories: StdMutex<HashMap<PathBuf, Weak<Mutex<()>>>>,
}

#[derive(Debug)]
struct VaultLane {
    access: Arc<RwLock<()>>,
    readers: Arc<Semaphore>,
}

#[derive(Debug)]
pub struct ExecutionPermit {
    _in_flight: OwnedSemaphorePermit,
    _lane: Arc<VaultLane>,
    _vault: VaultPermit,
    _repository: Option<OwnedMutexGuard<()>>,
}

#[derive(Debug)]
enum VaultPermit {
    Read {
        _limit: OwnedSemaphorePermit,
        _access: OwnedRwLockReadGuard<()>,
    },
    Mutation {
        _access: OwnedRwLockWriteGuard<()>,
    },
}

impl MutationScheduler {
    pub fn new(config: MutationSchedulerConfig) -> Result<Self, MutationScheduleError> {
        let config = config.validate()?;
        Ok(Self {
            config,
            in_flight: Arc::new(Semaphore::new(config.max_in_flight)),
            queued: std::sync::atomic::AtomicUsize::new(0),
            vaults: StdMutex::new(HashMap::new()),
            repositories: StdMutex::new(HashMap::new()),
        })
    }

    pub async fn acquire<F>(
        &self,
        context: &ExecutionContext,
        operation: ScheduledOperation,
        revalidate: F,
    ) -> Result<ExecutionPermit, MutationScheduleError>
    where
        F: FnOnce(&ExecutionContext) -> Result<(), MutationScheduleError>,
    {
        context.checkpoint().map_err(MutationScheduleError::from)?;
        let _queued = QueueAdmission::new(self)?;
        let in_flight = wait_for(context, Arc::clone(&self.in_flight).acquire_owned())
            .await?
            .map_err(|_| MutationScheduleError::Closed)?;
        let lane = self.vault_lane(&context.vault.canonical_root);
        let vault = match operation {
            ScheduledOperation::Read => {
                let limit = wait_for(context, Arc::clone(&lane.readers).acquire_owned())
                    .await?
                    .map_err(|_| MutationScheduleError::Closed)?;
                let access = wait_for(context, Arc::clone(&lane.access).read_owned()).await?;
                VaultPermit::Read {
                    _limit: limit,
                    _access: access,
                }
            }
            ScheduledOperation::Mutation => {
                let access = wait_for(context, Arc::clone(&lane.access).write_owned()).await?;
                VaultPermit::Mutation { _access: access }
            }
        };
        let repository = if operation == ScheduledOperation::Mutation {
            if let Some(repository) = &context.vault.repository {
                let lane = self.repository_lane(&repository.canonical_git_dir);
                Some(wait_for(context, lane.lock_owned()).await?)
            } else {
                None
            }
        } else {
            None
        };
        context.checkpoint().map_err(MutationScheduleError::from)?;
        revalidate(context)?;
        Ok(ExecutionPermit {
            _in_flight: in_flight,
            _lane: lane,
            _vault: vault,
            _repository: repository,
        })
    }

    fn vault_lane(&self, key: &PathBuf) -> Arc<VaultLane> {
        let mut lanes = self.vaults.lock().expect("vault scheduler lock");
        lanes.retain(|_, lane| lane.strong_count() > 0);
        if let Some(lane) = lanes.get(key).and_then(Weak::upgrade) {
            return lane;
        }
        let lane = Arc::new(VaultLane {
            access: Arc::new(RwLock::new(())),
            readers: Arc::new(Semaphore::new(self.config.max_reads_per_vault)),
        });
        lanes.insert(key.clone(), Arc::downgrade(&lane));
        lane
    }

    fn repository_lane(&self, key: &PathBuf) -> Arc<Mutex<()>> {
        let mut lanes = self.repositories.lock().expect("repository scheduler lock");
        lanes.retain(|_, lane| lane.strong_count() > 0);
        if let Some(lane) = lanes.get(key).and_then(Weak::upgrade) {
            return lane;
        }
        let lane = Arc::new(Mutex::new(()));
        lanes.insert(key.clone(), Arc::downgrade(&lane));
        lane
    }
}

struct QueueAdmission<'a> {
    scheduler: &'a MutationScheduler,
}

impl<'a> QueueAdmission<'a> {
    fn new(scheduler: &'a MutationScheduler) -> Result<Self, MutationScheduleError> {
        scheduler
            .queued
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |queued| (queued < scheduler.config.max_queued).then_some(queued + 1),
            )
            .map_err(|_| MutationScheduleError::QueueFull)?;
        Ok(Self { scheduler })
    }
}

impl Drop for QueueAdmission<'_> {
    fn drop(&mut self) {
        self.scheduler
            .queued
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

async fn wait_for<T>(
    context: &ExecutionContext,
    future: impl Future<Output = T>,
) -> Result<T, MutationScheduleError> {
    tokio::pin!(future);
    let deadline = context.deadline.map(|deadline| {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let remaining_ms = u128::from(deadline.unix_epoch_ms).saturating_sub(now_ms);
        tokio::time::Instant::now()
            + Duration::from_millis(u64::try_from(remaining_ms).unwrap_or(u64::MAX))
    });
    let mut cancellation_poll = tokio::time::interval(CANCELLATION_POLL);
    cancellation_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            result = &mut future => return Ok(result),
            _ = cancellation_poll.tick() => {
                context.checkpoint().map_err(MutationScheduleError::from)?;
            }
            () = async {
                if let Some(deadline) = deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => return Err(MutationScheduleError::DeadlineExceeded),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationScheduleError {
    InvalidConfiguration,
    QueueFull,
    Closed,
    Cancelled,
    DeadlineExceeded,
    Revalidation(String),
}

impl Display for MutationScheduleError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfiguration => {
                formatter.write_str("mutation scheduler limits must all be greater than zero")
            }
            Self::QueueFull => formatter.write_str("hosted execution queue is full"),
            Self::Closed => formatter.write_str("hosted execution scheduler is closed"),
            Self::Cancelled => {
                formatter.write_str("hosted execution was cancelled before dispatch")
            }
            Self::DeadlineExceeded => {
                formatter.write_str("hosted execution deadline expired before dispatch")
            }
            Self::Revalidation(message) => {
                write!(formatter, "execution revalidation failed: {message}")
            }
        }
    }
}

impl std::error::Error for MutationScheduleError {}

impl From<ExecutionContextError> for MutationScheduleError {
    fn from(error: ExecutionContextError) -> Self {
        match error {
            ExecutionContextError::Cancelled => Self::Cancelled,
            ExecutionContextError::DeadlineExceeded => Self::DeadlineExceeded,
            other => Self::Revalidation(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tempfile::TempDir;
    use vulcan_app::execution::{
        ExecutionAuthority, ExecutionCancellationToken, ExecutionIdentity,
        ExecutionRepositoryIdentity, ExecutionRetryClass, ExecutionVaultIdentity,
    };
    use vulcan_core::{PathPermission, PermissionGrant, ResourceLimits, ResourceSpecifier};

    fn grant() -> PermissionGrant {
        PermissionGrant {
            read: PathPermission {
                allow: vec![ResourceSpecifier::All],
                deny: Vec::new(),
            },
            write: PathPermission::default(),
            refactor: PathPermission::default(),
            git: false,
            network: false,
            network_domains: Vec::new(),
            index: false,
            config_read: false,
            config_write: false,
            execute: false,
            shell: false,
            limits: ResourceLimits::default(),
        }
    }

    fn context(root: &TempDir, git_dir: Option<&std::path::Path>) -> ExecutionContext {
        let repository = git_dir.map(|git_dir| {
            ExecutionRepositoryIdentity::resolve("repo", git_dir).expect("repository")
        });
        ExecutionContext::new(
            ExecutionVaultIdentity::resolve(root.path(), None, repository).expect("vault"),
            ExecutionAuthority::Caller {
                principal_id: "test".to_string(),
                credential_id: None,
                permission_ceiling: grant(),
            },
            grant(),
            ExecutionIdentity::new("test"),
            None,
            ExecutionRetryClass::IndeterminateAfterDispatch,
            ExecutionCancellationToken::default(),
            None,
        )
        .expect("context")
    }

    #[tokio::test]
    async fn same_vault_mutations_serialize_and_revalidate_after_waiting() {
        let scheduler = Arc::new(
            MutationScheduler::new(MutationSchedulerConfig::default()).expect("scheduler"),
        );
        let vault = tempfile::tempdir().expect("vault");
        let first_context = context(&vault, None);
        let second_context = context(&vault, None);
        let first = scheduler
            .acquire(&first_context, ScheduledOperation::Mutation, |_| Ok(()))
            .await
            .expect("first permit");
        let allowed = Arc::new(AtomicBool::new(true));
        let task = {
            let scheduler = Arc::clone(&scheduler);
            let allowed = Arc::clone(&allowed);
            tokio::spawn(async move {
                scheduler
                    .acquire(&second_context, ScheduledOperation::Mutation, |_| {
                        if allowed.load(Ordering::Acquire) {
                            Ok(())
                        } else {
                            Err(MutationScheduleError::Revalidation(
                                "permission changed".to_string(),
                            ))
                        }
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        allowed.store(false, Ordering::Release);
        drop(first);
        assert_eq!(
            task.await.expect("task").expect_err("revalidation"),
            MutationScheduleError::Revalidation("permission changed".to_string())
        );
    }

    #[tokio::test]
    async fn different_vaults_progress_while_shared_repository_mutations_serialize() {
        let scheduler = Arc::new(
            MutationScheduler::new(MutationSchedulerConfig::default()).expect("scheduler"),
        );
        let first_vault = tempfile::tempdir().expect("first vault");
        let second_vault = tempfile::tempdir().expect("second vault");
        let git = tempfile::tempdir().expect("git");
        let first_context = context(&first_vault, Some(git.path()));
        let second_context = context(&second_vault, Some(git.path()));
        let first = scheduler
            .acquire(&first_context, ScheduledOperation::Mutation, |_| Ok(()))
            .await
            .expect("first permit");
        let task = {
            let scheduler = Arc::clone(&scheduler);
            tokio::spawn(async move {
                scheduler
                    .acquire(&second_context, ScheduledOperation::Mutation, |_| Ok(()))
                    .await
            })
        };
        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "shared Git directory must serialize");
        drop(first);
        assert!(task.await.expect("task").is_ok());

        let third_vault = tempfile::tempdir().expect("third vault");
        let fourth_vault = tempfile::tempdir().expect("fourth vault");
        let third_context = context(&third_vault, None);
        let fourth_context = context(&fourth_vault, None);
        let third = scheduler
            .acquire(&third_context, ScheduledOperation::Mutation, |_| Ok(()))
            .await
            .expect("third permit");
        let fourth = scheduler
            .acquire(&fourth_context, ScheduledOperation::Mutation, |_| Ok(()))
            .await
            .expect("independent vault permit");
        drop((third, fourth));
    }

    #[tokio::test]
    async fn read_limit_is_bounded_and_mutation_waits_for_readers() {
        let scheduler = Arc::new(
            MutationScheduler::new(MutationSchedulerConfig {
                max_reads_per_vault: 1,
                ..MutationSchedulerConfig::default()
            })
            .expect("scheduler"),
        );
        let vault = tempfile::tempdir().expect("vault");
        let read_context = context(&vault, None);
        let mutation_context = context(&vault, None);
        let read = scheduler
            .acquire(&read_context, ScheduledOperation::Read, |_| Ok(()))
            .await
            .expect("read");
        let task = {
            let scheduler = Arc::clone(&scheduler);
            tokio::spawn(async move {
                scheduler
                    .acquire(&mutation_context, ScheduledOperation::Mutation, |_| Ok(()))
                    .await
            })
        };
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        drop(read);
        assert!(task.await.expect("task").is_ok());
    }

    #[tokio::test]
    async fn queue_and_deadline_fail_before_dispatch() {
        let scheduler = Arc::new(
            MutationScheduler::new(MutationSchedulerConfig {
                max_in_flight: 1,
                max_queued: 1,
                max_reads_per_vault: 1,
            })
            .expect("scheduler"),
        );
        let first_vault = tempfile::tempdir().expect("first vault");
        let second_vault = tempfile::tempdir().expect("second vault");
        let first_context = context(&first_vault, None);
        let second_context = context(&second_vault, None);
        let first = scheduler
            .acquire(&first_context, ScheduledOperation::Mutation, |_| Ok(()))
            .await
            .expect("first");
        let waiting = {
            let scheduler = Arc::clone(&scheduler);
            tokio::spawn(async move {
                scheduler
                    .acquire(&second_context, ScheduledOperation::Mutation, |_| Ok(()))
                    .await
            })
        };
        tokio::task::yield_now().await;
        let third_vault = tempfile::tempdir().expect("third vault");
        let third_context = context(&third_vault, None);
        assert_eq!(
            scheduler
                .acquire(&third_context, ScheduledOperation::Mutation, |_| Ok(()))
                .await
                .expect_err("queue full"),
            MutationScheduleError::QueueFull
        );
        drop(first);
        assert!(waiting.await.expect("waiting task").is_ok());
    }
}
