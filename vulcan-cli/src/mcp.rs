#![allow(clippy::needless_pass_by_value, clippy::struct_excessive_bools)]

mod catalog;

use crate::{
    cli_command_tree, collect_help_command_topics, custom_tool_registry_entry,
    permission_error_to_cli, resolve_help_topic, CliError, McpToolPackArg, McpToolPackModeArg,
    McpToolsReport, McpTransportArg, ToolRegistryEntry,
};
use catalog::{
    default_openai_tool_packs, is_default_tool_pack_args, mcp_tool_registry_entry, pack_name_list,
    resolve_selected_tool_packs, visible_tool_catalog, McpToolPack, McpToolPackMode,
};
use fs2::FileExt;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
#[cfg(feature = "oauth")]
use std::io::Write;
use std::io::{self, BufRead};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
#[cfg(feature = "oauth")]
use std::sync::Mutex;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;
#[cfg(feature = "oauth")]
use std::time::Instant;
#[cfg(feature = "oauth")]
use std::time::{SystemTime, UNIX_EPOCH};
use ulid::Ulid;
use vulcan_app::execution::ExecutionCancellationToken;
#[cfg(feature = "oauth")]
use vulcan_app::execution::{
    ExecutionAuthority, ExecutionContext, ExecutionDeadline, ExecutionIdentity,
    ExecutionRetryClass, ExecutionVaultIdentity,
};
use vulcan_app::mcp_assistant;
use vulcan_app::mcp_assistant::{prompt_files_fingerprint, resource_files_fingerprint};
#[cfg(feature = "oauth")]
use vulcan_app::mcp_dispatch::request_is_read_only;
#[cfg(feature = "oauth")]
use vulcan_app::mcp_dispatch::tool_error_response;
use vulcan_app::mcp_dispatch::{
    acquire_request_ordinary_write_gate, dispatch_protocol_method, jsonrpc_error,
    process_http_request, process_stdio_request, request_id, timeout_http_result,
    timeout_response_for_request, McpHttpProcessResult, McpMethodHandler, McpProtocolMethods,
};
use vulcan_app::mcp_protocol;
use vulcan_app::mcp_protocol::{
    McpCompletionParams, McpListSnapshot, McpMethodError, McpMethodOutcome, McpToolResourceStore,
    MCP_PROTOCOL_VERSION,
};
use vulcan_app::tools::{self as app_tools, CustomToolDescriptor};
#[cfg(all(test, feature = "oauth"))]
use vulcan_core::pkce_s256_challenge;
#[cfg(all(test, feature = "oauth"))]
use vulcan_core::ClientIdMetadataDocument;
#[cfg(feature = "oauth")]
use vulcan_core::LocalOAuthUserConfig;
#[cfg(all(test, feature = "oauth"))]
use vulcan_core::PermissionGuard;
#[cfg(feature = "oauth")]
use vulcan_core::{
    discover_indieauth_endpoints, LocalOAuthIssuer, LocalOAuthIssuerConfig, OAuthResourceServer,
    OAuthResourceServerConfig,
};
use vulcan_core::{
    resolve_permission_profile, watch_vault, ProfilePermissionGuard, VaultPaths, WatchOptions,
};
#[cfg(feature = "oauth")]
use vulcan_daemon::host::{
    RestartPolicy, ServiceDefinition, ServiceId, ServiceRegistration, ServiceScope,
};
#[cfg(feature = "oauth")]
use vulcan_daemon::hosted_executor::{
    HostedExecutionError, HostedExecutor, HostedOperationCompletion, HostedOperationFailure,
};
#[cfg(feature = "oauth")]
use vulcan_daemon::hosted_jobs::HostedJobLedger;
#[cfg(feature = "oauth")]
use vulcan_daemon::http_policy::mcp_oauth_redirect_uri_valid;
use vulcan_daemon::http_policy::mcp_origin_allowed;
use vulcan_daemon::mcp_http_codec::{
    parse_mcp_http_post, validate_mcp_protocol_version, validate_mcp_sse_accept,
    write_mcp_http_response, write_mcp_http_sse_event, write_mcp_http_sse_headers,
    write_mcp_http_sse_keepalive, McpHttpRequest, McpHttpResponse,
};
use vulcan_daemon::mcp_http_routes::{classify_mcp_http_route, McpHttpRoute};
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_authorize::subject_not_allowed_response as indieauth_subject_not_allowed_response;
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_oauth_authorize::{
    default_indieauth_exchange, IndieAuthExchange, McpAuthorizeEndpoint,
};
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_browser::{
    percent_encode, redirect_to_indieauth as local_oauth_redirect_to_indieauth,
    PendingConsent as LocalOAuthPendingConsent,
};
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_oauth_browser::{
    IndieAuthConfig as LocalOAuthIndieAuthConfig, PendingConsentMap, PendingIndieAuthMap,
};
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_oauth_clients::OAuthClientRegistry;
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_clients::RegisteredOAuthClient as LocalOAuthRegisteredClient;
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_codes::McpAuthorizationCode as LocalOAuthCode;
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_oauth_codes::McpAuthorizationCodeMap;
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_oauth_consent::McpConsentEndpoint;
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_policy::parse_mcp_oauth_scopes as parse_mcp_oauth_scopes_policy;
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_policy::McpOAuthPolicyError;
use vulcan_daemon::mcp_oauth_policy::DEFAULT_MCP_OAUTH_SCOPES;
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_policy::{McpTokenAuthMethod, McpTokenClientCredentials};
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_oauth_routes::{McpLocalOAuthRoutes, McpOAuthRoutes};
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_oauth_token::McpLocalTokenEndpoint;
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_remote::McpRemoteAuthentication;
use vulcan_daemon::mcp_remote::McpRemoteDefinition;
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_remote_runtime::{NamedMcpRuntime, NamedMcpVaultRuntime, NamedTokenRequest};
#[cfg(test)]
use vulcan_daemon::mcp_session::MAX_MCP_SSE_PENDING_EVENTS;
use vulcan_daemon::mcp_session::{
    mcp_notification_scope, mcp_request_key, McpHttpSession as HostedMcpHttpSession,
    McpSessionAuthority, McpSessionRegistry, SessionAdmissionError, SessionLookupError,
};
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_session::{MAX_MCP_HTTP_SESSIONS, MCP_HTTP_SESSION_IDLE_TIMEOUT};
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_state::McpAuthorizationStore;
#[cfg(feature = "oauth")]
use vulcan_daemon::mutation_scheduler::MutationSchedulerConfig;
#[cfg(feature = "oauth")]
use vulcan_daemon::mutation_scheduler::{
    MutationScheduleError, MutationScheduler, ScheduledOperation,
};
use vulcan_daemon::process::DaemonProcessContext;
use vulcan_daemon::shutdown::ShutdownSignal;

const MCP_HTTP_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const MCP_HTTP_POLL_INTERVAL: Duration = Duration::from_millis(250);
pub(crate) const DEFAULT_MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MCP_REQUEST_WORKER_STACK_SIZE: usize = 16 * 1024 * 1024;

#[derive(Default)]
struct McpHttpLifecycle<'a> {
    stop: Option<&'a ShutdownSignal>,
    ready: Option<&'a dyn Fn(SocketAddr) -> Result<(), CliError>>,
    #[cfg(feature = "oauth")]
    hosted: Option<HostedMcpExecution>,
    #[cfg(all(test, feature = "oauth"))]
    indieauth_exchange: Option<IndieAuthExchange>,
}
#[derive(Debug, Clone)]
pub(crate) struct McpHttpOptions {
    pub bind: String,
    pub endpoint: String,
    pub auth_token: Option<String>,
    #[cfg_attr(not(feature = "oauth"), allow(dead_code))]
    pub public_url: Option<String>,
    pub oauth_issuer: Option<String>,
    pub oauth_audience: Vec<String>,
    pub oauth_jwks_url: Option<String>,
    pub oauth_allowed_sub: Vec<String>,
    pub oauth_allowed_email: Vec<String>,
    pub oauth_local_client_id: Option<String>,
    #[cfg_attr(not(feature = "oauth"), allow(dead_code))]
    pub oauth_local_redirect_uri: Vec<String>,
    pub oauth_local_client_secret: Option<String>,
    pub oauth_local_approval_token: Option<String>,
    pub oauth_local_subject: Option<String>,
    pub oauth_local_email: Option<String>,
    pub oauth_dcr: bool,
    pub oauth_dcr_allowed_redirect_host: Vec<String>,
    pub oauth_indieauth_authorization_endpoint: Option<String>,
    pub oauth_indieauth_token_endpoint: Option<String>,
    pub oauth_indieauth_client_id: Option<String>,
    pub oauth_indieauth_redirect_uri: Option<String>,
    pub oauth_indieauth_me: Option<String>,
    pub oauth_local_user: Vec<String>,
    pub instance_id: Option<Ulid>,
    #[cfg_attr(not(feature = "oauth"), allow(dead_code))]
    pub oauth_storage_dir: Option<PathBuf>,
    pub request_timeout: Duration,
}

#[derive(Debug, Clone)]
struct McpServerCore {
    paths: VaultPaths,
    selection: vulcan_core::ResolvedPermissionProfile,
    guard: ProfilePermissionGuard,
    tool_pack_mode: McpToolPackMode,
    pinned_tool_packs: BTreeSet<McpToolPack>,
    selected_tool_packs: BTreeSet<McpToolPack>,
    tool_resources: McpToolResourceStore,
    snapshot: McpListSnapshot,
}

type McpHttpSession = HostedMcpHttpSession<McpServerCore>;
#[cfg(feature = "oauth")]
#[derive(Debug, Clone)]
enum McpOAuthMode {
    External(Arc<OAuthResourceServer>),
    Local(Arc<LocalOAuthIssuer>),
}

#[cfg(feature = "oauth")]
impl McpOAuthMode {
    fn public_url(&self) -> &str {
        match self {
            Self::External(server) => server.public_url(),
            Self::Local(issuer) => issuer.public_url(),
        }
    }
}

#[cfg(feature = "oauth")]
#[derive(Debug, Clone)]
struct HostedMcpExecution {
    scheduler: Arc<MutationScheduler>,
    executor: Arc<HostedExecutor>,
    runtime: tokio::runtime::Handle,
}

#[cfg(feature = "oauth")]
#[derive(Debug, Clone)]
struct ResidentMcpScheduling {
    scheduler: Arc<MutationScheduler>,
    runtime: tokio::runtime::Handle,
}

#[cfg(feature = "oauth")]
struct HostedMcpDispatch {
    http: McpHttpServerContext,
    inbound: McpHttpRequest,
    authority: McpSessionAuthority,
    execution: ExecutionContext,
}

#[cfg(feature = "oauth")]
impl HostedMcpExecution {
    fn prepare(
        &self,
        core: &McpServerCore,
        payload: &Value,
        authority: &McpSessionAuthority,
        cancellation: ExecutionCancellationToken,
        deadline: ExecutionDeadline,
    ) -> Result<ExecutionContext, Value> {
        let failure = |message: String| {
            jsonrpc_error(
                request_id(payload).unwrap_or(Value::Null),
                -32603,
                message,
                None,
            )
        };
        let kind = mcp_scheduled_operation(payload);
        let grant = core.selection.grant.clone();
        let principal_id = authority
            .subject
            .clone()
            .or_else(|| authority.client_id.clone())
            .unwrap_or_else(|| format!("mcp:{}", authority.remote_instance_id));
        let execution = ExecutionContext::new(
            ExecutionVaultIdentity::resolve(core.paths.vault_root(), None, None)
                .map_err(|error| failure(error.to_string()))?,
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
        .map_err(|error| failure(error.to_string()))?;
        if kind == ScheduledOperation::Mutation {
            self.runtime
                .block_on(self.executor.register(&execution))
                .map_err(|error| failure(error.to_string()))?;
        }
        Ok(execution)
    }

    fn execute(
        &self,
        core: &mut McpServerCore,
        payload: &Value,
        dispatch: &HostedMcpDispatch,
    ) -> Result<McpHttpProcessResult, Value> {
        let kind = mcp_scheduled_operation(payload);
        if kind == ScheduledOperation::Read {
            let permit = self
                .runtime
                .block_on(self.scheduler.acquire(&dispatch.execution, kind, |_| {
                    revalidate_hosted_mcp_authority(
                        &dispatch.http,
                        &dispatch.inbound,
                        &dispatch.authority,
                    )
                }))
                .map_err(|error| hosted_mcp_json_error(payload, error.to_string(), None))?;
            dispatch
                .execution
                .checkpoint()
                .map_err(|error| hosted_mcp_json_error(payload, error.to_string(), None))?;
            attenuate_mcp_core_profile(core)
                .map_err(|error| hosted_mcp_json_error(payload, error, None))?;
            let response = core.process_http_request(payload);
            drop(permit);
            return response;
        }
        let http = dispatch.http.clone();
        let inbound = dispatch.inbound.clone();
        let authority = dispatch.authority.clone();
        let mut next_core = core.clone();
        let request = payload.clone();
        let result = self
            .runtime
            .block_on(self.executor.execute_registered_caller(
                dispatch.execution.clone(),
                kind,
                move |_| revalidate_hosted_mcp_authority(&http, &inbound, &authority),
                move |execution| {
                    execution.checkpoint().map_err(|error| {
                        HostedOperationFailure::before_commit(error.to_string())
                    })?;
                    attenuate_mcp_core_profile(&mut next_core)
                        .map_err(HostedOperationFailure::before_commit)?;
                    let response = next_core.process_http_request(&request);
                    if kind == ScheduledOperation::Mutation
                        && (response.is_err()
                            || response.as_ref().is_ok_and(|result| {
                                result.response.as_ref().is_some_and(|value| {
                                    value.get("error").is_some()
                                        || value.pointer("/result/isError").and_then(Value::as_bool)
                                            == Some(true)
                                })
                            }))
                    {
                        return Err(HostedOperationFailure::indeterminate(
                            "MCP mutation returned an error; its write outcome is unverified",
                        ));
                    }
                    Ok(HostedOperationCompletion {
                        value: (next_core, response),
                        committed: kind == ScheduledOperation::Mutation,
                    })
                },
            ));
        match result {
            Ok((next, response)) => {
                *core = next;
                response
            }
            Err(error) => hosted_mcp_execution_error(
                payload,
                &dispatch.execution,
                &dispatch.http.endpoint,
                error,
            ),
        }
    }
}

#[cfg(feature = "oauth")]
fn revalidate_hosted_mcp_authority(
    http: &McpHttpServerContext,
    inbound: &McpHttpRequest,
    authority: &McpSessionAuthority,
) -> Result<(), MutationScheduleError> {
    let current = authenticate_mcp_http_request(http, inbound).map_err(|_| {
        MutationScheduleError::Revalidation("MCP authority is no longer valid".to_string())
    })?;
    if !current.matches(authority) {
        return Err(MutationScheduleError::Revalidation(
            "MCP authority changed while queued".to_string(),
        ));
    }
    Ok(())
}

#[cfg(feature = "oauth")]
fn hosted_mcp_json_error(payload: &Value, message: String, operation_id: Option<&str>) -> Value {
    jsonrpc_error(
        request_id(payload).unwrap_or(Value::Null),
        -32603,
        message,
        operation_id.map(|id| serde_json::json!({ "operation_id": id })),
    )
}

#[cfg(feature = "oauth")]
fn hosted_mcp_unknown_result(
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

#[cfg(feature = "oauth")]
fn hosted_mcp_execution_error(
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

#[cfg(feature = "oauth")]
fn attenuate_mcp_core_profile(core: &mut McpServerCore) -> Result<(), String> {
    let current_profile = resolve_permission_profile(&core.paths, Some(&core.selection.name))
        .map_err(|error| error.to_string())?;
    if !current_profile.grant.is_subset_of(&core.selection.grant) {
        return Err("MCP permission profile widened after session initialization".to_string());
    }
    core.guard = ProfilePermissionGuard::new(&core.paths, current_profile.clone());
    core.selection = current_profile;
    Ok(())
}

#[cfg(feature = "oauth")]
fn mcp_scheduled_operation(payload: &Value) -> ScheduledOperation {
    if request_is_read_only(payload) {
        ScheduledOperation::Read
    } else {
        ScheduledOperation::Mutation
    }
}

#[derive(Debug, Clone)]
struct McpHttpServerContext {
    paths: VaultPaths,
    requested_profile: Option<String>,
    tool_pack_args: Vec<McpToolPackArg>,
    tool_pack_mode_arg: McpToolPackModeArg,
    endpoint: String,
    auth_token: Option<String>,
    #[cfg(feature = "oauth")]
    oauth: Option<McpOAuthMode>,
    #[cfg(feature = "oauth")]
    hosted: Option<HostedMcpExecution>,
    bind_addr: SocketAddr,
    instance_id: Ulid,
    sessions: Arc<McpSessionRegistry<McpServerCore>>,
    #[cfg(feature = "oauth")]
    oauth_codes: Arc<McpAuthorizationCodeMap>,
    #[cfg(feature = "oauth")]
    oauth_clients: Arc<OAuthClientRegistry>,
    #[cfg(feature = "oauth")]
    oauth_pending_indieauth: Arc<PendingIndieAuthMap>,
    #[cfg(feature = "oauth")]
    oauth_pending_consent: Arc<PendingConsentMap>,
    #[cfg(feature = "oauth")]
    oauth_dcr_enabled: bool,
    #[cfg(feature = "oauth")]
    oauth_dcr_allowed_redirect_hosts: Vec<String>,
    #[cfg(feature = "oauth")]
    oauth_local_redirect_uris: Vec<String>,
    #[cfg(feature = "oauth")]
    oauth_indieauth: Option<LocalOAuthIndieAuthConfig>,
    #[cfg(all(test, feature = "oauth"))]
    indieauth_exchange: Option<IndieAuthExchange>,
    #[cfg(feature = "oauth")]
    named_runtime: Option<NamedMcpRuntime>,
    request_timeout: Duration,
}

pub(crate) fn build_mcp_tool_definitions(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    tool_pack_args: &[McpToolPackArg],
    tool_pack_mode_arg: McpToolPackModeArg,
) -> Result<McpToolsReport, CliError> {
    let tool_pack_mode = McpToolPackMode::from(tool_pack_mode_arg);
    let selected_tool_packs = resolve_selected_tool_packs(tool_pack_args, tool_pack_mode);
    let tools = build_mcp_tool_registry_entries(paths, requested_profile, &selected_tool_packs)?
        .into_iter()
        .map(|tool| tool.to_mcp_definition())
        .collect::<Vec<_>>();

    Ok(McpToolsReport {
        protocol_version: MCP_PROTOCOL_VERSION.to_string(),
        tool_pack_mode: tool_pack_mode.as_str().to_string(),
        selected_tool_packs: pack_name_list(&selected_tool_packs),
        tools,
    })
}

pub(crate) fn build_openai_tool_registry_entries(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    tool_pack_args: &[McpToolPackArg],
    tool_pack_mode_arg: McpToolPackModeArg,
) -> Result<Vec<ToolRegistryEntry>, CliError> {
    let tool_pack_mode = McpToolPackMode::from(tool_pack_mode_arg);
    let selected_tool_packs =
        if tool_pack_args.is_empty() || is_default_tool_pack_args(tool_pack_args) {
            default_openai_tool_packs()
        } else {
            resolve_selected_tool_packs(tool_pack_args, tool_pack_mode)
        };
    build_mcp_tool_registry_entries(paths, requested_profile, &selected_tool_packs)
}

pub(crate) fn run_mcp(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    tool_pack_args: &[McpToolPackArg],
    tool_pack_mode_arg: McpToolPackModeArg,
    transport_arg: McpTransportArg,
    http_options: &McpHttpOptions,
) -> Result<(), CliError> {
    match transport_arg {
        McpTransportArg::Stdio => run_mcp_stdio_server(
            paths,
            requested_profile,
            tool_pack_args,
            tool_pack_mode_arg,
            http_options.request_timeout,
        ),
        McpTransportArg::Http => run_mcp_http_server(
            paths,
            requested_profile,
            tool_pack_args,
            tool_pack_mode_arg,
            http_options,
        ),
    }
}

#[cfg(feature = "oauth")]
pub(crate) fn run_named_mcp_remote(
    process: &DaemonProcessContext,
    remote: &McpRemoteDefinition,
) -> Result<(), CliError> {
    run_named_mcp_remote_inner(process, remote, None, None, None)
}

#[cfg(feature = "oauth")]
#[allow(clippy::too_many_lines)] // Keeps instance ownership, executor recovery, and listener startup together.
fn run_named_mcp_remote_inner(
    process: &DaemonProcessContext,
    remote: &McpRemoteDefinition,
    stop: Option<&ShutdownSignal>,
    ready: Option<&dyn Fn(SocketAddr) -> Result<(), CliError>>,
    resident: Option<ResidentMcpScheduling>,
) -> Result<(), CliError> {
    run_named_mcp_remote_with_endpoints(process, remote, stop, ready, resident, None, None, None)
}

#[cfg(feature = "oauth")]
#[allow(clippy::too_many_lines)] // Keeps the validated named definition and shared listener startup together.
#[allow(clippy::too_many_arguments)] // Test-only IndieAuth exchange seam extends the existing startup inputs.
fn run_named_mcp_remote_with_endpoints(
    process: &DaemonProcessContext,
    remote: &McpRemoteDefinition,
    stop: Option<&ShutdownSignal>,
    ready: Option<&dyn Fn(SocketAddr) -> Result<(), CliError>>,
    resident: Option<ResidentMcpScheduling>,
    indieauth_endpoints: Option<&(String, String)>,
    foreground_scheduler: Option<Arc<MutationScheduler>>,
    indieauth_exchange: Option<IndieAuthExchange>,
) -> Result<(), CliError> {
    #[cfg(not(test))]
    let _ = indieauth_exchange;
    let mut vaults = BTreeMap::new();
    for vault in &remote.vaults {
        let registration = process
            .registry
            .show(&vault.wiki_id)
            .map_err(CliError::operation)?
            .registration;
        mcp_tool_pack_args_from_names(&vault.tool_packs)?;
        vaults.insert(
            vault.wiki_id.clone(),
            NamedMcpVaultRuntime {
                paths: VaultPaths::new(registration.path),
                ceiling_profile: vault.ceiling_profile.clone(),
                default_profile: vault.default_profile.clone(),
                eligible_tool_packs: vault.tool_packs.clone(),
            },
        );
    }
    let first = vaults.values().next().ok_or_else(|| {
        CliError::operation("named MCP remote must expose at least one registered vault")
    })?;
    let paths = first.paths.clone();
    let default_profile = first.default_profile.clone();
    let tool_packs = mcp_tool_pack_args_from_names(&first.eligible_tool_packs)?;
    let McpRemoteAuthentication::IndieAuth { identity } = &remote.authentication;
    let endpoint = public_url_path(&remote.public_url)?;
    let storage_dir = process
        .state_root
        .join("mcp-remotes")
        .join(remote.id.as_str());
    let _runtime_lock = acquire_named_remote_runtime_lock(&storage_dir, remote)?;
    let current = process
        .registry
        .show_mcp_remote(&remote.id)
        .map_err(CliError::operation)?;
    if current != *remote {
        return Err(CliError::operation(format!(
            "named MCP remote `{}` changed while starting; retry with the current definition",
            remote.id
        )));
    }
    for vault in vaults.values() {
        if vault.paths.vulcan_dir().exists()
            && vulcan_core::ordinary_write::recover_ordinary_write_batch(&vault.paths)
                .map_err(CliError::operation)?
                .is_some()
        {
            vulcan_core::scan_vault(&vault.paths, vulcan_core::ScanMode::Incremental)
                .map_err(CliError::operation)?;
        }
    }
    let foreground_runtime = if resident.is_none() {
        Some(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(CliError::operation)?,
        )
    } else {
        None
    };
    let scheduler = if let Some(resident) = resident.as_ref() {
        Arc::clone(&resident.scheduler)
    } else if let Some(scheduler) = foreground_scheduler {
        scheduler
    } else {
        Arc::new(
            MutationScheduler::new(MutationSchedulerConfig::default())
                .map_err(CliError::operation)?,
        )
    };
    let runtime = resident.map_or_else(
        || {
            foreground_runtime
                .as_ref()
                .expect("foreground runtime exists")
                .handle()
                .clone()
        },
        |resident| resident.runtime,
    );
    let ledger = Arc::new(HostedJobLedger::at(storage_dir.join("operations")));
    ledger
        .recover_interrupted(current_unix_millis())
        .map_err(CliError::operation)?;
    let hosted = HostedMcpExecution {
        executor: Arc::new(HostedExecutor::new(Arc::clone(&scheduler), ledger)),
        scheduler,
        runtime,
    };
    let options = McpHttpOptions {
        bind: remote.bind.clone(),
        endpoint,
        auth_token: None,
        public_url: Some(remote.public_url.clone()),
        oauth_issuer: None,
        oauth_audience: Vec::new(),
        oauth_jwks_url: None,
        oauth_allowed_sub: Vec::new(),
        oauth_allowed_email: Vec::new(),
        oauth_local_client_id: None,
        oauth_local_redirect_uri: Vec::new(),
        oauth_local_client_secret: None,
        oauth_local_approval_token: None,
        oauth_local_subject: None,
        oauth_local_email: None,
        oauth_dcr: true,
        oauth_dcr_allowed_redirect_host: vec!["chatgpt.com".to_string()],
        oauth_indieauth_authorization_endpoint: indieauth_endpoints.map(|item| item.0.clone()),
        oauth_indieauth_token_endpoint: indieauth_endpoints.map(|item| item.1.clone()),
        oauth_indieauth_client_id: None,
        oauth_indieauth_redirect_uri: None,
        oauth_indieauth_me: Some(identity.clone()),
        oauth_local_user: Vec::new(),
        instance_id: Some(remote.instance_id),
        oauth_storage_dir: Some(storage_dir),
        request_timeout: DEFAULT_MCP_REQUEST_TIMEOUT,
    };
    run_mcp_http_server_with_named_runtime(
        &paths,
        Some(&default_profile),
        &tool_packs,
        McpToolPackModeArg::Static,
        &options,
        NamedMcpRuntime {
            remote_id: remote.id.clone(),
            vaults,
            authorization_store: McpAuthorizationStore::at(&process.state_root),
        },
        McpHttpLifecycle {
            stop,
            ready,
            hosted: Some(hosted),
            #[cfg(test)]
            indieauth_exchange,
        },
    )
}

#[cfg(feature = "oauth")]
#[derive(Debug)]
enum ResidentMcpEvent {
    Ready(String),
    Exited(String, Result<(), String>),
}

#[cfg(feature = "oauth")]
#[allow(clippy::too_many_lines)] // Wires each named listener into one aggregate supervised service.
pub(crate) fn resident_named_mcp_service(
    process: &DaemonProcessContext,
    remotes: &[McpRemoteDefinition],
    scheduler: Arc<MutationScheduler>,
    runtime: tokio::runtime::Handle,
) -> Result<Option<ServiceRegistration>, CliError> {
    resident_named_mcp_service_with_endpoints(process, remotes, scheduler, runtime, None, None)
}

#[cfg(feature = "oauth")]
#[allow(clippy::too_many_lines)] // Wires each named listener into one aggregate supervised service.
fn resident_named_mcp_service_with_endpoints(
    process: &DaemonProcessContext,
    remotes: &[McpRemoteDefinition],
    scheduler: Arc<MutationScheduler>,
    runtime: tokio::runtime::Handle,
    indieauth_endpoints: Option<(String, String)>,
    indieauth_exchange: Option<IndieAuthExchange>,
) -> Result<Option<ServiceRegistration>, CliError> {
    if remotes.is_empty() {
        return Ok(None);
    }
    let definition = ServiceDefinition {
        id: ServiceId::parse("listener.mcp-remotes").map_err(CliError::operation)?,
        service_kind: "listener".to_string(),
        scope: ServiceScope::Global,
        enabled: true,
        required: true,
        dependencies: vec![ServiceId::parse("worker.sync-trigger").map_err(CliError::operation)?],
        restart: RestartPolicy::Never,
    };
    let process = process.clone();
    let remotes = remotes.to_vec();
    Ok(Some(ServiceRegistration::new(definition, move |service| {
        let (sender, receiver) = mpsc::channel::<ResidentMcpEvent>();
        let mut handles = Vec::with_capacity(remotes.len());
        for remote in remotes.clone() {
            let sender = sender.clone();
            let stop = Arc::clone(service.stop());
            let process = process.clone();
            let name = remote.id.to_string();
            let hosted = ResidentMcpScheduling {
                scheduler: Arc::clone(&scheduler),
                runtime: runtime.clone(),
            };
            let indieauth_endpoints = indieauth_endpoints.clone();
            let handle = match thread::Builder::new()
                .name(format!("mcp-remote-{name}"))
                .spawn(move || {
                    let on_ready = |_address| {
                        sender
                            .send(ResidentMcpEvent::Ready(name.clone()))
                            .map_err(CliError::operation)
                    };
                    report_resident_mcp_listener_exit(&sender, name.clone(), || {
                        run_named_mcp_remote_with_endpoints(
                            &process,
                            &remote,
                            Some(&stop),
                            Some(&on_ready),
                            Some(hosted),
                            indieauth_endpoints.as_ref(),
                            None,
                            indieauth_exchange,
                        )
                    });
                }) {
                Ok(handle) => handle,
                Err(error) => {
                    return Err(finish_failed_resident_mcp_startup(
                        service.stop(),
                        handles,
                        format!("failed to spawn named MCP remote: {error}"),
                    ));
                }
            };
            handles.push(handle);
        }
        drop(sender);
        let expected = remotes.iter().map(|remote| remote.id.to_string()).collect();
        if let Err(error) = await_resident_mcp_readiness(
            &receiver,
            &expected,
            Instant::now() + Duration::from_secs(8),
        ) {
            return Err(finish_failed_resident_mcp_startup(
                service.stop(),
                handles,
                error,
            ));
        }
        if let Err(error) = service.ready() {
            return Err(finish_failed_resident_mcp_startup(
                service.stop(),
                handles,
                error,
            ));
        }
        loop {
            if service.stop().wait_timeout(Duration::from_millis(50)) {
                break;
            }
            match receiver.try_recv() {
                Ok(ResidentMcpEvent::Exited(name, result)) => {
                    return Err(finish_failed_resident_mcp_startup(
                        service.stop(),
                        handles,
                        format!(
                            "named MCP remote `{name}` stopped unexpectedly: {}",
                            result
                                .err()
                                .unwrap_or_else(|| "listener exited".to_string())
                        ),
                    ));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(finish_failed_resident_mcp_startup(
                        service.stop(),
                        handles,
                        "named MCP listeners disconnected unexpectedly".to_string(),
                    ));
                }
                Ok(ResidentMcpEvent::Ready(_)) | Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        join_resident_mcp_threads(handles)
    })))
}

#[cfg(feature = "oauth")]
fn report_resident_mcp_listener_exit<F>(
    sender: &mpsc::Sender<ResidentMcpEvent>,
    name: String,
    run: F,
) where
    F: FnOnce() -> Result<(), CliError>,
{
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
        .map_err(|_| "listener panicked".to_string())
        .and_then(|result| result.map_err(|error| error.message));
    let _ = sender.send(ResidentMcpEvent::Exited(name, result));
}

#[cfg(feature = "oauth")]
fn await_resident_mcp_readiness(
    receiver: &mpsc::Receiver<ResidentMcpEvent>,
    expected: &BTreeSet<String>,
    deadline: Instant,
) -> Result<(), String> {
    let mut pending = expected.clone();
    while !pending.is_empty() {
        match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(ResidentMcpEvent::Ready(name)) => {
                if !expected.contains(&name) {
                    return Err(format!(
                        "unexpected named MCP remote `{name}` reported readiness"
                    ));
                }
                pending.remove(&name);
            }
            Ok(ResidentMcpEvent::Exited(name, result)) => {
                return Err(format!(
                    "named MCP remote `{name}` stopped during startup: {}",
                    result
                        .err()
                        .unwrap_or_else(|| "listener exited".to_string())
                ));
            }
            Err(error) => return Err(format!("named MCP listener startup timed out: {error}")),
        }
    }
    loop {
        match receiver.try_recv() {
            Ok(ResidentMcpEvent::Exited(name, result)) => {
                return Err(format!(
                    "named MCP remote `{name}` stopped during startup: {}",
                    result
                        .err()
                        .unwrap_or_else(|| "listener exited".to_string())
                ));
            }
            Ok(ResidentMcpEvent::Ready(_)) => {}
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err("named MCP listeners disconnected during startup".to_string());
            }
        }
    }
}

#[cfg(feature = "oauth")]
fn finish_failed_resident_mcp_startup(
    stop: &ShutdownSignal,
    handles: Vec<thread::JoinHandle<()>>,
    error: String,
) -> String {
    stop.cancel();
    match join_resident_mcp_threads(handles) {
        Ok(()) => error,
        Err(join_error) => format!("{error}; {join_error}"),
    }
}

#[cfg(feature = "oauth")]
fn join_resident_mcp_threads(handles: Vec<thread::JoinHandle<()>>) -> Result<(), String> {
    let mut first_error = None;
    for handle in handles {
        if handle.join().is_err() && first_error.is_none() {
            first_error = Some("named MCP listener thread panicked".to_string());
        }
    }
    first_error.map_or(Ok(()), Err)
}

pub(crate) fn acquire_named_remote_runtime_lock(
    storage_dir: &Path,
    remote: &McpRemoteDefinition,
) -> Result<fs::File, CliError> {
    fs::create_dir_all(storage_dir).map_err(CliError::operation)?;
    let path = storage_dir.join("runtime.lock");
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).map_err(CliError::operation)?;
    file.try_lock_exclusive().map_err(|error| {
        CliError::operation(format!(
            "named MCP remote `{}` is already running or its runtime lock at {} is unavailable: {error}",
            remote.id,
            path.display()
        ))
    })?;
    Ok(file)
}

fn mcp_tool_pack_args_from_names(names: &[String]) -> Result<Vec<McpToolPackArg>, CliError> {
    names
        .iter()
        .map(|name| match name.as_str() {
            "notes-read" => Ok(McpToolPackArg::NotesRead),
            "search" => Ok(McpToolPackArg::Search),
            "status" => Ok(McpToolPackArg::Status),
            "graph" => Ok(McpToolPackArg::Graph),
            "custom" => Ok(McpToolPackArg::Custom),
            "daily" => Ok(McpToolPackArg::Daily),
            "tasks" => Ok(McpToolPackArg::Tasks),
            "notes-write" => Ok(McpToolPackArg::NotesWrite),
            "notes-manage" => Ok(McpToolPackArg::NotesManage),
            "web" => Ok(McpToolPackArg::Web),
            "config" => Ok(McpToolPackArg::Config),
            "index" => Ok(McpToolPackArg::Index),
            "sync" => Ok(McpToolPackArg::Sync),
            _ => Err(CliError::operation(format!(
                "named MCP remote contains unknown tool pack `{name}`"
            ))),
        })
        .collect()
}

#[cfg(not(feature = "oauth"))]
pub(crate) fn run_named_mcp_remote(
    _process: &DaemonProcessContext,
    _remote: &McpRemoteDefinition,
) -> Result<(), CliError> {
    Err(CliError::operation(
        "named MCP remotes require a build with the `oauth` feature enabled",
    ))
}

#[cfg(feature = "oauth")]
fn public_url_path(public_url: &str) -> Result<String, CliError> {
    let (_, rest) = public_url
        .split_once("://")
        .ok_or_else(|| CliError::operation("named MCP public URL must be absolute"))?;
    let path = rest.find('/').map_or("/mcp", |index| &rest[index..]);
    if path.contains(['?', '#']) {
        return Err(CliError::operation(
            "named MCP public URL must not contain a query or fragment",
        ));
    }
    Ok(normalize_mcp_http_endpoint(path))
}

#[cfg(feature = "oauth")]
fn unix_timestamp_for_mcp() -> Result<u64, CliError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(CliError::operation)
}

fn run_mcp_stdio_server(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    tool_pack_args: &[McpToolPackArg],
    tool_pack_mode_arg: McpToolPackModeArg,
    request_timeout: Duration,
) -> Result<(), CliError> {
    let mut server =
        McpServerCore::new(paths, requested_profile, tool_pack_args, tool_pack_mode_arg)?;
    let stdin = io::stdin();

    for line in stdin.lock().lines() {
        let line = line.map_err(CliError::operation)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request = match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => value,
            Err(error) => {
                let response =
                    jsonrpc_error(Value::Null, -32700, format!("Parse error: {error}"), None);
                println!("{}", serde_json::to_string(&response).unwrap_or_default());
                continue;
            }
        };

        for message in server.process_request_with_timeout(request, request_timeout) {
            println!("{}", serde_json::to_string(&message).unwrap_or_default());
        }
    }

    Ok(())
}

fn run_mcp_http_server(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    tool_pack_args: &[McpToolPackArg],
    tool_pack_mode_arg: McpToolPackModeArg,
    options: &McpHttpOptions,
) -> Result<(), CliError> {
    run_mcp_http_server_inner(
        paths,
        requested_profile,
        tool_pack_args,
        tool_pack_mode_arg,
        options,
        #[cfg(feature = "oauth")]
        None,
        McpHttpLifecycle::default(),
    )
}

#[cfg(feature = "oauth")]
fn run_mcp_http_server_with_named_runtime(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    tool_pack_args: &[McpToolPackArg],
    tool_pack_mode_arg: McpToolPackModeArg,
    options: &McpHttpOptions,
    named_runtime: NamedMcpRuntime,
    lifecycle: McpHttpLifecycle<'_>,
) -> Result<(), CliError> {
    run_mcp_http_server_inner(
        paths,
        requested_profile,
        tool_pack_args,
        tool_pack_mode_arg,
        options,
        Some(named_runtime),
        lifecycle,
    )
}

#[allow(clippy::too_many_lines)] // Keeps listener setup and its lifecycle in one place.
fn run_mcp_http_server_inner(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    tool_pack_args: &[McpToolPackArg],
    tool_pack_mode_arg: McpToolPackModeArg,
    options: &McpHttpOptions,
    #[cfg(feature = "oauth")] named_runtime: Option<NamedMcpRuntime>,
    lifecycle: McpHttpLifecycle<'_>,
) -> Result<(), CliError> {
    #[cfg(feature = "oauth")]
    let oauth = build_mcp_oauth_validator(paths, requested_profile, options)?;
    #[cfg(not(feature = "oauth"))]
    reject_mcp_oauth_options_when_disabled(options)?;
    #[cfg(feature = "oauth")]
    let auth_enabled = options.auth_token.is_some() || oauth.is_some();
    #[cfg(not(feature = "oauth"))]
    let auth_enabled = options.auth_token.is_some();
    let bind_addr = parse_mcp_http_bind_addr(&options.bind, auth_enabled)?;
    let endpoint = normalize_mcp_http_endpoint(&options.endpoint);
    let listener = vulcan_daemon::mcp_transport::McpHttpListener::bind(bind_addr)
        .map_err(CliError::operation)?;
    let addr = listener.local_addr();
    eprintln!("MCP HTTP server listening on http://{addr}{endpoint}");
    if lifecycle.stop.is_none() {
        spawn_mcp_index_watcher(paths.clone(), WatchOptions::default());
        #[cfg(feature = "oauth")]
        if let Some(named) = named_runtime.as_ref() {
            for vault in named.vaults.values() {
                if vault.paths.vault_root() != paths.vault_root() {
                    spawn_mcp_index_watcher(vault.paths.clone(), WatchOptions::default());
                }
            }
        }
    }
    let context = McpHttpServerContext {
        paths: paths.clone(),
        requested_profile: requested_profile.map(ToOwned::to_owned),
        tool_pack_args: tool_pack_args.to_vec(),
        tool_pack_mode_arg,
        endpoint,
        auth_token: options.auth_token.clone(),
        #[cfg(feature = "oauth")]
        oauth,
        #[cfg(feature = "oauth")]
        hosted: lifecycle.hosted,
        bind_addr: addr,
        instance_id: match options.instance_id {
            Some(instance_id) => instance_id,
            None => Ulid::new(),
        },
        sessions: Arc::new(McpSessionRegistry::new()),
        #[cfg(feature = "oauth")]
        oauth_codes: Arc::new(McpAuthorizationCodeMap::default()),
        #[cfg(feature = "oauth")]
        oauth_clients: Arc::new(
            OAuthClientRegistry::at(oauth_clients_path(paths, options))
                .map_err(CliError::operation)?,
        ),
        #[cfg(feature = "oauth")]
        oauth_pending_indieauth: Arc::new(Mutex::new(BTreeMap::new())),
        #[cfg(feature = "oauth")]
        oauth_pending_consent: Arc::new(Mutex::new(BTreeMap::new())),
        #[cfg(feature = "oauth")]
        oauth_dcr_enabled: options.oauth_dcr,
        #[cfg(feature = "oauth")]
        oauth_dcr_allowed_redirect_hosts: if options.oauth_dcr_allowed_redirect_host.is_empty() {
            vec!["chatgpt.com".to_string()]
        } else {
            options.oauth_dcr_allowed_redirect_host.clone()
        },
        #[cfg(feature = "oauth")]
        oauth_local_redirect_uris: options.oauth_local_redirect_uri.clone(),
        #[cfg(feature = "oauth")]
        oauth_indieauth: build_indieauth_config(options)?,
        #[cfg(all(test, feature = "oauth"))]
        indieauth_exchange: lifecycle.indieauth_exchange,
        #[cfg(feature = "oauth")]
        named_runtime,
        request_timeout: options.request_timeout,
    };

    if let Some(ready) = lifecycle.ready {
        ready(addr)?;
    }
    let handler_context = context.clone();
    let result = listener.serve(lifecycle.stop, move |request, stream| {
        if let Err(error) = handle_mcp_http_connection(&handler_context, request, stream) {
            let response = mcp_http_json_error_response(500, error.to_string(), Value::Null);
            let _ = write_mcp_http_response(stream, &response);
        }
    });
    close_mcp_http_sessions(&context);
    result.map_err(CliError::operation)
}

fn close_mcp_http_sessions(context: &McpHttpServerContext) {
    context.sessions.close_all();
}

fn admit_mcp_http_session(
    context: &McpHttpServerContext,
    session_id: String,
    session: Arc<McpHttpSession>,
) -> Result<(), McpHttpResponse> {
    match context.sessions.admit(session_id, session) {
        Ok(()) => Ok(()),
        Err(SessionAdmissionError::Capacity) => {
            let mut response = mcp_http_json_error_response(
                503,
                "MCP session limit reached; close unused sessions or retry later",
                Value::Null,
            );
            response
                .extra_headers
                .push(("Retry-After".to_string(), "60".to_string()));
            Err(response)
        }
        Err(SessionAdmissionError::DuplicateId) => Err(mcp_http_json_error_response(
            500,
            "MCP session ID collision",
            Value::Null,
        )),
    }
}

#[cfg(all(test, feature = "oauth"))]
fn live_mcp_http_session(
    context: &McpHttpServerContext,
    session_id: &str,
) -> Option<Arc<McpHttpSession>> {
    context.sessions.live(session_id)
}

fn authorized_mcp_http_session(
    context: &McpHttpServerContext,
    session_id: &str,
    authority: &McpSessionAuthority,
    touch: bool,
) -> Result<Arc<McpHttpSession>, McpHttpResponse> {
    context
        .sessions
        .authorized(session_id, authority, touch)
        .map_err(|error| match error {
            SessionLookupError::Missing => {
                mcp_http_json_error_response(404, "unknown Mcp-Session-Id", Value::Null)
            }
            SessionLookupError::AuthorityMismatch => mcp_http_json_error_response(
                403,
                "MCP session authority does not match this request",
                Value::Null,
            ),
        })
}

fn spawn_mcp_index_watcher(paths: VaultPaths, options: WatchOptions) {
    thread::spawn(move || {
        if let Err(error) = watch_vault(&paths, &options, |report| -> Result<(), String> {
            if report.startup {
                eprintln!(
                    "MCP index watcher initialized: {} added, {} updated, {} unchanged, {} deleted",
                    report.summary.added,
                    report.summary.updated,
                    report.summary.unchanged,
                    report.summary.deleted
                );
            } else if report.summary.added + report.summary.updated + report.summary.deleted > 0 {
                eprintln!(
                    "MCP index watcher refreshed {} paths: {} added, {} updated, {} deleted",
                    report.paths.len(),
                    report.summary.added,
                    report.summary.updated,
                    report.summary.deleted
                );
            }
            Ok(())
        }) {
            eprintln!("MCP index watcher stopped: {error}");
        }
    });
}

fn handle_mcp_http_connection(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
    stream: &mut TcpStream,
) -> Result<(), CliError> {
    #[cfg(feature = "oauth")]
    let oauth_enabled = context.oauth.is_some();
    #[cfg(not(feature = "oauth"))]
    let oauth_enabled = false;
    #[cfg(feature = "oauth")]
    let local_oauth = matches!(context.oauth, Some(McpOAuthMode::Local(_)));
    #[cfg(not(feature = "oauth"))]
    let local_oauth = false;
    #[cfg(feature = "oauth")]
    let named_remote = context.named_runtime.is_some();
    #[cfg(not(feature = "oauth"))]
    let named_remote = false;
    let route = classify_mcp_http_route(
        request,
        &context.endpoint,
        oauth_enabled,
        local_oauth,
        named_remote,
    );
    #[cfg(feature = "oauth")]
    {
        match route {
            McpHttpRoute::LocalOAuthRegister
            | McpHttpRoute::LocalOAuthAuthorize
            | McpHttpRoute::LocalOAuthToken
            | McpHttpRoute::LocalOAuthIndieAuthCallback
            | McpHttpRoute::LocalOAuthConsent
            | McpHttpRoute::AuthorizationServerMetadata
            | McpHttpRoute::ProtectedResourceMetadata => {
                let response = handle_mcp_oauth_route(context, request, route);
                write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
                return Ok(());
            }
            McpHttpRoute::OperationStatus(operation_id) => {
                let response = if request.method == "GET" {
                    match authenticate_mcp_http_request(context, request) {
                        Ok(authority) => {
                            handle_named_mcp_operation_status(context, &authority, operation_id)
                        }
                        Err(response) => response,
                    }
                } else {
                    mcp_http_json_error_response(405, "Method Not Allowed", Value::Null)
                };
                write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
                return Ok(());
            }
            McpHttpRoute::McpEndpoint | McpHttpRoute::NotFound => {}
        }
    }

    if route != McpHttpRoute::McpEndpoint {
        let response = mcp_http_json_error_response(404, "Not Found", Value::Null);
        write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
        return Ok(());
    }
    let authority = match authenticate_mcp_http_request(context, request) {
        Ok(authority) => authority,
        Err(response) => {
            write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
            return Ok(());
        }
    };

    match request.method.as_str() {
        "POST" => {
            let response = handle_mcp_http_post(context, request, &authority);
            write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
        }
        "GET" => handle_mcp_http_sse(context, request, &authority, stream)?,
        "DELETE" => {
            let response = handle_mcp_http_delete(context, request, &authority);
            write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
        }
        _ => {
            let response = mcp_http_json_error_response(405, "Method Not Allowed", Value::Null);
            write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
        }
    }

    Ok(())
}

#[cfg(feature = "oauth")]
fn handle_named_mcp_operation_status(
    context: &McpHttpServerContext,
    authority: &McpSessionAuthority,
    operation_id: &str,
) -> McpHttpResponse {
    if !authority.allows_scope("mcp:tools") {
        return insufficient_scope_response(context, "mcp:tools");
    }
    let not_found = || mcp_http_json_error_response(404, "Not Found", Value::Null);
    let (Some(hosted), Some(named), Some(grant_id), Some(wiki_id)) = (
        context.hosted.as_ref(),
        context.named_runtime.as_ref(),
        authority.grant_id,
        authority.wiki_id.as_ref(),
    ) else {
        return not_found();
    };
    let Some(vault) = named.vaults.get(wiki_id) else {
        return not_found();
    };
    let Ok(record) = hosted.executor.ledger().load(operation_id) else {
        return not_found();
    };
    let Ok(selection) =
        resolve_permission_profile(&vault.paths, authority.permission_profile.as_deref())
    else {
        return not_found();
    };
    let grant = selection.grant;
    let caller = ExecutionContext::new(
        match ExecutionVaultIdentity::resolve(vault.paths.vault_root(), None, None) {
            Ok(vault) => vault,
            Err(_) => return not_found(),
        },
        ExecutionAuthority::Caller {
            principal_id: authority
                .subject
                .clone()
                .or_else(|| authority.client_id.clone())
                .unwrap_or_default(),
            credential_id: Some(grant_id.to_string()),
            permission_ceiling: grant.clone(),
        },
        grant,
        ExecutionIdentity::new(format!("mcp:{}", authority.remote_instance_id)),
        authority.audience.clone(),
        ExecutionRetryClass::ReadOnly,
        ExecutionCancellationToken::default(),
        None,
    );
    if !caller.is_ok_and(|caller| record.matches_caller(&caller)) {
        return not_found();
    }
    let body = serde_json::json!({
        "operation_id": record.operation_id,
        "state": record.state,
        "dispatched": record.dispatched,
        "committed": record.committed,
        "retry_disposition": record.retry_disposition,
        "updated_unix_ms": record.updated_unix_ms,
        "detail": record.detail,
    });
    McpHttpResponse {
        status: 200,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&body).expect("operation status should serialize"),
        extra_headers: vec![
            ("Cache-Control".to_string(), "no-store".to_string()),
            ("Vary".to_string(), "Authorization".to_string()),
        ],
    }
}

fn handle_mcp_http_post(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
    authority: &McpSessionAuthority,
) -> McpHttpResponse {
    let payload = match parse_mcp_http_post(request) {
        Ok(payload) => payload,
        Err(error) => {
            return mcp_http_json_error_response(error.status, error.message, Value::Null)
        }
    };
    if let Some(required) = mcp_protocol::required_scope_for_request(&payload) {
        if !authority.allows_scope(required) {
            return insufficient_scope_response(context, required);
        }
    }
    if let Err(error) = validate_mcp_protocol_version(request) {
        return mcp_http_json_error_response(error.status, error.message, Value::Null);
    }
    let (session_id, session, created_session) =
        match resolve_mcp_http_session(context, request, &payload, authority) {
            Ok(session) => session,
            Err(response) => return response,
        };

    if payload.get("method").and_then(Value::as_str) == Some("notifications/cancelled") {
        return handle_mcp_cancellation_notification(&payload, &session);
    }

    let active_id = request_id(&payload);
    let cancellation = ExecutionCancellationToken::default();
    if !register_mcp_http_request(&session, active_id.as_ref(), &cancellation) {
        return mcp_http_json_error_response(
            409,
            "MCP request ID is already active in this session",
            Value::Null,
        );
    }

    let result = {
        let mut core = session
            .core
            .lock()
            .expect("mcp core lock should not be poisoned");
        match core.process_http_request_with_timeout(
            payload.clone(),
            context.request_timeout,
            context,
            request,
            authority,
            cancellation,
        ) {
            Ok(result) => result,
            Err(error_response) => {
                if let Some(id) = active_id.as_ref() {
                    session.finish_request(id);
                }
                if created_session {
                    context.sessions.retire(&session_id);
                }
                return McpHttpResponse {
                    status: 400,
                    content_type: Some("application/json"),
                    body: serde_json::to_vec(&error_response).expect("json should serialize"),
                    extra_headers: Vec::new(),
                };
            }
        }
    };

    if let Some(id) = active_id.as_ref() {
        session.finish_request(id);
    }

    if result.session_stale {
        context.sessions.retire(&session_id);
    } else {
        session.broadcast(&result.notifications);
    }

    if result.accepted_notification {
        return McpHttpResponse {
            status: 202,
            content_type: None,
            body: Vec::new(),
            extra_headers: Vec::new(),
        };
    }

    let response_body = result
        .response
        .expect("MCP HTTP requests should produce a JSON-RPC response");
    let mut extra_headers = Vec::new();
    if created_session && !result.session_stale {
        extra_headers.push(("Mcp-Session-Id".to_string(), session_id));
    }
    McpHttpResponse {
        status: 200,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&response_body).expect("json should serialize"),
        extra_headers,
    }
}

fn handle_mcp_cancellation_notification(
    payload: &Value,
    session: &McpHttpSession,
) -> McpHttpResponse {
    let Some(target) = payload.pointer("/params/requestId") else {
        return mcp_http_json_error_response(
            400,
            "MCP cancellation requires params.requestId",
            Value::Null,
        );
    };
    if mcp_request_key(target).is_none() {
        return mcp_http_json_error_response(
            400,
            "MCP cancellation requestId must be a string or number",
            Value::Null,
        );
    }
    session.cancel_request(target);
    McpHttpResponse {
        status: 202,
        content_type: None,
        body: Vec::new(),
        extra_headers: Vec::new(),
    }
}

fn register_mcp_http_request(
    session: &McpHttpSession,
    id: Option<&Value>,
    cancellation: &ExecutionCancellationToken,
) -> bool {
    id.is_none_or(|id| session.register_request(id, cancellation.clone()))
}

fn insufficient_scope_response(context: &McpHttpServerContext, required: &str) -> McpHttpResponse {
    let message = format!("OAuth token does not grant required scope `{required}`");
    #[cfg(feature = "oauth")]
    if let Some(oauth) = context.oauth.as_ref() {
        let mut response = oauth_error_response(oauth, &message, "insufficient_scope");
        response.status = 403;
        if let Some((_, challenge)) = response
            .extra_headers
            .iter_mut()
            .find(|(name, _)| name == "WWW-Authenticate")
        {
            challenge.push_str(", scope=\"");
            challenge.push_str(required);
            challenge.push('"');
        }
        return response;
    }
    #[cfg(not(feature = "oauth"))]
    let _ = context;
    mcp_http_json_error_response(403, message, Value::Null)
}

fn resolve_mcp_http_session(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
    payload: &Value,
    authority: &McpSessionAuthority,
) -> Result<(String, Arc<McpHttpSession>, bool), McpHttpResponse> {
    let is_initialize = payload
        .as_object()
        .and_then(|object| object.get("method"))
        .and_then(Value::as_str)
        == Some("initialize");

    if is_initialize {
        let session_id = Ulid::new().to_string();
        #[cfg(feature = "oauth")]
        let named_session = context
            .named_runtime
            .as_ref()
            .map(|named| named.session_config(authority, context.instance_id))
            .transpose()
            .map_err(|message| mcp_http_json_error_response(403, message, Value::Null))?;
        #[cfg(feature = "oauth")]
        let paths = named_session
            .as_ref()
            .map_or(&context.paths, |session| &session.paths);
        #[cfg(not(feature = "oauth"))]
        let paths = &context.paths;
        #[cfg(feature = "oauth")]
        let named_profile = named_session
            .as_ref()
            .map(|session| session.permission_profile.as_str());
        #[cfg(not(feature = "oauth"))]
        let named_profile: Option<&str> = None;
        let requested_profile = named_profile
            .or(authority.permission_profile.as_deref())
            .or(context.requested_profile.as_deref());
        #[cfg(feature = "oauth")]
        let named_packs = named_session
            .as_ref()
            .map(|session| session.tool_packs.as_slice());
        #[cfg(not(feature = "oauth"))]
        let named_packs: Option<&[String]> = None;
        let pack_names = named_packs.or_else(|| {
            authority
                .grant_id
                .is_some()
                .then_some(authority.tool_packs.as_slice())
        });
        let authority_tool_packs = pack_names
            .map(|names| {
                mcp_tool_pack_args_from_names(names).map_err(|error| {
                    mcp_http_json_error_response(500, error.to_string(), Value::Null)
                })
            })
            .transpose()?;
        let core = McpServerCore::new(
            paths,
            requested_profile,
            authority_tool_packs
                .as_deref()
                .unwrap_or(&context.tool_pack_args),
            context.tool_pack_mode_arg,
        )
        .map_err(|error| mcp_http_json_error_response(500, error.to_string(), Value::Null))?;
        let session = Arc::new(McpHttpSession::new(core, authority.clone()));
        admit_mcp_http_session(context, session_id.clone(), Arc::clone(&session))?;
        return Ok((session_id, session, true));
    }

    let Some(session_id) = request.headers.get("mcp-session-id").cloned() else {
        return Err(mcp_http_json_error_response(
            400,
            "missing Mcp-Session-Id header",
            Value::Null,
        ));
    };
    let session = authorized_mcp_http_session(context, &session_id, authority, true)?;
    Ok((session_id, session, false))
}

fn handle_mcp_http_delete(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
    authority: &McpSessionAuthority,
) -> McpHttpResponse {
    let Some(session_id) = request.headers.get("mcp-session-id") else {
        return mcp_http_json_error_response(400, "missing Mcp-Session-Id header", Value::Null);
    };
    if let Err(response) = authorized_mcp_http_session(context, session_id, authority, false) {
        return response;
    }
    context.sessions.retire(session_id);
    McpHttpResponse {
        status: 204,
        content_type: None,
        body: Vec::new(),
        extra_headers: Vec::new(),
    }
}

fn handle_mcp_http_sse(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
    authority: &McpSessionAuthority,
    stream: &mut TcpStream,
) -> Result<(), CliError> {
    if let Err(error) = validate_mcp_sse_accept(request) {
        let response = mcp_http_json_error_response(error.status, error.message, Value::Null);
        write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
        return Ok(());
    }
    let Some(session_id) = request.headers.get("mcp-session-id") else {
        let response =
            mcp_http_json_error_response(400, "missing Mcp-Session-Id header", Value::Null);
        write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
        return Ok(());
    };
    let session = match authorized_mcp_http_session(context, session_id, authority, true) {
        Ok(session) => session,
        Err(response) => {
            write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
            return Ok(());
        }
    };

    write_mcp_http_sse_headers(stream).map_err(CliError::operation)?;
    let receiver = session.register_subscriber();
    let mut keepalive_elapsed = Duration::ZERO;

    loop {
        if session.is_idle_expired() {
            session.close();
            break;
        }
        match receiver.recv_timeout(MCP_HTTP_POLL_INTERVAL) {
            Ok(message) => {
                write_mcp_http_sse_event(stream, &message).map_err(CliError::operation)?;
                keepalive_elapsed = Duration::ZERO;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let notifications = {
                    let mut core = session
                        .core
                        .lock()
                        .expect("mcp core lock should not be poisoned");
                    core.list_changed_notifications()
                };
                for notification in notifications {
                    if mcp_notification_scope(&notification)
                        .is_none_or(|scope| session.authority.allows_scope(scope))
                    {
                        write_mcp_http_sse_event(stream, &notification)
                            .map_err(CliError::operation)?;
                    }
                }
                keepalive_elapsed += MCP_HTTP_POLL_INTERVAL;
                if keepalive_elapsed >= MCP_HTTP_KEEPALIVE_INTERVAL {
                    write_mcp_http_sse_keepalive(stream).map_err(CliError::operation)?;
                    keepalive_elapsed = Duration::ZERO;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(())
}

#[allow(clippy::too_many_lines)]
fn authenticate_mcp_http_request(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
) -> Result<McpSessionAuthority, McpHttpResponse> {
    let mut credential = "loopback-unauthenticated".to_string();
    #[allow(unused_mut)]
    let mut client_id = None;
    #[allow(unused_mut)]
    let mut subject = None;
    #[allow(unused_mut)]
    let mut permission_profile = context.requested_profile.clone();
    #[cfg(feature = "oauth")]
    let mut oauth_grant_id = None;
    #[allow(unused_mut)]
    let mut scopes = DEFAULT_MCP_OAUTH_SCOPES
        .iter()
        .map(|scope| (*scope).to_string())
        .collect::<Vec<_>>();
    #[cfg(feature = "oauth")]
    if let Some(oauth) = context.oauth.as_ref() {
        let Some(token) = bearer_token(&request.headers) else {
            return Err(oauth_error_response(
                oauth,
                "missing OAuth bearer token",
                "invalid_token",
            ));
        };
        match oauth {
            McpOAuthMode::External(external) => match external.validate_bearer_token(&token) {
                Ok(identity) => {
                    subject = Some(identity.subject);
                    scopes = identity.scopes;
                }
                Err(error) => {
                    eprintln!("MCP OAuth bearer token rejected: {error}");
                    return Err(oauth_error_response(
                        oauth,
                        error.to_string(),
                        "invalid_token",
                    ));
                }
            },
            McpOAuthMode::Local(local) => match local.validate_bearer_token(&token) {
                Ok(identity) => {
                    subject = Some(identity.subject);
                    client_id = identity.client_id;
                    scopes = identity.scopes;
                    oauth_grant_id = identity.grant_id;
                    if permission_profile.is_none() {
                        permission_profile = identity.permission_profile;
                    }
                }
                Err(error) => {
                    eprintln!("MCP OAuth bearer token rejected: {error}");
                    return Err(oauth_error_response(
                        oauth,
                        error.to_string(),
                        "invalid_token",
                    ));
                }
            },
        }
        credential = token;
    }
    if let Some(expected_token) = context.auth_token.as_deref() {
        let actual_token = bearer_or_shared_token(&request.headers);
        if actual_token.as_deref() != Some(expected_token) {
            return Err(mcp_http_json_error_response(
                401,
                "missing or invalid authentication token",
                Value::Null,
            ));
        }
        credential = actual_token.expect("validated token should be present");
    }
    if let Some(origin) = request.headers.get("origin") {
        if !mcp_origin_allowed(origin, context.bind_addr) {
            return Err(mcp_http_json_error_response(
                403,
                "invalid Origin header",
                Value::Null,
            ));
        }
    }
    #[cfg(feature = "oauth")]
    if let Some(named) = context.named_runtime.as_ref() {
        let grant_id = oauth_grant_id
            .as_deref()
            .and_then(|value| value.parse::<Ulid>().ok())
            .ok_or_else(|| {
                oauth_error_response(
                    context.oauth.as_ref().expect("named runtime has OAuth"),
                    "access token is not bound to a connection grant",
                    "invalid_token",
                )
            })?;
        let client_id = client_id.clone().ok_or_else(|| {
            oauth_error_response(
                context.oauth.as_ref().expect("named runtime has OAuth"),
                "access token has no OAuth client binding",
                "invalid_token",
            )
        })?;
        let now = unix_timestamp_for_mcp()
            .map_err(|error| mcp_http_json_error_response(500, error.to_string(), Value::Null))?;
        return named
            .authorize_token(&NamedTokenRequest {
                remote_instance_id: context.instance_id,
                grant_id,
                client_id: &client_id,
                subject: subject.as_deref(),
                scopes: &scopes,
                resource: context
                    .oauth
                    .as_ref()
                    .expect("named runtime has OAuth")
                    .public_url(),
                credential: &credential,
                now,
            })
            .map_err(|error| {
                oauth_error_response(
                    context.oauth.as_ref().expect("named runtime has OAuth"),
                    error,
                    "invalid_token",
                )
            });
    }
    let packs = pack_name_list(&resolve_selected_tool_packs(
        &context.tool_pack_args,
        McpToolPackMode::from(context.tool_pack_mode_arg),
    ));
    Ok(McpSessionAuthority::direct(
        context.instance_id,
        &credential,
        client_id,
        subject,
        permission_profile,
        packs,
        scopes,
    ))
}

impl McpServerCore {
    fn new(
        paths: &VaultPaths,
        requested_profile: Option<&str>,
        tool_pack_args: &[McpToolPackArg],
        tool_pack_mode_arg: McpToolPackModeArg,
    ) -> Result<Self, CliError> {
        let selection = resolve_permission_profile(paths, requested_profile)
            .map_err(permission_error_to_cli)?;
        let tool_pack_mode = McpToolPackMode::from(tool_pack_mode_arg);
        let selected_tool_packs = resolve_selected_tool_packs(tool_pack_args, tool_pack_mode);
        let pinned_tool_packs = selected_tool_packs
            .iter()
            .copied()
            .filter(|pack| *pack != McpToolPack::ToolPacks)
            .collect();
        let guard = ProfilePermissionGuard::new(paths, selection.clone());
        let snapshot = McpListSnapshot {
            tools: mcp_assistant::tool_catalog_fingerprint(
                paths,
                Some(selection.name.as_str()),
                &selected_tool_packs,
                &selection.profile,
                crate::custom_tool_registry_options,
            ),
            prompts: prompt_files_fingerprint(paths, &guard),
            resources: resource_files_fingerprint(paths, &guard),
        };

        Ok(Self {
            paths: paths.clone(),
            selection,
            guard,
            tool_pack_mode,
            pinned_tool_packs,
            selected_tool_packs,
            tool_resources: McpToolResourceStore::default(),
            snapshot,
        })
    }

    fn process_request_with_timeout(&mut self, request: Value, timeout: Duration) -> Vec<Value> {
        if timeout.is_zero() {
            return timeout_response_for_request(&request, timeout)
                .into_iter()
                .collect();
        }
        let timeout_request = request.clone();
        let mut worker = self.clone();
        let (sender, receiver) = mpsc::channel();
        if thread::Builder::new()
            .name("vulcan-mcp-request".to_string())
            .stack_size(MCP_REQUEST_WORKER_STACK_SIZE)
            .spawn(move || {
                let messages = worker.process_request(request);
                let _ = sender.send((worker, messages));
            })
            .is_err()
        {
            let id = request_id(&timeout_request).unwrap_or(Value::Null);
            return vec![jsonrpc_error(
                id,
                -32603,
                "MCP request worker could not be started".to_string(),
                None,
            )];
        }
        match receiver.recv_timeout(timeout) {
            Ok((next, messages)) => {
                *self = next;
                messages
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                timeout_response_for_request(&timeout_request, timeout)
                    .into_iter()
                    .collect()
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let id = request_id(&timeout_request).unwrap_or(Value::Null);
                vec![jsonrpc_error(
                    id,
                    -32603,
                    "MCP request worker stopped before producing a response".to_string(),
                    None,
                )]
            }
        }
    }

    fn process_request(&mut self, request: Value) -> Vec<Value> {
        let _read_guard = match acquire_request_ordinary_write_gate(&self.paths, &request) {
            Ok(guard) => guard,
            Err(message) => {
                return request_id(&request)
                    .map(|id| vec![jsonrpc_error(id, -32603, message, None)])
                    .unwrap_or_default()
            }
        };
        process_stdio_request(self, request)
    }

    fn process_http_request(&mut self, request: &Value) -> Result<McpHttpProcessResult, Value> {
        let _read_guard = match acquire_request_ordinary_write_gate(&self.paths, request) {
            Ok(guard) => guard,
            Err(message) => {
                return if let Some(id) = request_id(request) {
                    Err(jsonrpc_error(id, -32603, message, None))
                } else {
                    Ok(McpHttpProcessResult {
                        response: None,
                        notifications: Vec::new(),
                        accepted_notification: true,
                        session_stale: false,
                    })
                };
            }
        };
        process_http_request(self, request)
    }

    #[allow(clippy::too_many_lines)] // Registration must precede the worker, and all timeout branches share its ID.
    fn process_http_request_with_timeout(
        &mut self,
        request: Value,
        timeout: Duration,
        http_context: &McpHttpServerContext,
        inbound: &McpHttpRequest,
        authority: &McpSessionAuthority,
        cancellation: ExecutionCancellationToken,
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
        let mut worker = self.clone();
        let (sender, receiver) = mpsc::channel();
        #[cfg(feature = "oauth")]
        let hosted = http_context.hosted.clone();
        #[cfg(feature = "oauth")]
        let dispatch = hosted
            .as_ref()
            .map(|hosted| {
                hosted
                    .prepare(
                        self,
                        &request,
                        authority,
                        cancellation.clone(),
                        ExecutionDeadline::after(timeout),
                    )
                    .map(|execution| HostedMcpDispatch {
                        http: http_context.clone(),
                        inbound: inbound.clone(),
                        authority: authority.clone(),
                        execution,
                    })
            })
            .transpose()?;
        #[cfg(feature = "oauth")]
        let operation_id = dispatch
            .as_ref()
            .filter(|_| mcp_scheduled_operation(&request) == ScheduledOperation::Mutation)
            .map(|dispatch| dispatch.execution.identity.operation_id.clone());
        #[cfg(feature = "oauth")]
        let failed_ledger = hosted.as_ref().map(|hosted| hosted.executor.ledger());
        #[cfg(feature = "oauth")]
        let named_runtime = http_context.named_runtime.is_some();
        #[cfg(not(feature = "oauth"))]
        let _ = (http_context, inbound, authority);
        let worker_cancellation = cancellation.clone();
        if thread::Builder::new()
            .name("vulcan-mcp-http-request".to_string())
            .stack_size(MCP_REQUEST_WORKER_STACK_SIZE)
            .spawn(move || {
                #[cfg(feature = "oauth")]
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
                #[cfg(not(feature = "oauth"))]
                let result = if worker_cancellation.is_cancelled() {
                    Err(jsonrpc_error(
                        request_id(&request).unwrap_or(Value::Null),
                        -32800,
                        "MCP request cancelled before dispatch".to_string(),
                        None,
                    ))
                } else {
                    worker.process_http_request(&request)
                };
                let _ = sender.send((worker, result));
            })
            .is_err()
        {
            #[cfg(feature = "oauth")]
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
        match receiver.recv_timeout(timeout) {
            Ok((next, result)) => {
                *self = next;
                result
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                cancellation.cancel();
                #[cfg(feature = "oauth")]
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
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                #[cfg(feature = "oauth")]
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
        }
    }

    fn handle_method(
        &mut self,
        method: &str,
        params: Option<&Value>,
    ) -> Result<McpMethodOutcome, McpMethodError> {
        dispatch_protocol_method(self, method, params)
    }

    fn initialize_result(&self) -> Value {
        mcp_protocol::initialization_result(&self.active_tool_names(), env!("CARGO_PKG_VERSION"))
    }

    fn discovery<'a>(
        &'a self,
        registry: &'a vulcan_app::tools::CustomToolRegistryOptions,
    ) -> vulcan_app::mcp_discovery::McpDiscovery<'a> {
        vulcan_app::mcp_discovery::McpDiscovery {
            paths: &self.paths,
            guard: &self.guard,
            profile_name: self.selection.name.as_str(),
            profile: &self.selection.profile,
            selected_packs: &self.selected_tool_packs,
            resources: &self.tool_resources,
            custom_registry: registry,
        }
    }

    fn call_tool(
        &mut self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, McpMethodError> {
        let registry = crate::custom_tool_registry_options();
        vulcan_app::mcp_tool_exec::McpToolExecution {
            paths: &self.paths,
            guard: &self.guard,
            profile_name: self.selection.name.as_str(),
            profile: &self.selection.profile,
            selected_packs: &mut self.selected_tool_packs,
            pinned_packs: &self.pinned_tool_packs,
            pack_mode: self.tool_pack_mode,
            resources: &mut self.tool_resources,
            custom_registry: &registry,
        }
        .call_tool(name, arguments)
    }

    fn active_tool_names(&self) -> Vec<String> {
        vulcan_app::mcp_catalog::active_tool_names(
            &self.selected_tool_packs,
            &self.selection.profile,
        )
    }

    fn list_changed_notifications(&mut self) -> Vec<Value> {
        let current = McpListSnapshot {
            tools: mcp_assistant::tool_catalog_fingerprint(
                &self.paths,
                Some(self.selection.name.as_str()),
                &self.selected_tool_packs,
                &self.selection.profile,
                crate::custom_tool_registry_options,
            ),
            prompts: prompt_files_fingerprint(&self.paths, &self.guard),
            resources: resource_files_fingerprint(&self.paths, &self.guard),
        };
        self.snapshot.changed_notifications(current)
    }
}

impl McpProtocolMethods for McpServerCore {
    fn initialize_result(&self) -> Value {
        McpServerCore::initialize_result(self)
    }

    fn visible_tool_items(&self) -> Result<Vec<Value>, McpMethodError> {
        let registry = crate::custom_tool_registry_options();
        self.discovery(&registry).visible_tool_items()
    }

    fn call_tool(
        &mut self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, McpMethodError> {
        McpServerCore::call_tool(self, name, arguments)
    }

    fn visible_prompt_items(&self) -> Result<Vec<Value>, McpMethodError> {
        let registry = crate::custom_tool_registry_options();
        self.discovery(&registry).visible_prompt_items()
    }

    fn get_prompt(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, McpMethodError> {
        let registry = crate::custom_tool_registry_options();
        self.discovery(&registry).get_prompt(name, arguments)
    }

    fn visible_resources(&self) -> Result<Vec<Value>, McpMethodError> {
        let registry = crate::custom_tool_registry_options();
        self.discovery(&registry).visible_resources()
    }

    fn visible_resource_templates(&self) -> Vec<Value> {
        let registry = crate::custom_tool_registry_options();
        self.discovery(&registry).visible_resource_templates()
    }

    fn read_resource(&self, uri: &str) -> Result<Value, McpMethodError> {
        let registry = crate::custom_tool_registry_options();
        self.discovery(&registry).read_resource(uri, |topic_path| {
            resolve_help_topic(topic_path).map_err(|error| error.message)
        })
    }

    fn complete(&self, params: &McpCompletionParams) -> Result<Value, McpMethodError> {
        let registry = crate::custom_tool_registry_options();
        self.discovery(&registry)
            .complete(params, &help_topic_completion_candidates(""))
    }
}

impl McpMethodHandler for McpServerCore {
    fn handle_method(
        &mut self,
        method: &str,
        params: Option<&Value>,
    ) -> Result<McpMethodOutcome, McpMethodError> {
        McpServerCore::handle_method(self, method, params)
    }

    fn list_changed_notifications(&mut self) -> Vec<Value> {
        McpServerCore::list_changed_notifications(self)
    }
}

fn parse_mcp_http_bind_addr(bind: &str, allow_remote: bool) -> Result<SocketAddr, CliError> {
    let addr = bind.parse::<SocketAddr>().map_err(|_| {
        CliError::operation("mcp bind address must be a socket address like 127.0.0.1:8765")
    })?;
    if !addr.ip().is_loopback() && !allow_remote {
        return Err(CliError::operation(
            "non-loopback MCP HTTP binds require --auth-token",
        ));
    }
    Ok(addr)
}

#[allow(clippy::too_many_lines)]
#[cfg(feature = "oauth")]
fn build_mcp_oauth_validator(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    options: &McpHttpOptions,
) -> Result<Option<McpOAuthMode>, CliError> {
    let local_requested = options.oauth_local_client_id.is_some()
        || !options.oauth_local_redirect_uri.is_empty()
        || options.oauth_local_client_secret.is_some()
        || options.oauth_local_approval_token.is_some()
        || options.oauth_dcr
        || options.oauth_indieauth_authorization_endpoint.is_some()
        || options.oauth_indieauth_token_endpoint.is_some()
        || options.oauth_indieauth_me.is_some();
    if local_requested {
        if options.oauth_issuer.is_some() || options.auth_token.is_some() {
            return Err(CliError::operation(
                "local MCP OAuth issuer is mutually exclusive with --oauth-issuer and --auth-token",
            ));
        }
        let public_url = options.public_url.as_deref().ok_or_else(|| {
            CliError::operation("--public-url is required when using local MCP OAuth issuer")
        })?;
        let client_id = options
            .oauth_local_client_id
            .as_deref()
            .unwrap_or("vulcan-mcp");
        if options.oauth_local_client_id.is_some()
            && (options.oauth_local_redirect_uri.is_empty()
                || !options
                    .oauth_local_redirect_uri
                    .iter()
                    .all(|uri| mcp_oauth_redirect_uri_valid(uri)))
        {
            return Err(CliError::operation(
                "static local OAuth clients require at least one valid --oauth-local-redirect-uri",
            ));
        }
        let client_secret = match options.oauth_local_client_secret.as_deref() {
            Some(secret) => secret.to_string(),
            None if options.oauth_dcr => load_or_create_local_oauth_issuer_secret(paths, options)?,
            None => {
                return Err(CliError::operation(
                    "--oauth-local-client-secret is required unless --oauth-dcr is enabled",
                ))
            }
        };
        let approval_token = options.oauth_local_approval_token.as_deref().unwrap_or("");
        if approval_token.is_empty()
            && options.oauth_indieauth_authorization_endpoint.is_none()
            && options.oauth_indieauth_me.is_none()
        {
            return Err(CliError::operation(
                "--oauth-local-approval-token is required unless IndieAuth is configured",
            ));
        }
        let implicit_indieauth_subject = if options.oauth_local_user.is_empty()
            && options.oauth_local_subject.is_none()
            && requested_profile.is_some()
        {
            options.oauth_indieauth_me.as_deref()
        } else {
            None
        };
        if options.oauth_indieauth_me.is_some()
            && options.oauth_local_user.is_empty()
            && options.oauth_local_subject.is_none()
            && requested_profile.is_none()
        {
            return Err(CliError::operation(
                "single-user IndieAuth requires --permissions <profile>; alternatively add \
                 --oauth-local-user <subject>=<profile> for per-user permissions",
            ));
        }
        let subject = options
            .oauth_local_subject
            .as_deref()
            .or(implicit_indieauth_subject)
            .unwrap_or("local-user");
        return LocalOAuthIssuer::from_config(LocalOAuthIssuerConfig {
            public_url: public_url.to_string(),
            client_id: client_id.to_string(),
            client_secret,
            signing_key: load_or_create_local_oauth_signing_key(paths, options)?,
            approval_token: approval_token.to_string(),
            subject: subject.to_string(),
            email: options.oauth_local_email.clone(),
            users: parse_local_oauth_users(&options.oauth_local_user)?,
            dcr_enabled: options.oauth_dcr,
        })
        .map(Arc::new)
        .map(McpOAuthMode::Local)
        .map(Some)
        .map_err(CliError::operation);
    }

    let Some(issuer) = options.oauth_issuer.as_deref() else {
        if options.public_url.is_some()
            || !options.oauth_audience.is_empty()
            || options.oauth_jwks_url.is_some()
            || !options.oauth_allowed_sub.is_empty()
            || !options.oauth_allowed_email.is_empty()
        {
            return Err(CliError::operation(
                "--oauth-issuer is required when using MCP OAuth options",
            ));
        }
        return Ok(None);
    };
    if options.auth_token.is_some() {
        return Err(CliError::operation(
            "--auth-token and --oauth-issuer are mutually exclusive for direct MCP HTTP auth",
        ));
    }
    let public_url = options
        .public_url
        .as_deref()
        .ok_or_else(|| CliError::operation("--public-url is required when using --oauth-issuer"))?;
    if !public_url.starts_with("https://") {
        return Err(CliError::operation(
            "--public-url must be an HTTPS URL for MCP OAuth",
        ));
    }
    if options.oauth_audience.is_empty() {
        return Err(CliError::operation(
            "--oauth-audience is required when using --oauth-issuer",
        ));
    }
    if options.oauth_allowed_sub.is_empty() && options.oauth_allowed_email.is_empty() {
        return Err(CliError::operation(
            "at least one --oauth-allowed-sub or --oauth-allowed-email is required",
        ));
    }

    OAuthResourceServer::from_config(OAuthResourceServerConfig {
        issuer: issuer.to_string(),
        audiences: options.oauth_audience.clone(),
        jwks_url: options.oauth_jwks_url.clone(),
        allowed_subs: options.oauth_allowed_sub.clone(),
        allowed_emails: options.oauth_allowed_email.clone(),
        public_url: public_url.to_string(),
    })
    .map(Arc::new)
    .map(McpOAuthMode::External)
    .map(Some)
    .map_err(CliError::operation)
}

#[cfg(not(feature = "oauth"))]
fn reject_mcp_oauth_options_when_disabled(options: &McpHttpOptions) -> Result<(), CliError> {
    let oauth_requested = options.oauth_issuer.is_some()
        || !options.oauth_audience.is_empty()
        || options.oauth_jwks_url.is_some()
        || !options.oauth_allowed_sub.is_empty()
        || !options.oauth_allowed_email.is_empty()
        || options.oauth_local_client_id.is_some()
        || options.oauth_local_client_secret.is_some()
        || options.oauth_local_approval_token.is_some()
        || options.oauth_local_subject.is_some()
        || options.oauth_local_email.is_some()
        || options.oauth_dcr
        || !options.oauth_dcr_allowed_redirect_host.is_empty()
        || options.oauth_indieauth_authorization_endpoint.is_some()
        || options.oauth_indieauth_token_endpoint.is_some()
        || options.oauth_indieauth_client_id.is_some()
        || options.oauth_indieauth_redirect_uri.is_some()
        || options.oauth_indieauth_me.is_some()
        || !options.oauth_local_user.is_empty();
    if oauth_requested {
        return Err(CliError::operation(
            "MCP OAuth requires a build with the `oauth` feature enabled",
        ));
    }
    Ok(())
}

#[cfg(feature = "oauth")]
fn handle_mcp_oauth_route(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
    route: McpHttpRoute<'_>,
) -> McpHttpResponse {
    match context.oauth.as_ref().expect("OAuth route requires issuer") {
        McpOAuthMode::External(external) => {
            McpOAuthRoutes::External(external).handle(request, route)
        }
        McpOAuthMode::Local(local) => McpOAuthRoutes::Local(Box::new(McpLocalOAuthRoutes {
            issuer: local,
            authorize: local_authorize_endpoint(context, local),
            consent: local_consent_endpoint(context, local),
            token: local_token_endpoint(context, local),
            dcr_enabled: context.oauth_dcr_enabled,
            allowed_redirect_hosts: &context.oauth_dcr_allowed_redirect_hosts,
            refresh_supported: context.named_runtime.is_some(),
        }))
        .handle(request, route),
    }
}

#[cfg(all(test, feature = "oauth"))]
fn parse_query_params(query: &str) -> BTreeMap<String, String> {
    vulcan_daemon::mcp_oauth_routes::parse_oauth_params(query)
}

#[cfg(all(test, feature = "oauth"))]
fn oauth_authorization_server_metadata(
    context: &McpHttpServerContext,
    _oauth: &McpOAuthMode,
) -> Value {
    let request = McpHttpRequest {
        method: "GET".to_string(),
        path: "/.well-known/oauth-authorization-server".to_string(),
        query: String::new(),
        headers: BTreeMap::new(),
        body: Vec::new(),
    };
    let response =
        handle_mcp_oauth_route(context, &request, McpHttpRoute::AuthorizationServerMetadata);
    serde_json::from_slice(&response.body).expect("OAuth metadata JSON")
}

#[cfg(all(test, feature = "oauth"))]
fn handle_local_oauth_register(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
) -> McpHttpResponse {
    handle_mcp_oauth_route(context, request, McpHttpRoute::LocalOAuthRegister)
}

#[cfg(all(test, feature = "oauth"))]
fn handle_local_oauth_authorize(
    context: &McpHttpServerContext,
    _issuer: &LocalOAuthIssuer,
    request: &McpHttpRequest,
) -> McpHttpResponse {
    handle_mcp_oauth_route(context, request, McpHttpRoute::LocalOAuthAuthorize)
}

#[cfg(all(test, feature = "oauth"))]
fn handle_local_oauth_token(
    context: &McpHttpServerContext,
    _issuer: &LocalOAuthIssuer,
    request: &McpHttpRequest,
) -> McpHttpResponse {
    handle_mcp_oauth_route(context, request, McpHttpRoute::LocalOAuthToken)
}

#[cfg(all(test, feature = "oauth"))]
fn handle_local_oauth_consent(
    context: &McpHttpServerContext,
    _issuer: &LocalOAuthIssuer,
    request: &McpHttpRequest,
) -> McpHttpResponse {
    handle_mcp_oauth_route(context, request, McpHttpRoute::LocalOAuthConsent)
}

#[cfg(feature = "oauth")]
fn local_token_endpoint<'a>(
    context: &'a McpHttpServerContext,
    issuer: &'a LocalOAuthIssuer,
) -> McpLocalTokenEndpoint<'a> {
    McpLocalTokenEndpoint {
        issuer,
        clients: &context.oauth_clients,
        codes: &context.oauth_codes,
        named_runtime: context.named_runtime.as_ref(),
        instance_id: context.instance_id,
        allowed_redirect_hosts: &context.oauth_dcr_allowed_redirect_hosts,
    }
}

#[cfg(all(test, feature = "oauth"))]
fn handle_local_oauth_refresh(
    context: &McpHttpServerContext,
    issuer: &LocalOAuthIssuer,
    client_id: &str,
    params: &BTreeMap<String, String>,
) -> McpHttpResponse {
    local_token_endpoint(context, issuer).refresh(client_id, params)
}

#[cfg(feature = "oauth")]
fn local_authorize_endpoint<'a>(
    context: &'a McpHttpServerContext,
    issuer: &'a LocalOAuthIssuer,
) -> McpAuthorizeEndpoint<'a> {
    #[cfg(test)]
    let exchange = context
        .indieauth_exchange
        .unwrap_or(default_indieauth_exchange);
    #[cfg(not(test))]
    let exchange = default_indieauth_exchange;
    McpAuthorizeEndpoint {
        issuer,
        clients: &context.oauth_clients,
        codes: &context.oauth_codes,
        pending_indieauth: &context.oauth_pending_indieauth,
        pending_consent: &context.oauth_pending_consent,
        indieauth: context.oauth_indieauth.as_ref(),
        named_runtime: context.named_runtime.as_ref(),
        requested_profile: context.requested_profile.clone(),
        selected_packs: pack_name_list(&resolve_selected_tool_packs(
            &context.tool_pack_args,
            McpToolPackMode::from(context.tool_pack_mode_arg),
        )),
        fallback_vault_root: context.paths.vault_root().display().to_string(),
        local_redirect_uris: &context.oauth_local_redirect_uris,
        allowed_redirect_hosts: &context.oauth_dcr_allowed_redirect_hosts,
        exchange,
    }
}

#[cfg(feature = "oauth")]
fn local_consent_endpoint<'a>(
    context: &'a McpHttpServerContext,
    issuer: &'a LocalOAuthIssuer,
) -> McpConsentEndpoint<'a> {
    McpConsentEndpoint {
        issuer,
        pending: &context.oauth_pending_consent,
        codes: &context.oauth_codes,
        named_runtime: context.named_runtime.as_ref(),
        instance_id: context.instance_id,
    }
}

#[cfg(all(test, feature = "oauth"))]
fn create_named_connection_grant(
    context: &McpHttpServerContext,
    pending: &LocalOAuthPendingConsent,
    params: &BTreeMap<String, String>,
) -> Result<Option<String>, McpHttpResponse> {
    let issuer = match context.oauth.as_ref().expect("OAuth test context") {
        McpOAuthMode::Local(issuer) => issuer,
        McpOAuthMode::External(_) => unreachable!("named grants require local issuer"),
    };
    local_consent_endpoint(context, issuer).create_named_grant(pending, params)
}

#[cfg(all(test, feature = "oauth"))]
fn local_oauth_consent_form(
    context: &McpHttpServerContext,
    issuer: &LocalOAuthIssuer,
    transaction_id: &str,
    pending: &LocalOAuthPendingConsent,
) -> McpHttpResponse {
    local_authorize_endpoint(context, issuer).render_consent(transaction_id, pending)
}

#[cfg(all(test, feature = "oauth"))]
fn local_oauth_client_redirect_allowed(
    context: &McpHttpServerContext,
    issuer: &LocalOAuthIssuer,
    client_id: &str,
    redirect_uri: &str,
) -> bool {
    local_authorize_endpoint(context, issuer).client_redirect_allowed(client_id, redirect_uri)
}

#[cfg(feature = "oauth")]
fn oauth_clients_path(paths: &VaultPaths, options: &McpHttpOptions) -> PathBuf {
    options.oauth_storage_dir.as_ref().map_or_else(
        || paths.vulcan_dir().join("mcp-oauth-clients.json"),
        |directory| directory.join("oauth-clients.json"),
    )
}

#[cfg(feature = "oauth")]
fn oauth_issuer_secret_path(paths: &VaultPaths, options: &McpHttpOptions) -> PathBuf {
    options.oauth_storage_dir.as_ref().map_or_else(
        || paths.vulcan_dir().join("mcp-oauth-issuer-secret"),
        |directory| directory.join("oauth-issuer-secret"),
    )
}

#[cfg(feature = "oauth")]
fn oauth_signing_key_path(paths: &VaultPaths, options: &McpHttpOptions) -> PathBuf {
    options.oauth_storage_dir.as_ref().map_or_else(
        || paths.vulcan_dir().join("mcp-oauth-signing-key"),
        |directory| directory.join("oauth-signing-key"),
    )
}

#[cfg(feature = "oauth")]
fn generate_pkce_verifier() -> String {
    format!(
        "{}{}{}{}",
        Ulid::new(),
        Ulid::new(),
        Ulid::new(),
        Ulid::new()
    )
}

#[cfg(feature = "oauth")]
fn load_or_create_local_oauth_issuer_secret(
    paths: &VaultPaths,
    options: &McpHttpOptions,
) -> Result<String, CliError> {
    let path = oauth_issuer_secret_path(paths, options);
    if path.exists() {
        let secret = fs::read_to_string(&path).map_err(CliError::operation)?;
        let secret = secret.trim().to_string();
        if secret.is_empty() {
            return Err(CliError::operation(format!(
                "{} is empty; remove it so Vulcan can regenerate the OAuth issuer secret",
                path.display()
            )));
        }
        return Ok(secret);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    let secret = generate_pkce_verifier();
    write_secret_file(&path, &secret)?;
    Ok(secret)
}

#[cfg(feature = "oauth")]
fn load_or_create_local_oauth_signing_key(
    paths: &VaultPaths,
    options: &McpHttpOptions,
) -> Result<String, CliError> {
    load_or_create_secret_file(&oauth_signing_key_path(paths, options), "OAuth signing key")
}

#[cfg(feature = "oauth")]
fn load_or_create_secret_file(path: &Path, label: &str) -> Result<String, CliError> {
    if path.exists() {
        let secret = fs::read_to_string(path).map_err(CliError::operation)?;
        let secret = secret.trim().to_string();
        if secret.is_empty() {
            return Err(CliError::operation(format!(
                "{} is empty; remove it so Vulcan can regenerate the {label}",
                path.display()
            )));
        }
        return Ok(secret);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    let secret = generate_pkce_verifier();
    write_secret_file(path, &secret)?;
    Ok(secret)
}

#[cfg(all(unix, feature = "oauth"))]
fn write_secret_file(path: &Path, secret: &str) -> Result<(), CliError> {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(CliError::operation)?;
    file.write_all(secret.as_bytes())
        .map_err(CliError::operation)?;
    file.write_all(b"\n").map_err(CliError::operation)
}

#[cfg(all(not(unix), feature = "oauth"))]
fn write_secret_file(path: &Path, secret: &str) -> Result<(), CliError> {
    fs::write(path, format!("{secret}\n")).map_err(CliError::operation)
}

#[cfg(feature = "oauth")]
fn build_indieauth_config(
    options: &McpHttpOptions,
) -> Result<Option<LocalOAuthIndieAuthConfig>, CliError> {
    if options.oauth_indieauth_authorization_endpoint.is_none()
        && options.oauth_indieauth_token_endpoint.is_none()
        && options.oauth_indieauth_me.is_none()
    {
        return Ok(None);
    }
    let discovered = match (
        options.oauth_indieauth_authorization_endpoint.as_ref(),
        options.oauth_indieauth_token_endpoint.as_ref(),
        options.oauth_indieauth_me.as_ref(),
    ) {
        (Some(_), Some(_), _) => None,
        (_, _, Some(me)) => Some(discover_indieauth_endpoints(me).map_err(CliError::operation)?),
        _ => {
            return Err(CliError::operation(
                "--oauth-indieauth-me is required unless both IndieAuth endpoints are provided",
            ))
        }
    };
    let authorization_endpoint = options
        .oauth_indieauth_authorization_endpoint
        .clone()
        .or_else(|| {
            discovered
                .as_ref()
                .map(|endpoints| endpoints.authorization_endpoint.clone())
        })
        .ok_or_else(|| CliError::operation("IndieAuth authorization endpoint is required"))?;
    let token_endpoint = options
        .oauth_indieauth_token_endpoint
        .clone()
        .or_else(|| {
            discovered
                .as_ref()
                .map(|endpoints| endpoints.token_endpoint.clone())
        })
        .ok_or_else(|| CliError::operation("IndieAuth token endpoint is required"))?;
    let public_url = options
        .public_url
        .as_deref()
        .ok_or_else(|| CliError::operation("--public-url is required with IndieAuth options"))?;
    let origin = public_url_origin_for_cli(public_url)?;
    let client_id = options
        .oauth_indieauth_client_id
        .clone()
        .unwrap_or_else(|| origin.clone());
    let redirect_uri = options
        .oauth_indieauth_redirect_uri
        .clone()
        .unwrap_or_else(|| format!("{origin}/oauth/indieauth/callback"));
    Ok(Some(LocalOAuthIndieAuthConfig {
        authorization_endpoint,
        token_endpoint,
        client_id,
        redirect_uri,
        me: options.oauth_indieauth_me.clone(),
    }))
}

#[cfg(feature = "oauth")]
fn public_url_origin_for_cli(public_url: &str) -> Result<String, CliError> {
    let Some((scheme, rest)) = public_url.split_once("://") else {
        return Err(CliError::operation("--public-url must be absolute"));
    };
    let host = rest.split('/').next().unwrap_or(rest);
    Ok(format!("{scheme}://{host}"))
}

#[cfg(feature = "oauth")]
fn parse_local_oauth_users(users: &[String]) -> Result<Vec<LocalOAuthUserConfig>, CliError> {
    users
        .iter()
        .map(|entry| {
            let (subject, rest) = entry
                .split_once('=')
                .ok_or_else(|| CliError::operation("OAuth users must be subject=profile"))?;
            if subject.is_empty() || rest.is_empty() {
                return Err(CliError::operation("OAuth users must be subject=profile"));
            }
            let (profile, email) = rest
                .split_once(',')
                .map_or((rest, None), |(profile, email)| (profile, Some(email)));
            Ok(LocalOAuthUserConfig {
                subject: subject.to_string(),
                email: email.map(ToOwned::to_owned),
                permission_profile: Some(profile.to_string()),
            })
        })
        .collect()
}

#[cfg(all(test, feature = "oauth"))]
fn current_unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(feature = "oauth")]
fn current_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(all(test, feature = "oauth"))]
fn oauth_plain_response(status: u16, message: &str) -> McpHttpResponse {
    McpHttpResponse {
        status,
        content_type: Some("text/plain; charset=utf-8"),
        body: message.as_bytes().to_vec(),
        extra_headers: Vec::new(),
    }
}

#[cfg(all(test, feature = "oauth"))]
fn oauth_json_error_response(
    status: u16,
    error: &str,
    error_description: impl Into<String>,
) -> McpHttpResponse {
    let body = serde_json::json!({
        "error": error,
        "error_description": error_description.into(),
    });
    McpHttpResponse {
        status,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&body).expect("json should serialize"),
        extra_headers: vec![("Cache-Control".to_string(), "no-store".to_string())],
    }
}

#[cfg(feature = "oauth")]
fn oauth_error_response(
    oauth: &McpOAuthMode,
    message: impl Into<String>,
    error: &str,
) -> McpHttpResponse {
    let message = message.into();
    let error_description = escape_www_authenticate_value(&message);
    let mut response = mcp_http_json_error_response(401, message, Value::Null);
    response.extra_headers.push((
        "WWW-Authenticate".to_string(),
        format!(
            "Bearer error=\"{error}\", error_description=\"{}\", resource_metadata=\"{}\"",
            error_description,
            oauth_protected_resource_metadata_url(oauth),
        ),
    ));
    response
}

#[cfg(feature = "oauth")]
fn oauth_protected_resource_metadata_url(oauth: &McpOAuthMode) -> &str {
    match oauth {
        McpOAuthMode::External(external) => external.protected_resource_metadata_url(),
        McpOAuthMode::Local(local) => local.protected_resource_metadata_url(),
    }
}

#[cfg(feature = "oauth")]
fn escape_www_authenticate_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn normalize_mcp_http_endpoint(endpoint: &str) -> String {
    if endpoint.is_empty() || endpoint == "/" {
        "/mcp".to_string()
    } else if endpoint.starts_with('/') {
        endpoint.to_string()
    } else {
        format!("/{endpoint}")
    }
}

fn bearer_or_shared_token(headers: &BTreeMap<String, String>) -> Option<String> {
    if let Some(token) = bearer_token(headers) {
        return Some(token);
    }
    headers.get("x-vulcan-token").cloned()
}

fn bearer_token(headers: &BTreeMap<String, String>) -> Option<String> {
    if let Some(value) = headers.get("authorization") {
        if let Some(token) = value.strip_prefix("Bearer ") {
            return Some(token.to_string());
        }
    }
    None
}

fn mcp_http_json_error_response(
    status: u16,
    message: impl Into<String>,
    id: Value,
) -> McpHttpResponse {
    let body = jsonrpc_error(id, -32600, message.into(), None);
    McpHttpResponse {
        status,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&body).expect("json should serialize"),
        extra_headers: Vec::new(),
    }
}

#[cfg(all(test, feature = "oauth"))]
fn parse_mcp_oauth_scopes(scope: Option<&str>) -> Result<Vec<String>, McpHttpResponse> {
    parse_mcp_oauth_scopes_policy(scope).map_err(mcp_oauth_policy_error_response)
}

#[cfg(all(test, feature = "oauth"))]
fn mcp_oauth_policy_error_response(error: McpOAuthPolicyError) -> McpHttpResponse {
    match error {
        McpOAuthPolicyError::InvalidScope => oauth_json_error_response(
            400,
            "invalid_scope",
            "requested OAuth scope is empty, unsupported, or too large",
        ),
        McpOAuthPolicyError::InvalidAuthorizationRequest => {
            oauth_plain_response(400, "invalid OAuth authorization request")
        }
    }
}

fn build_mcp_tool_registry_entries(
    paths: &VaultPaths,
    requested_profile: Option<&str>,
    selected_tool_packs: &BTreeSet<McpToolPack>,
) -> Result<Vec<ToolRegistryEntry>, CliError> {
    let selection =
        resolve_permission_profile(paths, requested_profile).map_err(permission_error_to_cli)?;
    let mut tools = visible_tool_catalog(selected_tool_packs, &selection.profile)
        .into_iter()
        .map(mcp_tool_registry_entry)
        .collect::<Vec<_>>();
    tools.extend(
        visible_custom_tools(paths, requested_profile, selected_tool_packs)?
            .iter()
            .map(custom_tool_registry_entry),
    );
    Ok(tools)
}

fn visible_custom_tools(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    selected_tool_packs: &BTreeSet<McpToolPack>,
) -> Result<Vec<CustomToolDescriptor>, CliError> {
    if !selected_tool_packs.contains(&McpToolPack::Custom) {
        return Ok(Vec::new());
    }
    let selected_pack_names = pack_name_list(selected_tool_packs)
        .into_iter()
        .collect::<BTreeSet<_>>();
    Ok(app_tools::list_custom_tools(
        paths,
        active_permission_profile,
        &crate::custom_tool_registry_options(),
    )
    .map_err(CliError::operation)?
    .into_iter()
    .filter(|tool| tool.callable)
    .filter(|tool| {
        mcp_assistant::custom_tool_matches_selected_packs(&tool.summary.packs, &selected_pack_names)
    })
    .collect())
}

fn help_topic_completion_candidates(prefix: &str) -> Vec<String> {
    let mut values = vec!["overview".to_string()];
    values.extend(
        collect_help_command_topics(&cli_command_tree())
            .into_iter()
            .map(|topic| topic.name.replace(' ', "/")),
    );
    values.extend(
        [
            "getting-started",
            "examples",
            "filters",
            "query-dsl",
            "scripting",
            "sandbox",
            "js",
            "js.vault",
            "js.vault.graph",
            "js.vault.note",
            "js.plugins",
            "reports",
        ]
        .into_iter()
        .map(ToOwned::to_owned),
    );
    values.sort();
    values.dedup();
    values.retain(|value| value.starts_with(prefix));
    values
}

#[cfg(test)]
mod tests;
