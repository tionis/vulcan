//! Hosted MCP request identity and durable pre-dispatch registration.

use crate::hosted_executor::HostedExecutor;
use crate::mcp_session::McpSessionAuthority;
use crate::mutation_scheduler::ScheduledOperation;
use serde_json::Value;
use std::path::Path;
use vulcan_app::execution::{
    ExecutionAuthority, ExecutionCancellationToken, ExecutionContext, ExecutionDeadline,
    ExecutionIdentity, ExecutionRetryClass, ExecutionVaultIdentity,
};
use vulcan_app::mcp_dispatch::request_is_read_only;
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
    use super::{build_execution_context, prepare_hosted_mcp_request, scheduled_operation};
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
}
