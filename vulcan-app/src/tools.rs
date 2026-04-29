use crate::{trust, AppError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use serde_yaml::Value as YamlValue;
use std::fs;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vulcan_core::{
    assistant_tools_root, default_assistant_tool_reserved_names, evaluate_dataview_js_with_options,
    list_assistant_tool_manifest_paths, list_assistant_tools, load_assistant_tool,
    load_assistant_tool_manifest, resolve_permission_profile, validate_json_value_against_schema,
    AssistantTool, AssistantToolRuntime, AssistantToolSecretSpec, AssistantToolSummary,
    AssistantToolValidationOptions, DataviewJsEvalOptions, DataviewJsToolDefinition,
    DataviewJsToolDescriptor, DataviewJsToolRegistry, JsRuntimeSandbox, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomToolRegistryOptions {
    pub reserved_names: Vec<String>,
    pub allowed_pack_names: Vec<String>,
}

impl Default for CustomToolRegistryOptions {
    fn default() -> Self {
        Self {
            reserved_names: default_assistant_tool_reserved_names(),
            allowed_pack_names: vec!["custom".to_string()],
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
    let tools = list_assistant_tools(paths, &assistant_tool_validation_options(options))
        .map_err(AppError::operation)?;
    Ok(tools
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
        .collect())
}

pub fn show_custom_tool(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    name: &str,
    options: &CustomToolRegistryOptions,
) -> Result<CustomToolShowReport, AppError> {
    let tool = load_assistant_tool(paths, name, &assistant_tool_validation_options(options))
        .map_err(AppError::operation)?;
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
    let tool = load_assistant_tool(
        paths,
        name,
        &assistant_tool_validation_options(registry_options),
    )
    .map_err(AppError::operation)?;
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
        fs::write(&entrypoint_path, ensure_trailing_newline(&source))
            .map_err(AppError::operation)?;
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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
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
    let source = "function main(input, ctx) {\n  return {\n    result: {\n      ok: true,\n      tool: ctx.tool.name,\n      received: input,\n    },\n    text: `ran ${ctx.tool.name}`,\n  };\n}\n"
        .to_string();
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

        let tools = list_custom_tools(&paths, None, &CustomToolRegistryOptions::default())
            .expect("tools should load");
        assert_eq!(tools.len(), 1);
        assert!(!tools[0].callable);
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

        let tools = list_custom_tools(
            &paths,
            Some("blocked"),
            &CustomToolRegistryOptions::default(),
        )
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

        let tools = list_custom_tools(
            &paths,
            Some("readonly"),
            &CustomToolRegistryOptions::default(),
        )
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
            &CustomToolRegistryOptions::default(),
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
            &CustomToolRegistryOptions::default(),
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
            &CustomToolRegistryOptions::default(),
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
            &CustomToolRegistryOptions::default(),
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
            &CustomToolRegistryOptions::default(),
            &CustomToolRunOptions::default(),
        )
        .expect_err("broader requested profile should fail");
        assert!(error
            .to_string()
            .contains("tool `restricted_tool` requires permission profile `agent`"));
        let listed = list_custom_tools(
            &paths,
            Some("readonly"),
            &CustomToolRegistryOptions::default(),
        )
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

        let report = validate_custom_tools(&paths, None, &CustomToolRegistryOptions::default())
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
            &CustomToolRegistryOptions::default(),
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
        let source = fs::read_to_string(
            paths
                .vault_root()
                .join(report.entrypoint_path.as_deref().expect("entrypoint path")),
        )
        .expect("entrypoint should exist");
        assert!(source.contains("function main(input, ctx)"));
    }

    #[test]
    fn set_custom_tool_updates_manifest_fields() {
        let (_dir, paths) = test_paths();
        init_custom_tool(
            &paths,
            "meeting_summary",
            &CustomToolRegistryOptions::default(),
            &CustomToolInitOptions {
                description: Some("Summarize one meeting note.".to_string()),
                ..CustomToolInitOptions::default()
            },
        )
        .expect("tool init should succeed");

        let report = set_custom_tool(
            &paths,
            "meeting_summary",
            &CustomToolRegistryOptions::default(),
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
            "function main(input) {\n  const listed = tools.list();\n  const described = tools.get('inner_tool');\n  const called = tools.call('inner_tool', { note: input.note });\n  return {\n    listed: listed.map((tool) => tool.name),\n    callable: listed.every((tool) => tool.callable === true),\n    body_has_doc: described.body.includes('Inner tool documentation.'),\n    echoed: called.echoed,\n  };\n}\n",
        );

        let report = run_custom_tool(
            &paths,
            None,
            "outer_tool",
            &json!({ "note": "alpha" }),
            &CustomToolRegistryOptions::default(),
            &CustomToolRunOptions {
                surface: "cli".to_string(),
            },
        )
        .expect("nested tool calls should succeed");

        assert_eq!(report.result["listed"], json!(["inner_tool", "outer_tool"]));
        assert_eq!(report.result["callable"], json!(true));
        assert_eq!(report.result["body_has_doc"], json!(true));
        assert_eq!(report.result["echoed"], json!("ALPHA"));

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
            &CustomToolRegistryOptions::default(),
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
            &CustomToolRegistryOptions::default(),
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
}
