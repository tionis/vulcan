//! Transport-neutral MCP protocol methods over one vault session.

use serde_json::{Map, Value};
use std::collections::BTreeSet;
use vulcan_core::{PermissionError, VaultPaths};

use crate::mcp_catalog::{McpToolPack, McpToolPackMode};
use crate::mcp_dispatch::{
    acquire_request_ordinary_write_gate, dispatch_protocol_method, jsonrpc_error,
    process_http_request, process_stdio_request, request_id, McpHttpProcessResult,
    McpMethodHandler, McpProtocolMethods,
};
use crate::mcp_help::HelpTopicReport;
use crate::mcp_protocol::{self, McpCompletionParams, McpMethodError, McpMethodOutcome};
use crate::mcp_session::McpSessionState;
use crate::tools::CustomToolRegistryOptions;

#[derive(Debug, Clone, Copy)]
pub struct McpProtocolHost {
    pub registry_options: fn() -> CustomToolRegistryOptions,
    pub command_help: fn(&[String]) -> Result<HelpTopicReport, String>,
    pub help_candidates: fn(&str) -> Vec<String>,
    pub server_version: &'static str,
}

/// Persistent, transport-neutral protocol state for one MCP client session.
#[derive(Debug, Clone)]
pub struct McpProtocolCore {
    pub session: McpSessionState,
    host: McpProtocolHost,
}

impl McpProtocolCore {
    pub fn new(
        paths: &VaultPaths,
        requested_profile: Option<&str>,
        selected_packs: BTreeSet<McpToolPack>,
        pack_mode: McpToolPackMode,
        host: McpProtocolHost,
    ) -> Result<Self, PermissionError> {
        Ok(Self {
            session: McpSessionState::new(
                paths,
                requested_profile,
                selected_packs,
                pack_mode,
                host.registry_options,
            )?,
            host,
        })
    }

    #[must_use]
    pub fn protocol(&mut self) -> McpSessionProtocol<'_> {
        McpSessionProtocol::new(
            &mut self.session,
            self.host.registry_options,
            self.host.command_help,
            self.host.help_candidates,
            self.host.server_version,
        )
    }

    /// Process a local request while excluding cooperating writers from reads
    /// and refusing canonical state with an unfinished ordinary-write journal.
    pub fn process_request(&mut self, request: Value) -> Vec<Value> {
        let _read_guard = match acquire_request_ordinary_write_gate(self.session.paths(), &request)
        {
            Ok(guard) => guard,
            Err(message) => {
                return request_id(&request)
                    .map(|id| vec![jsonrpc_error(id, -32603, message, None)])
                    .unwrap_or_default();
            }
        };
        process_stdio_request(self, request)
    }

    /// Apply the same canonical read barrier before HTTP protocol dispatch.
    pub fn process_http_request(&mut self, request: &Value) -> Result<McpHttpProcessResult, Value> {
        let _read_guard = match acquire_request_ordinary_write_gate(self.session.paths(), request) {
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
}

impl McpMethodHandler for McpProtocolCore {
    fn handle_method(
        &mut self,
        method: &str,
        params: Option<&Value>,
    ) -> Result<McpMethodOutcome, McpMethodError> {
        self.protocol().handle_method(method, params)
    }

    fn list_changed_notifications(&mut self) -> Vec<Value> {
        self.protocol().list_changed_notifications()
    }
}

/// CLI and daemon adapters inject only host-specific help and reserved names.
/// Discovery, tool execution, and method routing remain in the app layer.
pub struct McpSessionProtocol<'a> {
    session: &'a mut McpSessionState,
    registry_options: fn() -> CustomToolRegistryOptions,
    command_help: fn(&[String]) -> Result<HelpTopicReport, String>,
    help_candidates: fn(&str) -> Vec<String>,
    server_version: &'static str,
}

impl<'a> McpSessionProtocol<'a> {
    pub fn new(
        session: &'a mut McpSessionState,
        registry_options: fn() -> CustomToolRegistryOptions,
        command_help: fn(&[String]) -> Result<HelpTopicReport, String>,
        help_candidates: fn(&str) -> Vec<String>,
        server_version: &'static str,
    ) -> Self {
        Self {
            session,
            registry_options,
            command_help,
            help_candidates,
            server_version,
        }
    }
}

impl McpProtocolMethods for McpSessionProtocol<'_> {
    fn initialize_result(&self) -> Value {
        mcp_protocol::initialization_result(&self.session.active_tool_names(), self.server_version)
    }

    fn visible_tool_items(&self) -> Result<Vec<Value>, McpMethodError> {
        let registry = (self.registry_options)();
        self.session.discovery(&registry).visible_tool_items()
    }

    fn call_tool(
        &mut self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, McpMethodError> {
        let registry = (self.registry_options)();
        self.session.call_tool(&registry, name, arguments)
    }

    fn visible_prompt_items(&self) -> Result<Vec<Value>, McpMethodError> {
        let registry = (self.registry_options)();
        self.session.discovery(&registry).visible_prompt_items()
    }

    fn get_prompt(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, McpMethodError> {
        let registry = (self.registry_options)();
        self.session
            .discovery(&registry)
            .get_prompt(name, arguments)
    }

    fn visible_resources(&self) -> Result<Vec<Value>, McpMethodError> {
        let registry = (self.registry_options)();
        self.session.discovery(&registry).visible_resources()
    }

    fn visible_resource_templates(&self) -> Vec<Value> {
        let registry = (self.registry_options)();
        self.session
            .discovery(&registry)
            .visible_resource_templates()
    }

    fn read_resource(&self, uri: &str) -> Result<Value, McpMethodError> {
        let registry = (self.registry_options)();
        self.session
            .discovery(&registry)
            .read_resource(uri, self.command_help)
    }

    fn complete(&self, params: &McpCompletionParams) -> Result<Value, McpMethodError> {
        let registry = (self.registry_options)();
        self.session
            .discovery(&registry)
            .complete(params, &(self.help_candidates)(""))
    }
}

impl McpMethodHandler for McpSessionProtocol<'_> {
    fn handle_method(
        &mut self,
        method: &str,
        params: Option<&Value>,
    ) -> Result<McpMethodOutcome, McpMethodError> {
        dispatch_protocol_method(self, method, params)
    }

    fn list_changed_notifications(&mut self) -> Vec<Value> {
        self.session
            .list_changed_notifications(self.registry_options)
    }
}

#[cfg(test)]
mod tests {
    use super::{McpProtocolCore, McpProtocolHost, McpSessionProtocol};
    use crate::mcp_catalog::{McpToolPack, McpToolPackMode};
    use crate::mcp_dispatch::McpMethodHandler;
    use crate::mcp_help::HelpTopicReport;
    use crate::mcp_session::McpSessionState;
    use crate::tools::CustomToolRegistryOptions;
    use std::collections::BTreeSet;
    use vulcan_core::VaultPaths;

    fn no_command_help(_: &[String]) -> Result<HelpTopicReport, String> {
        Err("unknown command help".to_string())
    }

    fn no_help_candidates(_: &str) -> Vec<String> {
        vec![]
    }

    #[test]
    fn shared_session_adapter_routes_initialize_and_tool_discovery() {
        let vault = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(vault.path());
        let mut session = McpSessionState::new(
            &paths,
            Some("readonly"),
            BTreeSet::from([McpToolPack::NotesRead]),
            McpToolPackMode::Static,
            CustomToolRegistryOptions::default,
        )
        .unwrap();
        let mut protocol = McpSessionProtocol::new(
            &mut session,
            CustomToolRegistryOptions::default,
            no_command_help,
            no_help_candidates,
            "test-version",
        );
        let initialize = protocol.handle_method("initialize", None).unwrap();
        let initial = initialize.response.unwrap();
        assert_eq!(initial["serverInfo"]["version"], "test-version");
        let tools = protocol.handle_method("tools/list", None).unwrap();
        let items = tools.response.unwrap()["tools"].as_array().unwrap().clone();
        assert!(items.iter().any(|tool| tool["name"] == "note_get"));
        assert!(!items.iter().any(|tool| tool["name"] == "note_create"));
    }

    #[test]
    fn persistent_core_routes_methods_with_injected_host_catalog() {
        let vault = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(vault.path());
        let mut core = McpProtocolCore::new(
            &paths,
            Some("readonly"),
            BTreeSet::from([McpToolPack::NotesRead]),
            McpToolPackMode::Static,
            McpProtocolHost {
                registry_options: CustomToolRegistryOptions::default,
                command_help: no_command_help,
                help_candidates: no_help_candidates,
                server_version: "app-core-test",
            },
        )
        .unwrap();
        let initialized = core.handle_method("initialize", None).unwrap();
        assert_eq!(
            initialized.response.unwrap()["serverInfo"]["version"],
            "app-core-test"
        );
        let listed = core.handle_method("tools/list", None).unwrap();
        assert!(listed.response.unwrap()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "note_get"));
    }

    #[test]
    fn both_core_transports_refuse_invalid_pending_write_state_without_removing_it() {
        let vault = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(vault.path());
        vulcan_core::initialize_vulcan_dir(&paths).unwrap();
        let mut core = McpProtocolCore::new(
            &paths,
            Some("readonly"),
            BTreeSet::from([McpToolPack::NotesRead]),
            McpToolPackMode::Static,
            McpProtocolHost {
                registry_options: CustomToolRegistryOptions::default,
                command_help: no_command_help,
                help_candidates: no_help_candidates,
                server_version: "app-core-test",
            },
        )
        .unwrap();
        let state = paths
            .operational_state_dir()
            .unwrap()
            .join("ordinary-write");
        std::fs::create_dir_all(&state).unwrap();
        let journal = state.join("journal.json");
        std::fs::write(&journal, b"{}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let request = serde_json::json!({"jsonrpc":"2.0", "id":7, "method":"tools/list"});
        let stdio = core.process_request(request.clone());
        assert_eq!(stdio[0]["id"], 7);
        assert_eq!(stdio[0]["error"]["code"], -32603);
        let http = core.process_http_request(&request).unwrap_err();
        assert_eq!(http, stdio[0]);
        let notification =
            serde_json::json!({"jsonrpc":"2.0", "method":"notifications/initialized"});
        assert!(core.process_request(notification.clone()).is_empty());
        let accepted = core.process_http_request(&notification).unwrap();
        assert!(accepted.accepted_notification);
        assert!(accepted.response.is_none());
        assert_eq!(std::fs::read(&journal).unwrap(), b"{}");
    }
}
