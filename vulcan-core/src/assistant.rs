use crate::config::load_vault_config;
use crate::paths::VaultPaths;
use crate::JsRuntimeSandbox;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
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
    pub license: Option<String>,
    pub compatibility: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub commands: Vec<AssistantSkillCommandSummary>,
    pub tags: Vec<String>,
    pub tools: Vec<String>,
    pub output_file: Option<String>,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantSkillCommandSummary {
    pub id: String,
    pub script: String,
    #[serde(default)]
    pub sandbox: Option<JsRuntimeSandbox>,
    #[serde(default)]
    pub permission_profile: Option<String>,
    #[serde(default)]
    pub packs: Vec<String>,
    #[serde(default)]
    pub expose: bool,
    #[serde(default = "default_object_schema")]
    pub input_schema: JsonValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<AssistantSkillCommandCli>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<AssistantSkillCommandExample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantSkillCommandExample {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_file: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cli_args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_output: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_output_file: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantSkillCommandCli {
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub args: Vec<AssistantSkillCommandCliArg>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantSkillCommandCliArg {
    pub flag: String,
    pub action: AssistantSkillCommandCliArgAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantSkillCommandCliArgAction {
    String,
    Json,
    StringFile,
    JsonFile,
    Boolean,
    Integer,
    Number,
    StringArray,
    JsonArray,
    Choice,
    AppendMessage,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<AssistantSkillCommandCli>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<AssistantSkillCommandExample>,
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
    license: Option<String>,
    #[serde(default)]
    compatibility: Vec<String>,
    #[serde(rename = "allowed-tools", default)]
    allowed_tools: Vec<String>,
    #[serde(default)]
    metadata: YamlValue,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    tools: Vec<String>,
    output_file: Option<String>,
}

#[must_use]
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
        "tool_pack_disable".to_string(),
        "tool_pack_enable".to_string(),
        "tool_pack_list".to_string(),
        "tool_pack_set".to_string(),
        "tool_run".to_string(),
        "tool_set".to_string(),
        "tool_show".to_string(),
        "tool_validate".to_string(),
    ]
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
            license: normalize_optional(frontmatter.license),
            compatibility: normalize_list(frontmatter.compatibility),
            allowed_tools: normalize_list(frontmatter.allowed_tools),
            commands: normalize_skill_commands(&frontmatter.metadata)?,
            tags: normalize_list(frontmatter.tags),
            tools: normalize_list(frontmatter.tools),
            output_file: normalize_optional(frontmatter.output_file),
            path: relative,
        },
        body: body.trim().to_string(),
    };
    Ok(skill)
}

fn normalize_skill_commands(
    metadata: &YamlValue,
) -> Result<Vec<AssistantSkillCommandSummary>, AssistantError> {
    let Some(vulcan) = metadata.get("vulcan") else {
        return Ok(Vec::new());
    };
    let Some(commands) = vulcan.get("commands") else {
        return Ok(Vec::new());
    };
    let Some(sequence) = commands.as_sequence() else {
        return Err(AssistantError::parse(
            "`metadata.vulcan.commands` must be a list".to_string(),
        ));
    };
    let mut parsed = Vec::new();
    for command in sequence {
        let summary: AssistantSkillCommandSummary = serde_yaml::from_value(command.clone())
            .map_err(|error| {
                AssistantError::parse(format!("invalid skill command metadata: {error}"))
            })?;
        if !is_valid_skill_command_id(&summary.id) {
            return Err(AssistantError::parse(format!(
                "invalid skill command id `{}`",
                summary.id
            )));
        }
        if summary.script.trim().is_empty()
            || summary.script.contains("..")
            || summary.script.starts_with('/')
        {
            return Err(AssistantError::parse(format!(
                "invalid script path for skill command `{}`",
                summary.id
            )));
        }
        normalize_schema_value(
            Some(summary.input_schema.clone()),
            Path::new("SKILL.md"),
            "input_schema",
        )?;
        if let Some(output_schema) = &summary.output_schema {
            normalize_schema_value(
                Some(output_schema.clone()),
                Path::new("SKILL.md"),
                "output_schema",
            )?;
        }
        if let Some(cli) = &summary.cli {
            validate_skill_command_cli(&summary.id, cli)?;
        }
        validate_skill_command_examples(&summary.id, &summary.examples)?;
        parsed.push(summary);
    }
    Ok(parsed)
}

fn validate_skill_command_examples(
    command_id: &str,
    examples: &[AssistantSkillCommandExample],
) -> Result<(), AssistantError> {
    for example in examples {
        if example.name.trim().is_empty() {
            return Err(AssistantError::parse(format!(
                "skill command `{command_id}` has an example with an empty name"
            )));
        }
        let input_sources = usize::from(example.input.is_some())
            + usize::from(example.input_file.is_some())
            + usize::from(!example.cli_args.is_empty());
        if input_sources > 1 {
            return Err(AssistantError::parse(format!(
                "skill command `{command_id}` example `{}` must set only one of `input`, `input_file`, or `cli_args`",
                example.name
            )));
        }
        if input_sources == 0 {
            return Err(AssistantError::parse(format!(
                "skill command `{command_id}` example `{}` must set `input`, `input_file`, or `cli_args`",
                example.name
            )));
        }
        if example
            .input_file
            .as_ref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(AssistantError::parse(format!(
                "skill command `{command_id}` example `{}` has an empty input_file",
                example.name
            )));
        }
        if example.expected_output.is_some() && example.expected_output_file.is_some() {
            return Err(AssistantError::parse(format!(
                "skill command `{command_id}` example `{}` must set either `expected_output` or `expected_output_file`, not both",
                example.name
            )));
        }
        if example
            .expected_output_file
            .as_ref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(AssistantError::parse(format!(
                "skill command `{command_id}` example `{}` has an empty expected_output_file",
                example.name
            )));
        }
    }
    Ok(())
}

fn validate_skill_command_cli(
    command_id: &str,
    cli: &AssistantSkillCommandCli,
) -> Result<(), AssistantError> {
    for alias in &cli.aliases {
        if !is_valid_skill_command_cli_name(alias) {
            return Err(AssistantError::parse(format!(
                "invalid CLI alias `{alias}` for skill command `{command_id}`"
            )));
        }
    }
    for arg in &cli.args {
        let flag = arg.flag.trim_start_matches('-');
        if !is_valid_skill_command_cli_name(flag) {
            return Err(AssistantError::parse(format!(
                "invalid CLI flag `{}` for skill command `{command_id}`",
                arg.flag
            )));
        }
        match arg.action {
            AssistantSkillCommandCliArgAction::AppendMessage => {
                if matches!(arg.role.as_deref(), None | Some("")) {
                    return Err(AssistantError::parse(format!(
                        "CLI flag `{}` for skill command `{command_id}` must set `role`",
                        arg.flag
                    )));
                }
            }
            AssistantSkillCommandCliArgAction::String
            | AssistantSkillCommandCliArgAction::Json
            | AssistantSkillCommandCliArgAction::StringFile
            | AssistantSkillCommandCliArgAction::JsonFile
            | AssistantSkillCommandCliArgAction::Boolean
            | AssistantSkillCommandCliArgAction::Integer
            | AssistantSkillCommandCliArgAction::Number
            | AssistantSkillCommandCliArgAction::StringArray
            | AssistantSkillCommandCliArgAction::JsonArray
            | AssistantSkillCommandCliArgAction::Choice => {
                if !arg
                    .field
                    .as_deref()
                    .is_some_and(is_valid_skill_command_cli_field_path)
                {
                    return Err(AssistantError::parse(format!(
                        "CLI flag `{}` for skill command `{command_id}` must set a valid `field`",
                        arg.flag
                    )));
                }
                if arg.action == AssistantSkillCommandCliArgAction::Choice && arg.choices.is_empty()
                {
                    return Err(AssistantError::parse(format!(
                        "CLI flag `{}` for skill command `{command_id}` must set non-empty `choices`",
                        arg.flag
                    )));
                }
            }
        }
        if let Some(completion) = &arg.completion {
            if !is_valid_skill_command_cli_completion(completion) {
                return Err(AssistantError::parse(format!(
                    "invalid CLI completion `{completion}` for skill command `{command_id}`"
                )));
            }
        }
    }
    Ok(())
}

fn is_valid_skill_command_cli_name(value: &str) -> bool {
    let value = value.trim_start_matches('-');
    !value.is_empty()
        && value
            .chars()
            .any(|character| character.is_ascii_alphabetic())
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn is_valid_skill_command_cli_field_path(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
        })
}

fn is_valid_skill_command_cli_completion(value: &str) -> bool {
    is_valid_skill_command_cli_name(value)
}

fn default_object_schema() -> JsonValue {
    serde_json::json!({ "type": "object" })
}

fn is_valid_skill_command_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
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
    fn skill_loader_reads_agent_skills_command_metadata() {
        let (_dir, paths) = test_paths();
        let skill_root = paths.vault_root().join(".agents/skills/link-curation");
        fs::create_dir_all(skill_root.join("scripts")).expect("skill dirs");
        fs::write(
            skill_root.join("SKILL.md"),
            r"---
name: link-curation
description: Curate graph links
license: MIT
compatibility:
  - codex
allowed-tools:
  - graph_communities
metadata:
  vulcan:
    commands:
      - id: suggest-bridges
        script: scripts/suggest-bridges.js
        sandbox: strict
        permission_profile: readonly
        packs: [notes-read]
        expose: true
        cli:
          aliases: [suggest-bridges]
          args:
            - flag: note
              action: string
              field: note
            - flag: user
              action: append_message
              role: user
        examples:
          - name: smoke
            cli_args: [--note, Home.md]
---
Use this skill to curate links.
",
        )
        .expect("skill file");

        let skill = load_assistant_skill(&paths, "link-curation").expect("skill should load");
        assert_eq!(skill.summary.license.as_deref(), Some("MIT"));
        assert_eq!(skill.summary.compatibility, vec!["codex".to_string()]);
        assert_eq!(
            skill.summary.allowed_tools,
            vec!["graph_communities".to_string()]
        );
        assert_eq!(skill.summary.commands.len(), 1);
        assert_eq!(skill.summary.commands[0].id, "suggest-bridges");
        assert_eq!(
            skill.summary.commands[0].script,
            "scripts/suggest-bridges.js"
        );
        assert!(skill.summary.commands[0].expose);
        let cli = skill.summary.commands[0]
            .cli
            .as_ref()
            .expect("cli metadata should parse");
        assert_eq!(cli.aliases, vec!["suggest-bridges".to_string()]);
        assert_eq!(cli.args[0].flag, "note");
        assert_eq!(cli.args[1].role.as_deref(), Some("user"));
        assert_eq!(skill.summary.commands[0].examples[0].name, "smoke");
        assert_eq!(
            skill.summary.commands[0].examples[0].cli_args,
            vec!["--note".to_string(), "Home.md".to_string()]
        );
    }

    #[test]
    fn agents_file_loader_returns_none_when_missing() {
        let (_dir, paths) = test_paths();
        assert!(read_vault_agents_file(&paths)
            .expect("agents file read should succeed")
            .is_none());
    }
}
