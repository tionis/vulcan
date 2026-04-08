use crate::{trust, CliError};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use vulcan_core::{
    evaluate_dataview_js_with_options, load_vault_config, resolve_permission_profile,
    DataviewJsEvalOptions, DataviewJsResult, JsRuntimeSandbox, PluginEvent, PluginRegistration,
    VaultPaths,
};

const PLUGINS_DIR: &str = ".vulcan/plugins";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PluginDescriptor {
    pub name: String,
    pub path: PathBuf,
    pub exists: bool,
    pub registered: bool,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<PluginEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<JsRuntimeSandbox>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
struct ResolvedPlugin {
    descriptor: PluginDescriptor,
    absolute_path: PathBuf,
}

pub(crate) fn list_plugins(paths: &VaultPaths) -> Vec<PluginDescriptor> {
    let registered = load_vault_config(paths).config.plugins;
    let discovered = discover_plugin_files(paths);
    let mut names = registered.keys().cloned().collect::<Vec<_>>();
    for name in discovered.keys() {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let registration = registered.get(&name);
            resolve_plugin_descriptor(paths, &name, registration, discovered.get(&name))
        })
        .collect()
}

pub(crate) fn plugin_default_config_path(name: &str) -> PathBuf {
    PathBuf::from(PLUGINS_DIR).join(format!("{name}.js"))
}

pub(crate) fn run_plugin(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    name: &str,
) -> Result<DataviewJsResult, CliError> {
    let plugin = resolve_plugin(paths, name)?;
    require_trusted_plugin_execution(paths, Some(&plugin.descriptor.name))?;
    invoke_plugin(
        paths,
        &plugin,
        active_permission_profile,
        "main",
        &json!({
            "kind": "manual",
            "plugin": plugin.descriptor.name,
        }),
    )
}

pub(crate) fn dispatch_plugin_event(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    event: PluginEvent,
    payload: &Value,
    quiet: bool,
) -> Result<(), CliError> {
    let plugins = list_plugins(paths)
        .into_iter()
        .filter(|plugin| {
            plugin.registered && plugin.enabled && plugin.exists && plugin.events.contains(&event)
        })
        .collect::<Vec<_>>();
    if plugins.is_empty() {
        return Ok(());
    }

    if !trust::is_trusted(paths.vault_root()) {
        if !quiet {
            eprintln!(
                "warning: skipping plugin hooks for {} because the vault is not trusted",
                event.handler_name()
            );
        }
        return Ok(());
    }

    for descriptor in plugins {
        let resolved = resolve_plugin(paths, &descriptor.name)?;
        let result = invoke_plugin(
            paths,
            &resolved,
            active_permission_profile,
            event.handler_name(),
            payload,
        );
        if event.is_blocking() {
            result?;
            continue;
        }
        if let Err(error) = result {
            eprintln!(
                "warning: plugin `{}` failed during {}: {}",
                descriptor.name,
                event.handler_name(),
                error
            );
        }
    }

    Ok(())
}

pub(crate) fn require_trusted_plugin_execution(
    paths: &VaultPaths,
    plugin_name: Option<&str>,
) -> Result<(), CliError> {
    if trust::is_trusted(paths.vault_root()) {
        return Ok(());
    }

    let target = plugin_name.map_or("plugins".to_string(), |name| format!("plugin `{name}`"));
    Err(CliError::operation(format!(
        "{target} require a trusted vault; run `vulcan trust add` first"
    )))
}

fn resolve_plugin(paths: &VaultPaths, name: &str) -> Result<ResolvedPlugin, CliError> {
    let registered = load_vault_config(paths).config.plugins;
    let discovered = discover_plugin_files(paths);
    let descriptor =
        resolve_plugin_descriptor(paths, name, registered.get(name), discovered.get(name));
    if !descriptor.exists {
        return Err(CliError::operation(format!(
            "plugin `{name}` was not found at {}",
            descriptor.path.display()
        )));
    }
    let absolute_path =
        absolute_plugin_path(paths, registered.get(name), discovered.get(name), name);
    Ok(ResolvedPlugin {
        descriptor,
        absolute_path,
    })
}

fn invoke_plugin(
    paths: &VaultPaths,
    plugin: &ResolvedPlugin,
    active_permission_profile: Option<&str>,
    handler_name: &str,
    payload: &Value,
) -> Result<DataviewJsResult, CliError> {
    let source = fs::read_to_string(&plugin.absolute_path).map_err(CliError::operation)?;
    let effective_permission_profile =
        effective_plugin_permission_profile(paths, active_permission_profile, &plugin.descriptor)?;
    let source = build_plugin_invocation_source(
        strip_shebang_line(&source),
        handler_name,
        payload,
        &plugin.descriptor,
    )?;
    evaluate_dataview_js_with_options(
        paths,
        &source,
        current_file_for_plugin(paths, &plugin.absolute_path).as_deref(),
        DataviewJsEvalOptions {
            timeout: None,
            sandbox: plugin.descriptor.sandbox,
            permission_profile: effective_permission_profile,
            ..DataviewJsEvalOptions::default()
        },
    )
    .map_err(CliError::operation)
}

fn build_plugin_invocation_source(
    source: &str,
    handler_name: &str,
    payload: &Value,
    descriptor: &PluginDescriptor,
) -> Result<String, CliError> {
    let handler_name = serde_json::to_string(handler_name).map_err(CliError::operation)?;
    let payload = serde_json::to_string(payload).map_err(CliError::operation)?;
    let context = serde_json::to_string(&json!({
        "plugin": {
            "name": descriptor.name,
            "path": descriptor.path,
            "registered": descriptor.registered,
            "enabled": descriptor.enabled,
            "events": descriptor.events,
            "sandbox": descriptor.sandbox,
            "permission_profile": descriptor.permission_profile,
            "description": descriptor.description,
        }
    }))
    .map_err(CliError::operation)?;

    Ok(format!(
        "const __vulcanPluginEvent = {payload};\n\
const __vulcanPluginContext = {context};\n\
{source}\n\
const __vulcanPluginHandlerName = {handler_name};\n\
const __vulcanPluginHandler = globalThis[__vulcanPluginHandlerName];\n\
if (typeof __vulcanPluginHandler !== 'function') {{\n\
  throw new Error(`plugin handler ${{__vulcanPluginHandlerName}} is not defined`);\n\
}}\n\
__vulcanPluginHandler(__vulcanPluginEvent, __vulcanPluginContext);\n"
    ))
}

fn effective_plugin_permission_profile(
    paths: &VaultPaths,
    active_permission_profile: Option<&str>,
    descriptor: &PluginDescriptor,
) -> Result<Option<String>, CliError> {
    match (
        active_permission_profile,
        descriptor.permission_profile.as_deref(),
    ) {
        (None, None) => Ok(None),
        (None, Some(requested)) => {
            resolve_permission_profile(paths, Some(requested)).map_err(CliError::operation)?;
            Ok(Some(requested.to_string()))
        }
        (Some(active), None) => Ok(Some(active.to_string())),
        (Some(active), Some(requested)) => {
            let active_profile =
                resolve_permission_profile(paths, Some(active)).map_err(CliError::operation)?;
            let requested_profile =
                resolve_permission_profile(paths, Some(requested)).map_err(CliError::operation)?;
            if requested_profile.grant.is_subset_of(&active_profile.grant) {
                Ok(Some(requested.to_string()))
            } else {
                Err(CliError::operation(format!(
                    "plugin `{}` requires permission profile `{requested}`, which is broader than active profile `{active}`",
                    descriptor.name
                )))
            }
        }
    }
}

fn current_file_for_plugin(paths: &VaultPaths, absolute_path: &Path) -> Option<String> {
    absolute_path
        .strip_prefix(paths.vault_root())
        .ok()
        .map(path_to_forward_string)
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

fn discover_plugin_files(paths: &VaultPaths) -> BTreeMap<String, PathBuf> {
    let plugin_root = paths.vault_root().join(PLUGINS_DIR);
    let Ok(entries) = fs::read_dir(plugin_root) else {
        return BTreeMap::new();
    };

    let mut discovered = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("js") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        discovered.insert(name.to_string(), path);
    }
    discovered
}

fn resolve_plugin_descriptor(
    paths: &VaultPaths,
    name: &str,
    registration: Option<&PluginRegistration>,
    discovered_path: Option<&PathBuf>,
) -> PluginDescriptor {
    let absolute_path = absolute_plugin_path(paths, registration, discovered_path, name);
    PluginDescriptor {
        name: name.to_string(),
        path: relativize_plugin_path(paths, &absolute_path),
        exists: absolute_path.is_file(),
        registered: registration.is_some(),
        enabled: registration.is_some_and(|plugin| plugin.enabled),
        events: registration
            .map(|plugin| plugin.events.clone())
            .unwrap_or_default(),
        sandbox: registration.and_then(|plugin| plugin.sandbox),
        permission_profile: registration.and_then(|plugin| plugin.permission_profile.clone()),
        description: registration.and_then(|plugin| plugin.description.clone()),
    }
}

fn absolute_plugin_path(
    paths: &VaultPaths,
    registration: Option<&PluginRegistration>,
    discovered_path: Option<&PathBuf>,
    name: &str,
) -> PathBuf {
    if let Some(path) = registration.and_then(|plugin| plugin.path.as_ref()) {
        if path.is_absolute() {
            return path.clone();
        }
        return paths.vault_root().join(path);
    }
    if let Some(path) = discovered_path {
        return path.clone();
    }
    paths.vault_root().join(plugin_default_config_path(name))
}

fn relativize_plugin_path(paths: &VaultPaths, path: &Path) -> PathBuf {
    path.strip_prefix(paths.vault_root())
        .map_or_else(|_| path.to_path_buf(), Path::to_path_buf)
}

fn path_to_forward_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn discovers_js_plugins_from_default_directory() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(PLUGINS_DIR)).expect("plugin dir should exist");
        fs::write(
            vault_root.join(PLUGINS_DIR).join("lint.js"),
            "function main() {}",
        )
        .expect("plugin should write");

        let plugins = list_plugins(&VaultPaths::new(vault_root));

        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "lint");
        assert!(!plugins[0].registered);
        assert!(!plugins[0].enabled);
        assert!(plugins[0].exists);
    }

    #[test]
    fn restricted_invocations_reject_broader_plugin_permission_profiles() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
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
        let paths = VaultPaths::new(vault_root);
        let descriptor = PluginDescriptor {
            name: "lint".to_string(),
            path: plugin_default_config_path("lint"),
            exists: true,
            registered: true,
            enabled: true,
            events: vec![PluginEvent::OnNoteWrite],
            sandbox: Some(JsRuntimeSandbox::Strict),
            permission_profile: Some("agent".to_string()),
            description: None,
        };

        let error = effective_plugin_permission_profile(&paths, Some("readonly"), &descriptor)
            .expect_err("broader profile should fail");
        assert!(
            error
                .to_string()
                .contains("plugin `lint` requires permission profile `agent`"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn restricted_invocations_allow_subset_plugin_permission_profiles() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let vault_root = temp_dir.path();
        fs::create_dir_all(vault_root.join(".vulcan")).expect("vulcan dir should exist");
        fs::write(
            vault_root.join(".vulcan/config.toml"),
            r#"
[permissions.profiles.agent]
read = "all"
write = { allow = ["folder:Projects/**"], deny = ["folder:Projects/Secret/**"] }
refactor = "none"
git = "deny"
network = { allow = true, domains = ["example.com"] }
index = "deny"
config = "read"
execute = "allow"
shell = "deny"
cpu_limit_ms = 5000
memory_limit_mb = 64

[permissions.profiles.plugin]
read = { allow = ["note:Projects/Alpha.md"] }
write = "none"
refactor = "none"
git = "deny"
network = "deny"
index = "deny"
config = "none"
execute = "allow"
shell = "deny"
cpu_limit_ms = 100
memory_limit_mb = 32
"#,
        )
        .expect("config should write");
        let paths = VaultPaths::new(vault_root);
        let descriptor = PluginDescriptor {
            name: "lint".to_string(),
            path: plugin_default_config_path("lint"),
            exists: true,
            registered: true,
            enabled: true,
            events: vec![PluginEvent::OnNoteWrite],
            sandbox: Some(JsRuntimeSandbox::Strict),
            permission_profile: Some("plugin".to_string()),
            description: None,
        };

        let effective = effective_plugin_permission_profile(&paths, Some("agent"), &descriptor)
            .expect("subset profile should be accepted");
        assert_eq!(effective.as_deref(), Some("plugin"));
    }
}
