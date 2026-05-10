use crate::{trust, AppError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use serde_yaml::Value as YamlValue;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use vulcan_core::{
    assistant_tools_root, default_assistant_tool_reserved_names, evaluate_dataview_js_with_options,
    list_assistant_skills, list_assistant_tool_manifest_paths, list_assistant_tools,
    load_assistant_skill, load_assistant_tool, load_assistant_tool_manifest, load_vault_config,
    resolve_permission_profile, validate_json_value_against_schema, AssistantSkill,
    AssistantSkillCommandCliArgAction, AssistantSkillCommandSummary, AssistantSkillSummary,
    AssistantTool, AssistantToolRuntime, AssistantToolSecretSpec, AssistantToolSummary,
    AssistantToolValidationOptions, DataviewJsEvalOptions, DataviewJsToolDefinition,
    DataviewJsToolDescriptor, DataviewJsToolRegistry, JsRuntimeSandbox, VaultPaths,
};

const CUSTOM_TOOL_SCRIPT_SHEBANG: &str = "#!/usr/bin/env -S vulcan run --script\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomToolRegistryOptions {
    pub reserved_names: Vec<String>,
    pub allowed_pack_names: Vec<String>,
    pub include_standalone_tools: bool,
}

impl Default for CustomToolRegistryOptions {
    fn default() -> Self {
        Self {
            reserved_names: default_assistant_tool_reserved_names(),
            allowed_pack_names: vec!["custom".to_string()],
            include_standalone_tools: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomToolRunOptions {
    pub surface: String,
}

impl Default for CustomToolRunOptions {
    fn default() -> Self {
        Self {
            surface: "cli".to_string(),
        }
    }
}

const DEFAULT_CUSTOM_TOOL_MAX_CALL_DEPTH: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CustomToolJsRegistryContext {
    surface: String,
    active_permission_profile: Option<String>,
    call_stack: Vec<String>,
    max_call_depth: usize,
}

impl CustomToolJsRegistryContext {
    fn root(surface: &str, active_permission_profile: Option<&str>) -> Self {
        Self {
            surface: surface.to_string(),
            active_permission_profile: active_permission_profile.map(ToOwned::to_owned),
            call_stack: Vec::new(),
            max_call_depth: DEFAULT_CUSTOM_TOOL_MAX_CALL_DEPTH,
        }
    }

    fn runtime_scope(&self, tool_name: &str, active_permission_profile: Option<String>) -> Self {
        let mut call_stack = self.call_stack.clone();
        call_stack.push(tool_name.to_string());
        Self {
            surface: self.surface.clone(),
            active_permission_profile,
            call_stack,
            max_call_depth: self.max_call_depth,
        }
    }

    fn nested_call_surface(&self) -> String {
        if self.surface.ends_with(".tools.call") {
            self.surface.clone()
        } else {
            format!("{}.tools.call", self.surface)
        }
    }
}

#[derive(Debug, Clone)]
struct JsCustomToolRegistry {
    paths: VaultPaths,
    registry_options: CustomToolRegistryOptions,
    context: CustomToolJsRegistryContext,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CustomToolDescriptor {
    #[serde(flatten)]
    pub summary: AssistantToolSummary,
    pub callable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CustomToolShowReport {
    #[serde(flatten)]
    pub tool: AssistantTool,
    pub callable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CustomToolRunReport {
    pub name: String,
    pub path: String,
    pub entrypoint_path: String,
    pub input: Value,
    pub result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CustomToolInitExample {
    #[default]
    Minimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomToolInitOptions {
    pub title: Option<String>,
    pub description: Option<String>,
    pub sandbox: JsRuntimeSandbox,
    pub permission_profile: Option<String>,
    pub timeout_ms: Option<usize>,
    pub example: CustomToolInitExample,
    pub overwrite: bool,
    pub dry_run: bool,
}

impl Default for CustomToolInitOptions {
    fn default() -> Self {
        Self {
            title: None,
            description: None,
            sandbox: JsRuntimeSandbox::Strict,
            permission_profile: None,
            timeout_ms: None,
            example: CustomToolInitExample::Minimal,
            overwrite: false,
            dry_run: false,
        }
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CustomToolSetOptions {
    pub title: Option<String>,
    pub clear_title: bool,
    pub description: Option<String>,
    pub sandbox: Option<JsRuntimeSandbox>,
    pub permission_profile: Option<String>,
    pub clear_permission_profile: bool,
    pub timeout_ms: Option<usize>,
    pub clear_timeout_ms: bool,
    pub packs: Option<Vec<String>>,
    pub clear_packs: bool,
    pub secrets: Option<Vec<AssistantToolSecretSpec>>,
    pub clear_secrets: bool,
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub input_schema: Option<Value>,
    pub output_schema: Option<Value>,
    pub clear_output_schema: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CustomToolWriteReport {
    pub name: String,
    pub updated: bool,
    pub dry_run: bool,
    pub tool_root: String,
    pub manifest_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CustomToolValidationItem {
    pub identifier: String,
    pub manifest_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub valid: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CustomToolValidationReport {
    pub checked: usize,
    pub valid: bool,
    pub tools: Vec<CustomToolValidationItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CustomToolLintReport {
    pub valid: bool,
    pub checked: usize,
    pub fixed: usize,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub tools: Vec<CustomToolLintToolReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CustomToolLintToolReport {
    pub name: String,
    pub path: String,
    pub entrypoint_path: String,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub fixes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CustomToolCompatReport {
    pub name: String,
    pub surfaces: Vec<CustomToolCompatSurfaceReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CustomToolCompatSurfaceReport {
    pub surface: String,
    pub compatible: bool,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CustomToolTypesReport {
    pub name: String,
    pub input_type: String,
    pub output_type: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CustomToolTypesSuiteReport {
    pub checked: usize,
    pub tools: Vec<CustomToolTypesReport>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct EditableToolFrontmatter {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<YamlValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entrypoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime: Option<AssistantToolRuntime>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sandbox: Option<JsRuntimeSandbox>,
    #[serde(skip_serializing_if = "Option::is_none")]
    permission_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    packs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    secrets: Vec<AssistantToolSecretSpec>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    read_only: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    destructive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_schema: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
struct EditableToolDocument {
    frontmatter: EditableToolFrontmatter,
    body: String,
}

pub fn list_custom_tools(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    options: &CustomToolRegistryOptions,
) -> Result<Vec<CustomToolDescriptor>, AppError> {
    let mut descriptors = if options.include_standalone_tools {
        list_assistant_tools(paths, &assistant_tool_validation_options(options))
            .map_err(AppError::operation)?
            .into_iter()
            .map(|summary| CustomToolDescriptor {
                callable: custom_tool_is_callable(
                    paths,
                    active_permission_profile,
                    &summary.name,
                    summary.permission_profile.as_deref(),
                ),
                summary,
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    descriptors.extend(skill_command_tool_descriptors(
        paths,
        active_permission_profile,
        options,
    )?);
    descriptors.sort_by(|left, right| left.summary.name.cmp(&right.summary.name));
    Ok(descriptors)
}

pub fn show_custom_tool(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    name: &str,
    options: &CustomToolRegistryOptions,
) -> Result<CustomToolShowReport, AppError> {
    let tool = match skill_command_tool(paths, name, options) {
        Ok(tool) => tool,
        Err(skill_error) if options.include_standalone_tools => {
            load_assistant_tool(paths, name, &assistant_tool_validation_options(options))
                .map_err(|tool_error| AppError::operation(format!("{skill_error}; {tool_error}")))?
        }
        Err(skill_error) => return Err(AppError::operation(skill_error)),
    };
    Ok(CustomToolShowReport {
        callable: custom_tool_is_callable(
            paths,
            active_permission_profile,
            &tool.summary.name,
            tool.summary.permission_profile.as_deref(),
        ),
        tool,
    })
}

#[must_use]
pub fn build_custom_tool_js_registry(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    surface: &str,
    options: &CustomToolRegistryOptions,
) -> Arc<dyn DataviewJsToolRegistry> {
    build_custom_tool_js_registry_with_context(
        paths,
        options,
        CustomToolJsRegistryContext::root(surface, active_permission_profile),
    )
}

fn build_custom_tool_js_registry_with_context(
    paths: &VaultPaths,
    options: &CustomToolRegistryOptions,
    context: CustomToolJsRegistryContext,
) -> Arc<dyn DataviewJsToolRegistry> {
    Arc::new(JsCustomToolRegistry {
        paths: paths.clone(),
        registry_options: options.clone(),
        context,
    })
}

pub fn require_trusted_tool_execution(
    paths: &VaultPaths,
    tool_name: Option<&str>,
) -> Result<(), AppError> {
    if trust::is_trusted(paths.vault_root()) {
        return Ok(());
    }

    let target = tool_name.map_or("custom tools".to_string(), |name| format!("tool `{name}`"));
    Err(AppError::operation(format!(
        "{target} require a trusted vault; run `vulcan trust add` first"
    )))
}

pub fn resolve_custom_tool_cli_name(
    paths: &VaultPaths,
    name: &str,
    registry_options: &CustomToolRegistryOptions,
) -> Result<String, AppError> {
    resolve_skill_command_tool_identifier(paths, name, registry_options)
        .map(|(resolved_name, _, _)| resolved_name)
}

pub fn collect_custom_tool_cli_name_candidates(
    paths: &VaultPaths,
    registry_options: &CustomToolRegistryOptions,
) -> Result<Vec<String>, AppError> {
    let mut seen = std::collections::BTreeSet::new();
    let mut candidates = Vec::new();
    for summary in list_assistant_skills(paths).map_err(AppError::operation)? {
        for command in summary.commands.iter().filter(|command| command.expose) {
            if !command_matches_allowed_packs(&command.packs, registry_options) {
                continue;
            }
            let tool_name = skill_command_tool_name(&summary.name, &command.id);
            if seen.insert(tool_name.clone()) {
                candidates.push(tool_name);
            }
            if let Some(cli) = &command.cli {
                for alias in &cli.aliases {
                    if seen.insert(alias.clone()) {
                        candidates.push(alias.clone());
                    }
                }
            }
        }
    }
    Ok(candidates)
}

pub fn collect_custom_tool_cli_flag_candidates(
    paths: &VaultPaths,
    name: &str,
    registry_options: &CustomToolRegistryOptions,
) -> Result<Vec<String>, AppError> {
    let (_resolved_name, _skill, command) =
        resolve_skill_command_tool_identifier(paths, name, registry_options)?;
    let Some(cli) = command.cli else {
        return Ok(Vec::new());
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut candidates = Vec::new();
    for arg in cli.args {
        let flag = format!("--{}", arg.flag.trim_start_matches('-'));
        if seen.insert(flag.clone()) {
            candidates.push(flag);
        }
    }
    Ok(candidates)
}

pub fn collect_custom_tool_cli_choice_candidates(
    paths: &VaultPaths,
    name: &str,
    flag: &str,
    registry_options: &CustomToolRegistryOptions,
) -> Result<Vec<String>, AppError> {
    let (_resolved_name, _skill, command) =
        resolve_skill_command_tool_identifier(paths, name, registry_options)?;
    let Some(cli) = command.cli else {
        return Ok(Vec::new());
    };
    let normalized_flag = flag.trim_start_matches('-');
    let Some(arg) = cli
        .args
        .into_iter()
        .find(|arg| arg.flag.trim_start_matches('-') == normalized_flag)
    else {
        return Ok(Vec::new());
    };
    Ok(arg.choices)
}

pub fn custom_tool_cli_flag_completion_context(
    paths: &VaultPaths,
    name: &str,
    flag: &str,
    registry_options: &CustomToolRegistryOptions,
) -> Result<Option<String>, AppError> {
    let (_resolved_name, _skill, command) =
        resolve_skill_command_tool_identifier(paths, name, registry_options)?;
    let Some(cli) = command.cli else {
        return Ok(None);
    };
    let normalized_flag = flag.trim_start_matches('-');
    Ok(cli
        .args
        .into_iter()
        .find(|arg| arg.flag.trim_start_matches('-') == normalized_flag)
        .and_then(|arg| arg.completion))
}

#[allow(clippy::too_many_lines)]
pub fn build_custom_tool_cli_input(
    paths: &VaultPaths,
    name: &str,
    args: &[String],
    registry_options: &CustomToolRegistryOptions,
) -> Result<(String, Value), AppError> {
    let (resolved_name, _skill, command) =
        resolve_skill_command_tool_identifier(paths, name, registry_options)?;
    let cli = command.cli.as_ref().ok_or_else(|| {
        AppError::operation(format!(
            "tool `{name}` does not declare custom CLI arguments; use --input-json or --input-file"
        ))
    })?;
    let mut input = serde_json::Map::new();
    let mut messages = Vec::new();
    let mut position = 0;
    while position < args.len() {
        let token = &args[position];
        if !token.starts_with("--") || token == "--" {
            return Err(AppError::operation(format!(
                "unexpected custom tool argument `{token}`; expected a declared --flag"
            )));
        }
        let flag = token.trim_start_matches('-');
        let spec = cli
            .args
            .iter()
            .find(|arg| arg.flag.trim_start_matches('-') == flag)
            .ok_or_else(|| {
                AppError::operation(format!(
                    "unknown custom CLI flag `--{flag}` for tool `{name}`"
                ))
            })?;
        position += 1;

        match spec.action {
            AssistantSkillCommandCliArgAction::Boolean => {
                insert_cli_field_value(&mut input, spec.field.as_deref(), Value::Bool(true))?;
            }
            AssistantSkillCommandCliArgAction::String => {
                let value = take_cli_flag_value(args, &mut position, flag)?;
                insert_cli_field(&mut input, spec.field.as_deref(), value.to_string())?;
            }
            AssistantSkillCommandCliArgAction::Json => {
                let value = take_cli_flag_value(args, &mut position, flag)?;
                let value = serde_json::from_str(value).map_err(|error| {
                    AppError::operation(format!(
                        "invalid JSON for custom CLI flag `--{flag}`: {error}"
                    ))
                })?;
                insert_cli_field_value(&mut input, spec.field.as_deref(), value)?;
            }
            AssistantSkillCommandCliArgAction::StringFile => {
                let value = take_cli_flag_value(args, &mut position, flag)?;
                let value = read_cli_field_source(value)?;
                insert_cli_field(&mut input, spec.field.as_deref(), value)?;
            }
            AssistantSkillCommandCliArgAction::JsonFile => {
                let value = take_cli_flag_value(args, &mut position, flag)?;
                let source = read_cli_field_source(value)?;
                let value = serde_json::from_str(&source).map_err(|error| {
                    AppError::operation(format!(
                        "invalid JSON for custom CLI flag `--{flag}` from `{value}`: {error}"
                    ))
                })?;
                insert_cli_field_value(&mut input, spec.field.as_deref(), value)?;
            }
            AssistantSkillCommandCliArgAction::Integer => {
                let raw = take_cli_flag_value(args, &mut position, flag)?;
                let value = raw.parse::<i64>().map_err(|error| {
                    AppError::operation(format!(
                        "invalid integer for custom CLI flag `--{flag}`: {error}"
                    ))
                })?;
                insert_cli_field_value(&mut input, spec.field.as_deref(), json!(value))?;
            }
            AssistantSkillCommandCliArgAction::Number => {
                let raw = take_cli_flag_value(args, &mut position, flag)?;
                let value = raw.parse::<f64>().map_err(|error| {
                    AppError::operation(format!(
                        "invalid number for custom CLI flag `--{flag}`: {error}"
                    ))
                })?;
                insert_cli_field_value(&mut input, spec.field.as_deref(), json!(value))?;
            }
            AssistantSkillCommandCliArgAction::StringArray => {
                let value = take_cli_flag_value(args, &mut position, flag)?;
                append_cli_field_array_value(
                    &mut input,
                    spec.field.as_deref(),
                    Value::String(value.to_string()),
                )?;
            }
            AssistantSkillCommandCliArgAction::JsonArray => {
                let raw = take_cli_flag_value(args, &mut position, flag)?;
                let value = serde_json::from_str(raw).map_err(|error| {
                    AppError::operation(format!(
                        "invalid JSON for custom CLI flag `--{flag}`: {error}"
                    ))
                })?;
                append_cli_field_array_value(&mut input, spec.field.as_deref(), value)?;
            }
            AssistantSkillCommandCliArgAction::Choice => {
                let value = take_cli_flag_value(args, &mut position, flag)?;
                if !spec.choices.iter().any(|choice| choice == value) {
                    return Err(AppError::operation(format!(
                        "invalid choice `{value}` for custom CLI flag `--{flag}`; expected one of: {}",
                        spec.choices.join(", ")
                    )));
                }
                insert_cli_field(&mut input, spec.field.as_deref(), value.to_string())?;
            }
            AssistantSkillCommandCliArgAction::AppendMessage => {
                let value = take_cli_flag_value(args, &mut position, flag)?;
                messages.push(json!({
                    "role": spec.role.as_deref().unwrap_or("user"),
                    "content": value,
                }));
            }
        }
    }
    if !messages.is_empty() {
        input.insert("messages".to_string(), Value::Array(messages));
    }
    Ok((resolved_name, Value::Object(input)))
}

fn take_cli_flag_value<'a>(
    args: &'a [String],
    position: &mut usize,
    flag: &str,
) -> Result<&'a str, AppError> {
    let value = args.get(*position).ok_or_else(|| {
        AppError::operation(format!("custom CLI flag `--{flag}` requires a value"))
    })?;
    *position += 1;
    Ok(value)
}

fn insert_cli_field(
    input: &mut serde_json::Map<String, Value>,
    field: Option<&str>,
    value: String,
) -> Result<(), AppError> {
    insert_cli_field_value(input, field, Value::String(value))
}

fn insert_cli_field_value(
    input: &mut serde_json::Map<String, Value>,
    field: Option<&str>,
    value: Value,
) -> Result<(), AppError> {
    let field = field.ok_or_else(|| AppError::operation("custom CLI argument is missing field"))?;
    insert_cli_field_path(input, field, value, false)
}

fn append_cli_field_array_value(
    input: &mut serde_json::Map<String, Value>,
    field: Option<&str>,
    value: Value,
) -> Result<(), AppError> {
    let field = field.ok_or_else(|| AppError::operation("custom CLI argument is missing field"))?;
    insert_cli_field_path(input, field, value, true)
}

fn insert_cli_field_path(
    input: &mut serde_json::Map<String, Value>,
    field: &str,
    value: Value,
    append: bool,
) -> Result<(), AppError> {
    let parts = field.split('.').collect::<Vec<_>>();
    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return Err(AppError::operation(format!(
            "custom CLI argument has invalid field path `{field}`"
        )));
    }
    insert_cli_field_parts(input, &parts, value, append)
}

fn insert_cli_field_parts(
    input: &mut serde_json::Map<String, Value>,
    parts: &[&str],
    value: Value,
    append: bool,
) -> Result<(), AppError> {
    if parts.len() == 1 {
        if append {
            let entry = input
                .entry(parts[0].to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            let Some(array) = entry.as_array_mut() else {
                return Err(AppError::operation(format!(
                    "custom CLI field `{}` is already set to a non-array value",
                    parts[0]
                )));
            };
            array.push(value);
        } else {
            input.insert(parts[0].to_string(), value);
        }
        return Ok(());
    }

    let entry = input
        .entry(parts[0].to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(object) = entry.as_object_mut() else {
        return Err(AppError::operation(format!(
            "custom CLI field `{}` is already set to a non-object value",
            parts[0]
        )));
    };
    insert_cli_field_parts(object, &parts[1..], value, append)
}

fn read_cli_field_source(path: &str) -> Result<String, AppError> {
    if path == "-" {
        let mut source = String::new();
        let mut stdin = std::io::stdin();
        stdin
            .read_to_string(&mut source)
            .map_err(AppError::operation)?;
        return Ok(source);
    }
    fs::read_to_string(path).map_err(AppError::operation)
}

pub fn run_custom_tool(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    name: &str,
    input: &Value,
    registry_options: &CustomToolRegistryOptions,
    run_options: &CustomToolRunOptions,
) -> Result<CustomToolRunReport, AppError> {
    let context =
        CustomToolJsRegistryContext::root(&run_options.surface, active_permission_profile);
    run_custom_tool_with_context(paths, &context, name, input, registry_options, run_options)
}

fn run_custom_tool_with_context(
    paths: &VaultPaths,
    context: &CustomToolJsRegistryContext,
    name: &str,
    input: &Value,
    registry_options: &CustomToolRegistryOptions,
    run_options: &CustomToolRunOptions,
) -> Result<CustomToolRunReport, AppError> {
    require_trusted_tool_execution(paths, Some(name))?;
    if !registry_options.include_standalone_tools {
        return run_skill_command_tool_with_context(
            paths,
            context,
            name,
            input,
            registry_options,
            run_options,
        );
    }
    let Ok(tool) = load_assistant_tool(
        paths,
        name,
        &assistant_tool_validation_options(registry_options),
    ) else {
        return run_skill_command_tool_with_context(
            paths,
            context,
            name,
            input,
            registry_options,
            run_options,
        );
    };
    validate_json_value_against_schema(input, &tool.summary.input_schema).map_err(|error| {
        AppError::operation(format!("tool `{name}` input validation failed: {error}"))
    })?;

    let effective_permission_profile = effective_tool_permission_profile(
        paths,
        context.active_permission_profile.as_deref(),
        &tool.summary.name,
        tool.summary.permission_profile.as_deref(),
    )?;
    let runtime_context =
        context.runtime_scope(&tool.summary.name, effective_permission_profile.clone());
    let source =
        fs::read_to_string(tool_entrypoint_path(paths, &tool)).map_err(AppError::operation)?;
    let current_file = current_file_for_tool(paths, &tool);
    let source = build_tool_invocation_source(
        paths,
        strip_shebang_line(&source),
        &tool,
        input,
        run_options,
    )?;
    let evaluation = evaluate_dataview_js_with_options(
        paths,
        &source,
        current_file.as_deref(),
        DataviewJsEvalOptions {
            timeout: tool.summary.timeout_ms.map(|timeout_ms| {
                Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(u64::MAX))
            }),
            sandbox: Some(tool.summary.sandbox),
            permission_profile: effective_permission_profile,
            tool_registry: Some(build_custom_tool_js_registry_with_context(
                paths,
                registry_options,
                runtime_context,
            )),
            ..DataviewJsEvalOptions::default()
        },
    )
    .map_err(AppError::operation)?;
    let value = evaluation.value.ok_or_else(|| {
        AppError::operation(format!(
            "tool `{}` did not return a JSON-serializable value",
            tool.summary.name
        ))
    })?;
    let (result, text) = normalize_tool_return_value(&tool, value)?;
    if let Some(output_schema) = &tool.summary.output_schema {
        validate_json_value_against_schema(&result, output_schema).map_err(|error| {
            AppError::operation(format!(
                "tool `{}` output validation failed: {error}",
                tool.summary.name
            ))
        })?;
    }

    Ok(CustomToolRunReport {
        name: tool.summary.name,
        path: tool.summary.path,
        entrypoint_path: tool.summary.entrypoint_path,
        input: input.clone(),
        result,
        text,
    })
}

fn skill_command_tool_descriptors(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    options: &CustomToolRegistryOptions,
) -> Result<Vec<CustomToolDescriptor>, AppError> {
    let skills = list_assistant_skills(paths).map_err(AppError::operation)?;
    Ok(skills
        .into_iter()
        .flat_map(|skill| {
            skill
                .commands
                .clone()
                .into_iter()
                .filter(|command| command.expose)
                .filter(|command| command_matches_allowed_packs(&command.packs, options))
                .map(move |command| CustomToolDescriptor {
                    callable: custom_tool_is_callable(
                        paths,
                        active_permission_profile,
                        &skill_command_tool_name(&skill.name, &command.id),
                        command.permission_profile.as_deref(),
                    ),
                    summary: skill_command_tool_summary(paths, &skill, &command),
                })
                .collect::<Vec<_>>()
        })
        .collect())
}

fn skill_command_tool(
    paths: &VaultPaths,
    name: &str,
    options: &CustomToolRegistryOptions,
) -> Result<AssistantTool, AppError> {
    let (_resolved_name, skill, command) =
        resolve_skill_command_tool_identifier(paths, name, options)?;
    Ok(AssistantTool {
        summary: skill_command_tool_summary(paths, &skill.summary, &command),
        body: skill.body,
    })
}

fn skill_command_tool_summary(
    paths: &VaultPaths,
    skill: &AssistantSkillSummary,
    command: &AssistantSkillCommandSummary,
) -> AssistantToolSummary {
    let script_path = skill_command_script_relative_path(paths, skill, command);
    AssistantToolSummary {
        name: skill_command_tool_name(&skill.name, &command.id),
        title: Some(format!("{}:{}", skill.name, command.id)),
        description: skill.description.clone().unwrap_or_else(|| {
            format!(
                "Run Agent Skill command `{}` from skill `{}`.",
                command.id, skill.name
            )
        }),
        version: None,
        runtime: AssistantToolRuntime::Quickjs,
        entrypoint: command.script.clone(),
        entrypoint_path: script_path,
        tags: skill.tags.clone(),
        sandbox: command.sandbox.unwrap_or(JsRuntimeSandbox::Strict),
        permission_profile: command.permission_profile.clone(),
        timeout_ms: None,
        packs: if command.packs.is_empty() {
            vec!["custom".to_string()]
        } else {
            command.packs.clone()
        },
        secrets: Vec::new(),
        read_only: !matches!(command.sandbox, Some(JsRuntimeSandbox::None)),
        destructive: false,
        input_schema: command.input_schema.clone(),
        output_schema: command.output_schema.clone(),
        cli: command.cli.clone(),
        examples: command.examples.clone(),
        path: skill.path.clone(),
    }
}

fn run_skill_command_tool_with_context(
    paths: &VaultPaths,
    context: &CustomToolJsRegistryContext,
    name: &str,
    input: &Value,
    registry_options: &CustomToolRegistryOptions,
    run_options: &CustomToolRunOptions,
) -> Result<CustomToolRunReport, AppError> {
    let _ = run_options;
    let (skill, command) = resolve_skill_command_tool(paths, name, registry_options)?;
    validate_json_value_against_schema(input, &command.input_schema).map_err(|error| {
        AppError::operation(format!(
            "skill command tool `{name}` input validation failed: {error}"
        ))
    })?;
    let effective_permission_profile = effective_tool_permission_profile(
        paths,
        context.active_permission_profile.as_deref(),
        name,
        command.permission_profile.as_deref(),
    )?;
    let runtime_context = context.runtime_scope(name, effective_permission_profile.clone());
    let script_path = skill_command_script_path(paths, &skill.summary, &command)?;
    let source = fs::read_to_string(&script_path).map_err(AppError::operation)?;
    let source = build_skill_command_invocation_source(
        &skill,
        &command,
        input,
        strip_shebang_line(&source),
    )?;
    let current_file = script_path
        .strip_prefix(paths.vault_root())
        .ok()
        .map(|path| path.to_string_lossy().replace('\\', "/"));
    let evaluation = evaluate_dataview_js_with_options(
        paths,
        &source,
        current_file.as_deref(),
        DataviewJsEvalOptions {
            sandbox: Some(command.sandbox.unwrap_or(JsRuntimeSandbox::Strict)),
            permission_profile: effective_permission_profile,
            tool_registry: Some(build_custom_tool_js_registry_with_context(
                paths,
                registry_options,
                runtime_context,
            )),
            ..DataviewJsEvalOptions::default()
        },
    )
    .map_err(AppError::operation)?;
    let result = evaluation.value.ok_or_else(|| {
        AppError::operation(format!(
            "skill command tool `{name}` did not return a JSON-serializable value"
        ))
    })?;
    if let Some(output_schema) = &command.output_schema {
        validate_json_value_against_schema(&result, output_schema).map_err(|error| {
            AppError::operation(format!(
                "skill command tool `{name}` output validation failed: {error}"
            ))
        })?;
    }
    Ok(CustomToolRunReport {
        name: name.to_string(),
        entrypoint_path: skill_command_script_relative_path(paths, &skill.summary, &command),
        path: skill.summary.path,
        input: input.clone(),
        result,
        text: None,
    })
}

fn resolve_skill_command_tool(
    paths: &VaultPaths,
    name: &str,
    options: &CustomToolRegistryOptions,
) -> Result<(AssistantSkill, AssistantSkillCommandSummary), AppError> {
    resolve_skill_command_tool_identifier(paths, name, options)
        .map(|(_resolved_name, skill, command)| (skill, command))
}

fn resolve_skill_command_tool_identifier(
    paths: &VaultPaths,
    name: &str,
    options: &CustomToolRegistryOptions,
) -> Result<(String, AssistantSkill, AssistantSkillCommandSummary), AppError> {
    let mut alias_matches = Vec::new();
    for summary in list_assistant_skills(paths).map_err(AppError::operation)? {
        for command in summary.commands.iter().filter(|command| command.expose) {
            let tool_name = skill_command_tool_name(&summary.name, &command.id);
            if tool_name == name {
                if !command_matches_allowed_packs(&command.packs, options) {
                    return Err(AppError::operation(format!(
                        "skill command tool `{name}` is not in an allowed tool pack"
                    )));
                }
                let skill =
                    load_assistant_skill(paths, &summary.name).map_err(AppError::operation)?;
                return Ok((tool_name, skill, command.clone()));
            }
            if command
                .cli
                .as_ref()
                .is_some_and(|cli| cli.aliases.iter().any(|alias| alias == name))
                && command_matches_allowed_packs(&command.packs, options)
            {
                alias_matches.push((tool_name, summary.name.clone(), command.clone()));
            }
        }
    }
    if alias_matches.len() == 1 {
        let (tool_name, skill_name, command) = alias_matches.remove(0);
        let skill = load_assistant_skill(paths, &skill_name).map_err(AppError::operation)?;
        return Ok((tool_name, skill, command));
    }
    if alias_matches.len() > 1 {
        let names = alias_matches
            .into_iter()
            .map(|(tool_name, _, _)| tool_name)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(AppError::operation(format!(
            "custom tool alias `{name}` is ambiguous: {names}"
        )));
    }
    Err(AppError::operation(format!("unknown custom tool `{name}`")))
}

fn build_skill_command_invocation_source(
    skill: &AssistantSkill,
    command: &AssistantSkillCommandSummary,
    input: &Value,
    source: &str,
) -> Result<String, AppError> {
    let input = serde_json::to_string(input).map_err(AppError::operation)?;
    let context = serde_json::to_string(&json!({
        "skill": {
            "name": skill.summary.name,
            "path": skill.summary.path,
            "description": skill.summary.description,
        },
        "command": command,
    }))
    .map_err(AppError::operation)?;
    Ok(format!(
        "const __vulcanSkillInput = {input};\n\
const __vulcanSkillContext = {context};\n\
{source}\n\
if (typeof main !== 'function') {{\n\
  throw new Error('skill command script must export `main(input, ctx)`');\n\
}}\n\
main(__vulcanSkillInput, __vulcanSkillContext);\n"
    ))
}

fn skill_command_script_path(
    paths: &VaultPaths,
    skill: &AssistantSkillSummary,
    command: &AssistantSkillCommandSummary,
) -> Result<PathBuf, AppError> {
    let config = load_vault_config(paths).config;
    Ok(paths
        .vault_root()
        .join(config.assistant.skills_folder)
        .join(&skill.path)
        .parent()
        .ok_or_else(|| AppError::operation("invalid skill path"))?
        .join(&command.script))
}

fn skill_command_script_relative_path(
    paths: &VaultPaths,
    skill: &AssistantSkillSummary,
    command: &AssistantSkillCommandSummary,
) -> String {
    skill_command_script_path(paths, skill, command)
        .and_then(|path| {
            path.strip_prefix(paths.vault_root())
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .map_err(AppError::operation)
        })
        .unwrap_or_else(|_| command.script.clone())
}

fn skill_command_tool_name(skill_name: &str, command_id: &str) -> String {
    format!(
        "skill_{}_{}",
        normalize_tool_name_component(skill_name),
        normalize_tool_name_component(command_id)
    )
}

fn normalize_tool_name_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn command_matches_allowed_packs(packs: &[String], options: &CustomToolRegistryOptions) -> bool {
    let packs = if packs.is_empty() {
        vec!["custom".to_string()]
    } else {
        packs.to_vec()
    };
    packs.iter().all(|pack| {
        options
            .allowed_pack_names
            .iter()
            .any(|allowed| allowed == pack)
    })
}

impl DataviewJsToolRegistry for JsCustomToolRegistry {
    fn list(&self) -> Result<Vec<DataviewJsToolDescriptor>, String> {
        list_custom_tools(
            &self.paths,
            self.context.active_permission_profile.as_deref(),
            &self.registry_options,
        )
        .map(|tools| {
            tools
                .into_iter()
                .map(|tool| DataviewJsToolDescriptor {
                    summary: tool.summary,
                    callable: tool.callable,
                })
                .collect()
        })
        .map_err(|error| error.to_string())
    }

    fn get(&self, name: &str) -> Result<DataviewJsToolDefinition, String> {
        show_custom_tool(
            &self.paths,
            self.context.active_permission_profile.as_deref(),
            name,
            &self.registry_options,
        )
        .map(|tool| DataviewJsToolDefinition {
            tool: tool.tool,
            callable: tool.callable,
        })
        .map_err(|error| error.to_string())
    }

    fn call(&self, name: &str, input: &Value, _options: Option<&Value>) -> Result<Value, String> {
        if self.context.call_stack.iter().any(|entry| entry == name) {
            let mut chain = self.context.call_stack.clone();
            chain.push(name.to_string());
            return Err(format!(
                "recursive custom tool call detected: {}",
                chain.join(" -> ")
            ));
        }
        if self.context.call_stack.len() >= self.context.max_call_depth {
            return Err(format!(
                "custom tool call depth exceeded maximum of {} while calling `{name}`",
                self.context.max_call_depth
            ));
        }

        let report = run_custom_tool_with_context(
            &self.paths,
            &CustomToolJsRegistryContext {
                surface: self.context.nested_call_surface(),
                active_permission_profile: self.context.active_permission_profile.clone(),
                call_stack: self.context.call_stack.clone(),
                max_call_depth: self.context.max_call_depth,
            },
            name,
            input,
            &self.registry_options,
            &CustomToolRunOptions {
                surface: self.context.nested_call_surface(),
            },
        )
        .map_err(|error| error.to_string())?;
        let CustomToolRunReport { result, text, .. } = report;
        Ok(match text {
            Some(text) => json!({ "result": result, "text": text }),
            None => result,
        })
    }
}

pub fn validate_custom_tools(
    paths: &VaultPaths,
    identifier: Option<&str>,
    options: &CustomToolRegistryOptions,
) -> Result<CustomToolValidationReport, AppError> {
    let validation_options = assistant_tool_validation_options(options);
    let manifest_paths = list_assistant_tool_manifest_paths(paths).map_err(AppError::operation)?;
    let selected_paths = if let Some(identifier) = identifier {
        vec![resolve_tool_manifest_path(
            paths,
            identifier,
            &validation_options,
            &manifest_paths,
        )?]
    } else {
        manifest_paths.clone()
    };

    let mut tools = Vec::new();
    for manifest_path in &selected_paths {
        match load_assistant_tool_manifest(paths, manifest_path, &validation_options) {
            Ok(tool) => tools.push(CustomToolValidationItem {
                identifier: tool_directory_name(manifest_path),
                manifest_path: manifest_path.clone(),
                name: Some(tool.summary.name),
                valid: true,
                errors: Vec::new(),
            }),
            Err(error) => tools.push(CustomToolValidationItem {
                identifier: tool_directory_name(manifest_path),
                manifest_path: manifest_path.clone(),
                name: None,
                valid: false,
                errors: vec![error.to_string()],
            }),
        }
    }

    let registry_error = if tools.iter().all(|tool| tool.valid) {
        list_assistant_tools(paths, &validation_options)
            .err()
            .map(|error| error.to_string())
    } else {
        None
    };
    Ok(CustomToolValidationReport {
        checked: tools.len(),
        valid: tools.iter().all(|tool| tool.valid) && registry_error.is_none(),
        tools,
        registry_error,
    })
}

pub fn lint_custom_tools(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    name: Option<&str>,
    strict: bool,
    fix: bool,
    registry_options: &CustomToolRegistryOptions,
) -> Result<CustomToolLintReport, AppError> {
    let tools = if let Some(name) = name {
        vec![
            show_custom_tool(paths, active_permission_profile, name, registry_options)?
                .tool
                .summary,
        ]
    } else {
        list_custom_tools(paths, active_permission_profile, registry_options)?
            .into_iter()
            .map(|descriptor| descriptor.summary)
            .collect()
    };

    let reports = tools
        .into_iter()
        .map(|tool| lint_one_custom_tool(paths, tool, fix))
        .collect::<Result<Vec<_>, _>>()?;

    let warnings = reports
        .iter()
        .flat_map(|tool| {
            tool.warnings
                .iter()
                .map(|warning| format!("{}: {warning}", tool.name))
        })
        .collect::<Vec<_>>();
    let errors = reports
        .iter()
        .flat_map(|tool| {
            tool.errors
                .iter()
                .map(|error| format!("{}: {error}", tool.name))
        })
        .collect::<Vec<_>>();
    Ok(CustomToolLintReport {
        valid: errors.is_empty() && (!strict || warnings.is_empty()),
        checked: reports.len(),
        fixed: reports.iter().map(|tool| tool.fixes.len()).sum(),
        warnings,
        errors,
        tools: reports,
    })
}

pub fn build_tool_compat_report(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    name: &str,
    requested_surfaces: &[String],
    registry_options: &CustomToolRegistryOptions,
) -> Result<CustomToolCompatReport, AppError> {
    let show = show_custom_tool(paths, active_permission_profile, name, registry_options)?;
    let surfaces = if requested_surfaces.is_empty() {
        vec![
            "cli".to_string(),
            "mcp".to_string(),
            "openai-tools".to_string(),
            "js".to_string(),
        ]
    } else {
        requested_surfaces.to_vec()
    };
    let surfaces = surfaces
        .iter()
        .map(|surface| tool_compat_surface_report(&show.tool.summary, show.callable, surface))
        .collect();
    Ok(CustomToolCompatReport {
        name: show.tool.summary.name,
        surfaces,
    })
}

pub fn build_tool_types_report(
    paths: &VaultPaths,
    active_profile: Option<&str>,
    name: &str,
    registry_options: &CustomToolRegistryOptions,
) -> Result<CustomToolTypesReport, AppError> {
    let show = show_custom_tool(paths, active_profile, name, registry_options)?;
    let base_name = ts_type_name(&show.tool.summary.name);
    let input_type = format!("{base_name}Input");
    let output_type = format!("{base_name}Output");
    let output_ts = show.tool.summary.output_schema.as_ref().map_or_else(
        || "unknown".to_string(),
        |schema| json_schema_to_typescript(schema, 0),
    );
    let source = format!(
        "export type {input_type} = {};\n\nexport type {output_type} = {};\n\nexport declare function {function_name}(input: {input_type}): {output_type};\n",
        json_schema_to_typescript(&show.tool.summary.input_schema, 0),
        output_ts,
        function_name = ts_function_name(&show.tool.summary.name),
    );
    Ok(CustomToolTypesReport {
        name: show.tool.summary.name,
        input_type,
        output_type,
        source,
    })
}

pub fn build_all_tool_types_report(
    paths: &VaultPaths,
    active_profile: Option<&str>,
    registry_options: &CustomToolRegistryOptions,
) -> Result<CustomToolTypesSuiteReport, AppError> {
    let reports = list_custom_tools(paths, active_profile, registry_options)?
        .iter()
        .map(|tool| {
            build_tool_types_report(paths, active_profile, &tool.summary.name, registry_options)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let source = reports
        .iter()
        .map(|report| report.source.trim_end())
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok(CustomToolTypesSuiteReport {
        checked: reports.len(),
        tools: reports,
        source: format!("{source}\n"),
    })
}

fn lint_one_custom_tool(
    paths: &VaultPaths,
    tool: AssistantToolSummary,
    fix: bool,
) -> Result<CustomToolLintToolReport, AppError> {
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    let mut fixes = Vec::new();
    collect_tool_metadata_lint(paths, &tool, &mut warnings)?;
    collect_tool_packaging_lint(paths, &tool, fix, &mut errors, &mut fixes)?;
    Ok(CustomToolLintToolReport {
        name: tool.name,
        path: tool.path,
        entrypoint_path: tool.entrypoint_path,
        warnings,
        errors,
        fixes,
    })
}

fn collect_tool_metadata_lint(
    paths: &VaultPaths,
    tool: &AssistantToolSummary,
    warnings: &mut Vec<String>,
) -> Result<(), AppError> {
    if tool.output_schema.is_none() {
        warnings.push("missing output_schema".to_string());
    }
    match &tool.cli {
        Some(cli) => {
            if cli.aliases.is_empty() {
                warnings.push("missing CLI alias".to_string());
            }
            let covered_fields = cli
                .args
                .iter()
                .map(|arg| arg.field.as_deref().unwrap_or(&arg.flag))
                .map(top_level_field_name)
                .collect::<BTreeSet<_>>();
            for field in json_schema_required_fields(&tool.input_schema) {
                if !covered_fields.contains(&field) {
                    warnings.push(format!("required input field `{field}` has no CLI flag"));
                }
            }
        }
        None => warnings.push("missing CLI metadata".to_string()),
    }
    if tool.examples.is_empty() {
        warnings.push("missing runnable examples".to_string());
    }
    if tool_is_mutation_capable(paths, tool)? {
        if !schema_has_dry_run_input(&tool.input_schema) {
            warnings
                .push("mutation-capable tool should expose a boolean dry_run input".to_string());
        }
        if !tool_has_dry_run_example(paths, tool)? {
            warnings.push(
                "mutation-capable tool should include at least one dry-run example".to_string(),
            );
        }
    }
    if matches!(tool.sandbox, JsRuntimeSandbox::Net) {
        warnings
            .push("net sandbox should be reserved for tools that need network access".to_string());
    }
    Ok(())
}

fn collect_tool_packaging_lint(
    paths: &VaultPaths,
    tool: &AssistantToolSummary,
    fix: bool,
    errors: &mut Vec<String>,
    fixes: &mut Vec<String>,
) -> Result<(), AppError> {
    if matches!(tool.sandbox, JsRuntimeSandbox::None) {
        errors.push("sandbox none is not allowed for exposed skill command tools".to_string());
    }
    let entrypoint_is_absolute =
        Path::new(&tool.entrypoint).is_absolute() || Path::new(&tool.entrypoint_path).is_absolute();
    if entrypoint_is_absolute {
        errors.push("entrypoint paths must be relative".to_string());
    }
    let entrypoint_path = paths.vault_root().join(&tool.entrypoint_path);
    if fix && !entrypoint_is_absolute {
        fixes.extend(apply_tool_lint_fixes(paths, tool, &entrypoint_path)?);
    }
    match fs::read_to_string(&entrypoint_path) {
        Ok(source) => {
            if !source.starts_with("#!") {
                errors.push("entrypoint script is missing a shebang".to_string());
            } else if !source.lines().next().unwrap_or_default().contains("vulcan") {
                errors.push("entrypoint shebang does not invoke vulcan".to_string());
            }
        }
        Err(error) => errors.push(format!("entrypoint script is not readable: {error}")),
    }
    if !script_is_executable(&entrypoint_path) {
        errors.push("entrypoint script is not executable".to_string());
    }
    Ok(())
}

fn apply_tool_lint_fixes(
    paths: &VaultPaths,
    tool: &AssistantToolSummary,
    entrypoint_path: &Path,
) -> Result<Vec<String>, AppError> {
    let mut fixes = Vec::new();
    if let Ok(source) = fs::read_to_string(entrypoint_path) {
        let fixed_source = normalize_tool_script_shebang(&source);
        if fixed_source != source {
            fs::write(entrypoint_path, fixed_source).map_err(AppError::operation)?;
            fixes.push("normalized Vulcan shebang".to_string());
        }
    }
    if entrypoint_path.exists() && !script_is_executable(entrypoint_path) {
        set_executable_permissions(entrypoint_path)?;
        fixes.push("set executable bit".to_string());
    }
    if tool.examples.is_empty() {
        let examples_dir = tool_example_base_dir(paths, tool)?.join("examples");
        if !examples_dir.exists() {
            fs::create_dir_all(&examples_dir).map_err(AppError::operation)?;
            fixes.push("created examples directory".to_string());
        }
    }
    Ok(fixes)
}

fn normalize_tool_script_shebang(source: &str) -> String {
    const SHEBANG: &str = "#!/usr/bin/env -S vulcan skill exec";
    if let Some(rest) = source.strip_prefix("#!") {
        let body = rest.split_once('\n').map_or("", |(_, body)| body);
        format!("{SHEBANG}\n{body}")
    } else {
        format!("{SHEBANG}\n{source}")
    }
}

fn tool_is_mutation_capable(
    paths: &VaultPaths,
    tool: &AssistantToolSummary,
) -> Result<bool, AppError> {
    if tool.destructive {
        return Ok(true);
    }
    let Some(permission_profile) = tool.permission_profile.as_deref() else {
        return Ok(false);
    };
    let resolved = resolve_permission_profile(paths, Some(permission_profile))
        .map_err(|error| AppError::operation(error.to_string()))?;
    Ok(permission_profile_allows_writes(&resolved.profile))
}

fn permission_profile_allows_writes(profile: &vulcan_core::PermissionProfile) -> bool {
    !matches!(
        profile.write,
        vulcan_core::PathPermissionConfig::Keyword(vulcan_core::PathPermissionKeyword::None)
    ) || !matches!(
        profile.refactor,
        vulcan_core::PathPermissionConfig::Keyword(vulcan_core::PathPermissionKeyword::None)
    ) || matches!(profile.git, vulcan_core::PermissionMode::Allow)
        || matches!(profile.config, vulcan_core::ConfigPermissionMode::Write)
}

fn schema_has_dry_run_input(schema: &Value) -> bool {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| {
            ["dry_run", "dryRun"].iter().any(|field| {
                properties.get(*field).is_some_and(|property| {
                    property
                        .get("type")
                        .and_then(Value::as_str)
                        .is_none_or(|kind| kind == "boolean")
                })
            })
        })
}

fn tool_has_dry_run_example(
    paths: &VaultPaths,
    tool: &AssistantToolSummary,
) -> Result<bool, AppError> {
    let base_dir = tool_example_base_dir(paths, tool)?;
    Ok(tool.examples.iter().any(|example| {
        example_cli_args_include_dry_run(&example.cli_args)
            || example_json_has_dry_run(example.input.as_ref())
            || example
                .input_file
                .as_deref()
                .and_then(|path| read_skill_example_json_file(&base_dir, path).ok())
                .as_ref()
                .is_some_and(|input| example_json_has_dry_run(Some(input)))
    }))
}

fn example_cli_args_include_dry_run(args: &[String]) -> bool {
    args.iter()
        .any(|arg| matches!(arg.as_str(), "--dry-run" | "--dry_run" | "--dryRun"))
}

fn example_json_has_dry_run(input: Option<&Value>) -> bool {
    input.is_some_and(|input| {
        ["dry_run", "dryRun"]
            .iter()
            .any(|field| input.get(*field).and_then(Value::as_bool) == Some(true))
    })
}

fn tool_compat_surface_report(
    tool: &AssistantToolSummary,
    callable: bool,
    surface: &str,
) -> CustomToolCompatSurfaceReport {
    let normalized = surface.trim().to_ascii_lowercase();
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    if !callable {
        errors.push("tool is not callable in the current vault/profile".to_string());
    }
    match normalized.as_str() {
        "cli" => collect_cli_compat(tool, &mut warnings, &mut errors),
        "mcp" => collect_mcp_compat(tool, &mut warnings, &mut errors),
        "openai-tools" | "openai" => collect_openai_tool_compat(tool, &mut warnings, &mut errors),
        "js" | "javascript" => collect_js_compat(tool, &mut warnings, &mut errors),
        other => errors.push(format!("unknown compatibility surface `{other}`")),
    }
    CustomToolCompatSurfaceReport {
        surface: normalized,
        compatible: errors.is_empty(),
        warnings,
        errors,
    }
}

fn collect_cli_compat(
    tool: &AssistantToolSummary,
    warnings: &mut Vec<String>,
    errors: &mut Vec<String>,
) {
    let Some(cli) = &tool.cli else {
        errors.push("missing CLI metadata".to_string());
        return;
    };
    if cli.aliases.is_empty() {
        warnings
            .push("no CLI alias is declared; callers must use the canonical tool name".to_string());
    }
    let covered_fields = cli
        .args
        .iter()
        .map(|arg| arg.field.as_deref().unwrap_or(&arg.flag))
        .map(top_level_field_name)
        .collect::<BTreeSet<_>>();
    for field in json_schema_required_fields(&tool.input_schema) {
        if !covered_fields.contains(&field) {
            errors.push(format!("required input field `{field}` has no CLI flag"));
        }
    }
}

fn collect_mcp_compat(
    tool: &AssistantToolSummary,
    warnings: &mut Vec<String>,
    errors: &mut Vec<String>,
) {
    if !tool.input_schema.is_object() {
        errors.push("input schema must be a JSON object schema".to_string());
    }
    if tool.output_schema.is_none() {
        warnings.push("missing output_schema limits structured-result usefulness".to_string());
    }
    if matches!(tool.sandbox, JsRuntimeSandbox::None) {
        errors.push("sandbox none is not exposed as a managed MCP tool".to_string());
    }
    if Path::new(&tool.entrypoint).is_absolute() || Path::new(&tool.entrypoint_path).is_absolute() {
        errors.push("entrypoint paths must be relative".to_string());
    }
}

fn collect_openai_tool_compat(
    tool: &AssistantToolSummary,
    warnings: &mut Vec<String>,
    errors: &mut Vec<String>,
) {
    collect_mcp_compat(tool, warnings, errors);
    if tool.name.len() > 64 {
        warnings.push(
            "tool name is longer than 64 characters and may be awkward for some clients"
                .to_string(),
        );
    }
    if tool.description.trim().is_empty() {
        errors.push("description is required for agent tool selection".to_string());
    }
}

fn collect_js_compat(
    tool: &AssistantToolSummary,
    warnings: &mut Vec<String>,
    errors: &mut Vec<String>,
) {
    if matches!(tool.sandbox, JsRuntimeSandbox::None) {
        errors.push("sandbox none is not callable from the managed JS tool registry".to_string());
    }
    if tool.secrets.is_empty() && matches!(tool.sandbox, JsRuntimeSandbox::Net) {
        warnings.push(
            "net sandbox tool declares no secrets; verify it does not need API credentials"
                .to_string(),
        );
    }
}

fn tool_example_base_dir(
    paths: &VaultPaths,
    tool: &AssistantToolSummary,
) -> Result<PathBuf, AppError> {
    let manifest_path = paths.vault_root().join(&tool.path);
    if manifest_path.exists() {
        return manifest_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| AppError::operation("tool manifest has no parent directory"));
    }
    let config = load_vault_config(paths).config;
    let manifest_path = paths
        .vault_root()
        .join(config.assistant.skills_folder)
        .join(&tool.path);
    manifest_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| AppError::operation("tool manifest has no parent directory"))
}

fn read_skill_example_json_file(base_dir: &Path, relative_path: &str) -> Result<Value, AppError> {
    let relative_path = safe_relative_example_path(relative_path)?;
    let path = base_dir.join(relative_path);
    let source = fs::read_to_string(&path).map_err(|error| {
        AppError::operation(format!("failed to read {}: {error}", path.display()))
    })?;
    serde_json::from_str(&source).map_err(|error| {
        AppError::operation(format!(
            "failed to parse {} as JSON: {error}",
            path.display()
        ))
    })
}

fn safe_relative_example_path(value: &str) -> Result<PathBuf, AppError> {
    let path = Path::new(value);
    if path.is_absolute() {
        return Err(AppError::operation(format!(
            "example file path `{value}` must be relative"
        )));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(AppError::operation(format!(
            "example file path `{value}` must stay inside the skill directory"
        )));
    }
    Ok(path.to_path_buf())
}

pub fn json_schema_to_typescript(schema: &Value, indent: usize) -> String {
    if let Some(value) = schema.get("const") {
        return json_value_to_typescript_literal(value);
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        let variants = values
            .iter()
            .map(json_value_to_typescript_literal)
            .collect::<Vec<_>>();
        return variants.join(" | ");
    }
    for union_key in ["anyOf", "oneOf"] {
        if let Some(variants) = schema.get(union_key).and_then(Value::as_array) {
            return variants
                .iter()
                .map(|variant| json_schema_to_typescript(variant, indent))
                .collect::<Vec<_>>()
                .join(" | ");
        }
    }
    match schema.get("type") {
        Some(Value::String(kind)) => json_schema_kind_to_typescript(kind, schema, indent),
        Some(Value::Array(kinds)) => kinds
            .iter()
            .filter_map(Value::as_str)
            .map(|kind| json_schema_kind_to_typescript(kind, schema, indent))
            .collect::<Vec<_>>()
            .join(" | "),
        None => json_object_schema_to_typescript(schema, indent),
        Some(_) => "unknown".to_string(),
    }
}

fn json_schema_kind_to_typescript(kind: &str, schema: &Value, indent: usize) -> String {
    match kind {
        "string" => "string".to_string(),
        "integer" | "number" => "number".to_string(),
        "boolean" => "boolean".to_string(),
        "null" => "null".to_string(),
        "array" => {
            let item_type = schema.get("items").map_or_else(
                || "unknown".to_string(),
                |items| json_schema_to_typescript(items, indent),
            );
            if item_type.contains(" | ") {
                format!("({item_type})[]")
            } else {
                format!("{item_type}[]")
            }
        }
        "object" => json_object_schema_to_typescript(schema, indent),
        _ => "unknown".to_string(),
    }
}

fn json_object_schema_to_typescript(schema: &Value, indent: usize) -> String {
    let properties = schema.get("properties").and_then(Value::as_object);
    let additional_properties = schema.get("additionalProperties");
    if properties.is_none() {
        return additional_properties
            .and_then(|schema| {
                if schema == &Value::Bool(false) {
                    Some("Record<string, never>".to_string())
                } else if schema == &Value::Bool(true) {
                    Some("Record<string, unknown>".to_string())
                } else if schema.is_object() {
                    Some(format!(
                        "Record<string, {}>",
                        json_schema_to_typescript(schema, indent)
                    ))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "Record<string, unknown>".to_string());
    }
    let properties = properties.expect("properties checked above");
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let pad = " ".repeat(indent);
    let child_pad = " ".repeat(indent + 2);
    let mut lines = vec!["{".to_string()];
    for (name, schema) in properties {
        let optional = if required.contains(name.as_str()) {
            ""
        } else {
            "?"
        };
        let property = if is_ts_identifier(name) {
            name.clone()
        } else {
            serde_json::to_string(name).unwrap_or_else(|_| "\"<invalid>\"".to_string())
        };
        lines.push(format!(
            "{child_pad}{property}{optional}: {};",
            json_schema_to_typescript(schema, indent + 2)
        ));
    }
    if let Some(additional_schema) = additional_properties {
        match additional_schema {
            Value::Bool(true) => {
                lines.push(format!("{child_pad}[key: string]: unknown;"));
            }
            Value::Object(_) => {
                lines.push(format!(
                    "{child_pad}[key: string]: {};",
                    json_schema_to_typescript(additional_schema, indent + 2)
                ));
            }
            _ => {}
        }
    }
    lines.push(format!("{pad}}}"));
    lines.join("\n")
}

fn json_value_to_typescript_literal(value: &Value) -> String {
    match value {
        Value::String(value) => {
            serde_json::to_string(value).unwrap_or_else(|_| "\"<invalid>\"".to_string())
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => compact_json(value),
        Value::Array(_) | Value::Object(_) => "unknown".to_string(),
    }
}

fn ts_type_name(value: &str) -> String {
    let mut result = String::new();
    for part in value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
    {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            result.push(first.to_ascii_uppercase());
            result.extend(chars.map(|character| character.to_ascii_lowercase()));
        }
    }
    if result.is_empty() {
        "Tool".to_string()
    } else {
        result
    }
}

fn ts_function_name(value: &str) -> String {
    let mut parts = value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty());
    let Some(first) = parts.next() else {
        return "callTool".to_string();
    };
    let mut result = first.to_ascii_lowercase();
    for part in parts {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            result.push(first.to_ascii_uppercase());
            result.extend(chars.map(|character| character.to_ascii_lowercase()));
        }
    }
    result
}

fn is_ts_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '$'
        })
}

fn json_schema_required_fields(schema: &Value) -> BTreeSet<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect()
}

fn top_level_field_name(value: &str) -> String {
    value
        .split('.')
        .next()
        .unwrap_or(value)
        .trim_start_matches('-')
        .to_string()
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unprintable>".to_string())
}

pub fn init_custom_tool(
    paths: &VaultPaths,
    name: &str,
    registry_options: &CustomToolRegistryOptions,
    options: &CustomToolInitOptions,
) -> Result<CustomToolWriteReport, AppError> {
    let validation_options = assistant_tool_validation_options(registry_options);
    let tool_name = validate_tool_name_candidate(name)?;

    let tool_root = assistant_tools_root(paths).join(&tool_name);
    let manifest_path = tool_root.join("TOOL.md");
    let entrypoint_path = tool_root.join("main.js");
    let existed_before = manifest_path.exists() || entrypoint_path.exists();
    if existed_before && !options.overwrite {
        return Err(AppError::operation(format!(
            "tool `{tool_name}` already exists; rerun with overwrite enabled to replace the scaffold"
        )));
    }

    let description = options
        .description
        .clone()
        .unwrap_or_else(|| format!("TODO: describe `{tool_name}`."));
    let (frontmatter, body, source) = scaffold_tool_template(&tool_name, &description, options);
    validate_editable_frontmatter(paths, &frontmatter, registry_options)?;
    let manifest_contents = render_tool_document(&frontmatter, &body)?;
    let manifest_relative = relative_to_vault(paths, &manifest_path)?;
    let manifest_relative_to_tools = relative_to_tools_root(paths, &manifest_path)?;
    let entrypoint_relative = relative_to_vault(paths, &entrypoint_path)?;
    if !options.dry_run {
        fs::create_dir_all(&tool_root).map_err(AppError::operation)?;
        fs::write(&manifest_path, manifest_contents).map_err(AppError::operation)?;
        write_executable_script(&entrypoint_path, &ensure_trailing_newline(&source))?;
        load_assistant_tool_manifest(paths, &manifest_relative_to_tools, &validation_options)
            .map_err(AppError::operation)?;
    }

    Ok(CustomToolWriteReport {
        name: tool_name,
        updated: true,
        dry_run: options.dry_run,
        tool_root: relative_to_vault(paths, &tool_root)?,
        manifest_path: manifest_relative,
        entrypoint_path: Some(entrypoint_relative),
        operations: if existed_before {
            vec!["replace scaffold".to_string()]
        } else {
            vec!["create scaffold".to_string()]
        },
    })
}

#[allow(clippy::too_many_lines)]
pub fn set_custom_tool(
    paths: &VaultPaths,
    identifier: &str,
    registry_options: &CustomToolRegistryOptions,
    options: &CustomToolSetOptions,
) -> Result<CustomToolWriteReport, AppError> {
    let validation_options = assistant_tool_validation_options(registry_options);
    let manifest_paths = list_assistant_tool_manifest_paths(paths).map_err(AppError::operation)?;
    let manifest_relative_to_tools =
        resolve_tool_manifest_path(paths, identifier, &validation_options, &manifest_paths)?;
    let manifest_path = assistant_tools_root(paths).join(&manifest_relative_to_tools);
    let mut document = read_tool_document(&manifest_path)?;
    let mut operations = Vec::new();

    if let Some(title) = &options.title {
        document.frontmatter.title = normalize_string_field(title);
        operations.push("set title".to_string());
    }
    if options.clear_title {
        document.frontmatter.title = None;
        operations.push("clear title".to_string());
    }
    if let Some(description) = &options.description {
        document.frontmatter.description = normalize_string_field(description);
        operations.push("set description".to_string());
    }
    if let Some(sandbox) = options.sandbox {
        document.frontmatter.sandbox = Some(sandbox);
        operations.push("set sandbox".to_string());
    }
    if let Some(permission_profile) = &options.permission_profile {
        document.frontmatter.permission_profile = normalize_string_field(permission_profile);
        operations.push("set permission profile".to_string());
    }
    if options.clear_permission_profile {
        document.frontmatter.permission_profile = None;
        operations.push("clear permission profile".to_string());
    }
    if let Some(timeout_ms) = options.timeout_ms {
        document.frontmatter.timeout_ms = Some(timeout_ms);
        operations.push("set timeout".to_string());
    }
    if options.clear_timeout_ms {
        document.frontmatter.timeout_ms = None;
        operations.push("clear timeout".to_string());
    }
    if let Some(packs) = &options.packs {
        document.frontmatter.packs = normalize_unique_strings(packs);
        operations.push("set packs".to_string());
    }
    if options.clear_packs {
        document.frontmatter.packs.clear();
        operations.push("clear packs".to_string());
    }
    if let Some(secrets) = &options.secrets {
        document.frontmatter.secrets = normalize_secret_specs(secrets);
        operations.push("set secrets".to_string());
    }
    if options.clear_secrets {
        document.frontmatter.secrets.clear();
        operations.push("clear secrets".to_string());
    }
    if let Some(read_only) = options.read_only {
        document.frontmatter.read_only = read_only;
        operations.push(if read_only {
            "mark read-only".to_string()
        } else {
            "mark writable".to_string()
        });
    }
    if let Some(destructive) = options.destructive {
        document.frontmatter.destructive = destructive;
        operations.push(if destructive {
            "mark destructive".to_string()
        } else {
            "mark non-destructive".to_string()
        });
    }
    if let Some(input_schema) = &options.input_schema {
        document.frontmatter.input_schema = Some(input_schema.clone());
        operations.push("set input schema".to_string());
    }
    if let Some(output_schema) = &options.output_schema {
        document.frontmatter.output_schema = Some(output_schema.clone());
        operations.push("set output schema".to_string());
    }
    if options.clear_output_schema {
        document.frontmatter.output_schema = None;
        operations.push("clear output schema".to_string());
    }
    if operations.is_empty() {
        return Err(AppError::operation(
            "tool set requires at least one change flag",
        ));
    }

    validate_editable_frontmatter(paths, &document.frontmatter, registry_options)?;

    let current_contents = fs::read_to_string(&manifest_path).map_err(AppError::operation)?;
    let rendered = render_tool_document(&document.frontmatter, &document.body)?;
    let updated = current_contents != rendered;
    if updated && !options.dry_run {
        fs::write(&manifest_path, rendered).map_err(AppError::operation)?;
        load_assistant_tool_manifest(paths, &manifest_relative_to_tools, &validation_options)
            .map_err(AppError::operation)?;
    }

    let tool_root = manifest_path
        .parent()
        .ok_or_else(|| AppError::operation("tool manifest has no parent directory"))?;
    let entrypoint_relative = document
        .frontmatter
        .entrypoint
        .clone()
        .unwrap_or_else(|| "main.js".to_string());
    let entrypoint_path = tool_root.join(&entrypoint_relative);

    Ok(CustomToolWriteReport {
        name: document
            .frontmatter
            .name
            .clone()
            .unwrap_or_else(|| tool_directory_name(&manifest_relative_to_tools)),
        updated,
        dry_run: options.dry_run,
        tool_root: relative_to_vault(paths, tool_root)?,
        manifest_path: relative_to_vault(paths, &manifest_path)?,
        entrypoint_path: Some(relative_to_vault(paths, &entrypoint_path)?),
        operations,
    })
}

fn assistant_tool_validation_options(
    options: &CustomToolRegistryOptions,
) -> AssistantToolValidationOptions {
    AssistantToolValidationOptions {
        reserved_names: options.reserved_names.clone(),
        allowed_pack_names: options.allowed_pack_names.clone(),
    }
}

fn tool_entrypoint_path(paths: &VaultPaths, tool: &AssistantTool) -> PathBuf {
    assistant_tools_root(paths).join(&tool.summary.entrypoint_path)
}

fn current_file_for_tool(paths: &VaultPaths, tool: &AssistantTool) -> Option<String> {
    tool_entrypoint_path(paths, tool)
        .strip_prefix(paths.vault_root())
        .ok()
        .map(path_to_forward_string)
}

fn build_tool_invocation_source(
    paths: &VaultPaths,
    source: &str,
    tool: &AssistantTool,
    input: &Value,
    options: &CustomToolRunOptions,
) -> Result<String, AppError> {
    let input = serde_json::to_string(input).map_err(AppError::operation)?;
    let context = serde_json::to_string(&tool_invocation_context(paths, tool, options))
        .map_err(AppError::operation)?;
    let secrets =
        serde_json::to_string(&resolve_tool_secret_values(tool)?).map_err(AppError::operation)?;
    Ok(format!(
        "const __vulcanToolInput = {input};\n\
const __vulcanToolContext = {context};\n\
const __vulcanToolSecrets = {secrets};\n\
globalThis.input = __vulcanToolInput;\n\
globalThis.ctx = __vulcanToolContext;\n\
__vulcanToolContext.secrets = {{\n\
  list() {{ return Object.keys(__vulcanToolSecrets); }},\n\
  get(name) {{\n\
    return Object.prototype.hasOwnProperty.call(__vulcanToolSecrets, name)\n\
      ? __vulcanToolSecrets[name]\n\
      : null;\n\
  }},\n\
  require(name) {{\n\
    const value = this.get(name);\n\
    if (value == null) {{\n\
      throw new Error(`secret ${{name}} is not available`);\n\
    }}\n\
    return value;\n\
  }},\n\
}};\n\
{source}\n\
if (typeof main !== 'function') {{\n\
  throw new Error('custom tool entrypoint must export `main(input, ctx)`');\n\
}}\n\
main(__vulcanToolInput, __vulcanToolContext);\n"
    ))
}

fn tool_invocation_context(
    paths: &VaultPaths,
    tool: &AssistantTool,
    options: &CustomToolRunOptions,
) -> Value {
    json!({
        "tool": {
            "name": tool.summary.name,
            "title": tool.summary.title,
            "description": tool.summary.description,
            "version": tool.summary.version,
            "runtime": tool.summary.runtime,
            "entrypoint": tool.summary.entrypoint,
            "entrypoint_path": tool.summary.entrypoint_path,
            "tags": tool.summary.tags,
            "sandbox": tool.summary.sandbox,
            "permission_profile": tool.summary.permission_profile,
            "timeout_ms": tool.summary.timeout_ms,
            "packs": tool.summary.packs,
            "secrets": tool.summary.secrets,
            "read_only": tool.summary.read_only,
            "destructive": tool.summary.destructive,
            "input_schema": tool.summary.input_schema,
            "output_schema": tool.summary.output_schema,
            "path": tool.summary.path,
            "manifest_path": assistant_tools_root(paths)
                .join(&tool.summary.path)
                .display()
                .to_string(),
            "tool_root": tool_entrypoint_path(paths, tool)
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .display()
                .to_string(),
        },
        "call": {
            "surface": options.surface,
            "timestamp_ms": current_timestamp_millis(),
        }
    })
}

fn resolve_tool_secret_values(tool: &AssistantTool) -> Result<Value, AppError> {
    let mut secrets = serde_json::Map::new();
    for secret in &tool.summary.secrets {
        let value = std::env::var(&secret.env).ok();
        if secret.required && value.is_none() {
            return Err(AppError::operation(format!(
                "tool `{}` requires secret `{}` from env var `{}`",
                tool.summary.name, secret.name, secret.env
            )));
        }
        match value {
            Some(value) => {
                secrets.insert(secret.name.clone(), Value::String(value));
            }
            None => {
                secrets.insert(secret.name.clone(), Value::Null);
            }
        }
    }
    Ok(Value::Object(secrets))
}

fn normalize_tool_return_value(
    tool: &AssistantTool,
    value: Value,
) -> Result<(Value, Option<String>), AppError> {
    let Value::Object(mut object) = value.clone() else {
        return Ok((value, None));
    };
    if !object.contains_key("result") || object.keys().any(|key| key != "result" && key != "text") {
        return Ok((Value::Object(object), None));
    }

    let result = object.remove("result").unwrap_or(Value::Null);
    let text = match object.remove("text") {
        Some(Value::String(text)) => Some(text),
        Some(_) => {
            return Err(AppError::operation(format!(
                "tool `{}` returned an invalid `text`; expected string",
                tool.summary.name
            )))
        }
        None => None,
    };
    Ok((result, text))
}

fn effective_tool_permission_profile(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    tool_name: &str,
    requested_permission_profile: Option<&str>,
) -> Result<Option<String>, AppError> {
    match (active_permission_profile, requested_permission_profile) {
        (None, None) => Ok(None),
        (None, Some(requested)) => {
            resolve_permission_profile(paths, Some(requested)).map_err(AppError::operation)?;
            Ok(Some(requested.to_string()))
        }
        (Some(active), None) => Ok(Some(active.to_string())),
        (Some(active), Some(requested)) => {
            let active_profile =
                resolve_permission_profile(paths, Some(active)).map_err(AppError::operation)?;
            let requested_profile =
                resolve_permission_profile(paths, Some(requested)).map_err(AppError::operation)?;
            if requested_profile.grant.is_subset_of(&active_profile.grant) {
                Ok(Some(requested.to_string()))
            } else {
                Err(AppError::operation(format!(
                    "tool `{tool_name}` requires permission profile `{requested}`, which is broader than active profile `{active}`"
                )))
            }
        }
    }
}

fn custom_tool_is_callable(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    tool_name: &str,
    requested_permission_profile: Option<&str>,
) -> bool {
    if !trust::is_trusted(paths.vault_root()) {
        return false;
    }
    let Ok(effective_permission_profile) = effective_tool_permission_profile(
        paths,
        active_permission_profile,
        tool_name,
        requested_permission_profile,
    ) else {
        return false;
    };
    match effective_permission_profile.as_deref() {
        Some(profile) => resolve_permission_profile(paths, Some(profile))
            .is_ok_and(|selection| selection.grant.execute),
        None => true,
    }
}

fn current_timestamp_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

fn strip_shebang_line(source: &str) -> &str {
    if let Some(stripped) = source.strip_prefix("#!") {
        stripped
            .split_once('\n')
            .map_or("", |(_, remainder)| remainder)
    } else {
        source
    }
}

fn path_to_forward_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn validate_editable_frontmatter(
    paths: &VaultPaths,
    frontmatter: &EditableToolFrontmatter,
    registry_options: &CustomToolRegistryOptions,
) -> Result<(), AppError> {
    let name = frontmatter
        .name
        .as_deref()
        .ok_or_else(|| AppError::operation("tool manifest must set `name`"))?;
    validate_tool_name_candidate(name)?;
    if normalize_string_field(frontmatter.description.as_deref().unwrap_or_default()).is_none() {
        return Err(AppError::operation(
            "tool manifest must set a non-empty `description`",
        ));
    }
    if frontmatter.runtime.unwrap_or_default() != AssistantToolRuntime::Quickjs {
        return Err(AppError::operation(
            "custom tools currently only support `runtime = quickjs`",
        ));
    }
    validate_entrypoint_candidate(frontmatter.entrypoint.as_deref().unwrap_or("main.js"))?;
    if frontmatter.sandbox.unwrap_or(JsRuntimeSandbox::Strict) == JsRuntimeSandbox::None {
        return Err(AppError::operation(
            "custom tools cannot set `sandbox = none`",
        ));
    }
    if let Some(permission_profile) = frontmatter.permission_profile.as_deref() {
        resolve_permission_profile(paths, Some(permission_profile)).map_err(AppError::operation)?;
    }
    if frontmatter.timeout_ms == Some(0) {
        return Err(AppError::operation(
            "tool manifest `timeout_ms` must be greater than 0",
        ));
    }
    validate_schema_object(frontmatter.input_schema.as_ref(), "input_schema", true)?;
    validate_schema_object(frontmatter.output_schema.as_ref(), "output_schema", false)?;
    validate_pack_names(&frontmatter.packs, registry_options)?;
    validate_secret_specs(&frontmatter.secrets)?;
    Ok(())
}

fn validate_tool_name_candidate(value: &str) -> Result<String, AppError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::operation(
            "custom tool names must be non-empty `snake_case` identifiers",
        ));
    }
    if !value.chars().all(|character| {
        character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
    }) {
        return Err(AppError::operation(format!(
            "invalid custom tool name `{value}`; expected `snake_case`"
        )));
    }
    if !value
        .chars()
        .any(|character| character.is_ascii_alphabetic())
    {
        return Err(AppError::operation(format!(
            "invalid custom tool name `{value}`; expected at least one ASCII letter"
        )));
    }
    Ok(value.to_string())
}

fn validate_entrypoint_candidate(value: &str) -> Result<(), AppError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::operation(
            "custom tool entrypoints must be non-empty relative paths",
        ));
    }
    if value.contains(':') {
        return Err(AppError::operation(format!(
            "invalid custom tool entrypoint `{value}`; drive and URI prefixes are not allowed"
        )));
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(AppError::operation(format!(
            "invalid custom tool entrypoint `{value}`; entrypoints must stay within the tool directory"
        )));
    }
    Ok(())
}

fn validate_schema_object(
    value: Option<&Value>,
    field: &str,
    required: bool,
) -> Result<(), AppError> {
    match value {
        Some(Value::Object(_)) => Ok(()),
        Some(_) => Err(AppError::operation(format!(
            "tool manifest field `{field}` must be a JSON object"
        ))),
        None if required => Err(AppError::operation(format!(
            "tool manifest must set `{field}`"
        ))),
        None => Ok(()),
    }
}

fn validate_pack_names(
    packs: &[String],
    registry_options: &CustomToolRegistryOptions,
) -> Result<(), AppError> {
    let mut allowed = registry_options.allowed_pack_names.clone();
    if !allowed.iter().any(|pack| pack == "custom") {
        allowed.push("custom".to_string());
    }
    for pack in packs {
        if pack.trim().is_empty() {
            return Err(AppError::operation(
                "tool manifest pack names must be non-empty",
            ));
        }
        if !allowed.iter().any(|allowed_pack| allowed_pack == pack) {
            return Err(AppError::operation(format!(
                "tool manifest uses unknown tool pack `{pack}`"
            )));
        }
    }
    Ok(())
}

fn validate_secret_specs(secrets: &[AssistantToolSecretSpec]) -> Result<(), AppError> {
    let mut seen = Vec::new();
    for secret in secrets {
        let name = validate_tool_name_candidate(&secret.name)?;
        if seen.contains(&name) {
            return Err(AppError::operation(format!(
                "duplicate custom tool secret `{name}`"
            )));
        }
        seen.push(name);
        validate_secret_env_name(&secret.env)?;
    }
    Ok(())
}

fn validate_secret_env_name(value: &str) -> Result<(), AppError> {
    let value = value.trim();
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return Err(AppError::operation(
            "custom tool secret env var names must be non-empty",
        ));
    };
    if !(first.is_ascii_alphabetic() || first == '_')
        || !characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(AppError::operation(format!(
            "invalid custom tool secret env var `{value}`"
        )));
    }
    Ok(())
}

fn resolve_tool_manifest_path(
    paths: &VaultPaths,
    identifier: &str,
    validation_options: &AssistantToolValidationOptions,
    manifest_paths: &[String],
) -> Result<String, AppError> {
    let identifier = identifier.trim().replace('\\', "/");
    if identifier.is_empty() {
        return Err(AppError::operation(
            "custom tool identifier must be non-empty",
        ));
    }

    let mut matches = manifest_paths
        .iter()
        .filter(|manifest_path| manifest_path_matches_identifier(manifest_path, &identifier))
        .cloned()
        .collect::<Vec<_>>();

    if matches.is_empty() {
        for manifest_path in manifest_paths {
            if let Ok(tool) = load_assistant_tool_manifest(paths, manifest_path, validation_options)
            {
                if tool.summary.name == identifier {
                    matches.push(manifest_path.clone());
                }
            }
        }
    }

    matches.sort();
    matches.dedup();
    match matches.as_slice() {
        [] => Err(AppError::operation(format!(
            "unknown custom tool `{identifier}`"
        ))),
        [manifest_path] => Ok(manifest_path.clone()),
        _ => Err(AppError::operation(format!(
            "custom tool `{identifier}` is ambiguous; use a manifest path such as `{}`",
            matches[0]
        ))),
    }
}

fn manifest_path_matches_identifier(manifest_path: &str, identifier: &str) -> bool {
    manifest_path == identifier
        || manifest_path
            .strip_suffix("/TOOL.md")
            .is_some_and(|prefix| prefix == identifier)
        || tool_directory_name(manifest_path) == identifier
}

fn tool_directory_name(manifest_path: &str) -> String {
    Path::new(manifest_path)
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or(manifest_path)
        .to_string()
}

fn scaffold_tool_template(
    tool_name: &str,
    description: &str,
    options: &CustomToolInitOptions,
) -> (EditableToolFrontmatter, String, String) {
    let frontmatter = EditableToolFrontmatter {
        name: Some(tool_name.to_string()),
        title: options.title.clone(),
        description: Some(description.to_string()),
        entrypoint: Some("main.js".to_string()),
        runtime: Some(AssistantToolRuntime::Quickjs),
        sandbox: Some(options.sandbox),
        permission_profile: options.permission_profile.clone(),
        timeout_ms: options.timeout_ms,
        packs: vec!["custom".to_string()],
        input_schema: Some(json!({
            "type": "object",
            "additionalProperties": false,
        })),
        output_schema: Some(json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "ok": { "type": "boolean" },
                "tool": { "type": "string" },
                "received": {}
            },
            "required": ["ok", "tool", "received"]
        })),
        ..EditableToolFrontmatter::default()
    };
    let body = match options.example {
        CustomToolInitExample::Minimal => format!(
            "## When to use\n\nDescribe when `{tool_name}` should run and what contract it provides.\n\n## Input\n\nKeep this section aligned with `input_schema`.\n\n## Secrets\n\nDeclare secret bindings in frontmatter `secrets:` and access them at runtime with `ctx.secrets.get()` or `ctx.secrets.require()`.\n\n## Output\n\nReturn JSON directly or return `{{ result, text }}` when you want both machine-readable output and a short human fallback."
        ),
    };
    let source = format!(
        "{CUSTOM_TOOL_SCRIPT_SHEBANG}function main(input, ctx) {{\n  return {{\n    result: {{\n      ok: true,\n      tool: ctx.tool.name,\n      received: input,\n    }},\n    text: `ran ${{ctx.tool.name}}`,\n  }};\n}}\n"
    );
    (frontmatter, body, source)
}

fn read_tool_document(manifest_path: &Path) -> Result<EditableToolDocument, AppError> {
    let source = fs::read_to_string(manifest_path).map_err(AppError::operation)?;
    let (frontmatter, body) = split_tool_frontmatter(&source, manifest_path)?;
    let frontmatter = frontmatter
        .map(|frontmatter| serde_yaml::from_str(frontmatter).map_err(AppError::operation))
        .transpose()?
        .unwrap_or_default();
    Ok(EditableToolDocument {
        frontmatter,
        body: body.trim().to_string(),
    })
}

fn render_tool_document(
    frontmatter: &EditableToolFrontmatter,
    body: &str,
) -> Result<String, AppError> {
    let yaml = serde_yaml::to_string(frontmatter).map_err(AppError::operation)?;
    let mut rendered = String::from("---\n");
    rendered.push_str(&yaml);
    rendered.push_str("---\n");
    let body = body.trim();
    if !body.is_empty() {
        rendered.push('\n');
        rendered.push_str(body);
        rendered.push('\n');
    }
    Ok(rendered)
}

fn split_tool_frontmatter<'a>(
    source: &'a str,
    path: &Path,
) -> Result<(Option<&'a str>, &'a str), AppError> {
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
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let frontmatter = &source[first_line.len()..offset];
            let body = &source[offset + line.len()..];
            return Ok((Some(frontmatter), body));
        }
        offset += line.len();
    }

    Err(AppError::operation(format!(
        "{} has an unterminated frontmatter block",
        path.display()
    )))
}

fn relative_to_vault(paths: &VaultPaths, path: &Path) -> Result<String, AppError> {
    path.strip_prefix(paths.vault_root())
        .map(path_to_forward_string)
        .map_err(|_| AppError::operation(format!("{} is outside the vault", path.display())))
}

fn relative_to_tools_root(paths: &VaultPaths, path: &Path) -> Result<String, AppError> {
    path.strip_prefix(assistant_tools_root(paths))
        .map(path_to_forward_string)
        .map_err(|_| AppError::operation(format!("{} is outside the tools folder", path.display())))
}

fn normalize_string_field(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn normalize_unique_strings(values: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for value in values {
        let Some(value) = normalize_string_field(value) else {
            continue;
        };
        if !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    normalized
}

fn normalize_secret_specs(secrets: &[AssistantToolSecretSpec]) -> Vec<AssistantToolSecretSpec> {
    let mut normalized = Vec::new();
    for secret in secrets {
        let Some(name) = normalize_string_field(&secret.name) else {
            continue;
        };
        let Some(env) = normalize_string_field(&secret.env) else {
            continue;
        };
        if normalized
            .iter()
            .any(|existing: &AssistantToolSecretSpec| existing.name == name)
        {
            continue;
        }
        normalized.push(AssistantToolSecretSpec {
            name,
            env,
            required: secret.required,
            description: secret
                .description
                .as_deref()
                .and_then(normalize_string_field),
        });
    }
    normalized
}

fn ensure_trailing_newline(value: &str) -> String {
    if value.ends_with('\n') {
        value.to_string()
    } else {
        format!("{value}\n")
    }
}

fn write_executable_script(path: &Path, contents: &str) -> Result<(), AppError> {
    fs::write(path, contents).map_err(AppError::operation)?;
    set_executable_permissions(path)
}

#[cfg(unix)]
fn set_executable_permissions(path: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path).map_err(AppError::operation)?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(path, permissions).map_err(AppError::operation)
}

#[cfg(not(unix))]
fn set_executable_permissions(_path: &Path) -> Result<(), AppError> {
    Ok(())
}

#[cfg(unix)]
fn script_is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn script_is_executable(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use tempfile::TempDir;
    use vulcan_core::paths::initialize_vulcan_dir;
    use vulcan_core::{scan_vault, ScanMode};

    fn test_paths() -> (TempDir, VaultPaths) {
        let dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(dir.path());
        initialize_vulcan_dir(&paths).expect("vault should initialize");
        (dir, paths)
    }

    fn write_tool(paths: &VaultPaths, name: &str, manifest: &str, source: &str) {
        let root = paths.vault_root().join(".agents/tools").join(name);
        fs::create_dir_all(&root).expect("tool dir should exist");
        fs::write(root.join("TOOL.md"), manifest).expect("tool manifest should write");
        fs::write(root.join("main.js"), source).expect("tool source should write");
    }

    fn write_skill(
        paths: &VaultPaths,
        name: &str,
        manifest: &str,
        source_name: &str,
        source: &str,
    ) {
        let root = paths.vault_root().join(".agents/skills").join(name);
        let scripts = root.join("scripts");
        fs::create_dir_all(&scripts).expect("skill scripts dir should exist");
        fs::write(root.join("SKILL.md"), manifest).expect("skill manifest should write");
        fs::write(scripts.join(source_name), source).expect("skill script should write");
    }

    fn legacy_tool_options() -> CustomToolRegistryOptions {
        CustomToolRegistryOptions {
            include_standalone_tools: true,
            ..CustomToolRegistryOptions::default()
        }
    }

    fn with_trusted_vault(paths: &VaultPaths) {
        trust::add_trust(paths.vault_root()).expect("trust should be added");
        assert!(trust::is_trusted(paths.vault_root()));
    }

    fn test_env_lock_guard() -> std::sync::MutexGuard<'static, ()> {
        trust::test_env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn list_custom_tools_marks_untrusted_vaults_as_not_callable() {
        let (_dir, paths) = test_paths();
        write_tool(
            &paths,
            "summary",
            r"---
name: summary_tool
description: Summarize one note.
input_schema:
  type: object
---
",
            "function main() { return null; }\n",
        );

        let tools =
            list_custom_tools(&paths, None, &legacy_tool_options()).expect("tools should load");
        assert_eq!(tools.len(), 1);
        assert!(!tools[0].callable);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn custom_tool_cli_completion_candidates_include_aliases_and_flags() {
        let (_dir, paths) = test_paths();
        write_skill(
            &paths,
            "conversation-export",
            r"---
name: conversation-export
description: Export conversations.
metadata:
  vulcan:
    commands:
      - id: export
        description: Export one conversation.
        script: scripts/export.js
        expose: true
        cli:
          aliases: [conversation-export]
          args:
            - flag: title
              action: string
              field: title
            - flag: dry-run
              action: boolean
              field: options.dry_run
            - flag: limit
              action: integer
              field: options.limit
            - flag: score
              action: number
              field: options.score
            - flag: source
              action: choice
              field: source
              choices: [chatgpt, codex]
            - flag: tag
              action: string_array
              field: tags
            - flag: user
              action: append_message
              role: user
        input_schema:
          type: object
---
# Conversation Export
",
            "export.js",
            "function main(input) { return input; }\n",
        );

        assert_eq!(
            collect_custom_tool_cli_name_candidates(&paths, &CustomToolRegistryOptions::default())
                .expect("name candidates"),
            vec![
                "skill_conversation_export_export".to_string(),
                "conversation-export".to_string()
            ]
        );
        assert_eq!(
            collect_custom_tool_cli_flag_candidates(
                &paths,
                "conversation-export",
                &CustomToolRegistryOptions::default(),
            )
            .expect("flag candidates"),
            vec![
                "--title".to_string(),
                "--dry-run".to_string(),
                "--limit".to_string(),
                "--score".to_string(),
                "--source".to_string(),
                "--tag".to_string(),
                "--user".to_string()
            ]
        );

        let (_resolved, input) = build_custom_tool_cli_input(
            &paths,
            "conversation-export",
            &[
                "--title".to_string(),
                "Chat".to_string(),
                "--dry-run".to_string(),
                "--limit".to_string(),
                "3".to_string(),
                "--score".to_string(),
                "1.5".to_string(),
                "--source".to_string(),
                "codex".to_string(),
                "--tag".to_string(),
                "alpha".to_string(),
                "--tag".to_string(),
                "beta".to_string(),
            ],
            &CustomToolRegistryOptions::default(),
        )
        .expect("cli input should build");
        assert_eq!(
            input,
            json!({
                "title": "Chat",
                "options": {
                    "dry_run": true,
                    "limit": 3,
                    "score": 1.5
                },
                "source": "codex",
                "tags": ["alpha", "beta"]
            })
        );
        let error = build_custom_tool_cli_input(
            &paths,
            "conversation-export",
            &[
                "--title".to_string(),
                "Chat".to_string(),
                "--source".to_string(),
                "gemini".to_string(),
            ],
            &CustomToolRegistryOptions::default(),
        )
        .expect_err("invalid choice should fail");
        assert!(
            error
                .to_string()
                .contains("invalid choice `gemini` for custom CLI flag `--source`"),
            "{error}"
        );
    }

    #[test]
    fn list_custom_tools_marks_execute_denied_profiles_as_not_callable() {
        let _lock = test_env_lock_guard();
        let (_dir, paths) = test_paths();
        let config_home = TempDir::new().expect("config home should be created");
        let previous_xdg = env::var_os("XDG_CONFIG_HOME");
        env::set_var("XDG_CONFIG_HOME", config_home.path());
        fs::write(
            paths.vault_root().join(".vulcan/config.toml"),
            r#"
[permissions.profiles.blocked]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "deny"
shell = "deny"
"#,
        )
        .expect("config should write");
        with_trusted_vault(&paths);
        write_tool(
            &paths,
            "summary",
            r"---
name: summary_tool
description: Summarize one note.
input_schema:
  type: object
---
",
            "function main() { return null; }\n",
        );

        let tools = list_custom_tools(&paths, Some("blocked"), &legacy_tool_options())
            .expect("tools should load");
        assert_eq!(tools.len(), 1);
        assert!(!tools[0].callable);

        trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
        match previous_xdg {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn list_custom_tools_marks_missing_and_broader_profiles_as_not_callable() {
        let _lock = test_env_lock_guard();
        let (_dir, paths) = test_paths();
        let config_home = TempDir::new().expect("config home should be created");
        let previous_xdg = env::var_os("XDG_CONFIG_HOME");
        env::set_var("XDG_CONFIG_HOME", config_home.path());
        fs::write(
            paths.vault_root().join(".vulcan/config.toml"),
            r#"
[permissions.profiles.readonly]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "deny"

[permissions.profiles.writer]
read = "all"
write = { allow = ["folder:Projects/**"] }
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "deny"

[permissions.profiles.networker]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = { allow = true, domains = ["example.com"] }
index = "deny"
config = "read"
execute = "allow"
shell = "deny"

[permissions.profiles.sheller]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "allow"
"#,
        )
        .expect("config should write");
        with_trusted_vault(&paths);
        write_tool(
            &paths,
            "writer",
            r"---
name: writer_tool
description: Needs write access.
permission_profile: writer
input_schema:
  type: object
---
",
            "function main() { return null; }\n",
        );
        write_tool(
            &paths,
            "networker",
            r"---
name: networker_tool
description: Needs network access.
permission_profile: networker
input_schema:
  type: object
---
",
            "function main() { return null; }\n",
        );
        write_tool(
            &paths,
            "sheller",
            r"---
name: sheller_tool
description: Needs shell access.
permission_profile: sheller
input_schema:
  type: object
---
",
            "function main() { return null; }\n",
        );
        write_tool(
            &paths,
            "missing",
            r"---
name: missing_profile_tool
description: References a missing profile.
permission_profile: missing_profile
input_schema:
  type: object
---
",
            "function main() { return null; }\n",
        );

        let tools = list_custom_tools(&paths, Some("readonly"), &legacy_tool_options())
            .expect("tools should load");
        assert_eq!(tools.len(), 4);
        for name in [
            "writer_tool",
            "networker_tool",
            "sheller_tool",
            "missing_profile_tool",
        ] {
            assert!(
                tools
                    .iter()
                    .find(|tool| tool.summary.name == name)
                    .is_some_and(|tool| !tool.callable),
                "tool `{name}` should stay visible but not callable"
            );
        }

        trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
        match previous_xdg {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn run_custom_tool_validates_input_secrets_and_output() {
        let _lock = test_env_lock_guard();
        let (_dir, paths) = test_paths();
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");
        let config_home = TempDir::new().expect("config home should be created");
        let previous_xdg = env::var_os("XDG_CONFIG_HOME");
        env::set_var("XDG_CONFIG_HOME", config_home.path());
        with_trusted_vault(&paths);
        let env_name = format!("VULCAN_TEST_TOOL_SECRET_{}", current_timestamp_millis());
        let old_value = env::var(&env_name).ok();
        env::set_var(&env_name, "secret-token");
        write_tool(
            &paths,
            "remote",
            &format!(
                r"---
name: remote_tool
description: Calls a remote API.
secrets:
  - name: api
    env: {env_name}
    required: true
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
    note:
      type: string
    secret:
      type: string
    surface:
      type: string
  required:
    - note
    - secret
    - surface
---
"
            ),
            "function main(input, ctx) {\n  return {\n    result: {\n      note: input.note,\n      secret: ctx.secrets.require('api'),\n      surface: ctx.call.surface,\n    },\n    text: `ran ${ctx.tool.name}`,\n  };\n}\n",
        );

        let report = run_custom_tool(
            &paths,
            None,
            "remote_tool",
            &json!({ "note": "Projects/Alpha.md" }),
            &legacy_tool_options(),
            &CustomToolRunOptions {
                surface: "cli".to_string(),
            },
        )
        .expect("tool should run");

        assert_eq!(report.name, "remote_tool");
        assert_eq!(report.result["note"], json!("Projects/Alpha.md"));
        assert_eq!(report.result["secret"], json!("secret-token"));
        assert_eq!(report.result["surface"], json!("cli"));
        assert_eq!(report.text.as_deref(), Some("ran remote_tool"));

        match old_value {
            Some(value) => env::set_var(&env_name, value),
            None => env::remove_var(&env_name),
        }
        trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
        match previous_xdg {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn run_custom_tool_rejects_missing_permission_profiles() {
        let _lock = test_env_lock_guard();
        let (_dir, paths) = test_paths();
        let config_home = TempDir::new().expect("config home should be created");
        let previous_xdg = env::var_os("XDG_CONFIG_HOME");
        env::set_var("XDG_CONFIG_HOME", config_home.path());
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");
        with_trusted_vault(&paths);
        write_tool(
            &paths,
            "missing-profile",
            r"---
name: missing_profile_tool
description: References a profile that does not exist.
permission_profile: missing_profile
input_schema:
  type: object
---
",
            "function main() { return null; }\n",
        );

        let error = run_custom_tool(
            &paths,
            None,
            "missing_profile_tool",
            &json!({}),
            &legacy_tool_options(),
            &CustomToolRunOptions::default(),
        )
        .expect_err("missing tool profile should fail");
        assert!(error
            .to_string()
            .contains("unknown permission profile `missing_profile`"));

        trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
        match previous_xdg {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn run_custom_tool_surfaces_runtime_script_errors() {
        let _lock = test_env_lock_guard();
        let (_dir, paths) = test_paths();
        let config_home = TempDir::new().expect("config home should be created");
        let previous_xdg = env::var_os("XDG_CONFIG_HOME");
        env::set_var("XDG_CONFIG_HOME", config_home.path());
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");
        with_trusted_vault(&paths);
        write_tool(
            &paths,
            "broken",
            r"---
name: broken_tool
description: Throws from JS.
input_schema:
  type: object
---
",
            "function main() { throw new Error('boom'); }\n",
        );

        let error = run_custom_tool(
            &paths,
            None,
            "broken_tool",
            &json!({}),
            &legacy_tool_options(),
            &CustomToolRunOptions::default(),
        )
        .expect_err("runtime failure should surface");
        assert!(error.to_string().contains("boom"));

        trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
        match previous_xdg {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn run_custom_tool_rejects_output_schema_mismatches() {
        let _lock = test_env_lock_guard();
        let (_dir, paths) = test_paths();
        let config_home = TempDir::new().expect("config home should be created");
        let previous_xdg = env::var_os("XDG_CONFIG_HOME");
        env::set_var("XDG_CONFIG_HOME", config_home.path());
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");
        with_trusted_vault(&paths);
        write_tool(
            &paths,
            "mismatch",
            r"---
name: mismatch_tool
description: Returns the wrong shape.
input_schema:
  type: object
output_schema:
  type: object
  additionalProperties: false
  properties:
    ok:
      type: boolean
  required:
    - ok
---
",
            "function main() { return { ok: 'nope' }; }\n",
        );

        let error = run_custom_tool(
            &paths,
            None,
            "mismatch_tool",
            &json!({}),
            &legacy_tool_options(),
            &CustomToolRunOptions::default(),
        )
        .expect_err("output schema mismatch should fail");
        assert!(error
            .to_string()
            .contains("tool `mismatch_tool` output validation failed"));

        trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
        match previous_xdg {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn run_custom_tool_rejects_broader_permission_profiles() {
        let _lock = test_env_lock_guard();
        let (_dir, paths) = test_paths();
        let config_home = TempDir::new().expect("config home should be created");
        let previous_xdg = env::var_os("XDG_CONFIG_HOME");
        env::set_var("XDG_CONFIG_HOME", config_home.path());
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");
        fs::write(
            paths.vault_root().join(".vulcan/config.toml"),
            r#"
[permissions.profiles.agent]
read = "all"
write = { allow = ["folder:Projects/**"] }
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "deny"

[permissions.profiles.readonly]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "deny"
"#,
        )
        .expect("config should write");
        with_trusted_vault(&paths);
        write_tool(
            &paths,
            "restricted",
            r"---
name: restricted_tool
description: Needs agent profile.
permission_profile: agent
input_schema:
  type: object
---
",
            "function main() { return null; }\n",
        );

        let error = run_custom_tool(
            &paths,
            Some("readonly"),
            "restricted_tool",
            &json!({}),
            &legacy_tool_options(),
            &CustomToolRunOptions::default(),
        )
        .expect_err("broader requested profile should fail");
        assert!(error
            .to_string()
            .contains("tool `restricted_tool` requires permission profile `agent`"));
        let listed = list_custom_tools(&paths, Some("readonly"), &legacy_tool_options())
            .expect("tools should list");
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].callable);
        trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
        match previous_xdg {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn validate_custom_tools_reports_invalid_manifests_per_file() {
        let (_dir, paths) = test_paths();
        write_tool(
            &paths,
            "valid",
            r"---
name: valid_tool
description: Valid tool.
input_schema:
  type: object
---
",
            "function main() { return null; }\n",
        );
        write_tool(
            &paths,
            "invalid",
            r"---
name: invalid_tool
description:
input_schema:
  type: object
---
",
            "function main() { return null; }\n",
        );

        let report = validate_custom_tools(&paths, None, &legacy_tool_options())
            .expect("validation should succeed");
        assert_eq!(report.checked, 2);
        assert!(!report.valid);
        assert!(report
            .tools
            .iter()
            .any(|tool| tool.name.as_deref() == Some("valid_tool") && tool.valid));
        assert!(report
            .tools
            .iter()
            .any(|tool| tool.identifier == "invalid" && !tool.valid));
    }

    #[test]
    fn init_custom_tool_scaffolds_manifest_and_entrypoint() {
        let (_dir, paths) = test_paths();

        let report = init_custom_tool(
            &paths,
            "meeting_summary",
            &legacy_tool_options(),
            &CustomToolInitOptions {
                description: Some("Summarize one meeting note.".to_string()),
                ..CustomToolInitOptions::default()
            },
        )
        .expect("tool init should succeed");

        assert!(report.updated);
        assert_eq!(report.name, "meeting_summary");
        assert_eq!(
            report.manifest_path,
            ".agents/tools/meeting_summary/TOOL.md"
        );
        let manifest = fs::read_to_string(paths.vault_root().join(&report.manifest_path))
            .expect("manifest should exist");
        assert!(manifest.contains("name: meeting_summary"));
        assert!(manifest.contains("description: Summarize one meeting note."));
        let entrypoint_path = paths
            .vault_root()
            .join(report.entrypoint_path.as_deref().expect("entrypoint path"));
        let source = fs::read_to_string(&entrypoint_path).expect("entrypoint should exist");
        assert!(source.starts_with(CUSTOM_TOOL_SCRIPT_SHEBANG));
        assert!(source.contains("function main(input, ctx)"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = fs::metadata(&entrypoint_path)
                .expect("entrypoint metadata should load")
                .permissions()
                .mode();
            assert_ne!(mode & 0o111, 0);
        }
    }

    #[test]
    fn set_custom_tool_updates_manifest_fields() {
        let (_dir, paths) = test_paths();
        init_custom_tool(
            &paths,
            "meeting_summary",
            &legacy_tool_options(),
            &CustomToolInitOptions {
                description: Some("Summarize one meeting note.".to_string()),
                ..CustomToolInitOptions::default()
            },
        )
        .expect("tool init should succeed");

        let report = set_custom_tool(
            &paths,
            "meeting_summary",
            &legacy_tool_options(),
            &CustomToolSetOptions {
                description: Some("Summarize one meeting note into JSON.".to_string()),
                timeout_ms: Some(2500),
                read_only: Some(true),
                secrets: Some(vec![AssistantToolSecretSpec {
                    name: "api".to_string(),
                    env: "MEETING_API_KEY".to_string(),
                    required: true,
                    description: Some("API key".to_string()),
                }]),
                ..CustomToolSetOptions::default()
            },
        )
        .expect("tool set should succeed");

        assert!(report.updated);
        let manifest = fs::read_to_string(paths.vault_root().join(&report.manifest_path))
            .expect("manifest should exist");
        assert!(manifest.contains("description: Summarize one meeting note into JSON."));
        assert!(manifest.contains("timeout_ms: 2500"));
        assert!(manifest.contains("read_only: true"));
        assert!(manifest.contains("env: MEETING_API_KEY"));
    }

    #[test]
    fn custom_tools_can_list_get_and_call_other_tools_from_js() {
        let _lock = test_env_lock_guard();
        let (_dir, paths) = test_paths();
        let config_home = TempDir::new().expect("config home should be created");
        let previous_xdg = env::var_os("XDG_CONFIG_HOME");
        env::set_var("XDG_CONFIG_HOME", config_home.path());
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");
        with_trusted_vault(&paths);
        write_tool(
            &paths,
            "inner",
            r"---
name: inner_tool
description: Inner helper.
input_schema:
  type: object
  additionalProperties: false
  properties:
    note:
      type: string
  required:
    - note
---

Inner tool documentation.
",
            "function main(input) {\n  return { echoed: String(input.note).toUpperCase() };\n}\n",
        );
        write_tool(
            &paths,
            "outer",
            r"---
name: outer_tool
description: Outer helper.
input_schema:
  type: object
  additionalProperties: false
  properties:
    note:
      type: string
  required:
    - note
---

Outer tool documentation.
",
            "function main(input) {\n  const normalized = tool.input({ fallback: true });\n  const listed = tools.list();\n  const described = tools.get('inner_tool');\n  const called = tools.callChecked('inner_tool', { note: input.note });\n  return tool.result().summary('nested call complete').data({\n    listed: listed.map((tool) => tool.name),\n    callable: listed.every((tool) => tool.callable === true),\n    body_has_doc: described.body.includes('Inner tool documentation.'),\n    echoed: called.expect('echoed'),\n    fallback: normalized.fallback,\n  }).ok();\n}\n",
        );

        let report = run_custom_tool(
            &paths,
            None,
            "outer_tool",
            &json!({ "note": "alpha" }),
            &legacy_tool_options(),
            &CustomToolRunOptions {
                surface: "cli".to_string(),
            },
        )
        .expect("nested tool calls should succeed");

        assert_eq!(report.result["ok"], json!(true));
        assert_eq!(report.result["summary"], json!("nested call complete"));
        assert_eq!(
            report.result["data"]["listed"],
            json!(["inner_tool", "outer_tool"])
        );
        assert_eq!(report.result["data"]["callable"], json!(true));
        assert_eq!(report.result["data"]["body_has_doc"], json!(true));
        assert_eq!(report.result["data"]["echoed"], json!("ALPHA"));
        assert_eq!(report.result["data"]["fallback"], json!(true));

        trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
        match previous_xdg {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn tools_namespace_preserves_nested_permission_ceiling() {
        let _lock = test_env_lock_guard();
        let (_dir, paths) = test_paths();
        let config_home = TempDir::new().expect("config home should be created");
        let previous_xdg = env::var_os("XDG_CONFIG_HOME");
        env::set_var("XDG_CONFIG_HOME", config_home.path());
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");
        fs::write(
            paths.vault_root().join(".vulcan/config.toml"),
            r#"
[permissions.profiles.agent]
read = "all"
write = { allow = ["folder:Projects/**"] }
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "deny"

[permissions.profiles.readonly]
read = "all"
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "read"
execute = "allow"
shell = "deny"
"#,
        )
        .expect("config should write");
        with_trusted_vault(&paths);
        write_tool(
            &paths,
            "inner",
            r"---
name: privileged_inner
description: Needs agent profile.
permission_profile: agent
input_schema:
  type: object
---
",
            "function main() { return { ok: true }; }\n",
        );
        write_tool(
            &paths,
            "outer",
            r"---
name: readonly_outer
description: Calls another tool.
permission_profile: readonly
input_schema:
  type: object
---
",
            "function main() { return tools.call('privileged_inner', {}); }\n",
        );

        let error = run_custom_tool(
            &paths,
            None,
            "readonly_outer",
            &json!({}),
            &legacy_tool_options(),
            &CustomToolRunOptions {
                surface: "cli".to_string(),
            },
        )
        .expect_err("nested broader tool should fail");
        assert!(error.to_string().contains(
            "tool `privileged_inner` requires permission profile `agent`, which is broader than active profile `readonly`"
        ));

        trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
        match previous_xdg {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn tools_namespace_rejects_recursive_tool_loops() {
        let _lock = test_env_lock_guard();
        let (_dir, paths) = test_paths();
        let config_home = TempDir::new().expect("config home should be created");
        let previous_xdg = env::var_os("XDG_CONFIG_HOME");
        env::set_var("XDG_CONFIG_HOME", config_home.path());
        scan_vault(&paths, ScanMode::Full).expect("vault should scan");
        with_trusted_vault(&paths);
        write_tool(
            &paths,
            "loop",
            r"---
name: loop_tool
description: Calls itself.
input_schema:
  type: object
---
",
            "function main() { return tools.call('loop_tool', {}); }\n",
        );

        let error = run_custom_tool(
            &paths,
            None,
            "loop_tool",
            &json!({}),
            &legacy_tool_options(),
            &CustomToolRunOptions {
                surface: "cli".to_string(),
            },
        )
        .expect_err("recursive tool call should fail");
        assert!(error
            .to_string()
            .contains("recursive custom tool call detected: loop_tool -> loop_tool"));

        trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
        match previous_xdg {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    #[test]
    fn json_schema_typescript_supports_composed_tool_schemas() {
        let schema = json!({
            "type": "object",
            "required": ["mode", "payload"],
            "properties": {
                "mode": { "const": "append" },
                "payload": {
                    "anyOf": [
                        { "type": "string" },
                        { "type": "array", "items": { "type": ["string", "null"] } }
                    ]
                },
                "labels": {
                    "type": "object",
                    "additionalProperties": { "type": "number" }
                },
                "status": { "enum": ["open", "done", null] }
            },
            "additionalProperties": false
        });

        let typescript = json_schema_to_typescript(&schema, 0);

        assert!(typescript.contains("mode: \"append\";"));
        assert!(typescript.contains("payload: string | (string | null)[];"));
        assert!(typescript.contains("labels?: Record<string, number>;"));
        assert!(typescript.contains("status?: \"open\" | \"done\" | null;"));
        assert!(!typescript.contains("[key: string]"));
    }
}
