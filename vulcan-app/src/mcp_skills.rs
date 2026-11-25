//! Skills served over MCP as `skill://{name}/{path}` resources: Vulcan's
//! bundled skills, rendered for MCP agents, and the vault's own skills, as
//! written. A vault skill overrides a bundled one with the same name.

use std::path::Path;

use base64::Engine;
use serde_json::{json, Value};
use vulcan_core::{
    list_assistant_skills, parse_assistant_skill_source, PermissionProfile, ProfilePermissionGuard,
    VaultPaths,
};

use crate::bundled_skills::{
    bundled_skill, bundled_skill_names, has_target_blocks, render_skill, SkillTarget,
};
use crate::mcp_assistant::skill_visible;
use crate::mcp_catalog::{tool_by_name, tool_visible};
use crate::mcp_protocol::{McpMethodError, MCP_RESOURCE_NOT_FOUND};

/// Limits from the skills extension: files per skill and bytes per skill.
const MAX_SKILL_FILES: usize = 512;
const MAX_SKILL_BYTES: usize = 16 * 1024 * 1024;

/// One skill as an MCP client sees it.
#[derive(Debug, Clone)]
pub struct McpSkill {
    pub name: String,
    pub description: Option<String>,
    pub files: Vec<(String, Vec<u8>)>,
}

impl McpSkill {
    #[must_use]
    pub fn skill_md_uri(&self) -> String {
        format!("skill://{}/SKILL.md", self.name)
    }
}

/// The skills this caller may load: bundled skills with an MCP rendering, and
/// vault skills under the read permission, each only if the caller may call
/// a built-in MCP tool its `tools:` names (or it names none).
pub fn visible_skills(
    paths: &VaultPaths,
    guard: &ProfilePermissionGuard,
    profile: &PermissionProfile,
) -> Result<Vec<McpSkill>, McpMethodError> {
    let mut skills = Vec::new();
    let vault_skills = if profile.read.is_none() {
        Vec::new()
    } else {
        list_assistant_skills(paths).unwrap_or_default()
    };
    let vault_names = vault_skills
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<Vec<_>>();
    for summary in vault_skills {
        if !skill_visible(paths, guard, &summary) || !tools_visible(&summary.tools, profile) {
            continue;
        }
        if let Some(skill) = load_vault_skill(
            paths,
            &summary.name,
            &summary.path,
            summary.description.as_ref(),
        ) {
            skills.push(skill);
        }
    }
    for name in bundled_skill_names() {
        if vault_names.iter().any(|vault| vault == name) {
            continue;
        }
        let files = bundled_skill(name);
        let Some(skill_md) = files.iter().find(|file| file.path.ends_with("/SKILL.md")) else {
            continue;
        };
        if !has_target_blocks(skill_md.contents, SkillTarget::Mcp) {
            continue;
        }
        let rendered = render_skill(skill_md.contents, SkillTarget::Mcp);
        let Ok(parsed) = parse_assistant_skill_source(&rendered, skill_md.path) else {
            continue;
        };
        if !tools_visible(&parsed.summary.tools, profile) {
            continue;
        }
        let files = files
            .iter()
            .map(|file| {
                let relative = file
                    .path
                    .split_once('/')
                    .map_or(file.path, |(_, rest)| rest);
                let contents = if file.path.ends_with("/SKILL.md") {
                    rendered.clone().into_bytes()
                } else {
                    file.contents.as_bytes().to_vec()
                };
                (relative.to_string(), contents)
            })
            .collect();
        skills.push(McpSkill {
            name: name.to_string(),
            description: parsed.summary.description,
            files,
        });
    }
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(skills)
}

/// A skill is useful to the caller when it names no built-in MCP tool or
/// at least one the caller may call; names that are not built-in MCP tools
/// (CLI commands, custom tools) do not count. A read-only connection still
/// gets the note skill for its reading half.
fn tools_visible(tools: &[String], profile: &PermissionProfile) -> bool {
    let mut named = tools
        .iter()
        .filter_map(|name| tool_by_name(name))
        .peekable();
    named.peek().is_none() || named.any(|tool| tool_visible(tool, profile))
}

fn load_vault_skill(
    paths: &VaultPaths,
    name: &str,
    skill_md: &str,
    description: Option<&String>,
) -> Option<McpSkill> {
    let root = vulcan_core::assistant_skills_root(paths).ok()?;
    let directory = root.join(Path::new(skill_md).parent()?);
    let mut files = Vec::new();
    let mut total = 0;
    collect_files(&directory, &directory, &mut files, &mut total).ok()?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Some(McpSkill {
        name: name.to_string(),
        description: description.cloned(),
        files,
    })
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<(String, Vec<u8>)>,
    total: &mut usize,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') || file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_files(root, &path, files, total)?;
        } else if file_type.is_file() {
            let bytes = std::fs::read(&path)?;
            *total += bytes.len();
            if files.len() >= MAX_SKILL_FILES || *total > MAX_SKILL_BYTES {
                return Err(std::io::Error::other("skill exceeds the MCP skill limits"));
            }
            let relative = path
                .strip_prefix(root)
                .map_err(std::io::Error::other)?
                .to_string_lossy()
                .replace('\\', "/");
            files.push((relative, bytes));
        }
    }
    Ok(())
}

/// The `resources/list` entries for skills: each skill's `SKILL.md`.
#[must_use]
pub fn skill_resources(skills: &[McpSkill]) -> Vec<Value> {
    skills
        .iter()
        .map(|skill| {
            let mut resource = json!({
                "uri": skill.skill_md_uri(),
                "name": skill.name,
                "title": format!("Skill: {}", skill.name),
                "mimeType": "text/markdown",
            });
            if let Some(description) = &skill.description {
                resource["description"] = Value::String(description.clone());
            }
            resource
        })
        .collect()
}

/// Read one `skill://{name}/{path}` file. `None` when the URI is not a
/// skill URI.
#[must_use]
pub fn read_skill_resource(
    skills: &[McpSkill],
    uri: &str,
) -> Option<Result<Value, McpMethodError>> {
    let rest = uri.strip_prefix("skill://")?;
    let not_found = || McpMethodError::JsonRpc {
        code: MCP_RESOURCE_NOT_FOUND,
        message: format!("Resource not found: {uri}"),
        data: Some(json!({ "uri": uri })),
    };
    let Some((name, path)) = rest.split_once('/') else {
        return Some(Err(not_found()));
    };
    let Some(bytes) = skills
        .iter()
        .find(|skill| skill.name == name)
        .and_then(|skill| skill.files.iter().find(|(file, _)| file == path))
        .map(|(_, bytes)| bytes)
    else {
        return Some(Err(not_found()));
    };
    let content = match std::str::from_utf8(bytes) {
        Ok(text) => json!({
            "uri": uri,
            "mimeType": if Path::new(path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            {
                "text/markdown"
            } else {
                "text/plain"
            },
            "text": text,
        }),
        Err(_) => json!({
            "uri": uri,
            "mimeType": "application/octet-stream",
            "blob": base64::engine::general_purpose::STANDARD.encode(bytes),
        }),
    };
    Some(Ok(json!({ "contents": [content] })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vulcan_core::resolve_permission_profile;

    fn skills(root: &Path, profile: &str) -> Vec<McpSkill> {
        let paths = VaultPaths::new(root);
        let selection = resolve_permission_profile(&paths, Some(profile)).unwrap();
        let guard = ProfilePermissionGuard::new(&paths, selection.clone());
        visible_skills(&paths, &guard, &selection.profile).unwrap()
    }

    #[test]
    fn bundled_skills_are_served_in_their_mcp_rendering_and_vault_skills_override_them() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        vulcan_core::paths::initialize_vulcan_dir(&VaultPaths::new(root)).unwrap();
        let served = skills(root, "unrestricted");
        let note = served
            .iter()
            .find(|skill| skill.name == "note-operations")
            .expect("note-operations has an MCP rendering");
        let text = String::from_utf8(note.files[0].1.clone()).unwrap();
        assert!(text.contains("`note_outline`"));
        assert!(!text.contains("vulcan note outline"));
        assert!(!text.contains("<!-- vulcan:"));
        // Skills without an MCP rendering stay CLI-only.
        assert!(!served
            .iter()
            .any(|skill| skill.name == "editor-integration"));
        // A read-only connection still gets the reading half of note work.
        assert!(skills(root, "readonly")
            .iter()
            .any(|skill| skill.name == "note-operations"));

        let skill_dir = root.join(".agents/skills/note-operations");
        std::fs::create_dir_all(skill_dir.join("references")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: note-operations\ndescription: The vault's own note rules.\n---\nOurs.\n",
        )
        .unwrap();
        std::fs::write(skill_dir.join("references/style.md"), "Style.\n").unwrap();
        let served = skills(root, "unrestricted");
        let note = served
            .iter()
            .find(|skill| skill.name == "note-operations")
            .unwrap();
        assert_eq!(
            note.description.as_deref(),
            Some("The vault's own note rules.")
        );
        let read = read_skill_resource(&served, "skill://note-operations/references/style.md")
            .unwrap()
            .unwrap();
        assert_eq!(read["contents"][0]["text"], "Style.\n");
        assert!(matches!(
            read_skill_resource(&served, "skill://note-operations/missing.md"),
            Some(Err(McpMethodError::JsonRpc { .. }))
        ));
        assert!(read_skill_resource(&served, "vulcan://help/tools").is_none());
        let listed = skill_resources(&served);
        assert!(listed
            .iter()
            .any(|resource| resource["uri"] == "skill://note-operations/SKILL.md"));
    }
}
