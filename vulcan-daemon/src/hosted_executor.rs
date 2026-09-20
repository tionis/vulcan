//! Bounded adapter for synchronous hosted operations.
//!
//! A response timeout only ends the caller's wait. Dispatched blocking work
//! remains supervised and updates the durable job record when it actually
//! finishes, so adapters never imply that an uncertain write was rolled back.

use crate::hosted_jobs::{HostedJobError, HostedJobLedger, HostedJobRecord};
use crate::mutation_scheduler::{MutationScheduleError, MutationScheduler, ScheduledOperation};
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;
use vulcan_app::execution::ExecutionContext;

const CANCELLATION_POLL: Duration = Duration::from_millis(20);

#[derive(Debug)]
pub struct HostedOperationCompletion<T> {
    pub value: T,
    pub committed: bool,
}

impl<T> HostedOperationCompletion<T> {
    #[must_use]
    pub fn read(value: T) -> Self {
        Self {
            value,
            committed: false,
        }
    }

    #[must_use]
    pub fn mutation(value: T) -> Self {
        Self {
            value,
            committed: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedOperationFailure {
    pub detail: String,
    /// `None` means the operation cannot prove whether its mutation committed.
    pub committed: Option<bool>,
}

impl HostedOperationFailure {
    #[must_use]
    pub fn before_commit(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            committed: Some(false),
        }
    }

    #[must_use]
    pub fn indeterminate(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            committed: None,
        }
    }
}

#[derive(Debug)]
pub struct HostedExecutor {
    scheduler: Arc<MutationScheduler>,
    ledger: Arc<HostedJobLedger>,
}

impl HostedExecutor {
    #[must_use]
    pub fn new(scheduler: Arc<MutationScheduler>, ledger: Arc<HostedJobLedger>) -> Self {
        Self { scheduler, ledger }
    }

    #[must_use]
    pub fn ledger(&self) -> Arc<HostedJobLedger> {
        Arc::clone(&self.ledger)
    }

    pub async fn status(
        &self,
        operation_id: String,
    ) -> Result<HostedJobRecord, HostedExecutionError> {
        let ledger = Arc::clone(&self.ledger);
        run_ledger(move || ledger.load(&operation_id)).await
    }

    pub async fn cancel(
        &self,
        operation_id: String,
    ) -> Result<HostedJobRecord, HostedExecutionError> {
        let ledger = Arc::clone(&self.ledger);
        run_ledger(move || ledger.request_cancel(&operation_id, now_unix_ms())).await
    }

    pub async fn execute<T, R, F>(
        &self,
        context: ExecutionContext,
        kind: ScheduledOperation,
        revalidate: R,
        operation: F,
    ) -> Result<T, HostedExecutionError>
    where
        T: Send + 'static,
        R: FnOnce(&ExecutionContext) -> Result<(), MutationScheduleError>,
        F: FnOnce(ExecutionContext) -> Result<HostedOperationCompletion<T>, HostedOperationFailure>
            + Send
            + 'static,
    {
        let operation_id = context.identity.operation_id.clone();
        let ledger = Arc::clone(&self.ledger);
        let register_context = context.clone();
        run_ledger(move || ledger.register(&register_context, now_unix_ms())).await?;

        let permit = match self.scheduler.acquire(&context, kind, revalidate).await {
            Ok(permit) => permit,
            Err(error) => {
                let detail = error.to_string();
                let ledger = Arc::clone(&self.ledger);
                let failed_id = operation_id.clone();
                let failed_detail = detail.clone();
                run_ledger(move || {
                    ledger.mark_failed(&failed_id, Some(false), failed_detail, now_unix_ms())
                })
                .await?;
                return Err(HostedExecutionError::BeforeDispatch {
                    operation_id,
                    detail,
                });
            }
        };

        let ledger = Arc::clone(&self.ledger);
        let running_id = operation_id.clone();
        run_ledger(move || ledger.mark_running(&running_id, now_unix_ms())).await?;

        let operation_context = context.clone();
        let blocking = tokio::task::spawn_blocking(move || {
            let result = operation(operation_context);
            drop(permit);
            result
        });
        let (sender, receiver) = oneshot::channel();
        let monitor_ledger = Arc::clone(&self.ledger);
        let monitor_id = operation_id.clone();
        tokio::spawn(async move {
            let outcome = match blocking.await {
                Ok(Ok(completion)) => {
                    let committed = completion.committed;
                    let value = completion.value;
                    let ledger = Arc::clone(&monitor_ledger);
                    let id = monitor_id.clone();
                    match run_ledger(move || ledger.mark_succeeded(&id, committed, now_unix_ms()))
                        .await
                    {
                        Ok(_) => Ok(value),
                        Err(error) => Err(error),
                    }
                }
                Ok(Err(failure)) => {
                    let detail = failure.detail.clone();
                    let committed = failure.committed;
                    let ledger = Arc::clone(&monitor_ledger);
                    let id = monitor_id.clone();
                    match run_ledger(move || {
                        ledger.mark_failed(&id, committed, detail, now_unix_ms())
                    })
                    .await
                    {
                        Ok(_) => Err(HostedExecutionError::Operation {
                            operation_id: monitor_id.clone(),
                            detail: failure.detail,
                            committed,
                        }),
                        Err(error) => Err(error),
                    }
                }
                Err(error) => {
                    let detail = format!("blocking operation task failed: {error}");
                    let ledger = Arc::clone(&monitor_ledger);
                    let id = monitor_id.clone();
                    let persisted =
                        run_ledger(move || ledger.mark_failed(&id, None, detail, now_unix_ms()))
                            .await;
                    match persisted {
                        Ok(_) => Err(HostedExecutionError::Operation {
                            operation_id: monitor_id.clone(),
                            detail: "blocking operation task failed".to_string(),
                            committed: None,
                        }),
                        Err(error) => Err(error),
                    }
                }
            };
            let _ = sender.send(outcome);
        });

        self.wait_for_result(context, operation_id, receiver).await
    }

    async fn wait_for_result<T>(
        &self,
        context: ExecutionContext,
        operation_id: String,
        mut receiver: oneshot::Receiver<Result<T, HostedExecutionError>>,
    ) -> Result<T, HostedExecutionError> {
        let deadline = context.deadline.map(|deadline| {
            let remaining = deadline.unix_epoch_ms.saturating_sub(now_unix_ms());
            tokio::time::Instant::now() + Duration::from_millis(remaining)
        });
        let mut cancellation_poll = tokio::time::interval(CANCELLATION_POLL);
        cancellation_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                result = &mut receiver => {
                    return result.map_err(|_| HostedExecutionError::MonitorStopped { operation_id })?;
                }
                _ = cancellation_poll.tick() => {
                    if context.cancellation.is_cancelled() {
                        return self.mark_uncertain(
                            operation_id,
                            "caller cancelled after dispatch; execution may still complete",
                        ).await;
                    }
                }
                () = async {
                    if let Some(deadline) = deadline {
                        tokio::time::sleep_until(deadline).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    context.cancellation.cancel();
                    return self.mark_uncertain(
                        operation_id,
                        "response deadline expired after dispatch; execution may still complete",
                    ).await;
                }
            }
        }
    }

    async fn mark_uncertain<T>(
        &self,
        operation_id: String,
        detail: &'static str,
    ) -> Result<T, HostedExecutionError> {
        let ledger = Arc::clone(&self.ledger);
        let id = operation_id.clone();
        let record = run_ledger(move || {
            let _ = ledger.request_cancel(&id, now_unix_ms());
            match ledger.mark_indeterminate(&id, detail, now_unix_ms()) {
                Ok(record) => Ok(record),
                Err(HostedJobError::InvalidTransition { .. }) => ledger.load(&id),
                Err(error) => Err(error),
            }
        })
        .await?;
        Err(HostedExecutionError::AfterDispatch {
            operation_id,
            state: record,
            detail: format!("{detail}; query operation status before any retry"),
        })
    }
}

async fn run_ledger<T>(
    operation: impl FnOnce() -> Result<T, HostedJobError> + Send + 'static,
) -> Result<T, HostedExecutionError>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| HostedExecutionError::LedgerTask(error.to_string()))?
        .map_err(HostedExecutionError::Ledger)
}

fn now_unix_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

#[derive(Debug)]
pub enum HostedExecutionError {
    BeforeDispatch {
        operation_id: String,
        detail: String,
    },
    AfterDispatch {
        operation_id: String,
        state: HostedJobRecord,
        detail: String,
    },
    Operation {
        operation_id: String,
        detail: String,
        committed: Option<bool>,
    },
    MonitorStopped {
        operation_id: String,
    },
    Ledger(HostedJobError),
    LedgerTask(String),
}

impl Display for HostedExecutionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BeforeDispatch {
                operation_id,
                detail,
            } => {
                write!(
                    formatter,
                    "hosted operation `{operation_id}` was not dispatched: {detail}"
                )
            }
            Self::AfterDispatch {
                operation_id,
                detail,
                ..
            } => {
                write!(
                    formatter,
                    "hosted operation `{operation_id}` has an unknown outcome: {detail}"
                )
            }
            Self::Operation {
                operation_id,
                detail,
                committed,
            } => write!(
                formatter,
                "hosted operation `{operation_id}` failed (committed={committed:?}): {detail}"
            ),
            Self::MonitorStopped { operation_id } => write!(
                formatter,
                "hosted operation `{operation_id}` monitor stopped before reporting status"
            ),
            Self::Ledger(error) => Display::fmt(error, formatter),
            Self::LedgerTask(error) => write!(formatter, "hosted job ledger task failed: {error}"),
        }
    }
}

impl std::error::Error for HostedExecutionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosted_jobs::HostedJobState;
    use crate::mutation_scheduler::MutationSchedulerConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use vulcan_app::execution::{
        ExecutionAuthority, ExecutionCancellationToken, ExecutionDeadline, ExecutionIdentity,
        ExecutionRetryClass, ExecutionVaultIdentity,
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

    fn context(vault: &std::path::Path, deadline: Option<ExecutionDeadline>) -> ExecutionContext {
        ExecutionContext::new(
            ExecutionVaultIdentity::resolve(vault, None, None).expect("vault"),
            ExecutionAuthority::Caller {
                principal_id: "test".to_string(),
                credential_id: None,
                permission_ceiling: grant(),
            },
            grant(),
            ExecutionIdentity::new("daemon:test"),
            None,
            ExecutionRetryClass::IndeterminateAfterDispatch,
            ExecutionCancellationToken::default(),
            deadline,
        )
        .expect("context")
    }

    fn executor(state: &std::path::Path, config: MutationSchedulerConfig) -> Arc<HostedExecutor> {
        Arc::new(HostedExecutor::new(
            Arc::new(MutationScheduler::new(config).expect("scheduler")),
            Arc::new(HostedJobLedger::at(state)),
        ))
    }

    async fn wait_for_terminal(executor: &HostedExecutor, operation_id: &str) -> HostedJobRecord {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let record = executor
                    .status(operation_id.to_string())
                    .await
                    .expect("status");
                if record.state.is_terminal() {
                    return record;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("operation should finish")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn successful_operation_persists_completion() {
        let state = tempfile::tempdir().expect("state");
        let vault = tempfile::tempdir().expect("vault");
        let executor = executor(state.path(), MutationSchedulerConfig::default());
        let context = context(vault.path(), None);
        let operation_id = context.identity.operation_id.clone();

        let value = executor
            .execute(
                context,
                ScheduledOperation::Read,
                |_| Ok(()),
                |_| Ok(HostedOperationCompletion::read(42)),
            )
            .await
            .expect("execute");

        assert_eq!(value, 42);
        let record = executor
            .status(operation_id)
            .await
            .expect("completed status");
        assert_eq!(record.state, HostedJobState::Succeeded);
        assert_eq!(record.committed, Some(false));
        assert!(record.dispatched);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timeout_after_commit_is_indeterminate_then_monitored_without_replay() {
        let state = tempfile::tempdir().expect("state");
        let vault = tempfile::tempdir().expect("vault");
        let executor = executor(state.path(), MutationSchedulerConfig::default());
        let context = context(
            vault.path(),
            Some(ExecutionDeadline::after(Duration::from_millis(40))),
        );
        let operation_id = context.identity.operation_id.clone();
        let applications = Arc::new(AtomicUsize::new(0));
        let operation_applications = Arc::clone(&applications);

        let error = executor
            .execute(
                context,
                ScheduledOperation::Mutation,
                |_| Ok(()),
                move |_| {
                    operation_applications.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(120));
                    Ok(HostedOperationCompletion::mutation(()))
                },
            )
            .await
            .expect_err("response should time out");

        match error {
            HostedExecutionError::AfterDispatch { state, detail, .. } => {
                assert_eq!(state.state, HostedJobState::Indeterminate);
                assert!(detail.contains("query operation status"));
            }
            other => panic!("unexpected error: {other}"),
        }
        let record = wait_for_terminal(&executor, &operation_id).await;
        assert_eq!(record.state, HostedJobState::Succeeded);
        assert_eq!(record.committed, Some(true));
        assert_eq!(applications.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queue_deadline_is_known_not_dispatched() {
        let state = tempfile::tempdir().expect("state");
        let vault = tempfile::tempdir().expect("vault");
        let executor = executor(
            state.path(),
            MutationSchedulerConfig {
                max_in_flight: 1,
                max_queued: 2,
                max_reads_per_vault: 1,
            },
        );
        let first = context(vault.path(), None);
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let first_executor = Arc::clone(&executor);
        let first_task = tokio::spawn(async move {
            first_executor
                .execute(
                    first,
                    ScheduledOperation::Mutation,
                    |_| Ok(()),
                    move |_| {
                        started_sender.send(()).expect("started");
                        release_receiver.recv().expect("release");
                        Ok(HostedOperationCompletion::mutation(()))
                    },
                )
                .await
        });
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("first started");

        let second = context(
            vault.path(),
            Some(ExecutionDeadline::after(Duration::from_millis(30))),
        );
        let second_id = second.identity.operation_id.clone();
        let result: Result<(), HostedExecutionError> = executor
            .execute(
                second,
                ScheduledOperation::Mutation,
                |_| Ok(()),
                |_| panic!("queued operation must not be dispatched"),
            )
            .await;
        assert!(matches!(
            result,
            Err(HostedExecutionError::BeforeDispatch { .. })
        ));
        let record = executor.status(second_id).await.expect("queued status");
        assert_eq!(record.state, HostedJobState::Failed);
        assert_eq!(record.committed, Some(false));
        assert!(!record.dispatched);

        release_sender.send(()).expect("release first");
        first_task.await.expect("join").expect("first operation");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn queued_revalidation_failure_is_persisted_before_dispatch() {
        let state = tempfile::tempdir().expect("state");
        let vault = tempfile::tempdir().expect("vault");
        let executor = executor(state.path(), MutationSchedulerConfig::default());
        let context = context(vault.path(), None);
        let operation_id = context.identity.operation_id.clone();

        let result: Result<(), HostedExecutionError> = executor
            .execute(
                context,
                ScheduledOperation::Mutation,
                |_| {
                    Err(MutationScheduleError::Revalidation(
                        "permission revoked".to_string(),
                    ))
                },
                |_| panic!("rejected operation must not be dispatched"),
            )
            .await;

        assert!(matches!(
            result,
            Err(HostedExecutionError::BeforeDispatch { .. })
        ));
        let record = executor.status(operation_id).await.expect("status");
        assert_eq!(record.state, HostedJobState::Failed);
        assert!(!record.dispatched);
        assert!(record
            .detail
            .expect("detail")
            .contains("permission revoked"));
    }
}
