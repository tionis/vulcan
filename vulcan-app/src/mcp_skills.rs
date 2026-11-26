//! Skills served over MCP as `skill://{name}/{path}` resources: Vulcan's
//! bundled skills, rendered for MCP agents, and the vault's own skills, as
//! written. A vault skill overrides a bundled one with the same name.

use std::path::Path;

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use vulcan_core::{
    list_assistant_skills, parse_assistant_skill_source, skill_frontmatter_json, PermissionProfile,
    ProfilePermissionGuard, VaultPaths,
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

/// The skills extension's identifier, for `capabilities.extensions`.
pub const MCP_SKILLS_EXTENSION: &str = "io.modelcontextprotocol/skills";
/// How long a client may keep a skill listing, manifest or directory.
pub const SKILL_TTL_MS: u64 = 300_000;

/// One skill as an MCP client sees it: its files as served (bundled skills
/// in their MCP rendering), so digests cover the rendered bytes.
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

    fn skill_md(&self) -> Option<&[u8]> {
        self.files
            .iter()
            .find(|(path, _)| path == "SKILL.md")
            .map(|(_, bytes)| bytes.as_slice())
    }

    /// The skill's frontmatter, every field as written, with the required
    /// `name` (the name the skill is served under) and `description`.
    fn frontmatter(&self) -> Value {
        let mut frontmatter = self
            .skill_md()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|source| skill_frontmatter_json(source, "SKILL.md").ok())
            .unwrap_or_else(|| json!({}));
        if let Some(object) = frontmatter.as_object_mut() {
            object.insert("name".to_string(), Value::String(self.name.clone()));
            if !object.get("description").is_some_and(Value::is_string) {
                object.insert(
                    "description".to_string(),
                    Value::String(self.description.clone().unwrap_or_default()),
                );
            }
        }
        frontmatter
    }

    /// The extension's `Skill` entry: `SKILL.md`'s URI, the frontmatter, and
    /// a complete manifest of the files served, each with its digest and size.
    #[must_use]
    pub fn entry(&self) -> Value {
        let resources = self
            .files
            .iter()
            .map(|(path, bytes)| {
                json!({
                    "uri": format!("skill://{}/{path}", self.name),
                    "digest": format!("sha256:{:x}", Sha256::digest(bytes)),
                    "size": bytes.len(),
                })
            })
            .collect::<Vec<_>>();
        json!({
            "uri": self.skill_md_uri(),
            "frontmatter": self.frontmatter(),
            "resources": resources,
        })
    }
}

/// `skills/list` entries, in name order.
#[must_use]
pub fn skill_entries(skills: &[McpSkill]) -> Vec<Value> {
    skills.iter().map(McpSkill::entry).collect()
}

/// `skills/get`: the entry for the skill whose `SKILL.md` is at `uri`.
pub fn get_skill(skills: &[McpSkill], uri: &str) -> Result<Value, McpMethodError> {
    skills
        .iter()
        .find(|skill| skill.skill_md_uri() == uri)
        .map(|skill| json!({ "skill": skill.entry() }))
        .ok_or_else(|| McpMethodError::JsonRpc {
            code: -32602,
            message: format!("No skill is served at {uri}"),
            data: Some(json!({ "uri": uri })),
        })
}

/// `resources/directory/read`: the direct children of a skill directory
/// (`skill://{name}` is the skill's root; no trailing slash), files with
/// their media type and subdirectories as `inode/directory`.
pub fn read_skill_directory(skills: &[McpSkill], uri: &str) -> Result<Vec<Value>, McpMethodError> {
    let not_directory = || McpMethodError::JsonRpc {
        code: -32602,
        message: format!("{uri} is not a directory resource"),
        data: Some(json!({ "uri": uri })),
    };
    let rest = uri.strip_prefix("skill://").ok_or_else(not_directory)?;
    if rest.is_empty() || rest.ends_with('/') {
        return Err(not_directory());
    }
    let (name, directory) = rest.split_once('/').unwrap_or((rest, ""));
    let skill = skills
        .iter()
        .find(|skill| skill.name == name)
        .ok_or_else(not_directory)?;
    let prefix = if directory.is_empty() {
        String::new()
    } else {
        format!("{directory}/")
    };
    let mut children = std::collections::BTreeMap::<String, bool>::new();
    for (path, _) in &skill.files {
        let Some(below) = path.strip_prefix(&prefix) else {
            continue;
        };
        match below.split_once('/') {
            Some((subdirectory, _)) => children.insert(subdirectory.to_string(), true),
            None => children.insert(below.to_string(), false),
        };
    }
    if children.is_empty() {
        // A file, or nothing at all: neither is a directory.
        return Err(not_directory());
    }
    Ok(children
        .into_iter()
        .map(|(child, is_directory)| {
            let child_uri = format!("{uri}/{child}");
            let mime_type = if is_directory {
                "inode/directory"
            } else {
                skill
                    .files
                    .iter()
                    .find(|(path, _)| *path == format!("{prefix}{child}"))
                    .map_or("application/octet-stream", |(path, bytes)| {
                        file_mime_type(path, bytes)
                    })
            };
            json!({ "uri": child_uri, "name": child, "mimeType": mime_type })
        })
        .collect())
}

fn file_mime_type(path: &str, bytes: &[u8]) -> &'static str {
    if std::str::from_utf8(bytes).is_err() {
        "application/octet-stream"
    } else if Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        "text/markdown"
    } else {
        "text/plain"
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
            "mimeType": file_mime_type(path, bytes),
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

    #[test]
    fn skill_entries_carry_verbatim_frontmatter_and_a_manifest_of_rendered_bytes() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        vulcan_core::paths::initialize_vulcan_dir(&VaultPaths::new(root)).unwrap();
        let skill_dir = root.join(".agents/skills/field-notes");
        std::fs::create_dir_all(skill_dir.join("references/deep")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: field-notes\ndescription: Take field notes.\nlicense: MIT\nmetadata:\n  team: research\n---\nRead references/guide.md.\n",
        )
        .unwrap();
        std::fs::write(skill_dir.join("references/guide.md"), "Guide.\n").unwrap();
        std::fs::write(skill_dir.join("references/deep/table.csv"), "a,b\n").unwrap();
        let served = skills(root, "unrestricted");

        let entry = get_skill(&served, "skill://field-notes/SKILL.md").unwrap()["skill"].clone();
        assert_eq!(entry["uri"], "skill://field-notes/SKILL.md");
        assert_eq!(entry["frontmatter"]["license"], "MIT");
        assert_eq!(entry["frontmatter"]["metadata"]["team"], "research");
        assert_eq!(entry["frontmatter"]["name"], "field-notes");
        let resources = entry["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 3);
        let guide = resources
            .iter()
            .find(|resource| resource["uri"] == "skill://field-notes/references/guide.md")
            .unwrap();
        assert_eq!(guide["size"], 7);
        assert_eq!(
            guide["digest"],
            format!("sha256:{:x}", Sha256::digest(b"Guide.\n"))
        );

        // Bundled skills are digested as rendered for MCP, not as written.
        let note = served
            .iter()
            .find(|skill| skill.name == "note-operations")
            .unwrap();
        let rendered = note.skill_md().unwrap();
        let entry = note.entry();
        let skill_md = entry["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|resource| resource["uri"] == "skill://note-operations/SKILL.md")
            .unwrap();
        assert_eq!(
            skill_md["digest"],
            format!("sha256:{:x}", Sha256::digest(rendered))
        );
        assert_eq!(skill_md["size"], rendered.len());
        assert!(entry["frontmatter"]["description"].is_string());
        assert_eq!(skill_entries(&served).len(), served.len());

        let unknown = get_skill(&served, "skill://missing/SKILL.md").unwrap_err();
        assert!(matches!(
            unknown,
            McpMethodError::JsonRpc { code: -32602, .. }
        ));
    }

    #[test]
    fn skill_directories_list_their_direct_children() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        vulcan_core::paths::initialize_vulcan_dir(&VaultPaths::new(root)).unwrap();
        let skill_dir = root.join(".agents/skills/field-notes");
        std::fs::create_dir_all(skill_dir.join("references/deep")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: field-notes\ndescription: Take field notes.\n---\nBody.\n",
        )
        .unwrap();
        std::fs::write(skill_dir.join("references/guide.md"), "Guide.\n").unwrap();
        std::fs::write(skill_dir.join("references/deep/table.csv"), "a,b\n").unwrap();
        let served = skills(root, "unrestricted");

        let top = read_skill_directory(&served, "skill://field-notes").unwrap();
        assert_eq!(
            top,
            vec![
                json!({"uri": "skill://field-notes/SKILL.md", "name": "SKILL.md", "mimeType": "text/markdown"}),
                json!({"uri": "skill://field-notes/references", "name": "references", "mimeType": "inode/directory"}),
            ]
        );
        let references = read_skill_directory(&served, "skill://field-notes/references").unwrap();
        assert_eq!(references[0]["mimeType"], "inode/directory");
        assert_eq!(
            references[1]["uri"],
            "skill://field-notes/references/guide.md"
        );
        for not_directory in [
            "skill://field-notes/SKILL.md",
            "skill://field-notes/",
            "skill://field-notes/missing",
            "skill://missing",
            "vulcan://help",
        ] {
            assert!(
                matches!(
                    read_skill_directory(&served, not_directory),
                    Err(McpMethodError::JsonRpc { code: -32602, .. })
                ),
                "{not_directory}"
            );
        }
    }
}
