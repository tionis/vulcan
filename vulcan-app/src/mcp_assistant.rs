//! Permission-filtered MCP prompt and skill discovery shared by transports.

#![allow(clippy::must_use_candidate)]

use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use vulcan_core::{
    assistant_prompts_root, assistant_skills_root, list_assistant_prompts, list_assistant_skills,
    load_assistant_prompt, load_vault_config, read_vault_agents_file, render_assistant_prompt,
    AssistantPromptSummary, AssistantSkillSummary, PermissionGuard, PermissionProfile,
    ProfilePermissionGuard, VaultPaths,
};

use crate::mcp_catalog::visible_tool_catalog;
use crate::mcp_protocol::{McpMethodError, MCP_RESOURCE_NOT_FOUND};
use crate::tools::{self, CustomToolDescriptor};

/// The vault-defined tools this profile may call.
pub fn visible_custom_tools(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
) -> Result<Vec<CustomToolDescriptor>, McpMethodError> {
    Ok(tools::list_custom_tools(paths, active_permission_profile)
        .map_err(|error| McpMethodError::tool(error.to_string()))?
        .into_iter()
        .filter(|tool| tool.callable)
        .collect())
}

/// Fingerprint the built-in and custom tools discoverable under one profile.
pub fn tool_catalog_fingerprint(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    profile: &PermissionProfile,
) -> String {
    let mut parts = visible_tool_catalog(profile)
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();
    if let Ok(custom_tools) = visible_custom_tools(paths, active_permission_profile) {
        parts.extend(
            custom_tools
                .into_iter()
                .filter_map(|tool| serde_json::to_string(&tool.summary).ok()),
        );
    }
    parts.join("\n")
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

/// Fingerprint only prompt files that this session's read grant can observe.
pub fn prompt_files_fingerprint(paths: &VaultPaths, guard: &ProfilePermissionGuard) -> String {
    if guard.selection().profile.read.is_none() {
        return String::new();
    }
    assistant_prompts_root(paths).map_or_else(
        |_| String::new(),
        |root| readable_assistant_tree_fingerprint(paths, guard, &root, false),
    )
}

/// Fingerprint only resource sources visible under the session's read and config grants.
pub fn resource_files_fingerprint(paths: &VaultPaths, guard: &ProfilePermissionGuard) -> String {
    let mut parts = Vec::new();
    if !guard.selection().profile.read.is_none() {
        parts.push(prompt_files_fingerprint(paths, guard));
        if let Ok(root) = assistant_skills_root(paths) {
            parts.push(readable_assistant_tree_fingerprint(
                paths, guard, &root, true,
            ));
        }
        if guard.check_read_path("AGENTS.md").is_ok() {
            parts.push(path_tree_fingerprint(&paths.vault_root().join("AGENTS.md")));
        }
    }
    if guard.check_config_read().is_ok() {
        parts.push(path_tree_fingerprint(paths.config_file()));
        parts.push(path_tree_fingerprint(
            &paths.vulcan_dir().join("config.local.toml"),
        ));
    }
    parts.retain(|part| !part.is_empty());
    parts.join("\n--\n")
}

fn readable_assistant_tree_fingerprint(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    root: &Path,
    skill_only: bool,
) -> String {
    fn collect(
        paths: &VaultPaths,
        guard: &ProfilePermissionGuard,
        path: &Path,
        skill_only: bool,
        lines: &mut Vec<String>,
    ) {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            return;
        };
        if metadata.file_type().is_symlink() {
            return;
        }
        if metadata.is_dir() {
            let Ok(entries) = fs::read_dir(path) else {
                return;
            };
            let mut children = entries
                .flatten()
                .map(|entry| entry.path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                collect(paths, guard, &child, skill_only, lines);
            }
            return;
        }
        if !metadata.is_file()
            || !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            || (skill_only && path.file_name().is_none_or(|name| name != "SKILL.md"))
        {
            return;
        }
        let Ok(relative) = path.strip_prefix(paths.vault_root()) else {
            return;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if guard.check_read_path(&relative).is_ok() {
            collect_path_tree_fingerprint(path, lines);
        }
    }

    let mut lines = Vec::new();
    collect(paths, guard, root, skill_only, &mut lines);
    lines.join("\u{1f}")
}

fn path_tree_fingerprint(path: &Path) -> String {
    let mut lines = Vec::new();
    collect_path_tree_fingerprint(path, &mut lines);
    lines.join("\u{1f}")
}

fn collect_path_tree_fingerprint(path: &Path, lines: &mut Vec<String>) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    if metadata.is_dir() {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        let mut child_paths = entries
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        child_paths.sort();
        for child_path in child_paths {
            collect_path_tree_fingerprint(&child_path, lines);
        }
        return;
    }

    lines.push(path.display().to_string());
    if metadata.is_file() {
        let Ok(contents) = fs::read(path) else {
            return;
        };
        lines.push(contents.len().to_string());
        lines.push(String::from_utf8_lossy(&contents).into_owned());
    }
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

/// List stable MCP resources for one effective authority: the help index,
/// the MCP tool reference, and the vault's AGENTS.md. Skills are listed by
/// [`crate::mcp_skills`].
pub fn visible_resources(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
) -> Result<Vec<Value>, McpMethodError> {
    let mut resources = vec![
        serde_json::json!({
            "uri": "vulcan://help/overview",
            "name": "Help Overview",
            "title": "Vulcan Help Overview",
            "description": "Overview of Vulcan and its help topics.",
            "mimeType": "text/markdown",
        }),
        serde_json::json!({
            "uri": "vulcan://help/tools",
            "name": "Tool Reference",
            "title": "Vulcan MCP Tool Reference",
            "description": "Every tool this connection may call, with its arguments.",
            "mimeType": "text/markdown",
        }),
    ];

    if !guard.selection().profile.read.is_none()
        && read_vault_agents_file(paths)
            .map_err(|error| McpMethodError::internal(error.to_string()))?
            .is_some()
        && can_read_relative_path(guard, "AGENTS.md")
    {
        resources.push(serde_json::json!({
            "uri": "vulcan://agents",
            "name": "AGENTS.md",
            "title": "Vault Agent Instructions",
            "description": "The vault's root AGENTS.md instructions.",
            "mimeType": "text/markdown",
        }));
    }
    Ok(resources)
}

pub fn visible_resource_templates(guard: &ProfilePermissionGuard) -> Vec<Value> {
    let mut templates = vec![serde_json::json!({
        "uriTemplate": "vulcan://help/{topic}",
        "name": "Help Topics",
        "title": "Help Topic Resource",
        "description": "One help topic as Markdown, such as `query-dsl` or `note/get`.",
        "mimeType": "text/markdown",
    })];

    if !guard.selection().profile.read.is_none() {
        templates.push(serde_json::json!({
            "uriTemplate": "vulcan://note/{path}",
            "name": "Notes",
            "title": "Vault Note",
            "description": "The Markdown source of one note, by vault-relative path such as `Projects/Alpha.md`.",
            "mimeType": "text/markdown",
        }));
        templates.push(serde_json::json!({
            "uriTemplate": "vulcan://file/{path}",
            "name": "Files",
            "title": "Vault File",
            "description": "One vault file by vault-relative path: text, or base64 for binary files (16 MiB at most).",
        }));
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
        "vulcan://agents" => {
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
        _ => return None,
    };
    Some(result)
}

fn can_read_relative_path(guard: &ProfilePermissionGuard, relative_path: &str) -> bool {
    if guard.read_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return true;
    }
    guard.check_read_path(relative_path).is_ok()
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
    fn assistant_change_snapshots_include_only_grant_readable_files() {
        let temporary = tempfile::tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        fs::create_dir_all(paths.vulcan_dir()).expect("vault config directory");
        let prompts = assistant_prompts_root(&paths).expect("prompt root");
        let skills = assistant_skills_root(&paths).expect("skill root");
        fs::create_dir_all(&prompts).expect("prompt directory");
        fs::create_dir_all(skills.join("visible")).expect("visible skill directory");
        fs::create_dir_all(skills.join("hidden")).expect("hidden skill directory");
        let visible_prompt = prompts.join("visible.md");
        let hidden_prompt = prompts.join("hidden.md");
        let visible_skill = skills.join("visible/SKILL.md");
        let hidden_skill = skills.join("hidden/SKILL.md");
        for path in [
            &visible_prompt,
            &hidden_prompt,
            &visible_skill,
            &hidden_skill,
        ] {
            fs::write(path, "---\nname: example\n---\nInitial.\n").expect("assistant file");
        }
        let visible_prompt_path = visible_prompt
            .strip_prefix(paths.vault_root())
            .expect("relative prompt")
            .to_string_lossy()
            .replace('\\', "/");
        let visible_skill_path = visible_skill
            .strip_prefix(paths.vault_root())
            .expect("relative skill")
            .to_string_lossy()
            .replace('\\', "/");
        fs::write(
            paths.config_file(),
            format!(
                "[permissions.profiles.scoped]\nread = {{ allow = [\"note:{visible_prompt_path}\", \"note:{visible_skill_path}\"] }}\n[permissions.profiles.blind]\nread = \"none\"\n"
            ),
        )
        .expect("permission profiles");
        let scoped = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("scoped")).expect("scoped profile"),
        );
        let blind = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("blind")).expect("blind profile"),
        );
        let prompt_before = prompt_files_fingerprint(&paths, &scoped);
        let resource_before = resource_files_fingerprint(&paths, &scoped);

        fs::write(
            &hidden_prompt,
            "---\nname: [invalid\n---\nHidden changed.\n",
        )
        .expect("hidden prompt change");
        fs::write(&hidden_skill, "---\nname: [invalid\n---\nHidden changed.\n")
            .expect("hidden skill change");
        assert_eq!(prompt_files_fingerprint(&paths, &scoped), prompt_before);
        assert_eq!(resource_files_fingerprint(&paths, &scoped), resource_before);
        assert!(prompt_files_fingerprint(&paths, &blind).is_empty());
        assert!(resource_files_fingerprint(&paths, &blind).is_empty());

        fs::write(&visible_skill, "---\nname: example\n---\nSkill changed.\n")
            .expect("visible skill change");
        assert_eq!(prompt_files_fingerprint(&paths, &scoped), prompt_before);
        assert_ne!(resource_files_fingerprint(&paths, &scoped), resource_before);
        fs::write(
            &visible_prompt,
            "---\nname: example\n---\nPrompt changed.\n",
        )
        .expect("visible prompt change");
        assert_ne!(prompt_files_fingerprint(&paths, &scoped), prompt_before);
    }

    #[test]
    fn tool_catalog_snapshot_tracks_profile_visibility() {
        let temporary = tempfile::tempdir().expect("temporary vault");
        let paths = VaultPaths::new(temporary.path());
        let readonly = resolve_permission_profile(&paths, Some("readonly"))
            .expect("readonly profile")
            .profile;
        let unrestricted = resolve_permission_profile(&paths, Some("unrestricted"))
            .expect("unrestricted profile")
            .profile;
        let read = tool_catalog_fingerprint(&paths, Some("readonly"), &readonly);
        let write = tool_catalog_fingerprint(&paths, Some("unrestricted"), &unrestricted);
        assert!(read.contains("note_get"));
        assert!(!read.contains("note_create"));
        assert!(write.contains("note_create"));
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

        let agents = read_resource(&paths, &readable, "vulcan://agents")
            .unwrap()
            .unwrap();
        assert_eq!(agents["contents"][0]["mimeType"], "text/markdown");
        assert_eq!(agents["contents"][0]["text"], "# Vault instructions\n");
        assert!(matches!(
            read_resource(&paths, &blind, "vulcan://agents")
                .unwrap()
                .unwrap_err(),
            McpMethodError::JsonRpc {
                code: MCP_RESOURCE_NOT_FOUND,
                ..
            }
        ));
        for removed in [
            "vulcan://assistant/agents",
            "vulcan://assistant/prompts/index",
            "vulcan://assistant/config",
        ] {
            assert!(
                read_resource(&paths, &readable, removed).is_none(),
                "{removed}"
            );
        }

        let uris = |guard| {
            visible_resources(&paths, guard)
                .unwrap()
                .into_iter()
                .filter_map(|resource| resource["uri"].as_str().map(str::to_string))
                .collect::<Vec<_>>()
        };
        let visible = uris(&readable);
        assert!(visible.contains(&"vulcan://agents".to_string()));
        assert!(visible.contains(&"vulcan://help/tools".to_string()));
        assert_eq!(
            uris(&blind),
            ["vulcan://help/overview", "vulcan://help/tools"]
        );
        let templates = visible_resource_templates(&readable)
            .into_iter()
            .filter_map(|template| template["uriTemplate"].as_str().map(str::to_string))
            .collect::<Vec<_>>();
        assert!(templates.contains(&"vulcan://note/{path}".to_string()));
        assert!(!templates
            .iter()
            .any(|uri| uri.contains("skill-commands") || uri.contains("assistant/tools")));
        assert_eq!(visible_resource_templates(&blind).len(), 1);
    }
}
