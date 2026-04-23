use crate::{trust, AppError};
use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vulcan_core::{
    assistant_tools_root, default_assistant_tool_reserved_names, evaluate_dataview_js_with_options,
    list_assistant_tools, load_assistant_tool, resolve_permission_profile,
    validate_json_value_against_schema, AssistantTool, AssistantToolSummary,
    AssistantToolValidationOptions, DataviewJsEvalOptions, VaultPaths,
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
    options: &CustomToolRegistryOptions,
) -> Result<Vec<CustomToolDescriptor>, AppError> {
    let callable = trust::is_trusted(paths.vault_root());
    let tools = list_assistant_tools(paths, &assistant_tool_validation_options(options))
        .map_err(AppError::operation)?;
    Ok(tools
        .into_iter()
        .map(|summary| CustomToolDescriptor { summary, callable })
        .collect())
}

pub fn show_custom_tool(
    paths: &VaultPaths,
    name: &str,
    options: &CustomToolRegistryOptions,
) -> Result<CustomToolShowReport, AppError> {
    let tool = load_assistant_tool(paths, name, &assistant_tool_validation_options(options))
        .map_err(AppError::operation)?;
    Ok(CustomToolShowReport {
        tool,
        callable: trust::is_trusted(paths.vault_root()),
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
        active_permission_profile,
        &tool.summary.name,
        tool.summary.permission_profile.as_deref(),
    )?;
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

        let tools = list_custom_tools(&paths, &CustomToolRegistryOptions::default())
            .expect("tools should load");
        assert_eq!(tools.len(), 1);
        assert!(!tools[0].callable);
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
        trust::revoke_trust(paths.vault_root()).expect("trust should be removed");
        match previous_xdg {
            Some(value) => env::set_var("XDG_CONFIG_HOME", value),
            None => env::remove_var("XDG_CONFIG_HOME"),
        }
    }
}
