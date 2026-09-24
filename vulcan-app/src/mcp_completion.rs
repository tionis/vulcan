//! Permission-filtered MCP completion shared by local and hosted transports.

#![allow(clippy::must_use_candidate)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path};
use vulcan_core::properties::load_note_index;
use vulcan_core::{PermissionGuard, PermissionMode, ProfilePermissionGuard, VaultPaths};

use crate::browse::collect_complete_candidates;
use crate::mcp_assistant::{prompt_visible, visible_prompts, visible_skills};
use crate::mcp_protocol::{McpCompletionParams, McpCompletionReference, McpMethodError};
use crate::notes::resolve_existing_note_path;

pub fn complete(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    params: &McpCompletionParams,
    help_topics: &[String],
) -> Result<serde_json::Value, McpMethodError> {
    let prefix = &params.argument.value;
    let values = match &params.reference {
        McpCompletionReference::Prompt { name } => {
            let prompt = vulcan_core::load_assistant_prompt(paths, name)
                .map_err(|error| McpMethodError::invalid_params(error.to_string()))?;
            if !prompt_visible(paths, guard, &prompt.summary) {
                return Err(McpMethodError::invalid_params(format!(
                    "prompt `{name}` is not available under profile `{}`",
                    guard.selection().name
                )));
            }
            let argument = prompt
                .summary
                .arguments
                .iter()
                .find(|argument| argument.name == params.argument.name)
                .ok_or_else(|| {
                    McpMethodError::invalid_params(format!(
                        "prompt `{name}` does not define argument `{}`",
                        params.argument.name
                    ))
                })?;
            complete_context(
                paths,
                guard,
                argument.completion.as_deref().unwrap_or_default(),
                prefix,
                help_topics,
            )?
        }
        McpCompletionReference::Resource { uri } if uri == "vulcan://help/{topic}" => {
            if params.argument.name != "topic" {
                return Err(McpMethodError::invalid_params(format!(
                    "resource template `{uri}` does not define argument `{}`",
                    params.argument.name
                )));
            }
            matching_help_topics(help_topics, prefix)
        }
        McpCompletionReference::Resource { uri } if uri == "vulcan://assistant/skills/{name}" => {
            if params.argument.name != "name" {
                return Err(McpMethodError::invalid_params(format!(
                    "resource template `{uri}` does not define argument `{}`",
                    params.argument.name
                )));
            }
            visible_skills(paths, guard)?
                .into_iter()
                .map(|skill| skill.name)
                .filter(|name| name.starts_with(prefix))
                .collect()
        }
        McpCompletionReference::Resource { uri } => {
            return Err(McpMethodError::invalid_params(format!(
                "unknown completion reference `{uri}`"
            )));
        }
    };

    Ok(serde_json::json!({
        "completion": {
            "values": values,
            "total": values.len(),
            "hasMore": false,
        }
    }))
}

fn complete_context(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    context: &str,
    prefix: &str,
    help_topics: &[String],
) -> Result<Vec<String>, McpMethodError> {
    match context {
        "" => Ok(Vec::new()),
        "note" => Ok(note_candidates(paths, guard, prefix)),
        "daily-date" => daily_date_candidates(paths, guard, prefix),
        "prompt-name" => Ok(visible_prompts(paths, guard)?
            .into_iter()
            .map(|prompt| prompt.name)
            .filter(|name| name.starts_with(prefix))
            .collect()),
        "skill-name" => Ok(visible_skills(paths, guard)?
            .into_iter()
            .map(|skill| skill.name)
            .filter(|name| name.starts_with(prefix))
            .collect()),
        "help-topic" => Ok(matching_help_topics(help_topics, prefix)),
        "bases-file" | "bases-view" | "kanban-board" | "vault-path" => {
            Ok(completion_candidates(paths, context, prefix)
                .into_iter()
                .filter(|candidate| can_read_relative_path(guard, candidate.trim_end_matches('/')))
                .collect())
        }
        "task-view" => {
            let config_visible = guard.check_config_read().is_ok();
            Ok(completion_candidates(paths, context, prefix)
                .into_iter()
                .filter(|candidate| {
                    if Path::new(candidate)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("base"))
                    {
                        return can_read_relative_path(guard, candidate);
                    }
                    config_visible
                })
                .collect())
        }
        "script" => {
            if !matches!(guard.selection().profile.execute, PermissionMode::Allow) {
                return Ok(Vec::new());
            }
            Ok(completion_candidates(paths, context, prefix)
                .into_iter()
                .filter(|candidate| {
                    can_read_relative_path(guard, &format!(".vulcan/scripts/{candidate}.js"))
                })
                .collect())
        }
        other => Ok(completion_candidates(paths, other, prefix)),
    }
}

fn note_candidates(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    prefix: &str,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    completion_candidates(paths, "note", prefix)
        .into_iter()
        .filter(|candidate| {
            if guard.read_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
                return true;
            }
            resolve_existing_note_path(paths, candidate)
                .is_ok_and(|path| guard.check_read_path(&path).is_ok())
        })
        .filter(|candidate| seen.insert(candidate.clone()))
        .collect()
}

fn daily_date_candidates(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    prefix: &str,
) -> Result<Vec<String>, McpMethodError> {
    if guard.selection().profile.read.is_none() {
        return Ok(Vec::new());
    }
    let mut dates = load_note_index(paths)
        .map_err(|error| McpMethodError::internal(error.to_string()))?
        .into_values()
        .filter(|note| note.periodic_type.as_deref() == Some("daily"))
        .filter(|note| can_read_relative_path(guard, &note.document_path))
        .filter_map(|note| note.periodic_date)
        .collect::<Vec<_>>();
    dates.sort_by(|left, right| right.cmp(left));
    dates.dedup();
    dates.retain(|date| date.starts_with(prefix));
    Ok(dates)
}

fn can_read_relative_path(guard: &ProfilePermissionGuard, relative_path: &str) -> bool {
    if guard.read_filter().path_permission().is_unrestricted() && !guard.has_policy_hook() {
        return true;
    }
    guard.check_read_path(relative_path).is_ok()
}

fn matching_help_topics(help_topics: &[String], prefix: &str) -> Vec<String> {
    let mut topics = help_topics
        .iter()
        .filter(|topic| topic.starts_with(prefix))
        .cloned()
        .collect::<Vec<_>>();
    topics.sort();
    topics.dedup();
    topics
}

fn completion_candidates(paths: &VaultPaths, context: &str, prefix: &str) -> Vec<String> {
    let candidates = match context {
        "vault-path" => return vault_path_candidates(paths, prefix),
        "script" => script_candidates(paths),
        _ => collect_complete_candidates(paths, context).unwrap_or_default(),
    };
    candidates
        .into_iter()
        .filter(|candidate| candidate.starts_with(prefix))
        .collect()
}

fn script_candidates(paths: &VaultPaths) -> Vec<String> {
    let scripts_dir = paths.vulcan_dir().join("scripts");
    if !scripts_dir.is_dir() {
        return Vec::new();
    }
    fs::read_dir(&scripts_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name();
                    let candidate = name.to_string_lossy();
                    candidate
                        .ends_with(".js")
                        .then(|| candidate.trim_end_matches(".js").to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

fn vault_path_candidates(paths: &VaultPaths, prefix: &str) -> Vec<String> {
    let prefix = prefix.replace('\\', "/");
    let trimmed = prefix.trim_start_matches("./");
    let (dir_prefix, partial_name) = match trimmed.rsplit_once('/') {
        Some((directory, partial)) => (directory.trim_end_matches('/'), partial),
        None => ("", trimmed),
    };
    if Path::new(dir_prefix).components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Vec::new();
    }
    let directory = if dir_prefix.is_empty() {
        paths.vault_root().to_path_buf()
    } else {
        paths.vault_root().join(dir_prefix)
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut candidates = BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.is_empty() || (dir_prefix.is_empty() && matches!(name.as_str(), ".git" | ".vulcan"))
        {
            continue;
        }
        if !name.starts_with(partial_name) {
            continue;
        }
        let mut candidate = if dir_prefix.is_empty() {
            name
        } else {
            format!("{dir_prefix}/{name}")
        };
        if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            candidate.push('/');
        }
        candidates.insert(candidate.replace('\\', "/"));
    }
    candidates.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use vulcan_core::resolve_permission_profile;

    #[test]
    fn help_completion_preserves_shape_and_rejects_unknown_arguments() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("readonly")).unwrap(),
        );
        let help_topics = vec!["note/get".to_string(), "note/set".to_string()];
        let params: McpCompletionParams = serde_json::from_value(json!({
            "ref": {"type": "ref/resource", "uri": "vulcan://help/{topic}"},
            "argument": {"name": "topic", "value": "note/"}
        }))
        .unwrap();
        let result = complete(&paths, &guard, &params, &help_topics).unwrap();
        assert_eq!(
            result["completion"]["values"],
            json!(["note/get", "note/set"])
        );
        assert_eq!(result["completion"]["hasMore"], false);
        let wrong: McpCompletionParams = serde_json::from_value(json!({
            "ref": {"type": "ref/resource", "uri": "vulcan://help/{topic}"},
            "argument": {"name": "name", "value": "note/"}
        }))
        .unwrap();
        assert!(complete(&paths, &guard, &wrong, &help_topics).is_err());
    }

    #[test]
    fn completion_cannot_escape_vault_or_reveal_blind_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        fs::create_dir_all(temporary.path().join(".vulcan")).unwrap();
        fs::write(
            temporary.path().join(".vulcan/config.toml"),
            "[permissions.profiles.blind]\nread = \"none\"\n",
        )
        .unwrap();
        fs::write(temporary.path().join("Secret.md"), "secret").unwrap();
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, Some("blind")).unwrap(),
        );
        assert!(vault_path_candidates(&paths, "../").is_empty());
        assert!(complete_context(&paths, &guard, "vault-path", "Sec", &[])
            .unwrap()
            .is_empty());
        assert!(complete_context(&paths, &guard, "daily-date", "", &[])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn script_completion_preserves_existing_cli_suffix_handling() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let scripts = paths.vulcan_dir().join("scripts");
        fs::create_dir_all(&scripts).unwrap();
        fs::write(scripts.join("example.js.js"), "").unwrap();
        assert_eq!(script_candidates(&paths), vec!["example".to_string()]);
    }
}
