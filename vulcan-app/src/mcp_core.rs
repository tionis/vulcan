//! The stateless MCP core: one request in, one outcome out.
//!
//! [`handle`] serves a method for a [`McpRequestContext`]: who the request
//! acts as ([`McpAuthority`]), who sent it ([`McpClientAttribution`]), and the
//! protocol era. Nothing survives the call. A request whose `_meta` names a
//! protocol version is modern (2026-07-28) and carries its own client
//! information; anything else belongs to a legacy connection, whose
//! `initialize` handshake [`McpLegacyConnection`] keeps.

use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};

use crate::mcp_assistant::{
    prompt_files_fingerprint, resource_files_fingerprint, tool_catalog_fingerprint,
};
use crate::mcp_authority::{McpAuthority, McpClientAttribution};
use crate::mcp_discovery::McpDiscovery;
use crate::mcp_help::HelpTopicReport;
use crate::mcp_input::{
    ambiguity_input, apply_input_responses, call_is_destructive, confirmation_input,
    confirmation_refused, declares_form_elicitation, InputAnswers,
};
use crate::mcp_operations::McpOperationLookup;
use crate::mcp_progress::McpProgress;
use crate::mcp_protocol::{
    server_instructions, tool_error_structured, McpCompletionParams, McpListParams,
    McpListSnapshot, McpMethodError, McpPromptGetParams, McpResourceReadParams, McpToolCallParams,
    MCP_PAGE_SIZE, MCP_RESOURCE_NOT_FOUND,
};
use crate::mcp_skills::{
    get_skill, read_skill_directory, skill_entries, MCP_SKILLS_EXTENSION, SKILL_TTL_MS,
};
use crate::mcp_task_ext::{handle_task_method, MCP_TASKS_EXTENSION};

/// The stateless protocol revision.
pub const MCP_MODERN_VERSION: &str = "2026-07-28";
/// Revisions served through the `initialize` handshake, newest first.
pub const MCP_LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18"];

const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// JSON-RPC error for a protocol version the server does not speak.
pub const MCP_UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
/// JSON-RPC error for HTTP headers that contradict the request body.
pub const MCP_HEADER_MISMATCH: i64 = -32020;

/// The protocol version a request's `_meta` names, if it names one: such a
/// request is modern and served without a session.
#[must_use]
pub fn requested_modern_version(request: &Value) -> Option<&Value> {
    request
        .get("params")
        .and_then(|params| params.get("_meta"))
        .and_then(|meta| meta.get(META_PROTOCOL_VERSION))
}

/// The JSON-RPC error for a protocol version the server does not speak.
#[must_use]
pub fn unsupported_version_error(id: Value, requested: &str) -> Value {
    crate::mcp_dispatch::jsonrpc_error(
        id,
        MCP_UNSUPPORTED_PROTOCOL_VERSION,
        "Unsupported protocol version".to_string(),
        Some(unsupported_version_data(requested)),
    )
}

/// How long a client may reuse a list: the lists depend on the vault's
/// configuration, which changes rarely and is announced when it does.
const LIST_TTL_MS: u64 = 60_000;

/// What the host contributes: CLI help for `vulcan://help/*` and the server
/// version.
#[derive(Debug, Clone, Copy)]
pub struct McpProtocolHost {
    pub command_help: fn(&[String]) -> Result<HelpTopicReport, String>,
    pub help_candidates: fn(&str) -> Vec<String>,
    pub server_version: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpEra {
    /// 2025-06-18 or 2025-11-25, after `initialize`.
    Legacy,
    /// 2026-07-28: every request carries its own `_meta`.
    Modern,
}

/// Everything one request needs; nothing outlives it.
pub struct McpRequestContext<'a> {
    pub authority: &'a McpAuthority,
    pub client: &'a McpClientAttribution,
    pub era: McpEra,
    pub host: &'a McpProtocolHost,
    /// Operation records, where the transport keeps them.
    pub operations: Option<&'a dyn McpOperationLookup>,
    /// Whether the transport serves `subscriptions/listen`.
    pub subscriptions: bool,
    /// Progress for this request, when it asked and the host can deliver.
    pub progress: Option<&'a McpProgress>,
}

/// The `_meta` of a modern request, once validated.
#[derive(Debug, Clone, PartialEq)]
pub struct McpModernMeta {
    pub protocol_version: String,
    pub client_capabilities: Value,
    pub client_info: Option<Value>,
}

/// Whether `params` is a modern request, and if so its validated `_meta`.
/// A request without a protocol version in `_meta` is legacy (`Ok(None)`).
pub fn modern_meta(params: Option<&Value>) -> Result<Option<McpModernMeta>, McpMethodError> {
    let Some(meta) = params
        .and_then(|params| params.get("_meta"))
        .and_then(Value::as_object)
    else {
        return Ok(None);
    };
    let Some(version) = meta.get(META_PROTOCOL_VERSION) else {
        return Ok(None);
    };
    let Some(version) = version.as_str() else {
        return Err(McpMethodError::invalid_params(format!(
            "`_meta[\"{META_PROTOCOL_VERSION}\"]` must be a string"
        )));
    };
    if version != MCP_MODERN_VERSION {
        return Err(unsupported_version(version));
    }
    let Some(capabilities) = meta
        .get(META_CLIENT_CAPABILITIES)
        .filter(|value| value.is_object())
    else {
        return Err(McpMethodError::invalid_params(format!(
            "a {MCP_MODERN_VERSION} request needs `_meta[\"{META_CLIENT_CAPABILITIES}\"]` (an object)"
        )));
    };
    Ok(Some(McpModernMeta {
        protocol_version: version.to_string(),
        client_capabilities: capabilities.clone(),
        client_info: meta.get(META_CLIENT_INFO).cloned(),
    }))
}

fn unsupported_version(requested: &str) -> McpMethodError {
    McpMethodError::JsonRpc {
        code: MCP_UNSUPPORTED_PROTOCOL_VERSION,
        message: "Unsupported protocol version".to_string(),
        data: Some(unsupported_version_data(requested)),
    }
}

fn unsupported_version_data(requested: &str) -> Value {
    json!({
        "supported": supported_versions(),
        "requested": requested,
    })
}

/// Whether the server speaks `version`, in either era.
#[must_use]
pub fn is_supported_version(version: &str) -> bool {
    version == MCP_MODERN_VERSION || MCP_LEGACY_VERSIONS.contains(&version)
}

fn supported_versions() -> Vec<&'static str> {
    std::iter::once(MCP_MODERN_VERSION)
        .chain(MCP_LEGACY_VERSIONS.iter().copied())
        .collect()
}

fn server_info(host: &McpProtocolHost) -> Value {
    json!({
        "name": "vulcan",
        "title": "Vulcan",
        "version": host.server_version,
    })
}

/// The server capabilities. Legacy connections are told about list changes
/// on their own stream; modern clients subscribe with `subscriptions/listen`
/// where the transport serves it.
///
/// The skills extension is a 2026-07-28 feature: modern clients are offered
/// `skills/list`, `skills/get` and `resources/directory/read`; legacy clients
/// find the same `skill://` files through `resources/list`. The tasks
/// extension is offered where the transport keeps operation records.
fn capabilities(era: McpEra, subscriptions: bool, tasks: bool) -> Value {
    let list_changed = era == McpEra::Legacy || subscriptions;
    let mut capabilities = json!({
        "tools": { "listChanged": list_changed },
        "resources": { "listChanged": list_changed },
        "prompts": { "listChanged": list_changed },
        "completions": {},
    });
    if era == McpEra::Modern {
        capabilities["extensions"] = json!({
            MCP_SKILLS_EXTENSION: { "directoryRead": true },
        });
        if tasks {
            capabilities["extensions"][MCP_TASKS_EXTENSION] = json!({});
        }
    }
    capabilities
}

const META_SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";

/// The list changes a `subscriptions/listen` stream delivers: what the client
/// asked for that the server can send. Resource update subscriptions are not
/// offered, so they are left out of the acknowledgement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McpSubscriptionFilter {
    pub tools: bool,
    pub prompts: bool,
    pub resources: bool,
}

impl McpSubscriptionFilter {
    /// Whether a list-change notification is one the client asked for.
    #[must_use]
    pub fn wants(&self, notification: &Value) -> bool {
        match notification.get("method").and_then(Value::as_str) {
            Some("notifications/tools/list_changed") => self.tools,
            Some("notifications/prompts/list_changed") => self.prompts,
            Some("notifications/resources/list_changed") => self.resources,
            _ => false,
        }
    }

    fn acknowledged(self) -> Value {
        let mut acknowledged = serde_json::Map::new();
        for (key, wanted) in [
            ("toolsListChanged", self.tools),
            ("promptsListChanged", self.prompts),
            ("resourcesListChanged", self.resources),
        ] {
            if wanted {
                acknowledged.insert(key.to_string(), Value::Bool(true));
            }
        }
        Value::Object(acknowledged)
    }
}

/// Validate a `subscriptions/listen` request and read what it asks for. It
/// is a 2026-07-28 method; a legacy client has its GET stream instead.
pub fn listen_request(params: Option<&Value>) -> Result<McpSubscriptionFilter, McpMethodError> {
    if modern_meta(params)?.is_none() {
        return Err(McpMethodError::JsonRpc {
            code: -32601,
            message: format!(
                "`subscriptions/listen` is a {MCP_MODERN_VERSION} method; 2025 clients receive list changes on their session's GET stream"
            ),
            data: None,
        });
    }
    let requested = params.and_then(|params| params.get("notifications"));
    if requested.is_some_and(|requested| !requested.is_object()) {
        return Err(McpMethodError::invalid_params(
            "`notifications` must be an object naming the notifications to receive",
        ));
    }
    let flag = |key: &str| {
        requested
            .and_then(|requested| requested.get(key))
            .and_then(Value::as_bool)
            == Some(true)
    };
    Ok(McpSubscriptionFilter {
        tools: flag("toolsListChanged"),
        prompts: flag("promptsListChanged"),
        resources: flag("resourcesListChanged"),
    })
}

fn subscription_meta(id: &Value) -> Value {
    json!({ META_SUBSCRIPTION_ID: id })
}

/// The first message on a subscription stream: what it will deliver.
#[must_use]
pub fn subscription_acknowledged(id: &Value, filter: McpSubscriptionFilter) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/subscriptions/acknowledged",
        "params": {
            "_meta": subscription_meta(id),
            "notifications": filter.acknowledged(),
        },
    })
}

/// A notification delivered on the subscription `id`.
#[must_use]
pub fn subscription_notification(id: &Value, mut notification: Value) -> Value {
    if !notification.get("params").is_some_and(Value::is_object) {
        notification["params"] = json!({});
    }
    notification["params"]["_meta"] = subscription_meta(id);
    notification
}

/// The response that ends a subscription the server closes on its own.
#[must_use]
pub fn subscription_closed(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "resultType": "complete", "_meta": subscription_meta(id) },
    })
}

/// Serve one method. `None` is the outcome of a notification.
pub fn handle(
    ctx: &McpRequestContext<'_>,
    method: &str,
    params: Option<&Value>,
) -> Result<Option<Value>, McpMethodError> {
    let confirm = method == "tools/call"
        && call_is_destructive(params)
        && ctx.authority.confirm_destructive();
    if ctx.era == McpEra::Legacy {
        // A 2025 client is never asked: it cannot answer a form here.
        if confirm {
            return Err(McpMethodError::tool(confirmation_refused(params, false)));
        }
        return dispatch(ctx, method, params);
    }
    // A modern tool call may be a retry answering an input request, and
    // may be answered with one when it names a note ambiguously or makes a
    // destructive change the vault asks to confirm.
    let mut call_params = None;
    let mut answers = InputAnswers::default();
    if method == "tools/call" {
        let mut owned = params.cloned().unwrap_or_else(|| json!({}));
        answers = apply_input_responses(&mut owned);
        call_params = Some(owned);
    }
    let params = call_params.as_ref().or(params);
    let can_ask = declares_form_elicitation(params);
    let result = if confirm && answers.confirmed != Some(true) {
        if can_ask && answers.confirmed.is_none() {
            return Ok(Some(decorate_modern(
                ctx,
                method,
                confirmation_input(params),
            )));
        }
        Err(McpMethodError::tool(confirmation_refused(params, can_ask)))
    } else {
        dispatch(ctx, method, params)
    };
    if let Err(McpMethodError::Tool { message, .. }) = &result {
        if method == "tools/call" && !answers.declined_note && can_ask {
            let arguments = params
                .and_then(|params| params.get("arguments"))
                .and_then(Value::as_object);
            if let Some(input) = ambiguity_input(ctx.authority, message, arguments) {
                return Ok(Some(decorate_modern(ctx, method, input)));
            }
        }
    }
    // A modern client gets tool failures as results like any other, and
    // every result marked as complete with the server named.
    let result = match result {
        Err(McpMethodError::Tool {
            message,
            structured,
        }) => Ok(Some(tool_error_result(&message, structured))),
        Err(McpMethodError::JsonRpc {
            code: MCP_RESOURCE_NOT_FOUND,
            message,
            data,
        }) => Err(McpMethodError::JsonRpc {
            code: -32602,
            message,
            data,
        }),
        other => other,
    }?;
    Ok(result.map(|value| decorate_modern(ctx, method, value)))
}

fn dispatch(
    ctx: &McpRequestContext<'_>,
    method: &str,
    params: Option<&Value>,
) -> Result<Option<Value>, McpMethodError> {
    let authority = ctx.authority;
    let discovery = || McpDiscovery {
        operations: ctx.operations.is_some(),
        ..authority.discovery()
    };
    Ok(Some(match method {
        "server/discover" => discover_result(ctx),
        "notifications/cancelled" => return Ok(None),
        "tools/list" => paginated(
            "tools",
            discovery().visible_tool_items()?,
            parse::<McpListParams>(params)?.cursor,
        )?,
        "tools/call" => {
            let params: McpToolCallParams = parse(params)?;
            authority.call_tool_with(
                ctx.client,
                ctx.operations,
                ctx.progress,
                &params.name,
                &params.arguments,
            )?
        }
        "prompts/list" => paginated(
            "prompts",
            discovery().visible_prompt_items()?,
            parse::<McpListParams>(params)?.cursor,
        )?,
        "prompts/get" => {
            let params: McpPromptGetParams = parse(params)?;
            authority
                .discovery()
                .get_prompt(&params.name, &params.arguments)?
        }
        "resources/list" => paginated(
            "resources",
            discovery().visible_resources()?,
            parse::<McpListParams>(params)?.cursor,
        )?,
        "resources/templates/list" => paginated(
            "resourceTemplates",
            discovery().visible_resource_templates(),
            parse::<McpListParams>(params)?.cursor,
        )?,
        "resources/read" => {
            let params: McpResourceReadParams = parse(params)?;
            authority
                .discovery()
                .read_resource(&params.uri, ctx.host.command_help)?
        }
        "tasks/get" | "tasks/update" | "tasks/cancel" if ctx.era == McpEra::Legacy => {
            return Err(McpMethodError::method_not_found(format!(
                "`{method}` belongs to the {MCP_TASKS_EXTENSION} extension, offered to {MCP_MODERN_VERSION} clients"
            )));
        }
        "tasks/get" | "tasks/update" | "tasks/cancel" => {
            handle_task_method(method, params, ctx.operations)?
        }
        "skills/list" | "skills/get" | "resources/directory/read" if ctx.era == McpEra::Legacy => {
            return Err(McpMethodError::method_not_found(format!(
                "`{method}` belongs to the {MCP_SKILLS_EXTENSION} extension, offered to {MCP_MODERN_VERSION} clients; read skills through resources/list"
            )));
        }
        "skills/list" => paginated(
            "skills",
            skill_entries(&authority.discovery().visible_skills()?),
            parse::<McpListParams>(params)?.cursor,
        )?,
        "skills/get" => {
            let params: McpResourceReadParams = parse(params)?;
            get_skill(&authority.discovery().visible_skills()?, &params.uri)?
        }
        "resources/directory/read" => {
            let params: McpDirectoryReadParams = parse(params)?;
            paginated(
                "resources",
                read_skill_directory(&authority.discovery().visible_skills()?, &params.uri)?,
                params.cursor,
            )?
        }
        "completion/complete" => {
            let params: McpCompletionParams = parse(params)?;
            authority
                .discovery()
                .complete(&params, &(ctx.host.help_candidates)(""))?
        }
        _ => {
            return Err(McpMethodError::method_not_found(format!(
                "Method not found: {method}"
            )))
        }
    }))
}

/// `resources/directory/read` parameters.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct McpDirectoryReadParams {
    uri: String,
    #[serde(default)]
    cursor: Option<String>,
}

fn discover_result(ctx: &McpRequestContext<'_>) -> Value {
    json!({
        "resultType": "complete",
        "supportedVersions": supported_versions(),
        "capabilities": capabilities(ctx.era, ctx.subscriptions, ctx.operations.is_some()),
        "instructions": server_instructions(&ctx.authority.active_tool_names()),
        "_meta": { META_SERVER_INFO: server_info(ctx.host) },
        "ttlMs": LIST_TTL_MS,
        "cacheScope": "private",
    })
}

fn decorate_modern(ctx: &McpRequestContext<'_>, method: &str, mut value: Value) -> Value {
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    object
        .entry("resultType")
        .or_insert_with(|| Value::from("complete"));
    let meta = object
        .entry("_meta")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(meta) = meta.as_object_mut() {
        meta.insert(META_SERVER_INFO.to_string(), server_info(ctx.host));
    }
    // Every list depends on the caller's authority; a read may change at
    // any moment.
    let ttl = match method {
        "tools/list" | "prompts/list" | "resources/list" | "resources/templates/list" => {
            Some(LIST_TTL_MS)
        }
        "resources/read" => Some(0),
        "skills/list" | "skills/get" | "resources/directory/read" => Some(SKILL_TTL_MS),
        _ => None,
    };
    if let Some(ttl) = ttl {
        object.insert("ttlMs".to_string(), Value::from(ttl));
        object.insert("cacheScope".to_string(), Value::from("private"));
    }
    value
}

/// A tool failure as a `tools/call` result.
#[must_use]
pub fn tool_error_result(message: &str, structured: Option<Value>) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "structuredContent": tool_error_structured(message, structured),
        "isError": true,
    })
}

pub(crate) fn parse<T: DeserializeOwned>(params: Option<&Value>) -> Result<T, McpMethodError> {
    let mut params = params.cloned().unwrap_or_else(|| Value::Object(Map::new()));
    if let Value::Object(object) = &mut params {
        object.remove("_meta");
    }
    serde_json::from_value(params)
        .map_err(|error| McpMethodError::invalid_params(error.to_string()))
}

fn paginated(
    key: &str,
    items: Vec<Value>,
    cursor: Option<String>,
) -> Result<Value, McpMethodError> {
    let start = match cursor {
        Some(cursor) if !cursor.is_empty() => cursor.parse::<usize>().map_err(|_| {
            McpMethodError::invalid_params(format!("invalid pagination cursor `{cursor}`"))
        })?,
        _ => 0,
    };
    if start > items.len() {
        return Err(McpMethodError::invalid_params(format!(
            "pagination cursor `{start}` is out of range"
        )));
    }
    let total = items.len();
    let end = usize::min(start + MCP_PAGE_SIZE, total);
    let page = items.into_iter().skip(start).take(end - start).collect();
    let mut result = Map::new();
    result.insert(key.to_string(), Value::Array(page));
    if end < total {
        result.insert("nextCursor".to_string(), Value::String(end.to_string()));
    }
    Ok(Value::Object(result))
}

/// What a legacy (2025) client expects a connection to remember: the
/// version it negotiated, what it said about itself in `initialize`, and
/// which list changes it has been told about.
#[derive(Debug, Clone)]
pub struct McpLegacyConnection {
    version: Option<String>,
    client: McpClientAttribution,
    snapshot: McpListSnapshot,
}

impl McpLegacyConnection {
    #[must_use]
    pub fn new(authority: &McpAuthority) -> Self {
        Self {
            version: None,
            client: McpClientAttribution::default(),
            snapshot: list_snapshot(authority),
        }
    }

    /// The version `initialize` settled on, if it has run.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// The client as it described itself in `initialize`.
    #[must_use]
    pub fn client(&self) -> &McpClientAttribution {
        &self.client
    }

    /// Answer `initialize`: the client's version when the server speaks it,
    /// otherwise the newest legacy version.
    pub fn initialize(
        &mut self,
        authority: &McpAuthority,
        host: &McpProtocolHost,
        params: Option<&Value>,
    ) -> Value {
        self.client.record_client_info(params);
        let requested = params
            .and_then(|params| params.get("protocolVersion"))
            .and_then(Value::as_str);
        let version = requested
            .filter(|version| MCP_LEGACY_VERSIONS.contains(version))
            .unwrap_or(MCP_LEGACY_VERSIONS[0]);
        self.version = Some(version.to_string());
        json!({
            "protocolVersion": version,
            "capabilities": capabilities(McpEra::Legacy, false, false),
            "serverInfo": server_info(host),
            "instructions": server_instructions(&authority.active_tool_names()),
        })
    }

    /// `list_changed` notifications for what changed since the last call.
    pub fn list_changed_notifications(&mut self, authority: &McpAuthority) -> Vec<Value> {
        self.snapshot
            .changed_notifications(list_snapshot(authority))
    }
}

fn list_snapshot(authority: &McpAuthority) -> McpListSnapshot {
    let selection = authority.selection();
    McpListSnapshot {
        tools: tool_catalog_fingerprint(
            authority.paths(),
            Some(selection.name.as_str()),
            &selection.profile,
        ),
        prompts: prompt_files_fingerprint(authority.paths(), authority.guard()),
        resources: resource_files_fingerprint(authority.paths(), authority.guard()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vulcan_core::VaultPaths;

    fn host() -> McpProtocolHost {
        McpProtocolHost {
            command_help: |_| Err("none".to_string()),
            help_candidates: |_| Vec::new(),
            server_version: "9.9.9",
        }
    }

    fn modern_params(extra: &Value) -> Value {
        let mut params = json!({
            "_meta": {
                META_PROTOCOL_VERSION: MCP_MODERN_VERSION,
                META_CLIENT_CAPABILITIES: {},
                META_CLIENT_INFO: { "name": "Probe", "version": "1" },
            }
        });
        if let (Some(params), Some(extra)) = (params.as_object_mut(), extra.as_object()) {
            params.extend(extra.clone());
        }
        params
    }

    #[test]
    fn listen_requests_acknowledge_what_can_be_delivered() {
        let params = modern_params(&json!({"notifications": {
            "toolsListChanged": true,
            "promptsListChanged": false,
            "resourceSubscriptions": ["vulcan://note/Home.md"],
        }}));
        let filter = listen_request(Some(&params)).unwrap();
        assert_eq!(
            filter,
            McpSubscriptionFilter {
                tools: true,
                prompts: false,
                resources: false,
            }
        );
        let ack = subscription_acknowledged(&json!(7), filter);
        assert_eq!(ack["method"], "notifications/subscriptions/acknowledged");
        assert_eq!(ack["params"]["_meta"][META_SUBSCRIPTION_ID], 7);
        assert_eq!(
            ack["params"]["notifications"],
            json!({"toolsListChanged": true})
        );

        let tools = json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
        let prompts = json!({"jsonrpc": "2.0", "method": "notifications/prompts/list_changed"});
        assert!(filter.wants(&tools));
        assert!(!filter.wants(&prompts));
        let tagged = subscription_notification(&json!(7), tools);
        assert_eq!(tagged["params"]["_meta"][META_SUBSCRIPTION_ID], 7);
        assert_eq!(
            subscription_closed(&json!(7))["result"]["resultType"],
            "complete"
        );

        assert!(matches!(
            listen_request(Some(&json!({}))),
            Err(McpMethodError::JsonRpc { code: -32601, .. })
        ));
        assert!(matches!(
            listen_request(Some(&modern_params(&json!({"notifications": true})))),
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));
    }

    #[test]
    fn modern_requests_are_recognized_and_validated() {
        assert_eq!(modern_meta(None).unwrap(), None);
        assert_eq!(modern_meta(Some(&json!({"_meta": {}}))).unwrap(), None);
        let meta = modern_meta(Some(&modern_params(&json!({}))))
            .unwrap()
            .unwrap();
        assert_eq!(meta.protocol_version, MCP_MODERN_VERSION);
        assert_eq!(meta.client_info.unwrap()["name"], "Probe");

        let missing = json!({"_meta": {META_PROTOCOL_VERSION: MCP_MODERN_VERSION}});
        assert!(matches!(
            modern_meta(Some(&missing)),
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));
        let old =
            json!({"_meta": {META_PROTOCOL_VERSION: "1900-01-01", META_CLIENT_CAPABILITIES: {}}});
        let Err(McpMethodError::JsonRpc { code, data, .. }) = modern_meta(Some(&old)) else {
            panic!("an unknown version is refused");
        };
        assert_eq!(code, MCP_UNSUPPORTED_PROTOCOL_VERSION);
        let data = data.unwrap();
        assert_eq!(data["requested"], "1900-01-01");
        assert_eq!(data["supported"][0], MCP_MODERN_VERSION);
    }

    #[test]
    fn modern_results_are_complete_cacheable_and_name_the_server() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let authority = McpAuthority::new(&paths, Some("readonly")).unwrap();
        let client = McpClientAttribution::default();
        let host = host();
        let ctx = McpRequestContext {
            authority: &authority,
            client: &client,
            era: McpEra::Modern,
            host: &host,
            operations: None,
            subscriptions: false,
            progress: None,
        };
        let discover = handle(&ctx, "server/discover", None).unwrap().unwrap();
        assert_eq!(discover["resultType"], "complete");
        assert_eq!(discover["supportedVersions"][0], MCP_MODERN_VERSION);
        assert_eq!(discover["_meta"][META_SERVER_INFO]["version"], "9.9.9");
        assert_eq!(discover["cacheScope"], "private");
        assert_eq!(discover["capabilities"]["tools"]["listChanged"], false);

        let tools = handle(&ctx, "tools/list", Some(&modern_params(&json!({}))))
            .unwrap()
            .unwrap();
        assert_eq!(tools["resultType"], "complete");
        assert_eq!(tools["ttlMs"], LIST_TTL_MS);
        assert!(tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "note_get"));

        // A tool failure is a result, also complete.
        let failed = handle(
            &ctx,
            "tools/call",
            Some(&modern_params(
                &json!({"name": "note_get", "arguments": {"note": "Missing"}}),
            )),
        )
        .unwrap()
        .unwrap();
        assert_eq!(failed["isError"], true);
        assert_eq!(failed["resultType"], "complete");

        // Resource not found is -32602 in this revision.
        let missing = handle(
            &ctx,
            "resources/read",
            Some(&modern_params(&json!({"uri": "vulcan://nothing"}))),
        );
        assert!(matches!(
            missing,
            Err(McpMethodError::JsonRpc { code: -32602, .. })
        ));
        // The handshake methods do not exist for modern requests.
        assert!(matches!(
            handle(&ctx, "ping", None),
            Err(McpMethodError::JsonRpc { code: -32601, .. })
        ));
    }

    #[test]
    fn the_skills_extension_is_offered_to_modern_clients_only() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        vulcan_core::paths::initialize_vulcan_dir(&paths).unwrap();
        let authority = McpAuthority::new(&paths, Some("readonly")).unwrap();
        let client = McpClientAttribution::default();
        let host = host();
        let context = |era| McpRequestContext {
            authority: &authority,
            client: &client,
            era,
            host: &host,
            operations: None,
            subscriptions: false,
            progress: None,
        };
        let modern = context(McpEra::Modern);
        let discover = handle(&modern, "server/discover", None).unwrap().unwrap();
        assert_eq!(
            discover["capabilities"]["extensions"][MCP_SKILLS_EXTENSION],
            json!({"directoryRead": true})
        );
        let listed = handle(&modern, "skills/list", Some(&modern_params(&json!({}))))
            .unwrap()
            .unwrap();
        assert_eq!(listed["ttlMs"], SKILL_TTL_MS);
        assert_eq!(listed["cacheScope"], "private");
        let note = listed["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|skill| skill["uri"] == "skill://note-operations/SKILL.md")
            .expect("read-only callers get the note skill")
            .clone();
        let got = handle(
            &modern,
            "skills/get",
            Some(&modern_params(
                &json!({"uri": "skill://note-operations/SKILL.md"}),
            )),
        )
        .unwrap()
        .unwrap();
        assert_eq!(got["skill"], note);
        let directory = handle(
            &modern,
            "resources/directory/read",
            Some(&modern_params(&json!({"uri": "skill://note-operations"}))),
        )
        .unwrap()
        .unwrap();
        assert_eq!(directory["ttlMs"], SKILL_TTL_MS);
        assert!(directory["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|resource| resource["name"] == "SKILL.md"));

        let legacy = context(McpEra::Legacy);
        let discover = handle(&legacy, "server/discover", None).unwrap().unwrap();
        assert!(discover["capabilities"].get("extensions").is_none());
        for method in ["skills/list", "skills/get", "resources/directory/read"] {
            assert!(
                matches!(
                    handle(
                        &legacy,
                        method,
                        Some(&json!({"uri": "skill://note-operations"}))
                    ),
                    Err(McpMethodError::JsonRpc { code: -32601, .. })
                ),
                "{method}"
            );
        }
    }

    #[test]
    fn legacy_initialize_negotiates_a_supported_version_and_records_the_client() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let authority = McpAuthority::new(&paths, Some("readonly")).unwrap();
        let mut connection = McpLegacyConnection::new(&authority);
        let result = connection.initialize(
            &authority,
            &host(),
            Some(&json!({"protocolVersion": "2025-06-18", "clientInfo": {"name": "Old", "version": "0.1"}})),
        );
        assert_eq!(result["protocolVersion"], "2025-06-18");
        assert_eq!(result["capabilities"]["tools"]["listChanged"], true);
        assert_eq!(connection.client().name.as_deref(), Some("Old"));
        let newer = connection.initialize(
            &authority,
            &host(),
            Some(&json!({"protocolVersion": MCP_MODERN_VERSION})),
        );
        assert_eq!(newer["protocolVersion"], MCP_LEGACY_VERSIONS[0]);
        assert!(connection.list_changed_notifications(&authority).is_empty());
    }
}
