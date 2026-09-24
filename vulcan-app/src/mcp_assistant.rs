//! Permission-filtered MCP prompt and skill discovery shared by transports.

#![allow(clippy::must_use_candidate)]

use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use vulcan_core::{
    assistant_config_summary, list_assistant_prompts, list_assistant_skills, load_assistant_prompt,
    load_assistant_skill, load_vault_config, read_vault_agents_file, render_assistant_prompt,
    AssistantPromptSummary, AssistantSkillSummary, PermissionGuard, ProfilePermissionGuard,
    VaultPaths,
};

use crate::mcp_protocol::{McpMethodError, MCP_RESOURCE_NOT_FOUND};
use crate::tools::{self, CustomToolDescriptor, CustomToolRegistryOptions};

pub fn custom_tool_matches_selected_packs(
    packs: &[String],
    selected_pack_names: &BTreeSet<String>,
) -> bool {
    if packs.is_empty() {
        return selected_pack_names.contains("custom");
    }
    packs.iter().any(|pack| selected_pack_names.contains(pack))
}

pub fn visible_custom_tools(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    selected_pack_names: &BTreeSet<String>,
    registry_options: &CustomToolRegistryOptions,
) -> Result<Vec<CustomToolDescriptor>, McpMethodError> {
    if !selected_pack_names.contains("custom") {
        return Ok(Vec::new());
    }
    Ok(
        tools::list_custom_tools(paths, active_permission_profile, registry_options)
            .map_err(|error| McpMethodError::tool(error.to_string()))?
            .into_iter()
            .filter(|tool| tool.callable)
            .filter(|tool| {
                custom_tool_matches_selected_packs(&tool.summary.packs, selected_pack_names)
            })
            .collect(),
    )
}

/// Handle projected skill-command resource reads using the same visibility rule as tools/list.
pub fn read_custom_tool_resource(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    selected_pack_names: &BTreeSet<String>,
    registry_options: &CustomToolRegistryOptions,
    uri: &str,
) -> Option<Result<Value, McpMethodError>> {
    let result = match uri {
        "vulcan://assistant/skill-commands/index" => visible_custom_tools(
            paths,
            active_permission_profile,
            selected_pack_names,
            registry_options,
        )
        .and_then(|tools| {
            let commands = tools
                .into_iter()
                .filter(|tool| tool.summary.name.starts_with("skill_"))
                .collect::<Vec<_>>();
            if commands.is_empty() {
                Err(resource_not_found_error(
                    uri,
                    "Resource not found".to_string(),
                ))
            } else {
                json_resource(uri, &commands)
            }
        }),
        "vulcan://assistant/tools/index" => visible_custom_tools(
            paths,
            active_permission_profile,
            selected_pack_names,
            registry_options,
        )
        .and_then(|tools| {
            if tools.is_empty() {
                Err(resource_not_found_error(
                    uri,
                    "Resource not found".to_string(),
                ))
            } else {
                json_resource(uri, &tools)
            }
        }),
        _ => {
            if let Some(name) = uri.strip_prefix("vulcan://assistant/skill-commands/") {
                tools::show_custom_tool(paths, active_permission_profile, name, registry_options)
                    .map_err(|error| resource_not_found_error(uri, error.to_string()))
                    .and_then(|report| {
                        if report.callable && report.tool.summary.name.starts_with("skill_") {
                            json_resource(uri, &report)
                        } else {
                            Err(custom_resource_permission_error(
                                uri,
                                active_permission_profile,
                            ))
                        }
                    })
            } else if let Some(name) = uri.strip_prefix("vulcan://assistant/tools/") {
                if selected_pack_names.contains("custom") {
                    tools::show_custom_tool(
                        paths,
                        active_permission_profile,
                        name,
                        registry_options,
                    )
                    .map_err(|error| resource_not_found_error(uri, error.to_string()))
                    .and_then(|report| {
                        if report.callable
                            && custom_tool_matches_selected_packs(
                                &report.tool.summary.packs,
                                selected_pack_names,
                            )
                        {
                            json_resource(uri, &report)
                        } else {
                            Err(custom_resource_permission_error(
                                uri,
                                active_permission_profile,
                            ))
                        }
                    })
                } else {
                    Err(custom_resource_permission_error(
                        uri,
                        active_permission_profile,
                    ))
                }
            } else {
                return None;
            }
        }
    };
    Some(result)
}

fn custom_resource_permission_error(uri: &str, profile: Option<&str>) -> McpMethodError {
    resource_not_found_error(
        uri,
        format!(
            "permission denied: resource `{uri}` is not available under profile `{}`",
            profile.unwrap_or("unrestricted")
        ),
    )
}

pub fn visible_prompts(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
) -> Result<Vec<AssistantPromptSummary>, McpMethodError> {
    if guard.selection().profile.read.is_none() {
        return Ok(Vec::new());
    }
    let prompts = list_assistant_prompts(paths)
        .map_err(|error| McpMethodError::internal(error.to_string()))?;
    Ok(prompts
        .into_iter()
        .filter(|prompt| prompt_visible(paths, guard, prompt))
        .collect())
}

pub fn visible_skills(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
) -> Result<Vec<AssistantSkillSummary>, McpMethodError> {
    if guard.selection().profile.read.is_none() {
        return Ok(Vec::new());
    }
    let skills = list_assistant_skills(paths)
        .map_err(|error| McpMethodError::internal(error.to_string()))?;
    Ok(skills
        .into_iter()
        .filter(|skill| skill_visible(paths, guard, skill))
        .collect())
}

/// List stable MCP resources for one effective authority. Custom tool names must already be
/// filtered by the caller's selected packs and permission profile.
pub fn visible_resources(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    custom_tool_names: &[String],
) -> Result<Vec<Value>, McpMethodError> {
    let mut resources = vec![serde_json::json!({
        "uri": "vulcan://help/overview",
        "name": "Help Overview",
        "title": "Vulcan Help Overview",
        "description": "Integrated overview of the Vulcan command surface and built-in help topics.",
        "mimeType": "application/json",
    })];

    if !guard.selection().profile.read.is_none() {
        resources.push(serde_json::json!({
            "uri": "vulcan://assistant/prompts/index",
            "name": "Assistant Prompt Index",
            "title": "Vault Prompt Index",
            "description": "Visible prompts loaded from the configured assistant prompts folder.",
            "mimeType": "application/json",
        }));
        resources.push(serde_json::json!({
            "uri": "vulcan://assistant/skills/index",
            "name": "Assistant Skill Index",
            "title": "Vault Skill Index",
            "description": "Visible skills loaded from the configured assistant skills folder.",
            "mimeType": "application/json",
        }));
        if custom_tool_names
            .iter()
            .any(|name| name.starts_with("skill_"))
        {
            resources.push(serde_json::json!({
                "uri": "vulcan://assistant/skill-commands/index",
                "name": "Assistant Skill Command Index",
                "title": "Vault Skill Command Index",
                "description": "Visible Agent Skills-compatible command tools projected into the shared tool registry.",
                "mimeType": "application/json",
            }));
        }
        if !custom_tool_names.is_empty() {
            resources.push(serde_json::json!({
                "uri": "vulcan://assistant/tools/index",
                "name": "Assistant Tool Index",
                "title": "Vault Custom Tool Index",
                "description": "Visible callable skill command tools projected into the shared tool registry.",
                "mimeType": "application/json",
            }));
        }
        if read_vault_agents_file(paths)
            .map_err(|error| McpMethodError::internal(error.to_string()))?
            .is_some()
            && can_read_relative_path(guard, "AGENTS.md")
        {
            resources.push(serde_json::json!({
                "uri": "vulcan://assistant/agents",
                "name": "AGENTS.md",
                "title": "Vault Agent Instructions",
                "description": "The vault's root AGENTS.md instructions.",
                "mimeType": "text/markdown",
            }));
        }
    }

    if guard.check_config_read().is_ok() {
        resources.push(serde_json::json!({
            "uri": "vulcan://assistant/config",
            "name": "Assistant Config Summary",
            "title": "Assistant Config Summary",
            "description": "Configured assistant prompt and skill folders for this vault.",
            "mimeType": "application/json",
        }));
    }
    Ok(resources)
}

pub fn visible_resource_templates(
    guard: &ProfilePermissionGuard,
    custom_pack_selected: bool,
) -> Vec<Value> {
    let mut templates = vec![serde_json::json!({
        "uriTemplate": "vulcan://help/{topic}",
        "name": "Help Topics",
        "title": "Help Topic Resource",
        "description": "Read one built-in or command help topic as structured JSON.",
        "mimeType": "application/json",
    })];

    if !guard.selection().profile.read.is_none() {
        templates.push(serde_json::json!({
            "uriTemplate": "vulcan://assistant/skills/{name}",
            "name": "Assistant Skills",
            "title": "Assistant Skill Resource",
            "description": "Read one visible assistant skill as structured JSON.",
            "mimeType": "application/json",
        }));
        templates.push(serde_json::json!({
            "uriTemplate": "vulcan://assistant/skill-commands/{name}",
            "name": "Assistant Skill Commands",
            "title": "Assistant Skill Command Resource",
            "description": "Read one visible projected skill command as structured JSON.",
            "mimeType": "application/json",
        }));
        if custom_pack_selected {
            templates.push(serde_json::json!({
                "uriTemplate": "vulcan://assistant/tools/{name}",
                "name": "Assistant Tools",
                "title": "Assistant Tool Resource",
                "description": "Read one visible callable skill command tool as structured JSON.",
                "mimeType": "application/json",
            }));
        }
    }
    templates
}

pub fn prompt_visible(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    prompt: &AssistantPromptSummary,
) -> bool {
    if guard.read_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return true;
    }
    guard
        .check_read_path(&prompt_relative_path(paths, prompt))
        .is_ok()
}

pub fn skill_visible(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    skill: &AssistantSkillSummary,
) -> bool {
    if guard.read_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return true;
    }
    guard
        .check_read_path(&skill_relative_path(paths, skill))
        .is_ok()
}

fn prompt_relative_path(paths: &VaultPaths, prompt: &AssistantPromptSummary) -> String {
    load_vault_config(paths)
        .config
        .assistant
        .prompts_folder
        .join(&prompt.path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn skill_relative_path(paths: &VaultPaths, skill: &AssistantSkillSummary) -> String {
    load_vault_config(paths)
        .config
        .assistant
        .skills_folder
        .join(&skill.path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn get_prompt(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    name: &str,
    arguments: &Map<String, Value>,
) -> Result<Value, McpMethodError> {
    let prompt = load_assistant_prompt(paths, name)
        .map_err(|error| McpMethodError::invalid_params(error.to_string()))?;
    if !prompt_visible(paths, guard, &prompt.summary) {
        return Err(McpMethodError::invalid_params(format!(
            "prompt `{name}` is not available under profile `{}`",
            guard.selection().name
        )));
    }
    let rendered = render_assistant_prompt(&prompt, &string_argument_map(arguments))
        .map_err(|error| McpMethodError::invalid_params(error.to_string()))?;
    Ok(serde_json::json!({
        "description": prompt.summary.description,
        "messages": [
            {
                "role": prompt.summary.role,
                "content": {
                    "type": "text",
                    "text": rendered,
                }
            }
        ]
    }))
}

/// Handle vault-owned assistant resources. Other resource namespaces remain with the caller.
pub fn read_resource(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    uri: &str,
) -> Option<Result<Value, McpMethodError>> {
    let result = match uri {
        "vulcan://assistant/prompts/index" => {
            visible_prompts(paths, guard).and_then(|prompts| json_resource(uri, &prompts))
        }
        "vulcan://assistant/skills/index" => {
            visible_skills(paths, guard).and_then(|skills| json_resource(uri, &skills))
        }
        "vulcan://assistant/config" => {
            if let Err(error) = guard.check_config_read() {
                Err(McpMethodError::tool(error.to_string()))
            } else {
                json_resource(uri, &assistant_config_summary(paths))
            }
        }
        "vulcan://assistant/agents" => {
            if can_read_relative_path(guard, "AGENTS.md") {
                match read_vault_agents_file(paths) {
                    Ok(Some(contents)) => Ok(serde_json::json!({
                        "contents": [{
                            "uri": uri,
                            "mimeType": "text/markdown",
                            "text": contents,
                        }]
                    })),
                    Ok(None) => Err(resource_not_found_error(
                        uri,
                        "Resource not found".to_string(),
                    )),
                    Err(error) => Err(McpMethodError::internal(error.to_string())),
                }
            } else {
                Err(resource_not_found_error(
                    uri,
                    format!(
                        "permission denied: resource `{uri}` is not available under profile `{}`",
                        guard.selection().name
                    ),
                ))
            }
        }
        _ => {
            let name = uri.strip_prefix("vulcan://assistant/skills/")?;
            match load_assistant_skill(paths, name) {
                Ok(skill) if skill_visible(paths, guard, &skill.summary) => {
                    json_resource(uri, &skill)
                }
                Ok(_) => Err(resource_not_found_error(
                    uri,
                    format!(
                        "permission denied: resource `{uri}` is not available under profile `{}`",
                        guard.selection().name
                    ),
                )),
                Err(error) => Err(resource_not_found_error(uri, error.to_string())),
            }
        }
    };
    Some(result)
}

fn can_read_relative_path(guard: &ProfilePermissionGuard, relative_path: &str) -> bool {
    if guard.read_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return true;
    }
    guard.check_read_path(relative_path).is_ok()
}

fn json_resource<T: serde::Serialize>(uri: &str, value: &T) -> Result<Value, McpMethodError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| McpMethodError::internal(error.to_string()))?;
    Ok(serde_json::json!({
        "contents": [{
            "uri": uri,
            "mimeType": "application/json",
            "text": text,
        }]
    }))
}

fn resource_not_found_error(uri: &str, message: String) -> McpMethodError {
    McpMethodError::JsonRpc {
        code: MCP_RESOURCE_NOT_FOUND,
        message,
        data: Some(serde_json::json!({ "uri": uri })),
    }
}

fn string_argument_map(arguments: &Map<String, Value>) -> BTreeMap<String, String> {
    arguments
        .iter()
        .map(|(key, value)| (key.clone(), json_value_to_string(value)))
        .collect()
}

pub fn json_value_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(values) => values
            .iter()
            .map(json_value_to_string)
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use vulcan_core::resolve_permission_profile;

    #[test]
    fn prompts_are_rendered_for_readers_and_hidden_from_blind_profiles() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let prompts = temporary.path().join("AI/Prompts");
        fs::create_dir_all(&prompts).unwrap();
        fs::create_dir_all(temporary.path().join(".vulcan")).unwrap();
        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            "[permissions.profiles.blind]\nread = \"none\"\n",
        )
        .unwrap();
        fs::write(
            prompts.join("summarize.md"),
            "---\nname: summarize\ntitle: Summarize Note\ndescription: Summarize one note\nversion: 1\nrole: assistant\narguments:\n  - name: note\n    required: true\n---\nSummarize {{note}}.\n",
        ).unwrap();

        let readable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let blind = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("blind")).unwrap(),
        );
        assert_eq!(visible_prompts(&paths, &readable).unwrap().len(), 1);
        assert!(visible_prompts(&paths, &blind).unwrap().is_empty());
        let rendered = get_prompt(
            &paths,
            &readable,
            "summarize",
            &json!({"note": "Projects/Alpha.md"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
        assert_eq!(
            rendered["messages"][0]["content"]["text"],
            "Summarize Projects/Alpha.md."
        );
        assert!(get_prompt(&paths, &blind, "summarize", &Map::new()).is_err());
    }

    #[test]
    fn prompt_arguments_preserve_existing_stringification() {
        assert_eq!(json_value_to_string(&json!(["a", true, 2])), "a,true,2");
        assert_eq!(json_value_to_string(&Value::Null), "");
    }

    #[test]
    fn assistant_resources_keep_permission_and_response_shapes() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        fs::create_dir_all(temporary.path().join(".vulcan")).unwrap();
        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            "[permissions.profiles.blind]\nread = \"none\"\n",
        )
        .unwrap();
        fs::write(temporary.path().join("AGENTS.md"), "# Vault instructions\n").unwrap();
        let readable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let blind = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("blind")).unwrap(),
        );

        let agents = read_resource(&paths, &readable, "vulcan://assistant/agents")
            .unwrap()
            .unwrap();
        assert_eq!(agents["contents"][0]["mimeType"], "text/markdown");
        assert_eq!(agents["contents"][0]["text"], "# Vault instructions\n");
        let denied = read_resource(&paths, &blind, "vulcan://assistant/agents")
            .unwrap()
            .unwrap_err();
        assert!(matches!(
            denied,
            McpMethodError::JsonRpc {
                code: MCP_RESOURCE_NOT_FOUND,
                ..
            }
        ));

        let prompts = read_resource(&paths, &blind, "vulcan://assistant/prompts/index")
            .unwrap()
            .unwrap();
        assert_eq!(prompts["contents"][0]["mimeType"], "application/json");
        assert_eq!(prompts["contents"][0]["text"], "[]");
        assert!(read_resource(&paths, &readable, "vulcan://help/overview").is_none());
    }

    #[test]
    fn resource_discovery_preserves_authority_and_custom_pack_visibility() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        fs::create_dir_all(temporary.path().join(".vulcan")).unwrap();
        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            "[permissions.profiles.blind]\nread = \"none\"\n",
        )
        .unwrap();
        fs::write(temporary.path().join("AGENTS.md"), "# Instructions\n").unwrap();
        let readable = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let blind = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("blind")).unwrap(),
        );
        let names = vec!["skill_summarize".to_string(), "other_tool".to_string()];

        let visible = visible_resources(&paths, &readable, &names).unwrap();
        let uris = visible
            .iter()
            .filter_map(|resource| resource["uri"].as_str())
            .collect::<Vec<_>>();
        assert!(uris.contains(&"vulcan://assistant/agents"));
        assert!(uris.contains(&"vulcan://assistant/skill-commands/index"));
        assert!(uris.contains(&"vulcan://assistant/tools/index"));
        let without_custom = visible_resources(&paths, &readable, &[]).unwrap();
        assert!(!without_custom
            .iter()
            .any(|resource| resource["uri"] == "vulcan://assistant/tools/index"));

        let hidden = visible_resources(&paths, &blind, &names).unwrap();
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0]["uri"], "vulcan://help/overview");

        let templates = visible_resource_templates(&readable, true);
        assert!(templates
            .iter()
            .any(|template| template["uriTemplate"] == "vulcan://assistant/tools/{name}"));
        let no_custom = visible_resource_templates(&readable, false);
        assert!(!no_custom
            .iter()
            .any(|template| template["uriTemplate"] == "vulcan://assistant/tools/{name}"));
        let blind_templates = visible_resource_templates(&blind, true);
        assert_eq!(blind_templates.len(), 1);
        assert_eq!(blind_templates[0]["uriTemplate"], "vulcan://help/{topic}");
    }

    #[test]
    fn custom_tool_resources_require_the_pack_even_for_guessed_uris() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let options = CustomToolRegistryOptions::default();
        let uri = "vulcan://assistant/tools/skill_example_run";
        let no_packs = BTreeSet::new();
        assert!(
            visible_custom_tools(&paths, Some("readonly"), &no_packs, &options)
                .unwrap()
                .is_empty()
        );
        let denied = read_custom_tool_resource(&paths, Some("readonly"), &no_packs, &options, uri)
            .unwrap()
            .unwrap_err();
        assert!(matches!(
            denied,
            McpMethodError::JsonRpc {
                code: MCP_RESOURCE_NOT_FOUND,
                ..
            }
        ));
        let selected = BTreeSet::from(["custom".to_string()]);
        assert!(custom_tool_matches_selected_packs(&[], &selected));
        assert!(!custom_tool_matches_selected_packs(
            &["admin".to_string()],
            &selected
        ));
        assert!(read_custom_tool_resource(
            &paths,
            Some("readonly"),
            &selected,
            &options,
            "vulcan://help/overview"
        )
        .is_none());
    }
}
