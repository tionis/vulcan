//! Transport-neutral MCP protocol methods over one vault session.

use serde_json::{Map, Value};

use crate::mcp_dispatch::{dispatch_protocol_method, McpMethodHandler, McpProtocolMethods};
use crate::mcp_help::HelpTopicReport;
use crate::mcp_protocol::{self, McpCompletionParams, McpMethodError, McpMethodOutcome};
use crate::mcp_session::McpSessionState;
use crate::tools::CustomToolRegistryOptions;

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
    use super::McpSessionProtocol;
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
}
