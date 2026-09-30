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
use serde_json::Value;
#[cfg(feature = "oauth")]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, BufRead};
use std::net::{SocketAddr, TcpStream};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(feature = "oauth")]
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::Duration;
#[cfg(feature = "oauth")]
use std::time::Instant;
#[cfg(feature = "oauth")]
use std::time::{SystemTime, UNIX_EPOCH};
use ulid::Ulid;
use vulcan_app::execution::ExecutionCancellationToken;
#[cfg(all(test, feature = "oauth"))]
use vulcan_app::execution::{
    ExecutionAuthority, ExecutionIdentity, ExecutionRetryClass, ExecutionVaultIdentity,
};
#[cfg(all(test, feature = "oauth"))]
use vulcan_app::execution::{ExecutionContext, ExecutionDeadline};
use vulcan_app::mcp_assistant;
use vulcan_app::mcp_dispatch::{jsonrpc_error, request_id, McpHttpProcessResult, McpMethodHandler};
use vulcan_app::mcp_help;
use vulcan_app::mcp_protocol::{McpMethodError, McpMethodOutcome, MCP_PROTOCOL_VERSION};
use vulcan_app::mcp_session_protocol::{McpProtocolCore, McpProtocolHost};
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
use vulcan_core::{resolve_permission_profile, watch_vault, VaultPaths, WatchOptions};
#[cfg(feature = "oauth")]
use vulcan_daemon::host::{
    RestartPolicy, ServiceDefinition, ServiceId, ServiceRegistration, ServiceScope,
};
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::hosted_executor::HostedExecutionError;
#[cfg(feature = "oauth")]
use vulcan_daemon::hosted_executor::HostedExecutor;
#[cfg(feature = "oauth")]
use vulcan_daemon::hosted_jobs::HostedJobLedger;
#[cfg(feature = "oauth")]
use vulcan_daemon::http_policy::mcp_oauth_redirect_uri_valid;
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_execution::HostedMcpExecution;
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_execution::{
    attenuate_mcp_core_profile, hosted_mcp_execution_error, hosted_mcp_unknown_result,
};
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_hosted::scheduled_operation;
use vulcan_daemon::mcp_http_auth::McpHttpAuthError;
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_http_auth::McpOAuthMode;
use vulcan_daemon::mcp_http_codec::{write_mcp_http_response, McpHttpRequest, McpHttpResponse};
use vulcan_daemon::mcp_http_host::McpHttpHost;
use vulcan_daemon::mcp_http_routes::{
    dispatch_mcp_http_request, McpHttpRoute, McpHttpRouteHandler, McpHttpRouteOptions,
};
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_authorize::subject_not_allowed_response as indieauth_subject_not_allowed_response;
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_authorize::McpAuthorizeEndpoint;
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_oauth_authorize::{default_indieauth_exchange, IndieAuthExchange};
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_oauth_browser::IndieAuthConfig as LocalOAuthIndieAuthConfig;
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_browser::{
    percent_encode, redirect_to_indieauth as local_oauth_redirect_to_indieauth,
    PendingConsent as LocalOAuthPendingConsent,
};
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_oauth_clients::OAuthClientRegistry;
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_clients::RegisteredOAuthClient as LocalOAuthRegisteredClient;
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_codes::McpAuthorizationCode as LocalOAuthCode;
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_oauth_codes::McpAuthorizationCodeMap;
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_consent::McpConsentEndpoint;
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_policy::parse_mcp_oauth_scopes as parse_mcp_oauth_scopes_policy;
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_policy::McpOAuthPolicyError;
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_policy::{McpTokenAuthMethod, McpTokenClientCredentials};
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_oauth_token::McpLocalTokenEndpoint;
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_remote::McpRemoteAuthentication;
use vulcan_daemon::mcp_remote::McpRemoteDefinition;
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_remote_runtime::{NamedMcpRuntime, NamedMcpVaultRuntime};
#[cfg(test)]
use vulcan_daemon::mcp_session::MAX_MCP_SSE_PENDING_EVENTS;
use vulcan_daemon::mcp_session::{
    McpCancellationError, McpHttpSession as HostedMcpHttpSession, McpSessionAuthority,
    McpSessionRegistry, ResolvedMcpSession, SessionAdmissionError, SessionLookupError,
    SessionResolutionError,
};
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mcp_session::{MAX_MCP_HTTP_SESSIONS, MCP_HTTP_SESSION_IDLE_TIMEOUT};
use vulcan_daemon::mcp_sse::{serve_mcp_sse, McpSseEnd};
#[cfg(feature = "oauth")]
use vulcan_daemon::mcp_state::McpAuthorizationStore;
#[cfg(feature = "oauth")]
use vulcan_daemon::mutation_scheduler::MutationScheduler;
#[cfg(feature = "oauth")]
use vulcan_daemon::mutation_scheduler::MutationSchedulerConfig;
#[cfg(all(test, feature = "oauth"))]
use vulcan_daemon::mutation_scheduler::ScheduledOperation;
use vulcan_daemon::process::DaemonProcessContext;
use vulcan_daemon::shutdown::ShutdownSignal;

pub(crate) const DEFAULT_MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

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
    #[cfg_attr(not(feature = "oauth"), allow(dead_code))]
    pub oauth_credentials: Option<vulcan_daemon::mcp_credentials::McpRemoteCredentials>,
    pub request_timeout: Duration,
}

#[derive(Debug, Clone)]
struct McpServerCore {
    inner: McpProtocolCore,
}

impl Deref for McpServerCore {
    type Target = McpProtocolCore;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for McpServerCore {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

type McpHttpSession = HostedMcpHttpSession<McpServerCore>;

#[cfg(feature = "oauth")]
#[derive(Debug, Clone)]
struct ResidentMcpScheduling {
    scheduler: Arc<MutationScheduler>,
    runtime: tokio::runtime::Handle,
}

#[derive(Debug, Clone)]
struct McpHttpServerContext {
    inner: McpHttpHost<McpServerCore>,
    #[cfg(feature = "oauth")]
    hosted: Option<HostedMcpExecution>,
    #[cfg(all(test, feature = "oauth"))]
    indieauth_exchange: Option<IndieAuthExchange>,
}

impl Deref for McpHttpServerContext {
    type Target = McpHttpHost<McpServerCore>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for McpHttpServerContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
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
        oauth_credentials: Some(vulcan_daemon::mcp_credentials::McpRemoteCredentials::at(
            &process.state_root,
            remote,
        )),
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

#[cfg(feature = "oauth")]
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
        #[cfg(feature = "oauth")]
        hosted: lifecycle.hosted,
        #[cfg(all(test, feature = "oauth"))]
        indieauth_exchange: lifecycle.indieauth_exchange,
        inner: McpHttpHost {
            paths: paths.clone(),
            requested_profile: requested_profile.map(ToOwned::to_owned),
            selected_tool_packs: resolve_selected_tool_packs(
                tool_pack_args,
                McpToolPackMode::from(tool_pack_mode_arg),
            ),
            tool_pack_mode: McpToolPackMode::from(tool_pack_mode_arg),
            endpoint,
            auth_token: options.auth_token.clone(),
            #[cfg(feature = "oauth")]
            oauth,
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
                match options.oauth_credentials.as_ref() {
                    Some(credentials) => OAuthClientRegistry::with_secret_store(
                        oauth_clients_path(paths, options),
                        credentials.client_custody().map_err(CliError::operation)?,
                    ),
                    None => OAuthClientRegistry::at(oauth_clients_path(paths, options)),
                }
                .map_err(CliError::operation)?,
            ),
            #[cfg(feature = "oauth")]
            oauth_pending_indieauth: Arc::new(Mutex::new(BTreeMap::new())),
            #[cfg(feature = "oauth")]
            oauth_pending_consent: Arc::new(Mutex::new(BTreeMap::new())),
            #[cfg(feature = "oauth")]
            oauth_dcr_enabled: options.oauth_dcr,
            #[cfg(feature = "oauth")]
            oauth_dcr_allowed_redirect_hosts: if options.oauth_dcr_allowed_redirect_host.is_empty()
            {
                vec!["chatgpt.com".to_string()]
            } else {
                options.oauth_dcr_allowed_redirect_host.clone()
            },
            #[cfg(feature = "oauth")]
            oauth_local_redirect_uris: options.oauth_local_redirect_uri.clone(),
            #[cfg(feature = "oauth")]
            oauth_indieauth: build_indieauth_config(options)?,
            #[cfg(feature = "oauth")]
            named_runtime,
            request_timeout: options.request_timeout,
        },
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

#[cfg(all(test, feature = "oauth"))]
fn admit_mcp_http_session(
    context: &McpHttpServerContext,
    session_id: String,
    session: Arc<McpHttpSession>,
) -> Result<(), McpHttpResponse> {
    context
        .sessions
        .admit(session_id, session)
        .map_err(session_admission_response)
}

fn session_admission_response(error: SessionAdmissionError) -> McpHttpResponse {
    match error {
        SessionAdmissionError::Capacity => {
            let mut response = mcp_http_json_error_response(
                503,
                "MCP session limit reached; close unused sessions or retry later",
                Value::Null,
            );
            response
                .extra_headers
                .push(("Retry-After".to_string(), "60".to_string()));
            response
        }
        SessionAdmissionError::DuplicateId => {
            mcp_http_json_error_response(500, "MCP session ID collision", Value::Null)
        }
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
        .map_err(session_lookup_response)
}

fn session_lookup_response(error: SessionLookupError) -> McpHttpResponse {
    match error {
        SessionLookupError::Missing => {
            mcp_http_json_error_response(404, "unknown Mcp-Session-Id", Value::Null)
        }
        SessionLookupError::AuthorityMismatch => mcp_http_json_error_response(
            403,
            "MCP session authority does not match this request",
            Value::Null,
        ),
    }
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
    dispatch_mcp_http_request(
        request,
        stream,
        &McpHttpRouteOptions {
            endpoint: &context.endpoint,
            oauth_enabled,
            local_oauth,
            named_remote,
        },
        context,
    )
    .map_err(CliError::operation)
}

impl McpHttpRouteHandler for McpHttpServerContext {
    type Authority = McpSessionAuthority;

    fn oauth(&self, request: &McpHttpRequest, route: McpHttpRoute<'_>) -> McpHttpResponse {
        #[cfg(feature = "oauth")]
        {
            handle_mcp_oauth_route(self, request, route)
        }
        #[cfg(not(feature = "oauth"))]
        {
            let _ = (request, route);
            mcp_http_json_error_response(404, "Not Found", Value::Null)
        }
    }

    fn authenticate(&self, request: &McpHttpRequest) -> Result<Self::Authority, McpHttpResponse> {
        authenticate_mcp_http_request(self, request)
    }

    fn authorize_scope(
        &self,
        authority: &Self::Authority,
        required: &str,
    ) -> Result<(), McpHttpResponse> {
        if authority.allows_scope(required) {
            Ok(())
        } else {
            Err(insufficient_scope_response(self, required))
        }
    }

    fn operation_status(&self, authority: &Self::Authority, operation_id: &str) -> McpHttpResponse {
        #[cfg(feature = "oauth")]
        {
            handle_named_mcp_operation_status(self, authority, operation_id)
        }
        #[cfg(not(feature = "oauth"))]
        {
            let _ = (authority, operation_id);
            mcp_http_json_error_response(404, "Not Found", Value::Null)
        }
    }

    fn post(
        &self,
        request: &McpHttpRequest,
        authority: &Self::Authority,
        payload: &Value,
    ) -> McpHttpResponse {
        handle_mcp_http_post(self, request, authority, payload)
    }

    fn sse(
        &self,
        request: &McpHttpRequest,
        authority: &Self::Authority,
        stream: &mut TcpStream,
    ) -> io::Result<()> {
        handle_mcp_http_sse(self, request, authority, stream).map_err(io::Error::other)
    }

    fn delete(&self, request: &McpHttpRequest, authority: &Self::Authority) -> McpHttpResponse {
        handle_mcp_http_delete(self, request, authority)
    }
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
    let (Some(hosted), Some(named)) = (context.hosted.as_ref(), context.named_runtime.as_ref())
    else {
        return not_found();
    };
    let Some(report) = vulcan_daemon::mcp_hosted::named_mcp_operation_status(
        &hosted.executor.ledger(),
        named,
        authority,
        operation_id,
    ) else {
        return not_found();
    };
    McpHttpResponse {
        status: 200,
        content_type: Some("application/json"),
        body: serde_json::to_vec(&report).expect("operation status should serialize"),
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
    payload: &Value,
) -> McpHttpResponse {
    let resolved = match resolve_mcp_http_session(context, request, payload, authority) {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };
    let session = Arc::clone(&resolved.session);

    if payload.get("method").and_then(Value::as_str) == Some("notifications/cancelled") {
        return handle_mcp_cancellation_notification(payload, &session);
    }

    let Some(active_request) = session.start_request(request_id(payload).as_ref()) else {
        return mcp_http_json_error_response(
            409,
            "MCP request ID is already active in this session",
            Value::Null,
        );
    };

    let result = {
        let mut core = session
            .core
            .lock()
            .expect("mcp core lock should not be poisoned");
        core.process_http_request_with_timeout(
            payload.clone(),
            context.request_timeout,
            context,
            request,
            authority,
            active_request.cancellation(),
        )
    };

    drop(active_request);
    context.sessions.finish_http_post(resolved, result)
}

fn handle_mcp_cancellation_notification(
    payload: &Value,
    session: &McpHttpSession,
) -> McpHttpResponse {
    if let Err(error) = session.cancel_notification(payload) {
        let message = match error {
            McpCancellationError::MissingRequestId => "MCP cancellation requires params.requestId",
            McpCancellationError::InvalidRequestId => {
                "MCP cancellation requestId must be a string or number"
            }
        };
        return mcp_http_json_error_response(400, message, Value::Null);
    }
    McpHttpResponse {
        status: 202,
        content_type: None,
        body: Vec::new(),
        extra_headers: Vec::new(),
    }
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
) -> Result<ResolvedMcpSession<McpServerCore>, McpHttpResponse> {
    let resolved = context
        .sessions
        .resolve_http(
            payload,
            request.headers.get("mcp-session-id").map(String::as_str),
            authority,
            || create_mcp_http_core(context, authority),
        )
        .map_err(|error| match error {
            SessionResolutionError::InvalidInitialize(error) => McpHttpResponse {
                status: 400,
                content_type: Some("application/json"),
                body: serde_json::to_vec(&error).expect("JSON-RPC error should serialize"),
                extra_headers: Vec::new(),
            },
            SessionResolutionError::MissingSessionId => {
                mcp_http_json_error_response(400, "missing Mcp-Session-Id header", Value::Null)
            }
            SessionResolutionError::Admission(error) => session_admission_response(error),
            SessionResolutionError::Lookup(error) => session_lookup_response(error),
            SessionResolutionError::Create(response) => response,
        })?;
    Ok(resolved)
}

fn create_mcp_http_core(
    context: &McpHttpServerContext,
    authority: &McpSessionAuthority,
) -> Result<McpServerCore, McpHttpResponse> {
    let config = context
        .inner
        .protocol_config(authority)
        .map_err(|error| mcp_http_json_error_response(error.status, error.message, Value::Null))?;
    McpServerCore::new_resolved(
        &config.paths,
        config.permission_profile.as_deref(),
        config.selected_tool_packs,
        config.tool_pack_mode,
    )
    .map_err(|error| mcp_http_json_error_response(500, error.to_string(), Value::Null))
}

fn handle_mcp_http_delete(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
    authority: &McpSessionAuthority,
) -> McpHttpResponse {
    let session_id = request
        .headers
        .get("mcp-session-id")
        .expect("daemon route preflight requires MCP session header");
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
    let session_id = request
        .headers
        .get("mcp-session-id")
        .expect("daemon route preflight requires MCP session header");
    let session = match authorized_mcp_http_session(context, session_id, authority, true) {
        Ok(session) => session,
        Err(response) => {
            write_mcp_http_response(stream, &response).map_err(CliError::operation)?;
            return Ok(());
        }
    };

    let end = serve_mcp_sse(
        &session,
        stream,
        || {
            authenticate_mcp_http_request(context, request)
                .is_ok_and(|current| session.authority.matches(&current))
                && session
                    .core
                    .lock()
                    .expect("mcp core lock should not be poisoned")
                    .session
                    .attenuate_profile()
                    .is_ok()
        },
        || {
            session
                .core
                .lock()
                .expect("mcp core lock should not be poisoned")
                .list_changed_notifications()
        },
    )
    .map_err(CliError::operation)?;
    if end == McpSseEnd::RetireSession {
        context.sessions.retire(session_id);
    }
    Ok(())
}

fn authenticate_mcp_http_request(
    context: &McpHttpServerContext,
    request: &McpHttpRequest,
) -> Result<McpSessionAuthority, McpHttpResponse> {
    context
        .inner
        .authenticate(&request.headers)
        .map_err(|error| match error {
            McpHttpAuthError::Http { status, message } => {
                mcp_http_json_error_response(status, message, Value::Null)
            }
            #[cfg(feature = "oauth")]
            McpHttpAuthError::OAuth {
                message,
                rejected_bearer,
            } => {
                if rejected_bearer {
                    eprintln!("MCP OAuth bearer token rejected: {message}");
                }
                oauth_error_response(
                    context
                        .oauth
                        .as_ref()
                        .expect("OAuth rejection has an issuer"),
                    message,
                    "invalid_token",
                )
            }
        })
}

impl McpServerCore {
    fn new(
        paths: &VaultPaths,
        requested_profile: Option<&str>,
        tool_pack_args: &[McpToolPackArg],
        tool_pack_mode_arg: McpToolPackModeArg,
    ) -> Result<Self, CliError> {
        let tool_pack_mode = McpToolPackMode::from(tool_pack_mode_arg);
        let selected_tool_packs = resolve_selected_tool_packs(tool_pack_args, tool_pack_mode);
        Self::new_resolved(
            paths,
            requested_profile,
            selected_tool_packs,
            tool_pack_mode,
        )
    }

    fn new_resolved(
        paths: &VaultPaths,
        requested_profile: Option<&str>,
        selected_tool_packs: BTreeSet<McpToolPack>,
        tool_pack_mode: McpToolPackMode,
    ) -> Result<Self, CliError> {
        Ok(Self {
            inner: McpProtocolCore::new(
                paths,
                requested_profile,
                selected_tool_packs,
                tool_pack_mode,
                McpProtocolHost {
                    registry_options: crate::custom_tool_registry_options,
                    command_help: resolve_command_help_for_mcp,
                    help_candidates: help_topic_completion_candidates,
                    server_version: env!("CARGO_PKG_VERSION"),
                },
            )
            .map_err(permission_error_to_cli)?,
        })
    }

    fn process_request_with_timeout(&mut self, request: Value, timeout: Duration) -> Vec<Value> {
        vulcan_daemon::mcp_execution::process_request_with_timeout(self, request, timeout)
    }

    #[cfg(test)]
    fn process_request(&mut self, request: Value) -> Vec<Value> {
        self.inner.process_request(request)
    }

    #[cfg(test)]
    fn process_http_request(&mut self, request: &Value) -> Result<McpHttpProcessResult, Value> {
        self.inner.process_http_request(request)
    }

    fn process_http_request_with_timeout(
        &mut self,
        request: Value,
        timeout: Duration,
        http_context: &McpHttpServerContext,
        inbound: &McpHttpRequest,
        authority: &McpSessionAuthority,
        cancellation: ExecutionCancellationToken,
    ) -> Result<McpHttpProcessResult, Value> {
        #[cfg(feature = "oauth")]
        let hosted = http_context.hosted.as_ref();
        #[cfg(not(feature = "oauth"))]
        let hosted = None;
        vulcan_daemon::mcp_execution::process_http_request_with_timeout(
            self,
            request,
            timeout,
            &http_context.inner,
            inbound,
            authority,
            &cancellation,
            hosted,
        )
    }
}

impl McpMethodHandler for McpServerCore {
    fn handle_method(
        &mut self,
        method: &str,
        params: Option<&Value>,
    ) -> Result<McpMethodOutcome, McpMethodError> {
        self.inner.handle_method(method, params)
    }

    fn list_changed_notifications(&mut self) -> Vec<Value> {
        self.inner.list_changed_notifications()
    }
}

impl vulcan_daemon::mcp_execution::McpRequestCore for McpServerCore {
    fn vault_paths(&self) -> &VaultPaths {
        self.session.paths()
    }

    fn permission_grant(&self) -> vulcan_core::PermissionGrant {
        self.session.selection().grant.clone()
    }

    fn attenuate_profile(&mut self) -> Result<(), String> {
        self.session.attenuate_profile()
    }

    fn process_request(&mut self, request: Value) -> Vec<Value> {
        self.inner.process_request(request)
    }

    fn process_http_request(&mut self, request: &Value) -> Result<McpHttpProcessResult, Value> {
        self.inner.process_http_request(request)
    }
}

fn resolve_command_help_for_mcp(
    topic_path: &[String],
) -> Result<mcp_help::HelpTopicReport, String> {
    resolve_help_topic(topic_path).map_err(|error| error.message)
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
    #[cfg(test)]
    let exchange = context
        .indieauth_exchange
        .unwrap_or(default_indieauth_exchange);
    #[cfg(not(test))]
    let exchange = default_indieauth_exchange;
    context
        .inner
        .oauth_routes(exchange)
        .expect("OAuth route requires issuer")
        .handle(request, route)
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

#[cfg(all(test, feature = "oauth"))]
fn local_token_endpoint<'a>(
    context: &'a McpHttpServerContext,
    issuer: &'a LocalOAuthIssuer,
) -> McpLocalTokenEndpoint<'a> {
    context.inner.token_endpoint(issuer)
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

#[cfg(all(test, feature = "oauth"))]
fn local_authorize_endpoint<'a>(
    context: &'a McpHttpServerContext,
    issuer: &'a LocalOAuthIssuer,
) -> McpAuthorizeEndpoint<'a> {
    context.inner.authorize_endpoint(
        issuer,
        context
            .indieauth_exchange
            .unwrap_or(default_indieauth_exchange),
    )
}

#[cfg(all(test, feature = "oauth"))]
fn local_consent_endpoint<'a>(
    context: &'a McpHttpServerContext,
    issuer: &'a LocalOAuthIssuer,
) -> McpConsentEndpoint<'a> {
    context.inner.consent_endpoint(issuer)
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
    if let Some(credentials) = options.oauth_credentials.as_ref() {
        return credentials.issuer_secret().map_err(CliError::operation);
    }
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
    if let Some(credentials) = options.oauth_credentials.as_ref() {
        return credentials.signing_key().map_err(CliError::operation);
    }
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
    use std::io::Write;
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
    mcp_help::help_completion_candidates(&collect_help_command_topics(&cli_command_tree()), prefix)
}

#[cfg(test)]
mod tests;
