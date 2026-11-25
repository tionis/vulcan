use crate::mdbase::mdbase_js_mutation_committer;
use crate::{trust, AppError};
use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use vulcan_core::Verbosity;
use vulcan_core::{
    evaluate_dataview_js_with_options, list_assistant_skills, load_assistant_skill,
    load_vault_config, resolve_permission_profile, validate_json_value_against_schema,
    AssistantSkill, AssistantSkillCommandSummary, AssistantSkillSummary, AssistantTool,
    AssistantToolRuntime, AssistantToolSummary, DataviewJsEvalOptions, DataviewJsToolDefinition,
    DataviewJsToolDescriptor, DataviewJsToolRegistry, JsRuntimeSandbox, VaultPaths,
};

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

pub fn list_custom_tools(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
) -> Result<Vec<CustomToolDescriptor>, AppError> {
    let mut descriptors = skill_command_tool_descriptors(paths, active_permission_profile)?;
    descriptors.sort_by(|left, right| left.summary.name.cmp(&right.summary.name));
    Ok(descriptors)
}

pub fn show_custom_tool(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    name: &str,
) -> Result<CustomToolShowReport, AppError> {
    let tool = skill_command_tool(paths, name).map_err(AppError::operation)?;
    Ok(CustomToolShowReport {
        callable: custom_tool_is_callable(
            paths,
            active_permission_profile,
            &tool.summary.name,
            tool.summary.permission_profile.as_deref(),
            tool.summary.sandbox,
        ),
        tool,
    })
}

#[must_use]
pub fn build_custom_tool_js_registry(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    surface: &str,
) -> Arc<dyn DataviewJsToolRegistry> {
    build_custom_tool_js_registry_with_context(
        paths,
        CustomToolJsRegistryContext::root(surface, active_permission_profile),
    )
}

fn build_custom_tool_js_registry_with_context(
    paths: &VaultPaths,
    context: CustomToolJsRegistryContext,
) -> Arc<dyn DataviewJsToolRegistry> {
    Arc::new(JsCustomToolRegistry {
        paths: paths.clone(),
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

pub fn run_custom_tool(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    name: &str,
    input: &Value,
    run_options: &CustomToolRunOptions,
) -> Result<CustomToolRunReport, AppError> {
    let context =
        CustomToolJsRegistryContext::root(&run_options.surface, active_permission_profile);
    run_custom_tool_with_context(paths, &context, name, input, run_options)
}

fn run_custom_tool_with_context(
    paths: &VaultPaths,
    context: &CustomToolJsRegistryContext,
    name: &str,
    input: &Value,
    run_options: &CustomToolRunOptions,
) -> Result<CustomToolRunReport, AppError> {
    let Some(trust_root) =
        tool_trust_root(paths, context.active_permission_profile.as_deref(), name)
    else {
        let in_trusted_changeset = crate::changesets::changeset_vault(paths.vault_root())
            .is_some_and(|vault| trust::is_trusted(&vault));
        if in_trusted_changeset {
            return Err(AppError::operation(format!(
                "tool `{name}` differs from its vault's copy in this changeset (its skill or permission profile changed), so it does not run with the vault's trust here; land or abandon the change first"
            )));
        }
        // Not trusted at all (a trusted vault is its own trust root).
        require_trusted_tool_execution(paths, Some(name))?;
        return Err(AppError::operation(format!(
            "tool `{name}` cannot run with trust here"
        )));
    };
    run_skill_command_tool_with_context(paths, &trust_root, context, name, input, run_options)
}

/// The vault whose trust and device grants a skill command runs under: the
/// vault itself when it is trusted, or, inside a changeset worktree, the
/// changeset's trusted vault when the command is the vault's own. That is,
/// the skill directory is byte-identical and the command's effective
/// permission profile grants no more here than in the vault, so a session
/// cannot edit a tool or its profile in its changeset and run it with the
/// vault's trust.
fn tool_trust_root(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    name: &str,
) -> Option<VaultPaths> {
    if trust::is_trusted(paths.vault_root()) {
        return Some(paths.clone());
    }
    let vault = VaultPaths::new(crate::changesets::changeset_vault(paths.vault_root())?);
    if !trust::is_trusted(vault.vault_root()) {
        return None;
    }
    let (skill, command) = resolve_skill_command_tool(paths, name).ok()?;
    let (vault_skill, vault_command) = resolve_skill_command_tool(&vault, name).ok()?;
    let here = crate::tool_grants::skill_digest(&skill_directory(paths, &skill.summary).ok()?);
    let there =
        crate::tool_grants::skill_digest(&skill_directory(&vault, &vault_skill.summary).ok()?);
    if here.ok()? != there.ok()? {
        return None;
    }
    let profile = effective_tool_permission_profile(
        paths,
        active_permission_profile,
        name,
        command.permission_profile.as_deref(),
    )
    .ok()?;
    let vault_profile = effective_tool_permission_profile(
        &vault,
        active_permission_profile,
        name,
        vault_command.permission_profile.as_deref(),
    )
    .ok()?;
    if profile != vault_profile {
        return None;
    }
    if let Some(profile) = profile.as_deref() {
        let here = resolve_permission_profile(paths, Some(profile)).ok()?;
        let there = resolve_permission_profile(&vault, Some(profile)).ok()?;
        if !here.grant.is_subset_of(&there.grant) {
            return None;
        }
    }
    Some(vault)
}

fn skill_command_tool_descriptors(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
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
                .map(move |command| CustomToolDescriptor {
                    callable: custom_tool_is_callable(
                        paths,
                        active_permission_profile,
                        &skill_command_tool_name(&skill.name, &command.id),
                        command.permission_profile.as_deref(),
                        command.sandbox.unwrap_or(JsRuntimeSandbox::Strict),
                    ),
                    summary: skill_command_tool_summary(paths, &skill, &command),
                })
                .collect::<Vec<_>>()
        })
        .collect())
}

fn skill_command_tool(paths: &VaultPaths, name: &str) -> Result<AssistantTool, AppError> {
    let (_resolved_name, skill, command) = resolve_skill_command_tool_identifier(paths, name)?;
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
    trust_root: &VaultPaths,
    context: &CustomToolJsRegistryContext,
    name: &str,
    input: &Value,
    run_options: &CustomToolRunOptions,
) -> Result<CustomToolRunReport, AppError> {
    let _ = run_options;
    let (skill, command) = resolve_skill_command_tool(paths, name)?;
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
    let tool_name = skill_command_tool_name(&skill.summary.name, &command.id);
    let ungranted = !command.host_commands.is_empty();
    // Grants belong to the trusted vault; the digest is of the skill as it
    // runs here, which `tool_trust_root` found identical to the vault's.
    let granted_host_commands = crate::tool_grants::active_host_commands(
        trust_root,
        &tool_name,
        &skill_directory(paths, &skill.summary)?,
        &command.host_commands,
    );
    let ungranted_now = ungranted && granted_host_commands.is_empty();
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
            permission_profile: effective_permission_profile.clone(),
            mutation_committer: Some(mdbase_js_mutation_committer(
                paths,
                effective_permission_profile.as_deref(),
                Verbosity::Quiet,
            )),
            tool_registry: Some(build_custom_tool_js_registry_with_context(
                paths,
                runtime_context,
            )),
            granted_host_commands: granted_host_commands.clone(),
            ..DataviewJsEvalOptions::default()
        },
    )
    .map_err(|error| {
        let message = error.to_string();
        // Say how to grant declared host commands rather than leave the
        // caller with the sandbox rule they cannot change.
        if ungranted_now && message.contains("host.exec()") {
            AppError::operation(format!(
                "{message}\n`{tool_name}` declares host commands that are not granted on this device (or its skill changed since); review the skill and run `vulcan tool grant {tool_name}`"
            ))
        } else {
            AppError::operation(message)
        }
    })?;
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
) -> Result<(AssistantSkill, AssistantSkillCommandSummary), AppError> {
    resolve_skill_command_tool_identifier(paths, name)
        .map(|(_resolved_name, skill, command)| (skill, command))
}

pub(crate) fn resolve_skill_command_tool_identifier(
    paths: &VaultPaths,
    name: &str,
) -> Result<(String, AssistantSkill, AssistantSkillCommandSummary), AppError> {
    let mut alias_matches = Vec::new();
    for summary in list_assistant_skills(paths).map_err(AppError::operation)? {
        for command in summary.commands.iter().filter(|command| command.expose) {
            let tool_name = skill_command_tool_name(&summary.name, &command.id);
            if tool_name == name {
                let skill =
                    load_assistant_skill(paths, &summary.name).map_err(AppError::operation)?;
                return Ok((tool_name, skill, command.clone()));
            }
            if command
                .cli
                .as_ref()
                .is_some_and(|cli| cli.aliases.iter().any(|alias| alias == name))
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

/// The directory holding `skill`'s `SKILL.md` and scripts.
fn skill_directory(paths: &VaultPaths, skill: &AssistantSkillSummary) -> Result<PathBuf, AppError> {
    let config = load_vault_config(paths).config;
    paths
        .vault_root()
        .join(config.assistant.skills_folder)
        .join(&skill.path)
        .parent()
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| AppError::operation("invalid skill path"))
}

/// A skill command tool's resolved name, skill directory, and declared host
/// commands, for granting them on this device.
pub(crate) fn skill_command_host_commands(
    paths: &VaultPaths,
    name: &str,
) -> Result<(String, PathBuf, Vec<Vec<String>>), AppError> {
    let (tool_name, skill, command) = resolve_skill_command_tool_identifier(paths, name)?;
    Ok((
        tool_name,
        skill_directory(paths, &skill.summary)?,
        command.host_commands,
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

pub(crate) fn skill_command_tool_name(skill_name: &str, command_id: &str) -> String {
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

impl DataviewJsToolRegistry for JsCustomToolRegistry {
    fn list(&self) -> Result<Vec<DataviewJsToolDescriptor>, String> {
        list_custom_tools(
            &self.paths,
            self.context.active_permission_profile.as_deref(),
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

/// A trusted vault's tool is callable when its permission profile fits the
/// caller's. Sandboxed tools (`strict`, `fs`, `net`) run under that profile
/// and its JavaScript limits like any other vault access; only a `none`
/// sandbox tool, which can start host processes and has no runtime limits,
/// also needs the profile's `execute` grant.
fn custom_tool_is_callable(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    tool_name: &str,
    requested_permission_profile: Option<&str>,
    sandbox: JsRuntimeSandbox,
) -> bool {
    if tool_trust_root(paths, active_permission_profile, tool_name).is_none() {
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
            .is_ok_and(|selection| sandbox != JsRuntimeSandbox::None || selection.grant.execute),
        None => true,
    }
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
