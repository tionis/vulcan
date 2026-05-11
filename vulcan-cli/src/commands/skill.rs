use crate::output::print_json;
use crate::{selected_permission_guard, Cli, CliError, OutputFormat, SkillCommand};
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use vulcan_core::{
    evaluate_dataview_js_with_options, list_assistant_skills, load_assistant_skill,
    load_vault_config, resolve_permission_profile, validate_json_value_against_schema,
    AssistantSkill, AssistantSkillCommandSummary, AssistantSkillSummary, DataviewJsEvalOptions,
    JsRuntimeSandbox, PermissionGuard, VaultPaths,
};

const SKILL_COMMAND_SCRIPT_SHEBANG: &str = "#!/usr/bin/env -S vulcan skill exec\n";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SkillListReport {
    skills: Vec<AssistantSkillSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SkillCommandsReport {
    commands: Vec<SkillCommandRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SkillCommandRow {
    skill: String,
    skill_path: String,
    #[serde(flatten)]
    command: AssistantSkillCommandSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SkillValidateReport {
    valid: bool,
    skills: usize,
    commands: usize,
    warnings: Vec<String>,
    errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct SkillRunReport {
    skill: String,
    #[serde(rename = "command")]
    tool: String,
    script: String,
    input: Value,
    result: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SkillInitReport {
    name: String,
    dry_run: bool,
    skill_root: String,
    manifest_path: String,
    script_path: Option<String>,
    operations: Vec<String>,
}

pub(crate) fn handle_skill_command(
    cli: &Cli,
    paths: &VaultPaths,
    command: &SkillCommand,
) -> Result<(), CliError> {
    match command {
        SkillCommand::List => {
            let report = SkillListReport {
                skills: visible_skills(cli, paths)?,
            };
            print_skill_list_report(cli.output, &report)
        }
        SkillCommand::Get { name } | SkillCommand::Show { name } => {
            let skill = visible_skill(cli, paths, name)?;
            print_skill_report(cli.output, &skill)
        }
        SkillCommand::Commands { name } => {
            let rows = if let Some(name) = name {
                let skill = visible_skill(cli, paths, name)?;
                skill_command_rows(&skill)
            } else {
                visible_skills(cli, paths)?
                    .into_iter()
                    .flat_map(|summary| {
                        let skill = summary.name.clone();
                        let skill_path = summary.path.clone();
                        summary
                            .commands
                            .into_iter()
                            .map(move |command| SkillCommandRow {
                                skill: skill.clone(),
                                skill_path: skill_path.clone(),
                                command,
                            })
                    })
                    .collect()
            };
            print_skill_commands_report(cli.output, &SkillCommandsReport { commands: rows })
        }
        SkillCommand::Validate => {
            let report = validate_visible_skills(cli, paths)?;
            print_skill_validate_report(cli.output, &report)
        }
        SkillCommand::Run {
            skill,
            command,
            input_json,
            input_file,
            input_args,
            input_json_args,
            input_file_args,
            input_json_file_args,
        } => {
            let input = read_skill_input(
                input_json.as_deref(),
                input_file.as_deref(),
                input_args,
                input_json_args,
                input_file_args,
                input_json_file_args,
            )?;
            let report = run_skill_command(cli, paths, skill, command, input)?;
            print_skill_run_report(cli.output, &report)
        }
        SkillCommand::Exec {
            script,
            input_json,
            input_file,
            input_args,
            input_json_args,
            input_file_args,
            input_json_file_args,
        } => {
            let input = read_skill_input_or_stdin(
                input_json.as_deref(),
                input_file.as_deref(),
                input_args,
                input_json_args,
                input_file_args,
                input_json_file_args,
            )?;
            let report = run_skill_command_script(cli, paths, script, input)?;
            print_skill_run_report(cli.output, &report)
        }
        SkillCommand::Init {
            name,
            description,
            starter_command,
            dry_run,
            overwrite,
        } => {
            let report = init_skill(
                paths,
                name,
                description.as_deref(),
                starter_command.as_deref(),
                *dry_run,
                *overwrite,
            )?;
            print_skill_init_report(cli.output, &report)
        }
    }
}

fn validate_visible_skills(cli: &Cli, paths: &VaultPaths) -> Result<SkillValidateReport, CliError> {
    let skills = visible_skills(cli, paths)?;
    let allowed_packs = crate::custom_tool_registry_options()
        .allowed_pack_names
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut command_count = 0;
    for summary in &skills {
        command_count += summary.commands.len();
        for command in &summary.commands {
            let script = skill_script_path(paths, summary, command)?;
            if !script.exists() {
                errors.push(format!(
                    "{}:{} script does not exist: {}",
                    summary.name, command.id, command.script
                ));
            }
            if let Some(profile) = command.permission_profile.as_deref() {
                if resolve_permission_profile(paths, Some(profile)).is_err() {
                    errors.push(format!(
                        "{}:{} references unknown permission profile `{profile}`",
                        summary.name, command.id
                    ));
                }
            }
            for pack in &command.packs {
                if !allowed_packs.contains(pack) {
                    errors.push(format!(
                        "{}:{} references unknown tool pack `{pack}`",
                        summary.name, command.id
                    ));
                }
            }
            if command.expose {
                if command.output_schema.is_none() {
                    warnings.push(format!(
                        "{}:{} is exposed but does not declare an output_schema",
                        summary.name, command.id
                    ));
                }
                if command
                    .cli
                    .as_ref()
                    .is_none_or(|cli| cli.aliases.is_empty())
                {
                    warnings.push(format!(
                        "{}:{} is exposed but does not declare a CLI alias",
                        summary.name, command.id
                    ));
                }
                if command.examples.is_empty() {
                    warnings.push(format!(
                        "{}:{} is exposed but does not declare examples",
                        summary.name, command.id
                    ));
                }
                if matches!(command.sandbox, Some(JsRuntimeSandbox::Net)) {
                    warnings.push(format!(
                        "{}:{} uses net sandbox; keep exposed network-capable tools narrowly scoped",
                        summary.name, command.id
                    ));
                }
            }
        }
    }
    Ok(SkillValidateReport {
        valid: errors.is_empty(),
        skills: skills.len(),
        commands: command_count,
        warnings,
        errors,
    })
}

fn run_skill_command(
    cli: &Cli,
    paths: &VaultPaths,
    skill_name: &str,
    command_id: &str,
    input: Value,
) -> Result<SkillRunReport, CliError> {
    crate::tools::require_trusted_tool_execution(paths, Some(command_id))?;
    let skill = visible_skill(cli, paths, skill_name)?;
    let command = skill
        .summary
        .commands
        .iter()
        .find(|command| command.id == command_id)
        .ok_or_else(|| {
            CliError::operation(format!(
                "skill `{}` has no command `{command_id}`",
                skill.summary.name
            ))
        })?;
    validate_json_value_against_schema(&input, &command.input_schema).map_err(|error| {
        CliError::operation(format!("skill command input validation failed: {error}"))
    })?;
    let script_path = skill_script_path(paths, &skill.summary, command)?;
    let source = fs::read_to_string(&script_path).map_err(CliError::operation)?;
    let invocation =
        build_skill_invocation_source(&skill, command, &input, strip_shebang_line(&source))?;
    let permission_profile = command
        .permission_profile
        .clone()
        .or_else(|| cli.permissions.clone());
    let evaluation = evaluate_dataview_js_with_options(
        paths,
        &invocation,
        script_path
            .strip_prefix(paths.vault_root())
            .ok()
            .and_then(Path::to_str),
        DataviewJsEvalOptions {
            sandbox: Some(command.sandbox.unwrap_or(JsRuntimeSandbox::Strict)),
            permission_profile,
            tool_registry: Some(crate::tools::runtime_tool_registry(
                paths,
                cli.permissions.as_deref(),
                "skill",
            )),
            ..DataviewJsEvalOptions::default()
        },
    )
    .map_err(CliError::operation)?;
    let result = evaluation
        .value
        .ok_or_else(|| CliError::operation("skill command did not return a JSON value"))?;
    if let Some(output_schema) = &command.output_schema {
        validate_json_value_against_schema(&result, output_schema).map_err(|error| {
            CliError::operation(format!("skill command output validation failed: {error}"))
        })?;
    }
    Ok(SkillRunReport {
        skill: skill.summary.name,
        tool: command.id.clone(),
        script: command.script.clone(),
        input,
        result,
    })
}

fn run_skill_command_script(
    cli: &Cli,
    paths: &VaultPaths,
    script: &Path,
    input: Value,
) -> Result<SkillRunReport, CliError> {
    let script_path = normalize_script_path(script)?;
    match resolve_skill_command_for_script(cli, paths, &script_path) {
        Ok((skill, command)) => {
            run_skill_command(cli, paths, &skill.summary.name, &command.id, input)
        }
        Err(error) => {
            let Some(inferred_paths) = infer_skill_script_vault(paths, &script_path) else {
                return Err(error);
            };
            if inferred_paths.vault_root() == paths.vault_root() {
                return Err(error);
            }
            let (skill, command) =
                resolve_skill_command_for_script(cli, &inferred_paths, &script_path)
                    .map_err(|_| error)?;
            run_skill_command(
                cli,
                &inferred_paths,
                &skill.summary.name,
                &command.id,
                input,
            )
        }
    }
}

fn normalize_script_path(script: &Path) -> Result<PathBuf, CliError> {
    let expanded = expand_home_path(script).unwrap_or_else(|| script.to_path_buf());
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .map_err(CliError::operation)?
            .join(expanded)
    };
    absolute
        .canonicalize()
        .map_err(|error| CliError::operation(format!("script not found: {error}")))
}

fn resolve_skill_command_for_script(
    cli: &Cli,
    paths: &VaultPaths,
    script_path: &Path,
) -> Result<(AssistantSkill, AssistantSkillCommandSummary), CliError> {
    for summary in visible_skills(cli, paths)? {
        let skill = visible_skill(cli, paths, &summary.name)?;
        for command in skill.summary.commands.clone() {
            let candidate = skill_script_path(paths, &skill.summary, &command)?;
            let Ok(candidate) = candidate.canonicalize() else {
                continue;
            };
            if candidate == script_path {
                return Ok((skill, command));
            }
        }
    }
    Err(CliError::operation(format!(
        "script `{}` is not declared by a visible skill command in this vault",
        script_path.display()
    )))
}

fn infer_skill_script_vault(current_paths: &VaultPaths, script_path: &Path) -> Option<VaultPaths> {
    for candidate_root in script_path.ancestors().skip(1) {
        let candidate_paths = VaultPaths::new(candidate_root);
        let config = load_vault_config(&candidate_paths).config;
        let skills_root = candidate_root.join(config.assistant.skills_folder);
        if script_path.starts_with(&skills_root) {
            return Some(candidate_paths);
        }
    }
    if script_path.starts_with(current_paths.vault_root()) {
        Some(current_paths.clone())
    } else {
        None
    }
}

fn build_skill_invocation_source(
    skill: &AssistantSkill,
    command: &AssistantSkillCommandSummary,
    input: &Value,
    source: &str,
) -> Result<String, CliError> {
    let input = serde_json::to_string(input).map_err(CliError::operation)?;
    let context = serde_json::to_string(&serde_json::json!({
        "skill": {
            "name": skill.summary.name,
            "path": skill.summary.path,
            "description": skill.summary.description,
        },
        "command": command,
    }))
    .map_err(CliError::operation)?;
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

fn read_skill_input(
    input_json: Option<&str>,
    input_file: Option<&Path>,
    input_args: &[String],
    input_json_args: &[String],
    input_file_args: &[String],
    input_json_file_args: &[String],
) -> Result<Value, CliError> {
    validate_single_stdin_field_arg(input_file_args, input_json_file_args)?;
    let input = match (input_json, input_file) {
        (None, None) => Ok(serde_json::json!({})),
        (Some(input_json), None) => serde_json::from_str(input_json).map_err(CliError::operation),
        (None, Some(input_file)) => {
            let source = fs::read_to_string(input_file).map_err(CliError::operation)?;
            serde_json::from_str(&source).map_err(CliError::operation)
        }
        (Some(_), Some(_)) => Err(CliError::operation(
            "skill input accepts either --input-json or --input-file, not both",
        )),
    }?;
    merge_skill_input_args(
        input,
        input_args,
        input_json_args,
        input_file_args,
        input_json_file_args,
    )
}

fn read_skill_input_or_stdin(
    input_json: Option<&str>,
    input_file: Option<&Path>,
    input_args: &[String],
    input_json_args: &[String],
    input_file_args: &[String],
    input_json_file_args: &[String],
) -> Result<Value, CliError> {
    validate_single_stdin_field_arg(input_file_args, input_json_file_args)?;
    let field_args_read_stdin =
        skill_input_field_args_read_stdin(input_file_args, input_json_file_args)?;
    if input_json.is_some() || input_file.is_some() || io::stdin().is_terminal() {
        return read_skill_input(
            input_json,
            input_file,
            input_args,
            input_json_args,
            input_file_args,
            input_json_file_args,
        );
    }
    if field_args_read_stdin {
        return merge_skill_input_args(
            serde_json::json!({}),
            input_args,
            input_json_args,
            input_file_args,
            input_json_file_args,
        );
    }
    let mut source = String::new();
    io::stdin()
        .read_to_string(&mut source)
        .map_err(CliError::operation)?;
    let input = if source.trim().is_empty() {
        Ok(serde_json::json!({}))
    } else {
        serde_json::from_str(&source).map_err(CliError::operation)
    }?;
    merge_skill_input_args(
        input,
        input_args,
        input_json_args,
        input_file_args,
        input_json_file_args,
    )
}

fn merge_skill_input_args(
    input: Value,
    input_args: &[String],
    input_json_args: &[String],
    input_file_args: &[String],
    input_json_file_args: &[String],
) -> Result<Value, CliError> {
    if input_args.is_empty()
        && input_json_args.is_empty()
        && input_file_args.is_empty()
        && input_json_file_args.is_empty()
    {
        return Ok(input);
    }
    let mut object = input.as_object().cloned().ok_or_else(|| {
        CliError::operation("skill input field arguments require an object input")
    })?;
    for arg in input_args {
        let (key, value) = parse_skill_input_assignment(arg, "--arg")?;
        object.insert(key.to_string(), Value::String(value.to_string()));
    }
    for arg in input_json_args {
        let (key, value) = parse_skill_input_assignment(arg, "--arg-json")?;
        let value = serde_json::from_str(value).map_err(|error| {
            CliError::operation(format!("invalid JSON for --arg-json `{key}`: {error}"))
        })?;
        object.insert(key.to_string(), value);
    }
    for arg in input_file_args {
        let (key, path) = parse_skill_input_assignment(arg, "--arg-file")?;
        let value = read_skill_input_field_source(path, "--arg-file", key)?;
        object.insert(key.to_string(), Value::String(value));
    }
    for arg in input_json_file_args {
        let (key, path) = parse_skill_input_assignment(arg, "--arg-json-file")?;
        let source = read_skill_input_field_source(path, "--arg-json-file", key)?;
        let value = serde_json::from_str(&source).map_err(|error| {
            CliError::operation(format!(
                "invalid JSON for --arg-json-file `{key}` from `{path}`: {error}"
            ))
        })?;
        object.insert(key.to_string(), value);
    }
    Ok(Value::Object(object))
}

fn validate_single_stdin_field_arg(
    input_file_args: &[String],
    input_json_file_args: &[String],
) -> Result<(), CliError> {
    let stdin_count = input_file_args
        .iter()
        .chain(input_json_file_args.iter())
        .filter_map(|arg| parse_skill_input_assignment(arg, "skill input field argument").ok())
        .filter(|(_, path)| *path == "-")
        .count();
    if stdin_count > 1 {
        return Err(CliError::operation(
            "only one skill input field can read from stdin",
        ));
    }
    Ok(())
}

fn skill_input_field_args_read_stdin(
    input_file_args: &[String],
    input_json_file_args: &[String],
) -> Result<bool, CliError> {
    for arg in input_file_args {
        let (_, path) = parse_skill_input_assignment(arg, "--arg-file")?;
        if path == "-" {
            return Ok(true);
        }
    }
    for arg in input_json_file_args {
        let (_, path) = parse_skill_input_assignment(arg, "--arg-json-file")?;
        if path == "-" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_skill_input_field_source(
    path: &str,
    flag_name: &str,
    key: &str,
) -> Result<String, CliError> {
    let mut source = String::new();
    if path == "-" {
        io::stdin()
            .read_to_string(&mut source)
            .map_err(CliError::operation)?;
        return Ok(source);
    }
    fs::read_to_string(path).map_err(|error| {
        CliError::operation(format!(
            "failed to read {flag_name} `{key}` from `{path}`: {error}"
        ))
    })
}

fn parse_skill_input_assignment<'a>(
    value: &'a str,
    flag_name: &str,
) -> Result<(&'a str, &'a str), CliError> {
    let Some((key, value)) = value.split_once('=') else {
        return Err(CliError::operation(format!(
            "{flag_name} expects KEY=VALUE"
        )));
    };
    if key.is_empty()
        || key.chars().any(char::is_control)
        || key.contains('.')
        || key.contains('/')
        || key.contains('\\')
    {
        return Err(CliError::operation(format!(
            "{flag_name} key must be a non-empty top-level JSON field name"
        )));
    }
    Ok((key, value))
}

fn init_skill(
    paths: &VaultPaths,
    name: &str,
    description: Option<&str>,
    starter_command: Option<&str>,
    dry_run: bool,
    overwrite: bool,
) -> Result<SkillInitReport, CliError> {
    let name = normalize_skill_name(name)?;
    let config = load_vault_config(paths).config;
    let skill_root = paths
        .vault_root()
        .join(config.assistant.skills_folder)
        .join(&name);
    let manifest_path = skill_root.join("SKILL.md");
    let script_path = starter_command.map(|command| {
        skill_root.join("scripts").join(format!(
            "{}.js",
            normalize_skill_name(command).unwrap_or_else(|_| "main".to_string())
        ))
    });
    if !overwrite
        && (manifest_path.exists() || script_path.as_ref().is_some_and(|path| path.exists()))
    {
        return Err(CliError::operation(format!(
            "skill `{name}` already exists; rerun with --overwrite to replace the scaffold"
        )));
    }
    let description = description.unwrap_or("TODO: describe this skill.");
    let manifest = render_skill_manifest(&name, description, starter_command);
    let script = format!(
        "{SKILL_COMMAND_SCRIPT_SHEBANG}function main(input, ctx) {{\n  return {{ input, skill: ctx.skill.name, command: ctx.command.id }};\n}}\n"
    );
    if !dry_run {
        fs::create_dir_all(&skill_root).map_err(CliError::operation)?;
        fs::write(&manifest_path, manifest).map_err(CliError::operation)?;
        if let Some(script_path) = &script_path {
            if let Some(parent) = script_path.parent() {
                fs::create_dir_all(parent).map_err(CliError::operation)?;
            }
            write_executable_script(script_path, &script)?;
        }
        load_assistant_skill(paths, &name).map_err(CliError::operation)?;
    }
    Ok(SkillInitReport {
        name,
        dry_run,
        skill_root: relative_to_vault(paths, &skill_root)?,
        manifest_path: relative_to_vault(paths, &manifest_path)?,
        script_path: script_path
            .as_ref()
            .map(|path| relative_to_vault(paths, path))
            .transpose()?,
        operations: vec!["create skill scaffold".to_string()],
    })
}

fn write_executable_script(path: &Path, contents: &str) -> Result<(), CliError> {
    fs::write(path, contents).map_err(CliError::operation)?;
    #[cfg(unix)]
    set_executable_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_executable_permissions(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path).map_err(CliError::operation)?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(path, permissions).map_err(CliError::operation)
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

fn render_skill_manifest(name: &str, description: &str, starter_command: Option<&str>) -> String {
    let quoted_name = serde_json::to_string(name).unwrap_or_else(|_| format!("\"{name}\""));
    let quoted_description =
        serde_json::to_string(description).unwrap_or_else(|_| "\"TODO\"".to_string());
    let commands = starter_command.map_or(String::new(), |command| {
        let command = normalize_skill_name(command).unwrap_or_else(|_| "main".to_string());
        format!(
            "metadata:\n  vulcan:\n    commands:\n      - id: {command}\n        script: scripts/{command}.js\n        sandbox: strict\n        packs: [custom]\n        expose: true\n        input_schema:\n          type: object\n"
        )
    });
    format!(
        "---\nname: {quoted_name}\ndescription: {quoted_description}\nlicense: UNLICENSED\ncompatibility:\n  - vulcan\nallowed-tools: []\n{commands}---\n\n# {name}\n\n{description}\n"
    )
}

fn normalize_skill_name(value: &str) -> Result<String, CliError> {
    let normalized = value.trim().replace('_', "-");
    if normalized.is_empty()
        || !normalized
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err(CliError::operation(format!("invalid skill name `{value}`")));
    }
    Ok(normalized)
}

fn expand_home_path(path: &Path) -> Option<PathBuf> {
    let path_str = path.to_str()?;
    if path_str == "~" {
        return std::env::var_os("HOME").map(PathBuf::from);
    }
    path_str
        .strip_prefix("~/")
        .and_then(|rest| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(rest)))
}

fn skill_script_path(
    paths: &VaultPaths,
    skill: &AssistantSkillSummary,
    command: &AssistantSkillCommandSummary,
) -> Result<PathBuf, CliError> {
    let config = load_vault_config(paths).config;
    Ok(paths
        .vault_root()
        .join(config.assistant.skills_folder)
        .join(&skill.path)
        .parent()
        .ok_or_else(|| CliError::operation("invalid skill path"))?
        .join(&command.script))
}

fn relative_to_vault(paths: &VaultPaths, path: &Path) -> Result<String, CliError> {
    path.strip_prefix(paths.vault_root())
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .map_err(CliError::operation)
}

fn skill_command_rows(skill: &AssistantSkill) -> Vec<SkillCommandRow> {
    skill
        .summary
        .commands
        .iter()
        .cloned()
        .map(|command| SkillCommandRow {
            skill: skill.summary.name.clone(),
            skill_path: skill.summary.path.clone(),
            command,
        })
        .collect()
}

fn visible_skills(cli: &Cli, paths: &VaultPaths) -> Result<Vec<AssistantSkillSummary>, CliError> {
    let guard = selected_permission_guard(cli, paths)?;
    let skills = list_assistant_skills(paths).map_err(CliError::operation)?;
    Ok(skills
        .into_iter()
        .filter(|skill| {
            guard
                .check_read_path(&skill_relative_path(paths, skill))
                .is_ok()
        })
        .collect())
}

fn visible_skill(cli: &Cli, paths: &VaultPaths, name: &str) -> Result<AssistantSkill, CliError> {
    let guard = selected_permission_guard(cli, paths)?;
    let skill = load_assistant_skill(paths, name).map_err(CliError::operation)?;
    let relative_path = skill_relative_path(paths, &skill.summary);
    guard
        .check_read_path(&relative_path)
        .map_err(CliError::operation)?;
    Ok(skill)
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

fn print_skill_list_report(output: OutputFormat, report: &SkillListReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.skills.is_empty() {
                println!("No visible skills.");
                return Ok(());
            }
            for skill in &report.skills {
                let title = skill.title.as_deref().unwrap_or(&skill.name);
                match skill.description.as_deref() {
                    Some(description) => {
                        println!("- {} [{}] — {}", skill.name, skill.path, description);
                    }
                    None => println!("- {} [{}]", title, skill.path),
                }
            }
            Ok(())
        }
    }
}

fn print_skill_commands_report(
    output: OutputFormat,
    report: &SkillCommandsReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            if report.commands.is_empty() {
                println!("No skill commands.");
                return Ok(());
            }
            for row in &report.commands {
                println!(
                    "- {}:{} -> {}",
                    row.skill, row.command.id, row.command.script
                );
            }
            Ok(())
        }
    }
}

fn print_skill_validate_report(
    output: OutputFormat,
    report: &SkillValidateReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "Valid skills: {} ({} skills, {} commands)",
                report.valid, report.skills, report.commands
            );
            for warning in &report.warnings {
                println!("warning: {warning}");
            }
            for error in &report.errors {
                println!("error: {error}");
            }
            Ok(())
        }
    }
}

fn print_skill_run_report(output: OutputFormat, report: &SkillRunReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            let display_value = redact_sensitive_json(&report.result);
            println!(
                "{}",
                serde_json::to_string_pretty(&display_value).map_err(CliError::operation)?
            );
            Ok(())
        }
    }
}

fn redact_sensitive_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let redacted = if is_sensitive_json_key(key) {
                        Value::String("[redacted]".to_string())
                    } else {
                        redact_sensitive_json(value)
                    };
                    (key.clone(), redacted)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_sensitive_json).collect()),
        Value::String(text) if text.trim_start().starts_with("Bearer ") => {
            Value::String("[redacted]".to_string())
        }
        _ => value.clone(),
    }
}

fn is_sensitive_json_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    [
        "secret",
        "token",
        "password",
        "apikey",
        "authorization",
        "credential",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn print_skill_init_report(output: OutputFormat, report: &SkillInitReport) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            let mode = if report.dry_run {
                "Would create"
            } else {
                "Created"
            };
            println!("{mode} skill {}", report.name);
            println!("Manifest: {}", report.manifest_path);
            if let Some(script_path) = report.script_path.as_deref() {
                println!("Script: {script_path}");
            }
            Ok(())
        }
    }
}

fn print_skill_report(output: OutputFormat, skill: &AssistantSkill) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(skill),
        OutputFormat::Human | OutputFormat::Markdown => {
            println!(
                "{}",
                skill
                    .summary
                    .title
                    .as_deref()
                    .unwrap_or(&skill.summary.name)
            );
            println!("Path: {}", skill.summary.path);
            if let Some(description) = skill.summary.description.as_deref() {
                println!("Description: {description}");
            }
            if !skill.summary.tools.is_empty() {
                println!("Tools: {}", skill.summary.tools.join(", "));
            }
            if let Some(output_file) = skill.summary.output_file.as_deref() {
                println!("Output file: {output_file}");
            }
            if !skill.summary.tags.is_empty() {
                println!("Tags: {}", skill.summary.tags.join(", "));
            }
            if !skill.body.is_empty() {
                println!();
                println!("{}", skill.body);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::redact_sensitive_json;

    #[test]
    fn redacts_sensitive_skill_output_fields_for_human_display() {
        let value = serde_json::json!({
            "access_token": "secret-token",
            "nested": {
                "apiKey": "secret-key",
                "safe": "value",
                "auth": "Bearer abc.def"
            },
            "items": [
                { "password": "hunter2" },
                "Bearer xyz"
            ]
        });

        assert_eq!(
            redact_sensitive_json(&value),
            serde_json::json!({
                "access_token": "[redacted]",
                "nested": {
                    "apiKey": "[redacted]",
                    "safe": "value",
                    "auth": "[redacted]"
                },
                "items": [
                    { "password": "[redacted]" },
                    "[redacted]"
                ]
            })
        );
    }
}
