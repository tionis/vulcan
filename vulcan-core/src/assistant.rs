use crate::config::load_vault_config;
use crate::paths::VaultPaths;
use crate::JsRuntimeSandbox;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use serde_yaml::Value as YamlValue;
use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Component, Path, PathBuf};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AssistantToolRuntime {
    #[default]
    Quickjs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantToolValidationOptions {
    pub reserved_names: Vec<String>,
    pub allowed_pack_names: Vec<String>,
}

impl Default for AssistantToolValidationOptions {
    fn default() -> Self {
        Self {
            reserved_names: default_assistant_tool_reserved_names(),
            allowed_pack_names: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AssistantToolSummary {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub version: Option<String>,
    pub runtime: AssistantToolRuntime,
    pub entrypoint: String,
    pub entrypoint_path: String,
    pub tags: Vec<String>,
    pub sandbox: JsRuntimeSandbox,
    pub permission_profile: Option<String>,
    pub timeout_ms: Option<usize>,
    pub packs: Vec<String>,
    pub secrets: Vec<AssistantToolSecretSpec>,
    pub read_only: bool,
    pub destructive: bool,
    pub input_schema: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<JsonValue>,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AssistantTool {
    #[serde(flatten)]
    pub summary: AssistantToolSummary,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantToolSecretSpec {
    pub name: String,
    pub env: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AssistantConfigSummary {
    pub prompts_folder: String,
    pub prompts_path: String,
    pub skills_folder: String,
    pub skills_path: String,
    pub tools_folder: String,
    pub tools_path: String,
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

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ToolFrontmatter {
    name: Option<String>,
    title: Option<String>,
    description: Option<String>,
    version: Option<YamlValue>,
    entrypoint: Option<String>,
    runtime: Option<AssistantToolRuntime>,
    #[serde(default)]
    tags: Vec<String>,
    sandbox: Option<JsRuntimeSandbox>,
    permission_profile: Option<String>,
    timeout_ms: Option<usize>,
    #[serde(default)]
    packs: Vec<String>,
    #[serde(default)]
    secrets: Vec<AssistantToolSecretSpec>,
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    destructive: bool,
    input_schema: Option<JsonValue>,
    output_schema: Option<JsonValue>,
}

#[must_use]
pub fn assistant_config_summary(paths: &VaultPaths) -> AssistantConfigSummary {
    let config = load_vault_config(paths).config.assistant;
    let prompts_path = paths.vault_root().join(&config.prompts_folder);
    let skills_path = paths.vault_root().join(&config.skills_folder);
    let tools_path = paths.vault_root().join(&config.tools_folder);

    AssistantConfigSummary {
        prompts_folder: config.prompts_folder.to_string_lossy().replace('\\', "/"),
        prompts_path: prompts_path.display().to_string(),
        skills_folder: config.skills_folder.to_string_lossy().replace('\\', "/"),
        skills_path: skills_path.display().to_string(),
        tools_folder: config.tools_folder.to_string_lossy().replace('\\', "/"),
        tools_path: tools_path.display().to_string(),
    }
}

#[must_use]
pub fn assistant_prompts_root(paths: &VaultPaths) -> PathBuf {
    paths
        .vault_root()
        .join(load_vault_config(paths).config.assistant.prompts_folder)
}

#[must_use]
pub fn assistant_skills_root(paths: &VaultPaths) -> PathBuf {
    paths
        .vault_root()
        .join(load_vault_config(paths).config.assistant.skills_folder)
}

#[must_use]
pub fn assistant_tools_root(paths: &VaultPaths) -> PathBuf {
    paths
        .vault_root()
        .join(load_vault_config(paths).config.assistant.tools_folder)
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

#[must_use]
pub fn default_assistant_tool_reserved_names() -> Vec<String> {
    vec![
        "tool_init".to_string(),
        "tool_list".to_string(),
        "tool_run".to_string(),
        "tool_set".to_string(),
        "tool_show".to_string(),
        "tool_validate".to_string(),
    ]
}

pub fn list_assistant_tools(
    paths: &VaultPaths,
    options: &AssistantToolValidationOptions,
) -> Result<Vec<AssistantToolSummary>, AssistantError> {
    let tools = collect_assistant_tools(&assistant_tools_root(paths), options)?;
    Ok(tools.into_iter().map(|tool| tool.summary).collect())
}

pub fn list_assistant_tool_manifest_paths(
    paths: &VaultPaths,
) -> Result<Vec<String>, AssistantError> {
    let root = assistant_tools_root(paths);
    collect_tool_files(&root)?
        .into_iter()
        .map(|path| relative_display(&root, &path))
        .collect()
}

pub fn load_assistant_tool(
    paths: &VaultPaths,
    identifier: &str,
    options: &AssistantToolValidationOptions,
) -> Result<AssistantTool, AssistantError> {
    let root = assistant_tools_root(paths);
    let tools = collect_assistant_tools(&root, options)?;
    let normalized = normalize_identifier(identifier);
    tools
        .into_iter()
        .find(|tool| {
            tool.summary.name == normalized
                || normalize_identifier(&tool.summary.path) == normalized
                || normalize_identifier(directory_name(&tool.summary.path)) == normalized
        })
        .ok_or_else(|| AssistantError::message(format!("unknown tool `{identifier}`")))
}

pub fn load_assistant_tool_manifest(
    paths: &VaultPaths,
    manifest_path: &str,
    options: &AssistantToolValidationOptions,
) -> Result<AssistantTool, AssistantError> {
    let root = assistant_tools_root(paths);
    let path = resolve_assistant_relative_path(&root, manifest_path, "tool manifest")?;
    let tool = parse_tool_file(&root, &path, options)?;
    validate_tool_name_collisions(std::slice::from_ref(&tool), options)?;
    Ok(tool)
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

fn collect_assistant_tools(
    root: &Path,
    options: &AssistantToolValidationOptions,
) -> Result<Vec<AssistantTool>, AssistantError> {
    let mut tools = collect_tool_files(root)?
        .into_iter()
        .map(|path| parse_tool_file(root, &path, options))
        .collect::<Result<Vec<_>, _>>()?;
    validate_tool_name_collisions(&tools, options)?;
    tools.sort_by(|left, right| left.summary.name.cmp(&right.summary.name));
    Ok(tools)
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

fn parse_tool_file(
    root: &Path,
    path: &Path,
    options: &AssistantToolValidationOptions,
) -> Result<AssistantTool, AssistantError> {
    let relative = relative_display(root, path)?;
    let source = fs::read_to_string(path).map_err(|error| AssistantError::io(path, error))?;
    let (frontmatter, body) = split_markdown_frontmatter(&source, path)?;
    let frontmatter = frontmatter
        .map(parse_yaml_frontmatter::<ToolFrontmatter>)
        .transpose()?
        .unwrap_or_default();

    let name = normalize_optional(frontmatter.name).ok_or_else(|| {
        AssistantError::parse(format!(
            "{} must set a non-empty `name` in frontmatter",
            path.display()
        ))
    })?;
    validate_tool_name(&name, path)?;

    let description = normalize_optional(frontmatter.description).ok_or_else(|| {
        AssistantError::parse(format!(
            "{} must set a non-empty `description` in frontmatter",
            path.display()
        ))
    })?;

    let input_schema = normalize_schema_value(frontmatter.input_schema, path, "input_schema")?;
    let output_schema =
        normalize_optional_schema_value(frontmatter.output_schema, path, "output_schema")?;
    let runtime = frontmatter.runtime.unwrap_or_default();
    if runtime != AssistantToolRuntime::Quickjs {
        return Err(AssistantError::parse(format!(
            "{} uses unsupported runtime `{:?}`; only `quickjs` is currently supported",
            path.display(),
            runtime
        )));
    }

    let sandbox = frontmatter.sandbox.unwrap_or(JsRuntimeSandbox::Strict);
    if sandbox == JsRuntimeSandbox::None {
        return Err(AssistantError::parse(format!(
            "{} cannot set `sandbox = none` for a custom tool",
            path.display()
        )));
    }
    if frontmatter.timeout_ms == Some(0) {
        return Err(AssistantError::parse(format!(
            "{} has invalid `timeout_ms`; expected a value greater than 0",
            path.display()
        )));
    }

    let tool_dir = path.parent().ok_or_else(|| {
        AssistantError::message(format!("{} has no parent directory", path.display()))
    })?;
    let entrypoint = normalize_tool_entrypoint(frontmatter.entrypoint, path)?;
    let absolute_entrypoint = tool_dir.join(&entrypoint);
    if !absolute_entrypoint.is_file() {
        return Err(AssistantError::parse(format!(
            "{} entrypoint `{}` was not found",
            path.display(),
            absolute_entrypoint.display()
        )));
    }

    let packs = normalize_tool_packs(frontmatter.packs, path, options)?;
    let secrets = normalize_tool_secrets(frontmatter.secrets, path)?;

    Ok(AssistantTool {
        summary: AssistantToolSummary {
            name,
            title: normalize_optional(frontmatter.title),
            description,
            version: frontmatter.version.as_ref().and_then(yaml_scalar_to_string),
            runtime,
            entrypoint,
            entrypoint_path: relative_display(root, &absolute_entrypoint)?,
            tags: normalize_list(frontmatter.tags),
            sandbox,
            permission_profile: normalize_optional(frontmatter.permission_profile),
            timeout_ms: frontmatter.timeout_ms,
            packs,
            secrets,
            read_only: frontmatter.read_only,
            destructive: frontmatter.destructive,
            input_schema,
            output_schema,
            path: relative,
        },
        body: body.trim().to_string(),
    })
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

fn collect_tool_files(root: &Path) -> Result<Vec<PathBuf>, AssistantError> {
    let mut files = Vec::new();
    collect_matching_files(root, &mut files, |path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("TOOL.md"))
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

fn resolve_assistant_relative_path(
    root: &Path,
    relative: &str,
    kind: &str,
) -> Result<PathBuf, AssistantError> {
    let candidate = Path::new(relative);
    if candidate.as_os_str().is_empty()
        || candidate.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(AssistantError::message(format!(
            "invalid {kind} path `{relative}`"
        )));
    }
    let resolved = root.join(candidate);
    if !resolved.is_file() {
        return Err(AssistantError::message(format!(
            "unknown {kind} `{relative}`"
        )));
    }
    Ok(resolved)
}

fn validate_tool_name_collisions(
    tools: &[AssistantTool],
    options: &AssistantToolValidationOptions,
) -> Result<(), AssistantError> {
    let mut seen = BTreeSet::new();
    let reserved = options
        .reserved_names
        .iter()
        .map(normalize_identifier)
        .collect::<BTreeSet<_>>();
    for tool in tools {
        if reserved.contains(&tool.summary.name) {
            return Err(AssistantError::parse(format!(
                "custom tool `{}` collides with a reserved or built-in tool name",
                tool.summary.name
            )));
        }
        if !seen.insert(tool.summary.name.clone()) {
            return Err(AssistantError::parse(format!(
                "duplicate custom tool name `{}`",
                tool.summary.name
            )));
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

fn validate_tool_name(name: &str, path: &Path) -> Result<(), AssistantError> {
    if !name.chars().all(|character| {
        character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
    }) {
        return Err(AssistantError::parse(format!(
            "{} has invalid tool name `{name}`; custom tool names must be `snake_case`",
            path.display()
        )));
    }
    if !name
        .chars()
        .any(|character| character.is_ascii_alphabetic())
    {
        return Err(AssistantError::parse(format!(
            "{} has invalid tool name `{name}`; custom tool names must include at least one ASCII letter",
            path.display()
        )));
    }
    Ok(())
}

fn normalize_schema_value(
    value: Option<JsonValue>,
    path: &Path,
    field_name: &str,
) -> Result<JsonValue, AssistantError> {
    let value = value.ok_or_else(|| {
        AssistantError::parse(format!(
            "{} must set `{field_name}` to a JSON Schema object",
            path.display()
        ))
    })?;
    if !value.is_object() {
        return Err(AssistantError::parse(format!(
            "{} field `{field_name}` must be a JSON Schema object",
            path.display()
        )));
    }
    Ok(value)
}

fn normalize_optional_schema_value(
    value: Option<JsonValue>,
    path: &Path,
    field_name: &str,
) -> Result<Option<JsonValue>, AssistantError> {
    value
        .map(|value| {
            if value.is_object() {
                Ok(value)
            } else {
                Err(AssistantError::parse(format!(
                    "{} field `{field_name}` must be a JSON Schema object",
                    path.display()
                )))
            }
        })
        .transpose()
}

fn normalize_tool_entrypoint(
    entrypoint: Option<String>,
    path: &Path,
) -> Result<String, AssistantError> {
    let entrypoint = normalize_optional(entrypoint).unwrap_or_else(|| "main.js".to_string());
    let entrypoint_path = Path::new(&entrypoint);
    if entrypoint_path.is_absolute() {
        return Err(AssistantError::parse(format!(
            "{} entrypoint `{entrypoint}` must be a relative path",
            path.display()
        )));
    }
    if entrypoint.contains(':') {
        return Err(AssistantError::parse(format!(
            "{} entrypoint `{entrypoint}` must not contain a drive or URI prefix",
            path.display()
        )));
    }
    for component in entrypoint_path.components() {
        if matches!(component, Component::ParentDir | Component::RootDir) {
            return Err(AssistantError::parse(format!(
                "{} entrypoint `{entrypoint}` must stay within the tool directory",
                path.display()
            )));
        }
    }
    Ok(entrypoint_path.to_string_lossy().replace('\\', "/"))
}

fn normalize_tool_packs(
    packs: Vec<String>,
    path: &Path,
    options: &AssistantToolValidationOptions,
) -> Result<Vec<String>, AssistantError> {
    let packs = if packs.is_empty() {
        vec!["custom".to_string()]
    } else {
        normalize_list(packs)
    };
    let mut allowed = options
        .allowed_pack_names
        .iter()
        .map(normalize_identifier)
        .collect::<BTreeSet<_>>();
    allowed.insert("custom".to_string());
    for pack in &packs {
        if !allowed.contains(pack) {
            return Err(AssistantError::parse(format!(
                "{} uses unknown tool pack `{pack}`",
                path.display()
            )));
        }
    }
    Ok(packs)
}

fn normalize_tool_secrets(
    secrets: Vec<AssistantToolSecretSpec>,
    path: &Path,
) -> Result<Vec<AssistantToolSecretSpec>, AssistantError> {
    let mut normalized = Vec::new();
    for secret in secrets {
        let Some(name) = normalize_optional(Some(secret.name)) else {
            return Err(AssistantError::parse(format!(
                "{} has a secret declaration with an empty `name`",
                path.display()
            )));
        };
        validate_tool_name(&name, path)?;
        let Some(env) = normalize_optional(Some(secret.env)) else {
            return Err(AssistantError::parse(format!(
                "{} secret `{name}` must set a non-empty `env`",
                path.display()
            )));
        };
        validate_secret_env_name(&env, path, &name)?;
        if normalized
            .iter()
            .any(|existing: &AssistantToolSecretSpec| existing.name == name)
        {
            return Err(AssistantError::parse(format!(
                "{} declares duplicate secret `{name}`",
                path.display()
            )));
        }
        normalized.push(AssistantToolSecretSpec {
            name,
            env,
            required: secret.required,
            description: normalize_optional(secret.description),
        });
    }
    Ok(normalized)
}

fn validate_secret_env_name(
    env: &str,
    path: &Path,
    secret_name: &str,
) -> Result<(), AssistantError> {
    let valid = env
        .chars()
        .enumerate()
        .all(|(index, character)| match index {
            0 => character.is_ascii_alphabetic() || character == '_',
            _ => character.is_ascii_alphanumeric() || character == '_',
        });
    if valid {
        Ok(())
    } else {
        Err(AssistantError::parse(format!(
            "{} secret `{secret_name}` has invalid env var name `{env}`",
            path.display()
        )))
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
        assistant_config_summary, default_assistant_tool_reserved_names, list_assistant_prompts,
        list_assistant_skills, list_assistant_tools, load_assistant_prompt, load_assistant_skill,
        load_assistant_tool, read_vault_agents_file, render_assistant_prompt,
        AssistantToolValidationOptions,
    };
    use crate::config::JsRuntimeSandbox;
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
        assert_eq!(summary.tools_folder, ".agents/tools");
    }

    #[test]
    fn prompt_loader_reads_frontmatter_and_renders_arguments() {
        let (_dir, paths) = test_paths();
        let prompts_root = paths.vault_root().join("AI/Prompts");
        fs::create_dir_all(&prompts_root).expect("prompts dir");
        fs::write(
            prompts_root.join("summarize.md"),
            r"---
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
",
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
            r"---
name: daily-review
description: Review the day
tools:
  - note_get
  - search
output_file: Reviews/{{date}}.md
---
Use this skill for a daily summary.
",
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

    #[test]
    fn tool_loader_reads_frontmatter_and_entrypoint() {
        let (_dir, paths) = test_paths();
        let tool_root = paths.vault_root().join(".agents/tools/summarize-meeting");
        fs::create_dir_all(&tool_root).expect("tool dir");
        fs::write(
            tool_root.join("TOOL.md"),
            r"---
name: summarize_meeting
title: Summarize Meeting
description: Summarize one note into decisions and actions.
version: 1
runtime: quickjs
entrypoint: main.js
tags:
  - meetings
sandbox: fs
permission_profile: readonly
timeout_ms: 5000
packs:
  - custom
secrets:
  - name: openai
    env: OPENAI_API_KEY
    required: true
    description: API key for remote summaries
read_only: true
input_schema:
  type: object
  additionalProperties: false
  properties:
    note:
      type: string
  required:
    - note
output_schema:
  type: object
  additionalProperties: false
  properties:
    title:
      type: string
---
Use this when the caller already knows the note to summarize.
",
        )
        .expect("tool manifest");
        fs::write(
            tool_root.join("main.js"),
            "function main(input) { return { title: input.note }; }\n",
        )
        .expect("tool entrypoint");

        let tools = list_assistant_tools(
            &paths,
            &AssistantToolValidationOptions {
                allowed_pack_names: vec!["custom".to_string()],
                ..AssistantToolValidationOptions::default()
            },
        )
        .expect("tools should load");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "summarize_meeting");
        assert_eq!(tools[0].sandbox, JsRuntimeSandbox::Fs);
        assert_eq!(tools[0].entrypoint_path, "summarize-meeting/main.js");

        let tool = load_assistant_tool(
            &paths,
            "summarize_meeting",
            &AssistantToolValidationOptions {
                allowed_pack_names: vec!["custom".to_string()],
                ..AssistantToolValidationOptions::default()
            },
        )
        .expect("tool should load");
        assert!(tool.body.contains("already knows the note"));
        assert_eq!(tool.summary.permission_profile.as_deref(), Some("readonly"));
        assert_eq!(tool.summary.timeout_ms, Some(5000));
        assert_eq!(tool.summary.secrets.len(), 1);
        assert_eq!(tool.summary.secrets[0].env, "OPENAI_API_KEY");
    }

    #[test]
    fn tool_loader_rejects_reserved_names() {
        let (_dir, paths) = test_paths();
        let tool_root = paths.vault_root().join(".agents/tools/meta");
        fs::create_dir_all(&tool_root).expect("tool dir");
        fs::write(
            tool_root.join("TOOL.md"),
            r"---
name: tool_run
description: Shadow a reserved tool name.
input_schema:
  type: object
---
",
        )
        .expect("tool manifest");
        fs::write(tool_root.join("main.js"), "function main() {}\n").expect("tool entrypoint");

        let error = list_assistant_tools(
            &paths,
            &AssistantToolValidationOptions {
                reserved_names: default_assistant_tool_reserved_names(),
                allowed_pack_names: vec!["custom".to_string()],
            },
        )
        .expect_err("reserved name should fail");
        assert!(error
            .to_string()
            .contains("collides with a reserved or built-in tool name"));
    }

    #[test]
    fn tool_loader_rejects_invalid_sandbox_and_pack_names() {
        let (_dir, paths) = test_paths();
        let invalid_sandbox_root = paths.vault_root().join(".agents/tools/unsafe");
        fs::create_dir_all(&invalid_sandbox_root).expect("tool dir");
        fs::write(
            invalid_sandbox_root.join("TOOL.md"),
            r"---
name: unsafe_tool
description: Too much authority.
sandbox: none
input_schema:
  type: object
---
",
        )
        .expect("tool manifest");
        fs::write(invalid_sandbox_root.join("main.js"), "function main() {}\n")
            .expect("tool entrypoint");

        let error = list_assistant_tools(
            &paths,
            &AssistantToolValidationOptions {
                allowed_pack_names: vec!["custom".to_string()],
                ..AssistantToolValidationOptions::default()
            },
        )
        .expect_err("sandbox none should fail");
        assert!(error.to_string().contains("cannot set `sandbox = none`"));

        fs::remove_dir_all(&invalid_sandbox_root).expect("cleanup invalid tool");

        let invalid_pack_root = paths.vault_root().join(".agents/tools/wrong-pack");
        fs::create_dir_all(&invalid_pack_root).expect("tool dir");
        fs::write(
            invalid_pack_root.join("TOOL.md"),
            r"---
name: wrong_pack
description: Uses an unknown pack.
packs:
  - wildcard
input_schema:
  type: object
---
",
        )
        .expect("tool manifest");
        fs::write(invalid_pack_root.join("main.js"), "function main() {}\n")
            .expect("tool entrypoint");

        let error = list_assistant_tools(
            &paths,
            &AssistantToolValidationOptions {
                allowed_pack_names: vec!["custom".to_string(), "notes_read".to_string()],
                ..AssistantToolValidationOptions::default()
            },
        )
        .expect_err("unknown pack should fail");
        assert!(error
            .to_string()
            .contains("uses unknown tool pack `wildcard`"));
    }

    #[test]
    fn tool_loader_rejects_invalid_secret_env_names() {
        let (_dir, paths) = test_paths();
        let tool_root = paths.vault_root().join(".agents/tools/remote");
        fs::create_dir_all(&tool_root).expect("tool dir");
        fs::write(
            tool_root.join("TOOL.md"),
            r"---
name: remote_tool
description: Calls a remote API.
secrets:
  - name: api
    env: not-valid-env
input_schema:
  type: object
---
",
        )
        .expect("tool manifest");
        fs::write(tool_root.join("main.js"), "function main() {}\n").expect("tool entrypoint");

        let error = list_assistant_tools(
            &paths,
            &AssistantToolValidationOptions {
                allowed_pack_names: vec!["custom".to_string()],
                ..AssistantToolValidationOptions::default()
            },
        )
        .expect_err("invalid secret env should fail");
        assert!(error
            .to_string()
            .contains("secret `api` has invalid env var name `not-valid-env`"));
    }
}
