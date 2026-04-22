use crate::config::load_vault_config;
use crate::paths::VaultPaths;
use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssistantError {
    Message(String),
    Io(String),
    Parse(String),
}

impl AssistantError {
    fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }

    fn parse(message: impl Into<String>) -> Self {
        Self::Parse(message.into())
    }

    fn io(path: &Path, error: impl Display) -> Self {
        Self::Io(format!("{}: {error}", path.display()))
    }
}

impl Display for AssistantError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(message) | Self::Io(message) | Self::Parse(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for AssistantError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AssistantPromptArgument {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssistantPromptSummary {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub tags: Vec<String>,
    pub arguments: Vec<AssistantPromptArgument>,
    pub role: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssistantPrompt {
    #[serde(flatten)]
    pub summary: AssistantPromptSummary,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssistantSkillSummary {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub tools: Vec<String>,
    pub output_file: Option<String>,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssistantSkill {
    #[serde(flatten)]
    pub summary: AssistantSkillSummary,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssistantConfigSummary {
    pub prompts_folder: String,
    pub prompts_path: String,
    pub skills_folder: String,
    pub skills_path: String,
}

#[derive(Debug, Deserialize, Default)]
struct PromptFrontmatter {
    name: Option<String>,
    title: Option<String>,
    description: Option<String>,
    version: Option<YamlValue>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    arguments: Vec<AssistantPromptArgument>,
    role: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct SkillFrontmatter {
    name: Option<String>,
    title: Option<String>,
    description: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    tools: Vec<String>,
    output_file: Option<String>,
}

pub fn assistant_config_summary(paths: &VaultPaths) -> AssistantConfigSummary {
    let config = load_vault_config(paths).config.assistant;
    let prompts_path = paths.vault_root().join(&config.prompts_folder);
    let skills_path = paths.vault_root().join(&config.skills_folder);

    AssistantConfigSummary {
        prompts_folder: config.prompts_folder.to_string_lossy().replace('\\', "/"),
        prompts_path: prompts_path.display().to_string(),
        skills_folder: config.skills_folder.to_string_lossy().replace('\\', "/"),
        skills_path: skills_path.display().to_string(),
    }
}

pub fn assistant_prompts_root(paths: &VaultPaths) -> PathBuf {
    paths
        .vault_root()
        .join(load_vault_config(paths).config.assistant.prompts_folder)
}

pub fn assistant_skills_root(paths: &VaultPaths) -> PathBuf {
    paths
        .vault_root()
        .join(load_vault_config(paths).config.assistant.skills_folder)
}

pub fn list_assistant_prompts(
    paths: &VaultPaths,
) -> Result<Vec<AssistantPromptSummary>, AssistantError> {
    let root = assistant_prompts_root(paths);
    let mut prompts = Vec::new();
    for path in collect_markdown_files(&root)? {
        let prompt = parse_prompt_file(&root, &path)?;
        prompts.push(prompt.summary);
    }
    prompts.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(prompts)
}

pub fn load_assistant_prompt(
    paths: &VaultPaths,
    identifier: &str,
) -> Result<AssistantPrompt, AssistantError> {
    let root = assistant_prompts_root(paths);
    let candidates = collect_markdown_files(&root)?;
    find_prompt_by_identifier(&root, &candidates, identifier)?
        .ok_or_else(|| AssistantError::message(format!("unknown prompt `{identifier}`")))
}

pub fn render_assistant_prompt(
    prompt: &AssistantPrompt,
    arguments: &std::collections::BTreeMap<String, String>,
) -> Result<String, AssistantError> {
    for argument in &prompt.summary.arguments {
        if argument.required && !arguments.contains_key(&argument.name) {
            return Err(AssistantError::message(format!(
                "missing required prompt argument `{}`",
                argument.name
            )));
        }
    }

    let mut rendered = prompt.body.clone();
    for argument in &prompt.summary.arguments {
        let placeholder = format!("{{{{{}}}}}", argument.name);
        let replacement = arguments.get(&argument.name).cloned().unwrap_or_default();
        rendered = rendered.replace(&placeholder, &replacement);
    }
    Ok(rendered)
}

pub fn list_assistant_skills(
    paths: &VaultPaths,
) -> Result<Vec<AssistantSkillSummary>, AssistantError> {
    let root = assistant_skills_root(paths);
    let mut skills = Vec::new();
    for path in collect_skill_files(&root)? {
        let skill = parse_skill_file(&root, &path)?;
        skills.push(skill.summary);
    }
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(skills)
}

pub fn load_assistant_skill(
    paths: &VaultPaths,
    identifier: &str,
) -> Result<AssistantSkill, AssistantError> {
    let root = assistant_skills_root(paths);
    let candidates = collect_skill_files(&root)?;
    find_skill_by_identifier(&root, &candidates, identifier)?
        .ok_or_else(|| AssistantError::message(format!("unknown skill `{identifier}`")))
}

pub fn read_vault_agents_file(paths: &VaultPaths) -> Result<Option<String>, AssistantError> {
    let path = paths.vault_root().join("AGENTS.md");
    if !path.is_file() {
        return Ok(None);
    }
    fs::read_to_string(&path)
        .map(Some)
        .map_err(|error| AssistantError::io(&path, error))
}

fn find_prompt_by_identifier(
    root: &Path,
    candidates: &[PathBuf],
    identifier: &str,
) -> Result<Option<AssistantPrompt>, AssistantError> {
    let normalized = normalize_identifier(identifier);
    for path in candidates {
        let prompt = parse_prompt_file(root, path)?;
        if prompt.summary.name == normalized
            || normalize_identifier(&prompt.summary.path) == normalized
            || normalize_identifier(path_stem_path(&prompt.summary.path)) == normalized
        {
            return Ok(Some(prompt));
        }
    }
    Ok(None)
}

fn find_skill_by_identifier(
    root: &Path,
    candidates: &[PathBuf],
    identifier: &str,
) -> Result<Option<AssistantSkill>, AssistantError> {
    let normalized = normalize_identifier(identifier);
    for path in candidates {
        let skill = parse_skill_file(root, path)?;
        if skill.summary.name == normalized
            || normalize_identifier(&skill.summary.path) == normalized
            || normalize_identifier(directory_name(&skill.summary.path)) == normalized
        {
            return Ok(Some(skill));
        }
    }
    Ok(None)
}

fn parse_prompt_file(root: &Path, path: &Path) -> Result<AssistantPrompt, AssistantError> {
    let relative = relative_display(root, path)?;
    let source = fs::read_to_string(path).map_err(|error| AssistantError::io(path, error))?;
    let (frontmatter, body) = split_markdown_frontmatter(&source, path)?;
    let frontmatter = frontmatter
        .map(parse_yaml_frontmatter::<PromptFrontmatter>)
        .transpose()?
        .unwrap_or_default();
    let default_name = normalize_identifier(path_stem_path(&relative));
    let prompt = AssistantPrompt {
        summary: AssistantPromptSummary {
            name: normalize_optional(frontmatter.name).unwrap_or(default_name),
            title: normalize_optional(frontmatter.title),
            description: normalize_optional(frontmatter.description),
            version: frontmatter.version.as_ref().and_then(yaml_scalar_to_string),
            tags: normalize_list(frontmatter.tags),
            arguments: normalize_prompt_arguments(frontmatter.arguments),
            role: normalize_prompt_role(frontmatter.role),
            path: relative,
        },
        body: body.trim().to_string(),
    };
    Ok(prompt)
}

fn parse_skill_file(root: &Path, path: &Path) -> Result<AssistantSkill, AssistantError> {
    let relative = relative_display(root, path)?;
    let source = fs::read_to_string(path).map_err(|error| AssistantError::io(path, error))?;
    let (frontmatter, body) = split_markdown_frontmatter(&source, path)?;
    let frontmatter = frontmatter
        .map(parse_yaml_frontmatter::<SkillFrontmatter>)
        .transpose()?
        .unwrap_or_default();
    let default_name = normalize_identifier(directory_name(&relative));
    let skill = AssistantSkill {
        summary: AssistantSkillSummary {
            name: normalize_optional(frontmatter.name).unwrap_or(default_name),
            title: normalize_optional(frontmatter.title),
            description: normalize_optional(frontmatter.description),
            tags: normalize_list(frontmatter.tags),
            tools: normalize_list(frontmatter.tools),
            output_file: normalize_optional(frontmatter.output_file),
            path: relative,
        },
        body: body.trim().to_string(),
    };
    Ok(skill)
}

fn parse_yaml_frontmatter<T: for<'de> Deserialize<'de>>(
    frontmatter: &str,
) -> Result<T, AssistantError> {
    serde_yaml::from_str(frontmatter)
        .map_err(|error| AssistantError::parse(format!("invalid assistant frontmatter: {error}")))
}

fn split_markdown_frontmatter<'a>(
    source: &'a str,
    path: &Path,
) -> Result<(Option<&'a str>, &'a str), AssistantError> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut lines = source.split_inclusive('\n');
    let Some(first_line) = lines.next() else {
        return Ok((None, source));
    };
    if first_line.trim_end_matches(['\r', '\n']) != "---" {
        return Ok((None, source));
    }

    let mut offset = first_line.len();
    for line in lines {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            let frontmatter = &source[first_line.len()..offset];
            let body = &source[offset + line.len()..];
            return Ok((Some(frontmatter), body));
        }
        offset += line.len();
    }

    Err(AssistantError::parse(format!(
        "{} has an unterminated frontmatter block",
        path.display()
    )))
}

fn collect_markdown_files(root: &Path) -> Result<Vec<PathBuf>, AssistantError> {
    let mut files = Vec::new();
    collect_matching_files(root, &mut files, |path| {
        path.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
    })?;
    files.sort();
    Ok(files)
}

fn collect_skill_files(root: &Path) -> Result<Vec<PathBuf>, AssistantError> {
    let mut files = Vec::new();
    collect_matching_files(root, &mut files, |path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md"))
    })?;
    files.sort();
    Ok(files)
}

fn collect_matching_files(
    root: &Path,
    files: &mut Vec<PathBuf>,
    include: impl Fn(&Path) -> bool + Copy,
) -> Result<(), AssistantError> {
    if !root.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(root).map_err(|error| AssistantError::io(root, error))?;
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| AssistantError::io(root, error))?;
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_matching_files(&path, files, include)?;
        } else if include(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn relative_display(root: &Path, path: &Path) -> Result<String, AssistantError> {
    path.strip_prefix(root)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .map_err(|_| {
            AssistantError::message(format!("{} is outside {}", path.display(), root.display()))
        })
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_list(values: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for value in values {
        let Some(value) = normalize_optional(Some(value)) else {
            continue;
        };
        if !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    normalized
}

fn normalize_prompt_arguments(
    arguments: Vec<AssistantPromptArgument>,
) -> Vec<AssistantPromptArgument> {
    let mut normalized = Vec::new();
    for argument in arguments {
        let Some(name) = normalize_optional(Some(argument.name)) else {
            continue;
        };
        if normalized
            .iter()
            .any(|existing: &AssistantPromptArgument| existing.name == name)
        {
            continue;
        }
        normalized.push(AssistantPromptArgument {
            name,
            title: normalize_optional(argument.title),
            description: normalize_optional(argument.description),
            required: argument.required,
            completion: normalize_optional(argument.completion),
        });
    }
    normalized
}

fn normalize_prompt_role(role: Option<String>) -> String {
    match normalize_optional(role).as_deref() {
        Some("assistant") => "assistant".to_string(),
        _ => "user".to_string(),
    }
}

fn yaml_scalar_to_string(value: &YamlValue) -> Option<String> {
    match value {
        YamlValue::Bool(value) => Some(value.to_string()),
        YamlValue::Number(value) => Some(value.to_string()),
        YamlValue::String(value) => normalize_optional(Some(value.clone())),
        _ => None,
    }
}

fn normalize_identifier(value: impl AsRef<str>) -> String {
    value
        .as_ref()
        .trim()
        .trim_end_matches(".md")
        .replace('\\', "/")
}

fn path_stem_path(path: &str) -> &str {
    path.strip_suffix(".md").unwrap_or(path)
}

fn directory_name(path: &str) -> &str {
    Path::new(path)
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::{
        assistant_config_summary, list_assistant_prompts, list_assistant_skills,
        load_assistant_prompt, load_assistant_skill, read_vault_agents_file,
        render_assistant_prompt,
    };
    use crate::paths::{initialize_vulcan_dir, VaultPaths};
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::tempdir;

    fn test_paths() -> (tempfile::TempDir, VaultPaths) {
        let dir = tempdir().expect("temp dir");
        let paths = VaultPaths::new(dir.path());
        initialize_vulcan_dir(&paths).expect("init should succeed");
        (dir, paths)
    }

    #[test]
    fn assistant_config_summary_uses_default_folders() {
        let (_dir, paths) = test_paths();

        let summary = assistant_config_summary(&paths);

        assert_eq!(summary.prompts_folder, "AI/Prompts");
        assert_eq!(summary.skills_folder, ".agents/skills");
    }

    #[test]
    fn prompt_loader_reads_frontmatter_and_renders_arguments() {
        let (_dir, paths) = test_paths();
        let prompts_root = paths.vault_root().join("AI/Prompts");
        fs::create_dir_all(&prompts_root).expect("prompts dir");
        fs::write(
            prompts_root.join("summarize.md"),
            r#"---
name: summarize
title: Summarize Note
description: Summarize one note
version: 1
role: assistant
tags:
  - review
arguments:
  - name: note
    required: true
    completion: note
---
Summarize {{note}}.
"#,
        )
        .expect("prompt file");

        let prompts = list_assistant_prompts(&paths).expect("prompts should load");
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "summarize");
        assert_eq!(prompts[0].role, "assistant");
        assert_eq!(prompts[0].arguments[0].completion.as_deref(), Some("note"));

        let prompt = load_assistant_prompt(&paths, "summarize").expect("prompt should load");
        let mut args = BTreeMap::new();
        args.insert("note".to_string(), "Projects/Alpha.md".to_string());
        let rendered = render_assistant_prompt(&prompt, &args).expect("render should succeed");
        assert_eq!(rendered, "Summarize Projects/Alpha.md.");
    }

    #[test]
    fn skill_loader_reads_default_skill_layout() {
        let (_dir, paths) = test_paths();
        let skill_root = paths.vault_root().join(".agents/skills/daily-review");
        fs::create_dir_all(&skill_root).expect("skill dir");
        fs::write(
            skill_root.join("SKILL.md"),
            r#"---
name: daily-review
description: Review the day
tools:
  - note_get
  - search
output_file: Reviews/{{date}}.md
---
Use this skill for a daily summary.
"#,
        )
        .expect("skill file");

        let skills = list_assistant_skills(&paths).expect("skills should load");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "daily-review");
        assert_eq!(
            skills[0].tools,
            vec!["note_get".to_string(), "search".to_string()]
        );

        let skill = load_assistant_skill(&paths, "daily-review").expect("skill should load");
        assert!(skill.body.contains("daily summary"));
    }

    #[test]
    fn agents_file_loader_returns_none_when_missing() {
        let (_dir, paths) = test_paths();
        assert!(read_vault_agents_file(&paths)
            .expect("agents file read should succeed")
            .is_none());
    }
}
