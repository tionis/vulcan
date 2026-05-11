use crate::AppError;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::Component;
use std::path::{Path, PathBuf};
use vulcan_core::{
    load_vault_config, resolve_permission_profile, AssistantToolSummary, JsRuntimeSandbox,
    VaultPaths,
};

use super::typescript::{json_schema_required_fields, top_level_field_name};
use super::{list_custom_tools, show_custom_tool, CustomToolRegistryOptions};

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
