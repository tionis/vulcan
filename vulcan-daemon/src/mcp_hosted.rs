//! Hosted MCP request identity and durable pre-dispatch registration.

use crate::hosted_executor::{
    HostedExecutionError, HostedExecutor, HostedOperationCompletion, HostedOperationFailure,
};
use crate::mcp_session::McpSessionAuthority;
use crate::mutation_scheduler::{MutationScheduleError, MutationScheduler, ScheduledOperation};
use serde_json::Value;
use std::path::Path;
use vulcan_app::execution::{
    ExecutionAuthority, ExecutionCancellationToken, ExecutionContext, ExecutionDeadline,
    ExecutionIdentity, ExecutionRetryClass, ExecutionVaultIdentity,
};
use vulcan_app::mcp_dispatch::{request_is_read_only, McpHttpProcessResult};
use vulcan_core::PermissionGrant;

/// Build one caller-bound execution and register its mutation before launching
/// the request worker. Reads retain the same identity without durable logging.
#[allow(clippy::too_many_arguments)]
pub fn prepare_hosted_mcp_request(
    executor: &HostedExecutor,
    runtime: &tokio::runtime::Handle,
    vault_root: &Path,
    grant: PermissionGrant,
    payload: &Value,
    authority: &McpSessionAuthority,
    cancellation: ExecutionCancellationToken,
    deadline: ExecutionDeadline,
) -> Result<ExecutionContext, String> {
    let kind = scheduled_operation(payload);
    let execution =
        build_execution_context(vault_root, grant, authority, cancellation, deadline, kind)?;
    if kind == ScheduledOperation::Mutation {
        runtime
            .block_on(executor.register(&execution))
            .map_err(|error| error.to_string())?;
    }
    Ok(execution)
}

#[must_use]
pub fn scheduled_operation(payload: &Value) -> ScheduledOperation {
    if request_is_read_only(payload) {
        ScheduledOperation::Read
    } else {
        ScheduledOperation::Mutation
    }
}

#[derive(Debug)]
pub enum HostedMcpRunError {
    BeforeDispatch(String),
    Execution(HostedExecutionError),
}

/// Execute one hosted request under the shared per-vault scheduler. Mutations
/// must already have been registered by `prepare_hosted_mcp_request` before the
/// adapter launches its worker; the executor retains the permit and outcome
/// monitor if the HTTP caller times out.
#[allow(clippy::too_many_arguments)]
pub fn run_hosted_mcp_request<C, R, A, P>(
    scheduler: &MutationScheduler,
    executor: &HostedExecutor,
    runtime: &tokio::runtime::Handle,
    mut core: C,
    request: Value,
    execution: ExecutionContext,
    revalidate: R,
    attenuate: A,
    process: P,
) -> Result<(C, Result<McpHttpProcessResult, Value>), HostedMcpRunError>
where
    C: Send + 'static,
    R: FnOnce(&ExecutionContext) -> Result<(), MutationScheduleError>,
    A: FnOnce(&mut C) -> Result<(), String> + Send + 'static,
    P: FnOnce(&mut C, &Value) -> Result<McpHttpProcessResult, Value> + Send + 'static,
{
    let kind = scheduled_operation(&request);
    if kind == ScheduledOperation::Read {
        let permit = runtime
            .block_on(scheduler.acquire(&execution, kind, revalidate))
            .map_err(|error| HostedMcpRunError::BeforeDispatch(error.to_string()))?;
        execution
            .checkpoint()
            .map_err(|error| HostedMcpRunError::BeforeDispatch(error.to_string()))?;
        attenuate(&mut core).map_err(HostedMcpRunError::BeforeDispatch)?;
        let response = process(&mut core, &request);
        drop(permit);
        return Ok((core, response));
    }
    runtime
        .block_on(executor.execute_registered_caller(
            execution,
            kind,
            revalidate,
            move |execution| {
                execution
                    .checkpoint()
                    .map_err(|error| HostedOperationFailure::before_commit(error.to_string()))?;
                attenuate(&mut core).map_err(HostedOperationFailure::before_commit)?;
                let response = process(&mut core, &request);
                if mutation_outcome_is_indeterminate(&response) {
                    return Err(HostedOperationFailure::indeterminate(
                        "MCP mutation returned an error; its write outcome is unverified",
                    ));
                }
                Ok(HostedOperationCompletion {
                    value: (core, response),
                    committed: true,
                })
            },
        ))
        .map_err(HostedMcpRunError::Execution)
}

fn mutation_outcome_is_indeterminate(response: &Result<McpHttpProcessResult, Value>) -> bool {
    response.is_err()
        || response.as_ref().is_ok_and(|result| {
            result.response.as_ref().is_some_and(|value| {
                value.get("error").is_some()
                    || value.pointer("/result/isError").and_then(Value::as_bool) == Some(true)
            })
        })
}

fn build_execution_context(
    vault_root: &Path,
    grant: PermissionGrant,
    authority: &McpSessionAuthority,
    cancellation: ExecutionCancellationToken,
    deadline: ExecutionDeadline,
    kind: ScheduledOperation,
) -> Result<ExecutionContext, String> {
    let principal_id = authority
        .subject
        .clone()
        .or_else(|| authority.client_id.clone())
        .unwrap_or_else(|| format!("mcp:{}", authority.remote_instance_id));
    ExecutionContext::new(
        ExecutionVaultIdentity::resolve(vault_root, None, None)
            .map_err(|error| error.to_string())?,
        ExecutionAuthority::Caller {
            principal_id,
            credential_id: authority.grant_id.map(|id| id.to_string()),
            permission_ceiling: grant.clone(),
        },
        grant,
        ExecutionIdentity::new(format!("mcp:{}", authority.remote_instance_id)),
        authority.audience.clone(),
        if kind == ScheduledOperation::Read {
            ExecutionRetryClass::ReadOnly
        } else {
            ExecutionRetryClass::IndeterminateAfterDispatch
        },
        cancellation,
        Some(deadline),
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        build_execution_context, prepare_hosted_mcp_request, run_hosted_mcp_request,
        scheduled_operation, HostedMcpRunError,
    };
    use crate::hosted_executor::HostedExecutor;
    use crate::hosted_jobs::{HostedJobLedger, HostedJobState};
    use crate::mcp_session::McpSessionAuthority;
    use crate::mutation_scheduler::{
        MutationScheduler, MutationSchedulerConfig, ScheduledOperation,
    };
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::tempdir;
    use ulid::Ulid;
    use vulcan_app::execution::{
        ExecutionAuthority, ExecutionCancellationToken, ExecutionDeadline, ExecutionRetryClass,
    };
    use vulcan_app::mcp_dispatch::McpHttpProcessResult;
    use vulcan_core::{PermissionGrant, PermissionProfile};

    #[test]
    fn hosted_context_binds_principal_grant_and_retry_class() {
        let vault = tempdir().unwrap();
        let remote = Ulid::new();
        let grant_id = Ulid::new();
        let mut authority = McpSessionAuthority::direct(
            remote,
            "test-credential",
            Some("client".to_string()),
            Some("subject".to_string()),
            None,
            vec![],
            vec![],
        );
        authority.grant_id = Some(grant_id);
        authority.audience = Some("https://example.test/mcp".to_string());
        let grant = PermissionGrant::from_profile(&PermissionProfile::default());
        let read = build_execution_context(
            vault.path(),
            grant.clone(),
            &authority,
            ExecutionCancellationToken::default(),
            ExecutionDeadline::after(std::time::Duration::from_secs(1)),
            ScheduledOperation::Read,
        )
        .unwrap();
        let write = build_execution_context(
            vault.path(),
            grant.clone(),
            &authority,
            ExecutionCancellationToken::default(),
            ExecutionDeadline::after(std::time::Duration::from_secs(1)),
            ScheduledOperation::Mutation,
        )
        .unwrap();
        assert_eq!(read.retry_class, ExecutionRetryClass::ReadOnly);
        assert_eq!(
            write.retry_class,
            ExecutionRetryClass::IndeterminateAfterDispatch
        );
        assert_eq!(read.audience.as_deref(), Some("https://example.test/mcp"));
        assert_eq!(read.effective_permissions, grant);
        assert_eq!(read.identity.service_instance_id, format!("mcp:{remote}"));
        match read.authority {
            ExecutionAuthority::Caller {
                principal_id,
                credential_id,
                ..
            } => {
                assert_eq!(principal_id, "subject");
                assert_eq!(
                    credential_id.as_deref(),
                    Some(grant_id.to_string().as_str())
                );
            }
            ExecutionAuthority::BackgroundService { .. } => {
                panic!("MCP request must carry caller authority")
            }
        }
    }

    #[test]
    fn only_known_read_methods_use_read_retry_class() {
        assert_eq!(
            scheduled_operation(&json!({"method": "tools/list", "id": 1})),
            ScheduledOperation::Read
        );
        assert_eq!(
            scheduled_operation(&json!({"method": "tools/call", "id": 2})),
            ScheduledOperation::Mutation
        );
    }

    #[test]
    fn mutation_is_durably_registered_before_worker_and_read_is_not() {
        let vault = tempdir().unwrap();
        let ledger = Arc::new(HostedJobLedger::at(vault.path().join("operations")));
        let scheduler =
            Arc::new(MutationScheduler::new(MutationSchedulerConfig::default()).unwrap());
        let executor = HostedExecutor::new(scheduler, Arc::clone(&ledger));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let authority = McpSessionAuthority::direct(
            Ulid::new(),
            "test-credential",
            Some("client".to_string()),
            Some("subject".to_string()),
            None,
            vec![],
            vec![],
        );
        let grant = PermissionGrant::from_profile(&PermissionProfile::default());
        let read = prepare_hosted_mcp_request(
            &executor,
            runtime.handle(),
            vault.path(),
            grant.clone(),
            &json!({"method": "tools/list", "id": 1}),
            &authority,
            ExecutionCancellationToken::default(),
            ExecutionDeadline::after(std::time::Duration::from_secs(1)),
        )
        .unwrap();
        assert!(ledger.load(&read.identity.operation_id).is_err());
        let write = prepare_hosted_mcp_request(
            &executor,
            runtime.handle(),
            vault.path(),
            grant,
            &json!({"method": "tools/call", "id": 2}),
            &authority,
            ExecutionCancellationToken::default(),
            ExecutionDeadline::after(std::time::Duration::from_secs(1)),
        )
        .unwrap();
        let record = ledger.load(&write.identity.operation_id).unwrap();
        assert_eq!(record.state, HostedJobState::Queued);
        assert_eq!(record.request_id, write.identity.request_id);
    }

    #[test]
    fn read_lane_revalidates_and_processes_without_a_durable_job() {
        let vault = tempdir().unwrap();
        let ledger = Arc::new(HostedJobLedger::at(vault.path().join("operations")));
        let scheduler =
            Arc::new(MutationScheduler::new(MutationSchedulerConfig::default()).unwrap());
        let executor = HostedExecutor::new(Arc::clone(&scheduler), Arc::clone(&ledger));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let authority = McpSessionAuthority::direct(
            Ulid::new(),
            "test-credential",
            None,
            Some("subject".to_string()),
            None,
            vec![],
            vec![],
        );
        let request = json!({"method": "tools/list", "id": 1});
        let execution = prepare_hosted_mcp_request(
            &executor,
            runtime.handle(),
            vault.path(),
            PermissionGrant::from_profile(&PermissionProfile::default()),
            &request,
            &authority,
            ExecutionCancellationToken::default(),
            ExecutionDeadline::after(std::time::Duration::from_secs(5)),
        )
        .unwrap();
        let operation_id = execution.identity.operation_id.clone();
        let (core, result) = run_hosted_mcp_request(
            &scheduler,
            &executor,
            runtime.handle(),
            0_usize,
            request,
            execution,
            |_| Ok(()),
            |core| {
                *core += 1;
                Ok(())
            },
            |core, _| {
                *core += 1;
                Ok(McpHttpProcessResult {
                    response: Some(json!({"result": {"tools": []}})),
                    notifications: vec![],
                    accepted_notification: false,
                    session_stale: false,
                })
            },
        )
        .unwrap();
        assert_eq!(core, 2);
        assert_eq!(
            result.unwrap().response,
            Some(json!({"result": {"tools": []}}))
        );
        assert!(ledger.load(&operation_id).is_err());
    }

    #[test]
    fn mutation_tool_error_records_unknown_commit_state() {
        let vault = tempdir().unwrap();
        let ledger = Arc::new(HostedJobLedger::at(vault.path().join("operations")));
        let scheduler =
            Arc::new(MutationScheduler::new(MutationSchedulerConfig::default()).unwrap());
        let executor = HostedExecutor::new(Arc::clone(&scheduler), Arc::clone(&ledger));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let authority = McpSessionAuthority::direct(
            Ulid::new(),
            "test-credential",
            None,
            Some("subject".to_string()),
            None,
            vec![],
            vec![],
        );
        let request = json!({"method": "tools/call", "id": 1});
        let execution = prepare_hosted_mcp_request(
            &executor,
            runtime.handle(),
            vault.path(),
            PermissionGrant::from_profile(&PermissionProfile::default()),
            &request,
            &authority,
            ExecutionCancellationToken::default(),
            ExecutionDeadline::after(std::time::Duration::from_secs(5)),
        )
        .unwrap();
        let operation_id = execution.identity.operation_id.clone();
        let result = run_hosted_mcp_request(
            &scheduler,
            &executor,
            runtime.handle(),
            0_usize,
            request,
            execution,
            |_| Ok(()),
            |_| Ok(()),
            |_, _| {
                Ok(McpHttpProcessResult {
                    response: Some(json!({"result": {"isError": true}})),
                    notifications: vec![],
                    accepted_notification: false,
                    session_stale: false,
                })
            },
        );
        assert!(matches!(result, Err(HostedMcpRunError::Execution(_))));
        let record = ledger.load(&operation_id).unwrap();
        assert_eq!(record.state, HostedJobState::Failed);
        assert_eq!(record.committed, None);
    }
}
