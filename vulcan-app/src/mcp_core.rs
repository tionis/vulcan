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
use crate::mcp_help::HelpTopicReport;
use crate::mcp_protocol::{
    server_instructions, tool_error_structured, McpCompletionParams, McpListParams,
    McpListSnapshot, McpMethodError, McpPromptGetParams, McpResourceReadParams, McpToolCallParams,
    MCP_PAGE_SIZE, MCP_RESOURCE_NOT_FOUND,
};

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
        data: Some(json!({
            "supported": supported_versions(),
            "requested": requested,
        })),
    }
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

/// The server capabilities for an era. Legacy connections are told about
/// list changes on their own stream; modern list-change subscriptions are not
/// offered yet.
fn capabilities(era: McpEra) -> Value {
    let list_changed = era == McpEra::Legacy;
    json!({
        "tools": { "listChanged": list_changed },
        "resources": { "listChanged": list_changed },
        "prompts": { "listChanged": list_changed },
        "completions": {},
    })
}

/// Serve one method. `None` is the outcome of a notification.
pub fn handle(
    ctx: &McpRequestContext<'_>,
    method: &str,
    params: Option<&Value>,
) -> Result<Option<Value>, McpMethodError> {
    let result = dispatch(ctx, method, params);
    if ctx.era == McpEra::Legacy {
        return result;
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
    Ok(Some(match method {
        "server/discover" => discover_result(ctx),
        "notifications/cancelled" => return Ok(None),
        "tools/list" => paginated(
            "tools",
            authority.discovery().visible_tool_items()?,
            parse::<McpListParams>(params)?.cursor,
        )?,
        "tools/call" => {
            let params: McpToolCallParams = parse(params)?;
            authority.call_tool(ctx.client, &params.name, &params.arguments)?
        }
        "prompts/list" => paginated(
            "prompts",
            authority.discovery().visible_prompt_items()?,
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
            authority.discovery().visible_resources()?,
            parse::<McpListParams>(params)?.cursor,
        )?,
        "resources/templates/list" => paginated(
            "resourceTemplates",
            authority.discovery().visible_resource_templates(),
            parse::<McpListParams>(params)?.cursor,
        )?,
        "resources/read" => {
            let params: McpResourceReadParams = parse(params)?;
            authority
                .discovery()
                .read_resource(&params.uri, ctx.host.command_help)?
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

fn discover_result(ctx: &McpRequestContext<'_>) -> Value {
    json!({
        "resultType": "complete",
        "supportedVersions": supported_versions(),
        "capabilities": capabilities(ctx.era),
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
            "capabilities": capabilities(McpEra::Legacy),
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
