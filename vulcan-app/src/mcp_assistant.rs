//! Permission-filtered MCP prompt and skill discovery shared by transports.

#![allow(clippy::must_use_candidate)]

use serde_json::{Map, Value};
use std::collections::BTreeMap;
use vulcan_core::{
    assistant_config_summary, list_assistant_prompts, list_assistant_skills, load_assistant_prompt,
    load_assistant_skill, load_vault_config, read_vault_agents_file, render_assistant_prompt,
    AssistantPromptSummary, AssistantSkillSummary, PermissionGuard, ProfilePermissionGuard,
    VaultPaths,
};

use crate::mcp_protocol::{McpMethodError, MCP_RESOURCE_NOT_FOUND};

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
}
