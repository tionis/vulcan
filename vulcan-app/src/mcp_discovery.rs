//! Session-scoped MCP discovery and resource routing shared by transports.

use serde_json::{json, Map, Value};
use vulcan_core::{
    AssistantPromptSummary, JsRuntimeSandbox, PermissionProfile, ProfilePermissionGuard, VaultPaths,
};

use crate::mcp_assistant;
use crate::mcp_catalog::{tool_input_schema, visible_tool_catalog, ALWAYS_LOADED_TOOLS};
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
                    "inputSchema": tool_input_schema(tool, self.profile),
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

    /// The tools this connection may call, in MCP terms, as Markdown.
    fn tool_reference_markdown(&self) -> Result<String, McpMethodError> {
        use std::fmt::Write as _;

        let mut text = String::from(
            "# Vulcan MCP tools\n\nThe tools this connection's permission profile allows, with their arguments.\n",
        );
        for item in self.visible_tool_items()? {
            let name = item["name"].as_str().unwrap_or_default();
            let title = item["title"].as_str().unwrap_or(name);
            let description = item["description"].as_str().unwrap_or_default();
            let _ = write!(text, "\n## `{name}`: {title}\n\n{description}\n");
            let lines = argument_lines(&item["inputSchema"]);
            if !lines.is_empty() {
                text.push('\n');
                text.push_str(&lines.join("\n"));
                text.push('\n');
            }
        }
        Ok(text)
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
        let mut resources = mcp_assistant::visible_resources(self.paths, self.guard)?;
        resources.extend(crate::mcp_skills::skill_resources(&self.visible_skills()?));
        Ok(resources)
    }

    fn visible_skills(&self) -> Result<Vec<crate::mcp_skills::McpSkill>, McpMethodError> {
        crate::mcp_skills::visible_skills(self.paths, self.guard, self.profile)
    }

    #[must_use]
    pub fn visible_resource_templates(&self) -> Vec<Value> {
        mcp_assistant::visible_resource_templates(self.guard)
    }

    pub fn read_resource(
        &self,
        uri: &str,
        resolve_command_help: impl FnOnce(&[String]) -> Result<HelpTopicReport, String>,
    ) -> Result<Value, McpMethodError> {
        if let Some(result) = crate::mcp_files::read_vault_resource(self.paths, self.guard, uri) {
            return result;
        }
        if uri.starts_with("skill://") {
            let skills = self.visible_skills()?;
            if let Some(result) = crate::mcp_skills::read_skill_resource(&skills, uri) {
                return result;
            }
        }
        if let Some(result) = mcp_assistant::read_resource(self.paths, self.guard, uri) {
            return result;
        }
        if uri == "vulcan://help/tools" {
            return Ok(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "text/markdown",
                    "text": self.tool_reference_markdown()?,
                }]
            }));
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
    // The MCP prompt argument shape: name, description, required.
    let arguments = prompt
        .arguments
        .iter()
        .map(|argument| {
            let mut item = json!({ "name": argument.name, "required": argument.required });
            if let Some(description) = &argument.description {
                item["description"] = Value::String(description.clone());
            }
            item
        })
        .collect::<Vec<_>>();
    json!({
        "name": prompt.name,
        "title": prompt.title,
        "description": prompt.description,
        "arguments": arguments,
    })
}

/// One line per argument of a tool's input schema, required ones first.
fn argument_lines(schema: &Value) -> Vec<String> {
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |names| {
            names.iter().filter_map(Value::as_str).collect::<Vec<_>>()
        });
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut lines = properties
        .iter()
        .map(|(name, property)| {
            let description = property
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let marker = if required.contains(&name.as_str()) {
                " (required)"
            } else {
                ""
            };
            (
                required.contains(&name.as_str()),
                format!("- `{name}`{marker}: {description}"),
            )
        })
        .collect::<Vec<_>>();
    lines.sort_by_key(|(is_required, _)| !*is_required);
    lines.into_iter().map(|(_, line)| line).collect()
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
        let reference = discovery
            .read_resource("vulcan://help/tools", |_| Err("unused".to_string()))
            .unwrap();
        let reference = reference["contents"][0]["text"].as_str().unwrap();
        assert!(reference.contains("## `note_get`"));
        assert!(reference.contains("- `note` (required):"));
        assert!(!reference.contains("`note_create`"));
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
