use crate::AppError;
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use vulcan_core::{list_assistant_skills, AssistantSkillCommandCliArgAction, VaultPaths};

use super::{
    command_matches_allowed_packs, resolve_skill_command_tool_identifier, skill_command_tool_name,
    CustomToolRegistryOptions,
};

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
