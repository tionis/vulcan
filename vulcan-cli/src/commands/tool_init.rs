use crate::output::print_json;
use crate::tools;
use crate::{CliError, OutputFormat, ToolInitTemplateArg};
use serde::Serialize;
use std::fs;
use std::path::Path;
use vulcan_core::{load_vault_config, VaultPaths};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ToolInitReport {
    name: String,
    skill: String,
    command: String,
    template: String,
    dry_run: bool,
    skill_root: String,
    manifest_path: String,
    script_path: String,
    operations: Vec<String>,
}

pub(crate) struct ToolInitCliOptions<'a> {
    pub(crate) name: &'a str,
    pub(crate) description: Option<&'a str>,
    pub(crate) command: &'a str,
    pub(crate) template: ToolInitTemplateArg,
    pub(crate) dry_run: bool,
    pub(crate) overwrite: bool,
}

pub(crate) fn init_skill_backed_tool(
    paths: &VaultPaths,
    options: &ToolInitCliOptions<'_>,
    registry_options: &tools::CustomToolRegistryOptions,
) -> Result<ToolInitReport, CliError> {
    let tool_alias = normalize_skill_backed_tool_name(options.name)?;
    if registry_options
        .reserved_names
        .iter()
        .any(|reserved| reserved == &tool_alias)
    {
        return Err(CliError::operation(format!(
            "`{tool_alias}` is reserved by the built-in CLI surface"
        )));
    }
    if tools::resolve_custom_tool_cli_name(paths, &tool_alias, registry_options).is_ok()
        && !options.overwrite
    {
        return Err(CliError::operation(format!(
            "tool `{tool_alias}` already exists; rerun with --overwrite to replace the scaffold"
        )));
    }

    let command = normalize_skill_backed_tool_name(options.command)?;
    let config = load_vault_config(paths).config;
    let skill_root = paths
        .vault_root()
        .join(config.assistant.skills_folder)
        .join(&tool_alias);
    let manifest_path = skill_root.join("SKILL.md");
    let script_path = skill_root.join("scripts").join(format!("{command}.js"));
    if !options.overwrite && (manifest_path.exists() || script_path.exists()) {
        return Err(CliError::operation(format!(
            "tool `{tool_alias}` already exists; rerun with --overwrite to replace the scaffold"
        )));
    }

    let description = options.description.unwrap_or("TODO: describe this tool.");
    let manifest =
        render_skill_backed_tool_manifest(&tool_alias, &command, description, options.template)?;
    let script = render_skill_backed_tool_script(options.template);
    if !options.dry_run {
        fs::create_dir_all(skill_root.join("scripts")).map_err(CliError::operation)?;
        fs::write(&manifest_path, manifest).map_err(CliError::operation)?;
        write_executable_vulcan_tool_script(&script_path, &script)?;
        tools::show_custom_tool(paths, None, &tool_alias, registry_options)?;
    }

    Ok(ToolInitReport {
        name: tool_alias.clone(),
        skill: tool_alias,
        command,
        template: tool_init_template_name(options.template).to_string(),
        dry_run: options.dry_run,
        skill_root: relative_path_from_vault(paths, &skill_root)?,
        manifest_path: relative_path_from_vault(paths, &manifest_path)?,
        script_path: relative_path_from_vault(paths, &script_path)?,
        operations: vec!["create skill-backed custom tool scaffold".to_string()],
    })
}

pub(crate) fn print_tool_init_report(
    output: OutputFormat,
    report: &ToolInitReport,
) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(report),
        OutputFormat::Human | OutputFormat::Markdown => {
            let mode = if report.dry_run {
                "Would create"
            } else {
                "Created"
            };
            println!("{mode} custom tool {}", report.name);
            println!("Template: {}", report.template);
            println!("Skill: {}", report.skill_root);
            println!("Manifest: {}", report.manifest_path);
            println!("Script: {}", report.script_path);
            Ok(())
        }
    }
}

fn render_skill_backed_tool_manifest(
    tool_alias: &str,
    command: &str,
    description: &str,
    template: ToolInitTemplateArg,
) -> Result<String, CliError> {
    let quoted_name = serde_json::to_string(tool_alias).map_err(CliError::operation)?;
    let quoted_description = serde_json::to_string(description).map_err(CliError::operation)?;
    let quoted_alias = serde_json::to_string(tool_alias).map_err(CliError::operation)?;
    let template_spec = tool_init_template_spec(template);
    Ok(format!(
        r"---
name: {quoted_name}
description: {quoted_description}
license: UNLICENSED
compatibility:
  - vulcan
allowed-tools: []
metadata:
  vulcan:
    commands:
      - id: {command}
        script: scripts/{command}.js
        sandbox: {sandbox}
        packs: [custom]
        expose: true
        input_schema:
{input_schema}
        output_schema:
{output_schema}
        cli:
          aliases: [{quoted_alias}]
          args:
{cli_args}
        examples:
{examples}
---

# {tool_alias}

{description}
",
        sandbox = template_spec.sandbox,
        input_schema = indent_block(template_spec.input_schema, 10),
        output_schema = indent_block(template_spec.output_schema, 10),
        cli_args = indent_block(template_spec.cli_args, 12),
        examples = indent_block(template_spec.examples, 10),
    ))
}

struct ToolInitTemplateSpec {
    sandbox: &'static str,
    input_schema: &'static str,
    output_schema: &'static str,
    cli_args: &'static str,
    examples: &'static str,
    script: &'static str,
}

fn tool_init_template_name(template: ToolInitTemplateArg) -> &'static str {
    match template {
        ToolInitTemplateArg::Minimal => "minimal",
        ToolInitTemplateArg::Reader => "reader",
        ToolInitTemplateArg::Mutation => "mutation",
        ToolInitTemplateArg::Exporter => "exporter",
        ToolInitTemplateArg::Wrapper => "wrapper",
    }
}

#[allow(clippy::too_many_lines)]
fn tool_init_template_spec(template: ToolInitTemplateArg) -> ToolInitTemplateSpec {
    match template {
        ToolInitTemplateArg::Minimal => ToolInitTemplateSpec {
            sandbox: "strict",
            input_schema: r"type: object
properties:
  message:
    type: string
    description: Input text for the tool.
required: [message]",
            output_schema: r"type: object
properties:
  message:
    type: string
  length:
    type: integer
required: [message, length]",
            cli_args: r"- flag: message
  field: message
  action: string
  description: Input text for the tool.",
            examples: r"- name: smoke
  cli_args: [--message, hello]
  expected_output:
    message: hello
    length: 5",
            script: "function main(input) {\n  const message = String(input.message ?? \"\");\n  return { message, length: message.length };\n}\n",
        },
        ToolInitTemplateArg::Reader => ToolInitTemplateSpec {
            sandbox: "fs",
            input_schema: r"type: object
properties:
  note:
    type: string
    description: Note path to read.
required: [note]",
            output_schema: r"type: object
properties:
  path:
    type: string
  title:
    type: string
  word_count:
    type: integer
required: [path, title, word_count]",
            cli_args: r"- flag: note
  field: note
  action: string
  completion: note
  description: Note path to read.",
            examples: r"- name: smoke
  input:
    note: Home
  expected_output_file: examples/smoke.expected.json",
            script: "function main() {\n  const input = tool.input();\n  const note = vault.note(input.note);\n  const content = note.content ?? \"\";\n  return tool.result()\n    .summary(`Read ${note.file.path}`)\n    .data({ path: note.file.path, title: note.file.name, word_count: content.trim() ? content.trim().split(/\\s+/).length : 0 })\n    .ok({ path: note.file.path, title: note.file.name, word_count: content.trim() ? content.trim().split(/\\s+/).length : 0 });\n}\n",
        },
        ToolInitTemplateArg::Mutation => ToolInitTemplateSpec {
            sandbox: "fs",
            input_schema: r"type: object
properties:
  note:
    type: string
    description: Note path to append to.
  text:
    type: string
    description: Markdown text to append.
  dry_run:
    type: boolean
    description: Preview changes without writing.
required: [note, text]",
            output_schema: r"type: object
properties:
  ok:
    type: boolean
  dry_run:
    type: boolean
  changed_paths:
    type: array
    items:
      type: string
required: [ok, dry_run, changed_paths]",
            cli_args: r"- flag: note
  field: note
  action: string
  completion: note
  description: Note path to append to.
- flag: text
  field: text
  action: string
  description: Markdown text to append.
- flag: dry-run
  field: dry_run
  action: boolean
  description: Preview changes without writing.",
            examples: r#"- name: dry-run
  cli_args: [--note, Home, --text, "Example", --dry-run]"#,
            script: "function main() {\n  const input = tool.input({ dry_run: true });\n  return vault.plan({ dry_run: input.dry_run })\n    .append(input.note, input.text)\n    .result();\n}\n",
        },
        ToolInitTemplateArg::Exporter => ToolInitTemplateSpec {
            sandbox: "fs",
            input_schema: r"type: object
properties:
  query:
    type: string
    description: Search query to export.
  limit:
    type: integer
    description: Maximum hits.
required: [query]",
            output_schema: r"type: object
properties:
  count:
    type: integer
  results:
    type: array
required: [count, results]",
            cli_args: r"- flag: query
  field: query
  action: string
  description: Search query to export.
- flag: limit
  field: limit
  action: integer
  description: Maximum hits.",
            examples: r#"- name: smoke
  cli_args: [--query, Home, --limit, "5"]"#,
            script: "function main() {\n  const input = tool.input({ limit: 10 });\n  const results = vault.search(String(input.query), { limit: input.limit }).hits ?? [];\n  return { count: results.length, results };\n}\n",
        },
        ToolInitTemplateArg::Wrapper => ToolInitTemplateSpec {
            sandbox: "strict",
            input_schema: r"type: object
properties:
  tool:
    type: string
    description: Tool name to call.
  input:
    type: object
    description: Input object for the nested tool.
required: [tool]",
            output_schema: r"type: object
properties:
  called:
    type: string
  result:
    type: object
required: [called, result]",
            cli_args: r"- flag: tool
  field: tool
  action: string
  completion: custom-tool
  description: Tool name to call.
- flag: input
  field: input
  action: json
  description: Input object for the nested tool.",
            examples: r"- name: smoke
  input:
    tool: skill_example_run
    input: {}",
            script: "function main() {\n  const input = tool.input({ input: {} });\n  const result = tools.callChecked(input.tool, input.input ?? {});\n  return { called: input.tool, result };\n}\n",
        },
    }
}

fn render_skill_backed_tool_script(template: ToolInitTemplateArg) -> String {
    format!(
        "#!/usr/bin/env -S vulcan skill exec\n{}",
        tool_init_template_spec(template).script
    )
}

fn indent_block(value: &str, spaces: usize) -> String {
    let indent = " ".repeat(spaces);
    value
        .lines()
        .map(|line| format!("{indent}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_skill_backed_tool_name(value: &str) -> Result<String, CliError> {
    let normalized = value.trim().replace('_', "-");
    if normalized.is_empty()
        || !normalized
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err(CliError::operation(format!(
            "invalid custom tool name `{value}`"
        )));
    }
    Ok(normalized)
}

fn relative_path_from_vault(paths: &VaultPaths, path: &Path) -> Result<String, CliError> {
    path.strip_prefix(paths.vault_root())
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .map_err(CliError::operation)
}

fn write_executable_vulcan_tool_script(path: &Path, contents: &str) -> Result<(), CliError> {
    fs::write(path, contents).map_err(CliError::operation)?;
    set_vulcan_tool_script_executable(path)
}

#[cfg(unix)]
fn set_vulcan_tool_script_executable(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path).map_err(CliError::operation)?;
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(path, permissions).map_err(CliError::operation)
}

#[cfg(not(unix))]
fn set_vulcan_tool_script_executable(_path: &Path) -> Result<(), CliError> {
    Ok(())
}
