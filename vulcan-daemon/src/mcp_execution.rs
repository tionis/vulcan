//! Shared request execution for client-owned stdio and foreground/resident HTTP.
//! Registration precedes worker launch; a response timeout never proves a write stopped.

use crate::mcp_http_codec::McpHttpRequest;
use crate::mcp_http_host::McpHttpHost;
use crate::mcp_session::McpSessionAuthority;
use crate::mcp_worker::{run_mcp_worker, McpWorkerResult};
use crate::{
    hosted_executor::{HostedExecutionError, HostedExecutor},
    mcp_hosted::{
        prepare_hosted_mcp_request, run_hosted_mcp_request, scheduled_operation, HostedMcpRunError,
    },
    mutation_scheduler::{MutationScheduleError, MutationScheduler, ScheduledOperation},
};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use vulcan_app::execution::ExecutionCancellationToken;
use vulcan_app::execution::{ExecutionContext, ExecutionDeadline};
use vulcan_app::mcp_dispatch::tool_error_response;
use vulcan_app::mcp_dispatch::{
    jsonrpc_error, request_id, timeout_http_result, timeout_response_for_request,
    McpHttpProcessResult,
};
use vulcan_app::mcp_session_protocol::McpProtocolCore;
use vulcan_core::{PermissionGrant, VaultPaths};

/// Owned protocol state is supplied by the app, never by CLI options or handlers.
pub trait McpRequestCore: Clone + Send + 'static {
    fn vault_paths(&self) -> &VaultPaths;
    fn permission_grant(&self) -> PermissionGrant;
    fn attenuate_profile(&mut self) -> Result<(), String>;
    fn process_request(&mut self, request: Value) -> Vec<Value>;
    fn process_http_request(&mut self, request: &Value) -> Result<McpHttpProcessResult, Value>;
}

impl McpRequestCore for McpProtocolCore {
    fn vault_paths(&self) -> &VaultPaths {
        self.session.paths()
    }
    fn permission_grant(&self) -> PermissionGrant {
        self.session.selection().grant.clone()
    }
    fn attenuate_profile(&mut self) -> Result<(), String> {
        self.session.attenuate_profile()
    }
    fn process_request(&mut self, request: Value) -> Vec<Value> {
        McpProtocolCore::process_request(self, request)
    }
    fn process_http_request(&mut self, request: &Value) -> Result<McpHttpProcessResult, Value> {
        McpProtocolCore::process_http_request(self, request)
    }
}

#[derive(Debug, Clone)]
pub struct HostedMcpExecution {
    pub scheduler: Arc<MutationScheduler>,
    pub executor: Arc<HostedExecutor>,
    pub runtime: tokio::runtime::Handle,
}

struct HostedMcpDispatch<C: McpRequestCore> {
    http: McpHttpHost<C>,
    inbound: McpHttpRequest,
    authority: McpSessionAuthority,
    execution: ExecutionContext,
}

impl HostedMcpExecution {
    pub fn prepare<C: McpRequestCore>(
        &self,
        core: &C,
        payload: &Value,
        authority: &McpSessionAuthority,
        cancellation: ExecutionCancellationToken,
        deadline: ExecutionDeadline,
    ) -> Result<ExecutionContext, Value> {
        prepare_hosted_mcp_request(
            &self.executor,
            &self.runtime,
            core.vault_paths().vault_root(),
            core.permission_grant(),
            payload,
            authority,
            cancellation,
            deadline,
        )
        .map_err(|message| {
            jsonrpc_error(
                request_id(payload).unwrap_or(Value::Null),
                -32603,
                message,
                None,
            )
        })
    }

    fn execute<C: McpRequestCore>(
        &self,
        core: &mut C,
        payload: &Value,
        dispatch: &HostedMcpDispatch<C>,
    ) -> Result<McpHttpProcessResult, Value> {
        let http = dispatch.http.clone();
        let inbound = dispatch.inbound.clone();
        let authority = dispatch.authority.clone();
        match run_hosted_mcp_request(
            &self.scheduler,
            &self.executor,
            &self.runtime,
            core.clone(),
            payload.clone(),
            dispatch.execution.clone(),
            move |_| revalidate_hosted_mcp_authority(&http, &inbound, &authority),
            attenuate_mcp_core_profile,
            C::process_http_request,
        ) {
            Ok((next, response)) => {
                *core = next;
                response
            }
            Err(HostedMcpRunError::BeforeDispatch(message)) => {
                Err(hosted_mcp_json_error(payload, message, None))
            }
            Err(HostedMcpRunError::Execution(error)) => hosted_mcp_execution_error(
                payload,
                &dispatch.execution,
                &dispatch.http.endpoint,
                error,
            ),
        }
    }
}

fn revalidate_hosted_mcp_authority<C: McpRequestCore>(
    http: &McpHttpHost<C>,
    inbound: &McpHttpRequest,
    authority: &McpSessionAuthority,
) -> Result<(), MutationScheduleError> {
    let current = http.authenticate(&inbound.headers).map_err(|_| {
        MutationScheduleError::Revalidation("MCP authority is no longer valid".to_string())
    })?;
    if !current.matches(authority) {
        return Err(MutationScheduleError::Revalidation(
            "MCP authority changed while queued".to_string(),
        ));
    }
    Ok(())
}

fn hosted_mcp_json_error(payload: &Value, message: String, operation_id: Option<&str>) -> Value {
    jsonrpc_error(
        request_id(payload).unwrap_or(Value::Null),
        -32603,
        message,
        operation_id.map(|id| serde_json::json!({ "operation_id": id })),
    )
}

#[must_use]
pub fn hosted_mcp_unknown_result(
    payload: &Value,
    operation_id: &str,
    detail: &str,
    endpoint: &str,
) -> McpHttpProcessResult {
    let response = request_id(payload).map(|id| {
        let structured = serde_json::json!({
            "error": detail,
            "operation_id": operation_id,
            "status_path": format!("{}/operations/{operation_id}", endpoint.trim_end_matches('/')),
            "outcome": "indeterminate",
            "retry": "check_operation_status_first",
        });
        if payload.get("method").and_then(Value::as_str) == Some("tools/call") {
            tool_error_response(id, detail.to_string(), Some(structured))
        } else {
            jsonrpc_error(id, -32000, detail.to_string(), Some(structured))
        }
    });
    McpHttpProcessResult {
        accepted_notification: response.is_none(),
        response,
        notifications: Vec::new(),
        session_stale: true,
    }
}

pub fn hosted_mcp_execution_error(
    payload: &Value,
    execution: &ExecutionContext,
    endpoint: &str,
    error: HostedExecutionError,
) -> Result<McpHttpProcessResult, Value> {
    let operation_id = &execution.identity.operation_id;
    match error {
        HostedExecutionError::BeforeDispatch { detail, .. } => Err(jsonrpc_error(
            request_id(payload).unwrap_or(Value::Null),
            -32603,
            detail,
            Some(serde_json::json!({
                "operation_id": operation_id,
                "status_path": format!("{}/operations/{operation_id}", endpoint.trim_end_matches('/')),
                "dispatched": false,
            })),
        )),
        HostedExecutionError::Operation {
            detail,
            committed: Some(false),
            ..
        } => Err(jsonrpc_error(
            request_id(payload).unwrap_or(Value::Null),
            -32603,
            detail,
            Some(serde_json::json!({
                "operation_id": operation_id,
                "status_path": format!("{}/operations/{operation_id}", endpoint.trim_end_matches('/')),
                "dispatched": true,
                "committed": false,
            })),
        )),
        other => Ok(hosted_mcp_unknown_result(
            payload,
            operation_id,
            &other.to_string(),
            endpoint,
        )),
    }
}

pub fn attenuate_mcp_core_profile<C: McpRequestCore>(core: &mut C) -> Result<(), String> {
    core.attenuate_profile()
}

pub fn process_request_with_timeout<C: McpRequestCore>(
    core: &mut C,
    request: Value,
    timeout: Duration,
) -> Vec<Value> {
    if timeout.is_zero() {
        return timeout_response_for_request(&request, timeout)
            .into_iter()
            .collect();
    }
    let timeout_request = request.clone();
    let mut worker = core.clone();
    match run_mcp_worker("vulcan-mcp-request", timeout, None, move || {
        let messages = worker.process_request(request);
        (worker, messages)
    }) {
        McpWorkerResult::Completed((next, messages)) => {
            *core = next;
            messages
        }
        McpWorkerResult::TimedOut => timeout_response_for_request(&timeout_request, timeout)
            .into_iter()
            .collect(),
        McpWorkerResult::Disconnected => {
            let id = request_id(&timeout_request).unwrap_or(Value::Null);
            vec![jsonrpc_error(
                id,
                -32603,
                "MCP request worker stopped before producing a response".to_string(),
                None,
            )]
        }
        McpWorkerResult::SpawnFailed => {
            let id = request_id(&timeout_request).unwrap_or(Value::Null);
            vec![jsonrpc_error(
                id,
                -32603,
                "MCP request worker could not be started".to_string(),
                None,
            )]
        }
    }
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)] // Registration must precede the worker, and all timeout branches share its ID.
pub fn process_http_request_with_timeout<C: McpRequestCore>(
    core: &mut C,
    request: Value,
    timeout: Duration,
    http_context: &McpHttpHost<C>,
    inbound: &McpHttpRequest,
    authority: &McpSessionAuthority,
    cancellation: &ExecutionCancellationToken,
    hosted: Option<&HostedMcpExecution>,
) -> Result<McpHttpProcessResult, Value> {
    if cancellation.is_cancelled() {
        return Err(jsonrpc_error(
            request_id(&request).unwrap_or(Value::Null),
            -32800,
            "MCP request cancelled before dispatch".to_string(),
            None,
        ));
    }
    if timeout.is_zero() {
        return Ok(timeout_http_result(&request, timeout));
    }
    let timeout_request = request.clone();
    let mut worker = core.clone();
    let hosted = hosted.cloned();
    // One deadline shared by the scheduler and this response wait, so a queued mutation whose
    // pre-dispatch deadline fires before the worker wait still gets the durable status response.
    let dispatch_deadline = ExecutionDeadline::after(timeout);
    let dispatch = hosted
        .as_ref()
        .map(|hosted| {
            hosted
                .prepare(
                    core,
                    &request,
                    authority,
                    cancellation.clone(),
                    dispatch_deadline,
                )
                .map(|execution| HostedMcpDispatch {
                    http: http_context.clone(),
                    inbound: inbound.clone(),
                    authority: authority.clone(),
                    execution,
                })
        })
        .transpose()?;
    let operation_id = dispatch
        .as_ref()
        .filter(|_| scheduled_operation(&request) == ScheduledOperation::Mutation)
        .map(|dispatch| dispatch.execution.identity.operation_id.clone());
    let failed_ledger = hosted.as_ref().map(|hosted| hosted.executor.ledger());
    #[cfg(feature = "oauth")]
    let named_runtime = http_context.named_runtime.is_some();
    #[cfg(not(feature = "oauth"))]
    let named_runtime = false;
    let worker_cancellation = cancellation.clone();
    let worker_result = run_mcp_worker(
        "vulcan-mcp-http-request",
        timeout,
        Some(cancellation),
        move || {
            let result = if let (Some(hosted), Some(dispatch)) = (hosted, dispatch) {
                // A hosted mutation is already durably registered. Let its executor
                // record pre-dispatch cancellation and retain the operation ID.
                hosted.execute(&mut worker, &request, &dispatch)
            } else if worker_cancellation.is_cancelled() {
                Err(jsonrpc_error(
                    request_id(&request).unwrap_or(Value::Null),
                    -32800,
                    "MCP request cancelled before dispatch".to_string(),
                    None,
                ))
            } else if named_runtime {
                attenuate_mcp_core_profile(&mut worker)
                    .map_err(|message| {
                        jsonrpc_error(
                            request_id(&request).unwrap_or(Value::Null),
                            -32603,
                            message,
                            None,
                        )
                    })
                    .and_then(|()| worker.process_http_request(&request))
            } else {
                worker.process_http_request(&request)
            };
            (worker, result)
        },
    );
    if matches!(worker_result, McpWorkerResult::SpawnFailed) {
        if let (Some(operation_id), Some(ledger)) = (&operation_id, failed_ledger) {
            let _ = ledger.mark_failed(
                operation_id,
                Some(false),
                "MCP request worker could not be started",
                current_unix_millis(),
            );
        }
        return Err(jsonrpc_error(
            request_id(&timeout_request).unwrap_or(Value::Null),
            -32603,
            "MCP request worker could not be started".to_string(),
            None,
        ));
    }
    match worker_result {
        McpWorkerResult::Completed((next, result)) => {
            *core = next;
            if let (Err(_), Some(operation_id)) = (&result, operation_id.as_deref()) {
                if dispatch_deadline.is_expired_at(SystemTime::now()) {
                    return Ok(hosted_mcp_unknown_result(
                        &timeout_request,
                        operation_id,
                        "MCP response deadline expired; write outcome is not yet known",
                        &http_context.endpoint,
                    ));
                }
            }
            result
        }
        McpWorkerResult::TimedOut => {
            if let Some(operation_id) = operation_id.as_deref() {
                return Ok(hosted_mcp_unknown_result(
                    &timeout_request,
                    operation_id,
                    "MCP response deadline expired; write outcome is not yet known",
                    &http_context.endpoint,
                ));
            }
            Ok(timeout_http_result(&timeout_request, timeout))
        }
        McpWorkerResult::Disconnected => {
            if let Some(operation_id) = operation_id.as_deref() {
                return Ok(hosted_mcp_unknown_result(
                    &timeout_request,
                    operation_id,
                    "MCP request worker stopped; write outcome is not yet known",
                    &http_context.endpoint,
                ));
            }
            Err(jsonrpc_error(
                request_id(&timeout_request).unwrap_or(Value::Null),
                -32603,
                "MCP request worker stopped before producing a response".to_string(),
                None,
            ))
        }
        McpWorkerResult::SpawnFailed => unreachable!("handled before result dispatch"),
    }
}
fn current_unix_millis() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_session::McpSessionRegistry;
    #[cfg(feature = "oauth")]
    use crate::{
        mcp_oauth_browser::{PendingConsentMap, PendingIndieAuthMap},
        mcp_oauth_clients::OAuthClientRegistry,
        mcp_oauth_codes::McpAuthorizationCodeMap,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{mpsc, Arc, Mutex};
    use ulid::Ulid;
    use vulcan_app::mcp_catalog::{McpToolPack, McpToolPackMode};
    use vulcan_core::PermissionProfile;

    struct Gate {
        release: Mutex<mpsc::Receiver<()>>,
        entered: mpsc::Sender<()>,
        finished: mpsc::Sender<()>,
    }

    #[derive(Clone)]
    struct CounterCore {
        paths: VaultPaths,
        calls: usize,
        panic: bool,
        gate: Option<Arc<Gate>>,
    }

    impl CounterCore {
        fn new(paths: &VaultPaths) -> Self {
            Self {
                paths: paths.clone(),
                calls: 0,
                panic: false,
                gate: None,
            }
        }
        fn run(&mut self) {
            assert!(!self.panic, "test worker panic");
            if let Some(gate) = &self.gate {
                gate.entered.send(()).unwrap();
                gate.release.lock().unwrap().recv().unwrap();
            }
            self.calls += 1;
            if let Some(gate) = &self.gate {
                gate.finished.send(()).unwrap();
            }
        }
    }

    impl McpRequestCore for CounterCore {
        fn vault_paths(&self) -> &VaultPaths {
            &self.paths
        }
        fn permission_grant(&self) -> PermissionGrant {
            PermissionGrant::from_profile(&PermissionProfile::readonly())
        }
        fn attenuate_profile(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn process_request(&mut self, request: Value) -> Vec<Value> {
            self.run();
            vec![
                serde_json::json!({"jsonrpc":"2.0", "id":request["id"], "result":{"calls":self.calls}}),
            ]
        }
        fn process_http_request(&mut self, request: &Value) -> Result<McpHttpProcessResult, Value> {
            self.run();
            Ok(McpHttpProcessResult {
                response: Some(
                    serde_json::json!({"jsonrpc":"2.0", "id":request["id"], "result":{"calls":self.calls}}),
                ),
                notifications: Vec::new(),
                accepted_notification: false,
                session_stale: false,
            })
        }
    }

    fn host(paths: &VaultPaths) -> McpHttpHost<CounterCore> {
        McpHttpHost {
            paths: paths.clone(),
            requested_profile: Some("readonly".into()),
            selected_tool_packs: BTreeSet::from([McpToolPack::NotesRead, McpToolPack::Search]),
            tool_pack_mode: McpToolPackMode::Static,
            endpoint: "/mcp".into(),
            auth_token: None,
            bind_addr: "127.0.0.1:4321".parse().unwrap(),
            instance_id: Ulid::new(),
            sessions: Arc::new(McpSessionRegistry::new()),
            request_timeout: Duration::from_secs(30),
            #[cfg(feature = "oauth")]
            oauth: None,
            #[cfg(feature = "oauth")]
            oauth_codes: Arc::new(McpAuthorizationCodeMap::default()),
            #[cfg(feature = "oauth")]
            oauth_clients: Arc::new(OAuthClientRegistry::ephemeral()),
            #[cfg(feature = "oauth")]
            oauth_pending_indieauth: Arc::new(PendingIndieAuthMap::default()),
            #[cfg(feature = "oauth")]
            oauth_pending_consent: Arc::new(PendingConsentMap::default()),
            #[cfg(feature = "oauth")]
            oauth_dcr_enabled: true,
            #[cfg(feature = "oauth")]
            oauth_dcr_allowed_redirect_hosts: vec!["client.example.test".into()],
            #[cfg(feature = "oauth")]
            oauth_local_redirect_uris: Vec::new(),
            #[cfg(feature = "oauth")]
            oauth_indieauth: None,
            #[cfg(feature = "oauth")]
            named_runtime: None,
        }
    }

    fn inbound() -> McpHttpRequest {
        McpHttpRequest {
            method: "POST".into(),
            path: "/mcp".into(),
            query: String::new(),
            headers: BTreeMap::new(),
            body: Vec::new(),
        }
    }

    #[test]
    fn stdio_execution_publishes_only_completed_core_and_preserves_error_shapes() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let mut core = CounterCore::new(&paths);
        let request = serde_json::json!({"id":7,"method":"tools/list"});
        let timed = process_request_with_timeout(&mut core, request.clone(), Duration::ZERO);
        assert_eq!(core.calls, 0);
        assert_eq!(timed[0]["id"], 7);
        assert!(timed[0].get("error").is_some());
        assert!(process_request_with_timeout(
            &mut core,
            serde_json::json!({"method":"notifications/initialized"}),
            Duration::ZERO
        )
        .is_empty());
        let complete =
            process_request_with_timeout(&mut core, request.clone(), Duration::from_secs(5));
        assert_eq!(core.calls, 1);
        assert_eq!(complete[0]["result"]["calls"], 1);
        core.panic = true;
        let failed = process_request_with_timeout(&mut core, request, Duration::from_secs(5));
        assert_eq!(core.calls, 1);
        assert_eq!(failed[0]["error"]["code"], -32603);
        assert!(failed[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("stopped before producing"));
    }

    #[test]
    fn http_execution_rejects_cancelled_requests_and_does_not_publish_timed_out_core() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let mut core = CounterCore::new(&paths);
        let host = host(&paths);
        let inbound = inbound();
        let authority = host.authenticate(&inbound.headers).unwrap();
        let request = serde_json::json!({"id":7,"method":"tools/list"});
        let cancellation = ExecutionCancellationToken::default();
        cancellation.cancel();
        let denied = process_http_request_with_timeout(
            &mut core,
            request.clone(),
            Duration::from_secs(5),
            &host,
            &inbound,
            &authority,
            &cancellation,
            None,
        )
        .unwrap_err();
        assert_eq!(denied["error"]["code"], -32800);
        assert_eq!(core.calls, 0);
        let complete = process_http_request_with_timeout(
            &mut core,
            request.clone(),
            Duration::from_secs(5),
            &host,
            &inbound,
            &authority,
            &ExecutionCancellationToken::default(),
            None,
        )
        .unwrap();
        assert!(!complete.session_stale);
        assert_eq!(core.calls, 1);

        let (release, wait) = mpsc::channel();
        let (entered, observed_entered) = mpsc::channel();
        let (finished, observed_finished) = mpsc::channel();
        core.gate = Some(Arc::new(Gate {
            release: Mutex::new(wait),
            entered,
            finished,
        }));
        let cancellation = ExecutionCancellationToken::default();
        let result = process_http_request_with_timeout(
            &mut core,
            request,
            Duration::from_millis(10),
            &host,
            &inbound,
            &authority,
            &cancellation,
            None,
        )
        .unwrap();
        assert!(result.session_stale);
        assert!(cancellation.is_cancelled());
        assert_eq!(core.calls, 1);
        observed_entered
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        release.send(()).unwrap();
        observed_finished
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(core.calls, 1);
    }

    #[cfg(feature = "oauth")]
    #[test]
    fn dispatched_hosted_timeout_retains_operation_identity_and_terminal_evidence() {
        use crate::hosted_jobs::{HostedJobLedger, HostedJobState};
        use crate::mutation_scheduler::MutationSchedulerConfig;
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let mut core = CounterCore::new(&paths);
        let (release, wait) = mpsc::channel();
        let (entered, observed_entered) = mpsc::channel();
        let (finished, observed_finished) = mpsc::channel();
        core.gate = Some(Arc::new(Gate {
            release: Mutex::new(wait),
            entered,
            finished,
        }));
        let host = host(&paths);
        let inbound = inbound();
        let authority = host.authenticate(&inbound.headers).unwrap();
        let request = serde_json::json!({"id":9,"method":"tools/call","params":{"name":"custom-write","arguments":{}}});
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let scheduler =
            Arc::new(MutationScheduler::new(MutationSchedulerConfig::default()).unwrap());
        let ledger = Arc::new(HostedJobLedger::at(temporary.path().join("operations")));
        let executor = Arc::new(HostedExecutor::new(
            Arc::clone(&scheduler),
            Arc::clone(&ledger),
        ));
        let hosted = HostedMcpExecution {
            scheduler,
            executor,
            runtime: runtime.handle().clone(),
        };
        let worker = std::thread::spawn(move || {
            let result = process_http_request_with_timeout(
                &mut core,
                request,
                Duration::from_secs(2),
                &host,
                &inbound,
                &authority,
                &ExecutionCancellationToken::default(),
                Some(&hosted),
            );
            (core, result)
        });
        observed_entered
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let (core, result) = worker.join().unwrap();
        let result = result.unwrap();
        assert!(result.session_stale);
        assert_eq!(core.calls, 0);
        let response = result.response.unwrap();
        let operation = response
            .pointer("/result/structuredContent/operation_id")
            .and_then(Value::as_str)
            .unwrap();
        assert_eq!(
            response
                .pointer("/result/structuredContent/status_path")
                .unwrap(),
            &format!("/mcp/operations/{operation}")
        );
        let running = ledger.load(operation).unwrap();
        assert!(running.dispatched);
        assert_ne!(running.state, HostedJobState::Succeeded);
        release.send(()).unwrap();
        observed_finished
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        // Completion is persisted by the same executor even though the response worker's receiver has gone away.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let record = ledger.load(operation).unwrap();
            if record.state == HostedJobState::Succeeded {
                assert_eq!(record.committed, Some(true));
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
}
