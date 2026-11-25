//! Session-scoped MCP discovery and resource routing shared by transports.

use serde_json::{json, Map, Value};
use vulcan_core::{
    AssistantPromptSummary, JsRuntimeSandbox, PermissionProfile, ProfilePermissionGuard, VaultPaths,
};

use crate::mcp_assistant;
use crate::mcp_catalog::{visible_tool_catalog, ALWAYS_LOADED_TOOLS};
use crate::mcp_completion;
use crate::mcp_help::{self, HelpTopicReport};
use crate::mcp_protocol::{McpCompletionParams, McpMethodError, MCP_RESOURCE_NOT_FOUND};
use crate::tools::CustomToolDescriptor;

pub struct McpDiscovery<'a> {
    pub paths: &'a VaultPaths,
    pub guard: &'a ProfilePermissionGuard,
    pub profile_name: &'a str,
    pub profile: &'a PermissionProfile,
}

impl McpDiscovery<'_> {
    fn visible_custom_tools(&self) -> Result<Vec<CustomToolDescriptor>, McpMethodError> {
        mcp_assistant::visible_custom_tools(self.paths, Some(self.profile_name))
    }

    pub fn visible_tool_items(&self) -> Result<Vec<Value>, McpMethodError> {
        let mut tools = visible_tool_catalog(self.profile)
            .into_iter()
            .map(|tool| {
                let mut item = json!({
                    "name": tool.name,
                    "title": tool.title,
                    "description": tool.description,
                    "inputSchema": (tool.input_schema)(),
                    "annotations": tool.annotations,
                });
                if let Some(schema) = tool.output_schema {
                    item["outputSchema"] = schema();
                }
                if ALWAYS_LOADED_TOOLS.contains(&tool.name) {
                    item["_meta"] = json!({ "anthropic/alwaysLoad": true });
                }
                item
            })
            .collect::<Vec<_>>();
        tools.extend(self.visible_custom_tools()?.iter().map(|tool| {
            let mut item = json!({
                "name": tool.summary.name,
                "title": tool.summary.title.as_ref().unwrap_or(&tool.summary.name),
                "description": tool.summary.description,
                "inputSchema": tool.summary.input_schema,
                "annotations": {
                    "readOnlyHint": tool.summary.read_only,
                    "destructiveHint": tool.summary.destructive,
                    "idempotentHint": tool.summary.read_only && !tool.summary.destructive,
                    "openWorldHint": matches!(tool.summary.sandbox, JsRuntimeSandbox::Net),
                },
            });
            // MCP has no null output schema: a tool without one omits it.
            if let Some(schema) = &tool.summary.output_schema {
                item["outputSchema"] = schema.clone();
            }
            item
        }));
        Ok(tools)
    }

    pub fn visible_prompt_items(&self) -> Result<Vec<Value>, McpMethodError> {
        let prompts = mcp_assistant::visible_prompts(self.paths, self.guard)?;
        Ok(prompts.iter().map(prompt_list_item).collect())
    }

    pub fn get_prompt(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, McpMethodError> {
        mcp_assistant::get_prompt(self.paths, self.guard, name, arguments)
    }

    pub fn visible_resources(&self) -> Result<Vec<Value>, McpMethodError> {
        let custom_names = if self.profile.read.is_none() {
            Vec::new()
        } else {
            self.visible_custom_tools()?
                .into_iter()
                .map(|tool| tool.summary.name)
                .collect()
        };
        mcp_assistant::visible_resources(self.paths, self.guard, &custom_names)
    }

    #[must_use]
    pub fn visible_resource_templates(&self) -> Vec<Value> {
        let has_custom_tools = !self.profile.read.is_none()
            && self
                .visible_custom_tools()
                .is_ok_and(|tools| !tools.is_empty());
        mcp_assistant::visible_resource_templates(self.guard, has_custom_tools)
    }

    pub fn read_resource(
        &self,
        uri: &str,
        resolve_command_help: impl FnOnce(&[String]) -> Result<HelpTopicReport, String>,
    ) -> Result<Value, McpMethodError> {
        if let Some(result) = crate::mcp_files::read_vault_resource(self.paths, self.guard, uri) {
            return result;
        }
        if let Some(result) = mcp_assistant::read_resource(self.paths, self.guard, uri) {
            return result;
        }
        if let Some(result) =
            mcp_assistant::read_custom_tool_resource(self.paths, Some(self.profile_name), uri)
        {
            return result;
        }
        if let Some(result) = mcp_help::read_help_resource(uri, resolve_command_help) {
            return result;
        }
        Err(McpMethodError::JsonRpc {
            code: MCP_RESOURCE_NOT_FOUND,
            message: "Resource not found".to_string(),
            data: Some(json!({ "uri": uri })),
        })
    }

    pub fn complete(
        &self,
        params: &McpCompletionParams,
        help_candidates: &[String],
    ) -> Result<Value, McpMethodError> {
        mcp_completion::complete(self.paths, self.guard, params, help_candidates)
    }
}

fn prompt_list_item(prompt: &AssistantPromptSummary) -> Value {
    json!({
        "name": prompt.name,
        "title": prompt.title,
        "description": prompt.description,
        "arguments": prompt.arguments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vulcan_core::resolve_permission_profile;

    #[test]
    fn discovery_filters_tools_and_keeps_unknown_resources_scoped() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let selection = resolve_permission_profile(&paths, Some("readonly")).unwrap();
        let guard = ProfilePermissionGuard::new(&paths, selection.clone());
        let discovery = McpDiscovery {
            paths: &paths,
            guard: &guard,
            profile_name: &selection.name,
            profile: &selection.profile,
        };
        let tools = discovery.visible_tool_items().unwrap();
        let note_get = tools
            .iter()
            .find(|tool| tool["name"] == "note_get")
            .unwrap();
        assert_eq!(note_get["annotations"]["readOnlyHint"], true);
        assert!(note_get["inputSchema"].is_object());
        assert!(!tools.iter().any(|tool| tool["name"] == "note_create"));
        assert!(discovery.visible_prompt_items().unwrap().is_empty());
        assert!(matches!(
            discovery.read_resource("vulcan://missing", |_| Err("missing".to_string())),
            Err(McpMethodError::JsonRpc {
                code: MCP_RESOURCE_NOT_FOUND,
                ..
            })
        ));
    }
}
