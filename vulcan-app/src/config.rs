use crate::AppError;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value as TomlValue;
use vulcan_core::{
    default_config_template, ensure_vulcan_dir, load_permission_profiles,
    load_permission_profiles_with_overrides, load_vault_config, load_vault_config_with_overrides,
    validate_vulcan_overrides_toml, ConfigDiagnostic, PermissionProfile, VaultConfig, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigShowReport {
    pub section: Option<String>,
    pub config: Value,
    pub diagnostics: Vec<ConfigDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_permission_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_permission_profiles: Vec<String>,
    #[serde(skip_serializing)]
    pub rendered_toml: TomlValue,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigGetReport {
    pub key: String,
    pub value: Value,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigSetReport {
    pub key: String,
    pub value: Value,
    pub config_path: PathBuf,
    pub created_config: bool,
    pub updated: bool,
    pub dry_run: bool,
    pub diagnostics: Vec<ConfigDiagnostic>,
    #[serde(skip_serializing)]
    pub absolute_config_path: PathBuf,
    #[serde(skip_serializing)]
    pub rendered_contents: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigValueKind {
    String,
    Integer,
    Float,
    Boolean,
    Array,
    Object,
    Enum,
    Flexible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigTarget {
    Shared,
    Local,
}

impl ConfigTarget {
    #[must_use]
    pub fn path(self, paths: &VaultPaths) -> PathBuf {
        match self {
            Self::Shared => paths.config_file().to_path_buf(),
            Self::Local => paths.local_config_file().to_path_buf(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigTargetSupport {
    SharedOnly,
    LocalOnly,
    SharedAndLocal,
}

impl ConfigTargetSupport {
    #[must_use]
    pub fn allows(self, target: ConfigTarget) -> bool {
        matches!(
            (self, target),
            (Self::SharedOnly, ConfigTarget::Shared)
                | (Self::LocalOnly, ConfigTarget::Local)
                | (Self::SharedAndLocal, _)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigValueSource {
    Default,
    ObsidianImport,
    SharedOverride,
    LocalOverride,
    Unset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigDescriptor {
    pub key: String,
    pub storage_key: String,
    pub section: String,
    pub section_title: String,
    pub section_description: String,
    pub description: String,
    pub kind: ConfigValueKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<String>,
    pub target_support: ConfigTargetSupport,
    pub creatable_when_absent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_display: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigListEntry {
    pub key: String,
    pub storage_key: String,
    pub section: String,
    pub section_title: String,
    pub section_description: String,
    pub description: String,
    pub kind: ConfigValueKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<String>,
    pub target_support: ConfigTargetSupport,
    pub creatable_when_absent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_display: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_value: Option<Value>,
    pub value_source: ConfigValueSource,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigListReport {
    pub section: Option<String>,
    pub entries: Vec<ConfigListEntry>,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigUnsetReport {
    pub key: String,
    pub config_path: PathBuf,
    pub removed: bool,
    pub updated: bool,
    pub dry_run: bool,
    pub diagnostics: Vec<ConfigDiagnostic>,
    #[serde(skip_serializing)]
    pub absolute_config_path: PathBuf,
    #[serde(skip_serializing)]
    pub rendered_contents: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigMutationOperation {
    Set { key: String, value: TomlValue },
    Unset { key: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigBatchReport {
    pub config_path: PathBuf,
    pub created_config: bool,
    pub updated: bool,
    pub dry_run: bool,
    pub diagnostics: Vec<ConfigDiagnostic>,
    #[serde(skip_serializing)]
    pub absolute_config_path: PathBuf,
    #[serde(skip_serializing)]
    pub rendered_contents: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConfigDocumentSaveReport {
    pub config_path: PathBuf,
    pub created_config: bool,
    pub updated: bool,
    pub diagnostics: Vec<ConfigDiagnostic>,
    #[serde(skip_serializing)]
    pub absolute_config_path: PathBuf,
    #[serde(skip_serializing)]
    pub rendered_contents: String,
}

#[derive(Debug, Clone)]
struct DisplayConfigState {
    json: Value,
    toml: TomlValue,
    diagnostics: Vec<ConfigDiagnostic>,
    permission_profiles: Vec<String>,
}

#[derive(Debug, Clone)]
struct PermissionConfigMetadata {
    active_profile: String,
    available_profiles: Vec<String>,
}

pub fn load_config_file_toml(path: &Path) -> Result<TomlValue, AppError> {
    if !path.exists() {
        return Ok(TomlValue::Table(toml::map::Map::new()));
    }

    let contents = fs::read_to_string(path).map_err(AppError::operation)?;
    if contents.trim().is_empty() {
        return Ok(TomlValue::Table(toml::map::Map::new()));
    }

    let value = contents.parse::<TomlValue>().map_err(|error| {
        AppError::operation(format!("failed to parse {}: {error}", path.display()))
    })?;
    if !value.is_table() {
        return Err(AppError::operation(format!(
            "expected {} to contain a TOML table",
            path.display()
        )));
    }
    Ok(value)
}

#[must_use]
pub fn config_toml_path_exists(config: &TomlValue, path: &[&str]) -> bool {
    let mut current = config;
    for segment in path {
        let Some(next) = current.get(*segment) else {
            return false;
        };
        current = next;
    }
    true
}

pub fn set_config_toml_value(
    config: &mut TomlValue,
    path: &[&str],
    value: TomlValue,
) -> Result<(), AppError> {
    let Some(root) = config.as_table_mut() else {
        return Err(AppError::operation(
            "expected config file to contain a TOML table",
        ));
    };

    set_config_toml_value_in_table(root, path, value)
}

fn set_config_toml_value_in_table(
    table: &mut toml::map::Map<String, TomlValue>,
    path: &[&str],
    value: TomlValue,
) -> Result<(), AppError> {
    let Some((segment, rest)) = path.split_first() else {
        return Err(AppError::operation("config key cannot be empty"));
    };

    if rest.is_empty() {
        table.insert((*segment).to_string(), value);
        return Ok(());
    }

    let entry = table
        .entry((*segment).to_string())
        .or_insert_with(|| TomlValue::Table(toml::map::Map::new()));
    if !entry.is_table() {
        *entry = TomlValue::Table(toml::map::Map::new());
    }
    let Some(child_table) = entry.as_table_mut() else {
        return Err(AppError::operation(format!(
            "expected config key `{}` to contain a table",
            path[..path.len() - rest.len()].join(".")
        )));
    };

    set_config_toml_value_in_table(child_table, rest, value)
}

pub fn remove_config_toml_value(config: &mut TomlValue, path: &[&str]) -> Result<bool, AppError> {
    let Some(root) = config.as_table_mut() else {
        return Err(AppError::operation(
            "expected config file to contain a TOML table",
        ));
    };

    remove_config_toml_value_in_table(root, path)
}

fn remove_config_toml_value_in_table(
    table: &mut toml::map::Map<String, TomlValue>,
    path: &[&str],
) -> Result<bool, AppError> {
    let Some((segment, rest)) = path.split_first() else {
        return Err(AppError::operation("config key cannot be empty"));
    };

    if rest.is_empty() {
        return Ok(table.remove(*segment).is_some());
    }

    let Some(child) = table.get_mut(*segment) else {
        return Ok(false);
    };
    let Some(child_table) = child.as_table_mut() else {
        return Err(AppError::operation(format!(
            "expected config key `{}` to contain a table",
            path[..path.len() - rest.len()].join(".")
        )));
    };
    let removed = remove_config_toml_value_in_table(child_table, rest)?;
    if child_table.is_empty() {
        table.remove(*segment);
    }
    Ok(removed)
}

pub fn build_config_show_report(
    paths: &VaultPaths,
    section: Option<&str>,
    selected_permission_profile: Option<&str>,
) -> Result<ConfigShowReport, AppError> {
    let display_config = load_display_config_state(paths)?;
    let selected_json = select_config_json_section(&display_config.json, section)?;
    let selected_toml = select_config_toml_section(&display_config.toml, section)?;
    let permission_metadata =
        config_permission_metadata(section, selected_permission_profile, &display_config);

    Ok(ConfigShowReport {
        section: section.map(ToOwned::to_owned),
        config: selected_json,
        diagnostics: display_config.diagnostics,
        active_permission_profile: permission_metadata
            .as_ref()
            .map(|metadata| metadata.active_profile.clone()),
        available_permission_profiles: permission_metadata
            .map_or_else(Vec::new, |metadata| metadata.available_profiles),
        rendered_toml: selected_toml,
    })
}

pub fn build_config_show_report_from_overrides(
    paths: &VaultPaths,
    shared_toml: &TomlValue,
    local_toml: &TomlValue,
    section: Option<&str>,
    selected_permission_profile: Option<&str>,
) -> Result<ConfigShowReport, AppError> {
    let display_config = load_display_config_state_from_overrides(paths, shared_toml, local_toml)?;
    let selected_json = select_config_json_section(&display_config.json, section)?;
    let selected_toml = select_config_toml_section(&display_config.toml, section)?;
    let permission_metadata =
        config_permission_metadata(section, selected_permission_profile, &display_config);

    Ok(ConfigShowReport {
        section: section.map(ToOwned::to_owned),
        config: selected_json,
        diagnostics: display_config.diagnostics,
        active_permission_profile: permission_metadata
            .as_ref()
            .map(|metadata| metadata.active_profile.clone()),
        available_permission_profiles: permission_metadata
            .map_or_else(Vec::new, |metadata| metadata.available_profiles),
        rendered_toml: selected_toml,
    })
}

pub fn build_config_get_report(paths: &VaultPaths, key: &str) -> Result<ConfigGetReport, AppError> {
    let display_config = load_display_config_state(paths)?;
    let value = select_config_json_value(&display_config.json, key)?;

    Ok(ConfigGetReport {
        key: key.to_string(),
        value,
        diagnostics: display_config.diagnostics,
    })
}

pub fn plan_config_set_report(
    paths: &VaultPaths,
    key: &str,
    raw_value: &str,
    dry_run: bool,
) -> Result<ConfigSetReport, AppError> {
    let value = parse_config_set_value(raw_value);
    plan_config_set_report_to(paths, key, &value, ConfigTarget::Shared, dry_run)
}

pub fn plan_config_set_report_for_target(
    paths: &VaultPaths,
    key: &str,
    raw_value: &str,
    target: ConfigTarget,
    dry_run: bool,
) -> Result<ConfigSetReport, AppError> {
    let value = parse_config_set_value(raw_value);
    plan_config_set_report_to(paths, key, &value, target, dry_run)
}

pub fn plan_config_set_report_to(
    paths: &VaultPaths,
    key: &str,
    value: &TomlValue,
    target: ConfigTarget,
    dry_run: bool,
) -> Result<ConfigSetReport, AppError> {
    let mutation = plan_config_value_write(paths, key, value.clone(), target, dry_run)?;
    Ok(ConfigSetReport {
        key: key.to_string(),
        value: serde_json::to_value(value).map_err(AppError::operation)?,
        config_path: mutation.config_path,
        created_config: mutation.created_config,
        updated: mutation.updated,
        dry_run,
        diagnostics: mutation.diagnostics,
        absolute_config_path: mutation.absolute_config_path,
        rendered_contents: mutation.rendered_contents,
    })
}

pub fn plan_config_unset_report(
    paths: &VaultPaths,
    key: &str,
    target: ConfigTarget,
    dry_run: bool,
) -> Result<ConfigUnsetReport, AppError> {
    let (mutation, removed) = plan_config_value_unset(paths, key, target, dry_run)?;
    Ok(ConfigUnsetReport {
        key: key.to_string(),
        config_path: mutation.config_path,
        removed,
        updated: mutation.updated,
        dry_run,
        diagnostics: mutation.diagnostics,
        absolute_config_path: mutation.absolute_config_path,
        rendered_contents: mutation.rendered_contents,
    })
}

pub fn plan_config_batch_report(
    paths: &VaultPaths,
    operations: &[ConfigMutationOperation],
    target: ConfigTarget,
    dry_run: bool,
) -> Result<ConfigBatchReport, AppError> {
    let absolute_config_path = target.path(paths);
    let created_config = !absolute_config_path.exists();
    let existing_contents = fs::read_to_string(&absolute_config_path).ok();
    let mut config_value = load_config_file_toml(&absolute_config_path)?;

    for operation in operations {
        apply_config_operation_to_value(&mut config_value, operation, target)?;
    }

    let rendered_contents = toml::to_string_pretty(&config_value).map_err(AppError::operation)?;
    validate_vulcan_overrides_toml(&rendered_contents).map_err(AppError::operation)?;
    let updated = existing_contents.as_deref() != Some(rendered_contents.as_str());

    Ok(ConfigBatchReport {
        config_path: relativize_config_path(paths, &absolute_config_path),
        created_config,
        updated,
        dry_run,
        diagnostics: normalize_config_diagnostics(paths, &load_vault_config(paths).diagnostics),
        absolute_config_path,
        rendered_contents,
    })
}

pub fn apply_config_set_report(
    paths: &VaultPaths,
    mut report: ConfigSetReport,
) -> Result<ConfigSetReport, AppError> {
    report.diagnostics = apply_config_mutation_plan(
        paths,
        &ConfigMutationPlan {
            config_path: report.config_path.clone(),
            created_config: report.created_config,
            updated: report.updated,
            diagnostics: report.diagnostics.clone(),
            absolute_config_path: report.absolute_config_path.clone(),
            rendered_contents: report.rendered_contents.clone(),
        },
    )?;
    Ok(report)
}

pub fn apply_config_batch_report(
    paths: &VaultPaths,
    mut report: ConfigBatchReport,
) -> Result<ConfigBatchReport, AppError> {
    report.diagnostics = apply_config_mutation_plan(
        paths,
        &ConfigMutationPlan {
            config_path: report.config_path.clone(),
            created_config: report.created_config,
            updated: report.updated,
            diagnostics: report.diagnostics.clone(),
            absolute_config_path: report.absolute_config_path.clone(),
            rendered_contents: report.rendered_contents.clone(),
        },
    )?;
    Ok(report)
}

pub fn apply_config_unset_report(
    paths: &VaultPaths,
    mut report: ConfigUnsetReport,
) -> Result<ConfigUnsetReport, AppError> {
    report.diagnostics = apply_config_mutation_plan(
        paths,
        &ConfigMutationPlan {
            config_path: report.config_path.clone(),
            created_config: false,
            updated: report.updated,
            diagnostics: report.diagnostics.clone(),
            absolute_config_path: report.absolute_config_path.clone(),
            rendered_contents: report.rendered_contents.clone(),
        },
    )?;
    Ok(report)
}

pub fn plan_config_document_save(
    paths: &VaultPaths,
    rendered_contents: &str,
) -> Result<ConfigDocumentSaveReport, AppError> {
    plan_config_document_save_for_target(paths, rendered_contents, ConfigTarget::Shared)
}

pub fn plan_config_document_save_for_target(
    paths: &VaultPaths,
    rendered_contents: &str,
    target: ConfigTarget,
) -> Result<ConfigDocumentSaveReport, AppError> {
    validate_vulcan_overrides_toml(rendered_contents).map_err(AppError::operation)?;

    let absolute_config_path = target.path(paths);
    let created_config = !absolute_config_path.exists();
    let existing_contents = fs::read_to_string(&absolute_config_path).ok();
    let updated = existing_contents.as_deref() != Some(rendered_contents);

    Ok(ConfigDocumentSaveReport {
        config_path: relativize_config_path(paths, &absolute_config_path),
        created_config,
        updated,
        diagnostics: normalize_config_diagnostics(paths, &load_vault_config(paths).diagnostics),
        absolute_config_path,
        rendered_contents: rendered_contents.to_string(),
    })
}

pub fn apply_config_document_save(
    paths: &VaultPaths,
    mut report: ConfigDocumentSaveReport,
) -> Result<ConfigDocumentSaveReport, AppError> {
    if report.updated {
        ensure_vulcan_dir(paths).map_err(AppError::operation)?;
        fs::write(&report.absolute_config_path, &report.rendered_contents)
            .map_err(AppError::operation)?;
    }
    report.diagnostics = normalize_config_diagnostics(paths, &load_vault_config(paths).diagnostics);
    Ok(report)
}

fn load_display_config_state(paths: &VaultPaths) -> Result<DisplayConfigState, AppError> {
    let loaded = load_vault_config(paths);
    let permission_profiles = load_permission_profiles(paths);
    let json = display_config_json(&loaded.config, &permission_profiles.profiles)?;
    let toml = display_config_toml(&loaded.config, &permission_profiles.profiles)?;

    Ok(DisplayConfigState {
        json,
        toml,
        diagnostics: merge_config_diagnostics(
            paths,
            &loaded.diagnostics,
            &permission_profiles.diagnostics,
        ),
        permission_profiles: permission_profiles.profiles.keys().cloned().collect(),
    })
}

fn load_display_config_state_from_overrides(
    paths: &VaultPaths,
    shared_toml: &TomlValue,
    local_toml: &TomlValue,
) -> Result<DisplayConfigState, AppError> {
    let loaded = load_vault_config_with_overrides(paths, Some(shared_toml), Some(local_toml));
    let permission_profiles =
        load_permission_profiles_with_overrides(paths, Some(shared_toml), Some(local_toml));
    let json = display_config_json(&loaded.config, &permission_profiles.profiles)?;
    let toml = display_config_toml(&loaded.config, &permission_profiles.profiles)?;

    Ok(DisplayConfigState {
        json,
        toml,
        diagnostics: merge_config_diagnostics(
            paths,
            &loaded.diagnostics,
            &permission_profiles.diagnostics,
        ),
        permission_profiles: permission_profiles.profiles.keys().cloned().collect(),
    })
}

fn display_config_json(
    config: &VaultConfig,
    permission_profiles: &BTreeMap<String, PermissionProfile>,
) -> Result<Value, AppError> {
    let mut json = serde_json::to_value(config).map_err(AppError::operation)?;
    let Value::Object(object) = &mut json else {
        return Err(AppError::operation(
            "vault config did not serialize to an object",
        ));
    };
    object.insert(
        "permissions".to_string(),
        serde_json::json!({ "profiles": permission_profiles }),
    );
    Ok(json)
}

fn display_config_toml(
    config: &VaultConfig,
    permission_profiles: &BTreeMap<String, PermissionProfile>,
) -> Result<TomlValue, AppError> {
    let mut toml_config = TomlValue::try_from(config).map_err(AppError::operation)?;
    let TomlValue::Table(table) = &mut toml_config else {
        return Err(AppError::operation(
            "vault config did not serialize to a TOML table",
        ));
    };

    let mut permissions_table = toml::map::Map::new();
    permissions_table.insert(
        "profiles".to_string(),
        TomlValue::try_from(permission_profiles).map_err(AppError::operation)?,
    );
    table.insert(
        "permissions".to_string(),
        TomlValue::Table(permissions_table),
    );
    Ok(toml_config)
}

fn merge_config_diagnostics(
    paths: &VaultPaths,
    left: &[ConfigDiagnostic],
    right: &[ConfigDiagnostic],
) -> Vec<ConfigDiagnostic> {
    let mut merged = normalize_config_diagnostics(paths, left);
    for diagnostic in normalize_config_diagnostics(paths, right) {
        if merged.iter().any(|existing| {
            existing.path == diagnostic.path && existing.message == diagnostic.message
        }) {
            continue;
        }
        merged.push(diagnostic);
    }
    merged
}

fn normalize_config_diagnostics(
    paths: &VaultPaths,
    diagnostics: &[ConfigDiagnostic],
) -> Vec<ConfigDiagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| ConfigDiagnostic {
            path: relativize_config_path(paths, &diagnostic.path),
            message: diagnostic.message.clone(),
        })
        .collect()
}

fn config_permission_metadata(
    section: Option<&str>,
    selected_permission_profile: Option<&str>,
    display_config: &DisplayConfigState,
) -> Option<PermissionConfigMetadata> {
    let section = section?;
    if section != "permissions" && !section.starts_with("permissions.") {
        return None;
    }

    Some(PermissionConfigMetadata {
        active_profile: selected_permission_profile
            .unwrap_or("unrestricted")
            .to_string(),
        available_profiles: display_config.permission_profiles.clone(),
    })
}

fn select_config_json_section(config: &Value, section: Option<&str>) -> Result<Value, AppError> {
    let Some(section) = section else {
        return Ok(config.clone());
    };

    select_config_json_path(config, section, "section")
}

fn select_config_json_value(config: &Value, key: &str) -> Result<Value, AppError> {
    let value = select_config_json_path(config, key, "key")?;
    if value.is_object() {
        return Err(AppError::operation(format!(
            "config key `{key}` resolves to a section; use `vulcan config show {key}` instead"
        )));
    }
    Ok(value)
}

fn select_config_json_path(config: &Value, path: &str, kind: &str) -> Result<Value, AppError> {
    let mut current = config;
    for part in parse_config_path(path, kind)? {
        current = current
            .get(part)
            .ok_or_else(|| AppError::operation(format!("unknown config {kind} `{path}`")))?;
    }
    Ok(current.clone())
}

fn select_config_toml_section(
    config: &TomlValue,
    section: Option<&str>,
) -> Result<TomlValue, AppError> {
    let Some(section) = section else {
        return Ok(config.clone());
    };

    let mut current = config;
    for part in parse_config_path(section, "section")? {
        current = current
            .get(part)
            .ok_or_else(|| AppError::operation(format!("unknown config section `{section}`")))?;
    }
    Ok(current.clone())
}

fn parse_config_path<'a>(path: &'a str, kind: &str) -> Result<Vec<&'a str>, AppError> {
    if path.is_empty() || path.starts_with('.') || path.ends_with('.') {
        return Err(AppError::operation(format!(
            "invalid config {kind} `{path}`"
        )));
    }
    let parts = path.split('.').collect::<Vec<_>>();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(AppError::operation(format!(
            "invalid config {kind} `{path}`"
        )));
    }
    Ok(parts)
}

fn parse_config_set_value(raw_value: &str) -> TomlValue {
    let wrapped = format!("value = {raw_value}\n");
    wrapped
        .parse::<TomlValue>()
        .ok()
        .and_then(|value| value.get("value").cloned())
        .unwrap_or_else(|| TomlValue::String(raw_value.to_string()))
}

#[derive(Debug, Clone)]
struct TemplateDescriptorSeed {
    storage_segments: Vec<String>,
    kind: ConfigValueKind,
    sample_value: Option<TomlValue>,
    enum_values: Vec<String>,
}

#[derive(Debug, Clone)]
struct ConfigDescriptorMatch {
    descriptor: ConfigDescriptor,
    storage_segments: Vec<String>,
}

struct CategoryDescriptor {
    key: &'static str,
    title: &'static str,
    description: &'static str,
}

#[must_use]
pub fn config_descriptor_catalog() -> Vec<ConfigDescriptor> {
    let default_values = default_config_value_map();
    let mut descriptors = BTreeMap::<String, ConfigDescriptor>::new();

    for seed in parse_default_config_template_seeds() {
        if is_sample_dynamic_storage_path(&seed.storage_segments) {
            continue;
        }
        let display_segments = storage_path_to_display_path(&seed.storage_segments);
        let key = display_segments.join(".");
        descriptors.entry(key.clone()).or_insert_with(|| {
            let default_value = default_values
                .get(&key)
                .cloned()
                .or_else(|| seed.sample_value.clone());
            let kind = if seed.enum_values.is_empty() {
                default_value
                    .as_ref()
                    .map_or_else(|| seed.kind.clone(), config_value_kind_from_toml)
            } else {
                ConfigValueKind::Enum
            };
            build_descriptor(
                &key,
                &seed.storage_segments.join("."),
                kind,
                seed.enum_values.clone(),
                config_target_support_for_key(&key),
                true,
                default_value,
            )
        });
    }

    for (key, value) in &default_values {
        descriptors.entry(key.clone()).or_insert_with(|| {
            build_descriptor(
                key,
                &display_key_to_storage_key(key),
                config_value_kind_from_toml(value),
                Vec::new(),
                config_target_support_for_key(key),
                true,
                Some(value.clone()),
            )
        });
    }

    for descriptor in dynamic_config_descriptors() {
        descriptors.insert(descriptor.key.clone(), descriptor);
    }

    descriptors.into_values().collect()
}

#[allow(clippy::needless_pass_by_value)]
fn build_descriptor(
    key: &str,
    storage_key: &str,
    kind: ConfigValueKind,
    enum_values: Vec<String>,
    target_support: ConfigTargetSupport,
    creatable_when_absent: bool,
    default_value: Option<TomlValue>,
) -> ConfigDescriptor {
    let display_segments = parse_key_segments(key);
    let category = category_descriptor(&display_segments);
    let default_json = default_value
        .as_ref()
        .and_then(|value| serde_json::to_value(value).ok());
    let default_display = default_value.as_ref().map(render_toml_summary);

    ConfigDescriptor {
        key: key.to_string(),
        storage_key: storage_key.to_string(),
        section: category.key.to_string(),
        section_title: category.title.to_string(),
        section_description: category.description.to_string(),
        description: config_path_description(key),
        kind,
        enum_values,
        target_support,
        creatable_when_absent,
        default_value: default_json,
        default_display,
        examples: config_path_examples(key),
        preferred_command: preferred_command_for_key(key),
    }
}

#[allow(clippy::too_many_lines)]
fn dynamic_config_descriptors() -> Vec<ConfigDescriptor> {
    let mut descriptors = Vec::new();
    let mut push = |key: &str,
                    kind: ConfigValueKind,
                    target_support: ConfigTargetSupport,
                    preferred: Option<&str>,
                    default_value: Option<TomlValue>,
                    enum_values: &[&str]| {
        descriptors.push(build_descriptor(
            key,
            &display_key_to_storage_key(key),
            kind,
            enum_values
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            target_support,
            true,
            default_value,
        ));
        if let Some(preferred) = preferred {
            descriptors
                .last_mut()
                .expect("descriptor should exist")
                .preferred_command = Some(preferred.to_string());
        }
    };

    push(
        "aliases.<name>",
        ConfigValueKind::String,
        ConfigTargetSupport::SharedAndLocal,
        Some("vulcan config alias set"),
        None,
        &[],
    );
    push(
        "property_types.<name>",
        ConfigValueKind::String,
        ConfigTargetSupport::SharedAndLocal,
        None,
        None,
        &[],
    );
    push(
        "plugins.<name>",
        ConfigValueKind::Object,
        ConfigTargetSupport::SharedAndLocal,
        Some("vulcan plugin set"),
        None,
        &[],
    );
    push(
        "plugins.<name>.enabled",
        ConfigValueKind::Boolean,
        ConfigTargetSupport::SharedAndLocal,
        Some("vulcan plugin enable"),
        Some(TomlValue::Boolean(true)),
        &[],
    );
    push(
        "plugins.<name>.path",
        ConfigValueKind::String,
        ConfigTargetSupport::SharedAndLocal,
        Some("vulcan plugin set --path"),
        None,
        &[],
    );
    push(
        "plugins.<name>.events",
        ConfigValueKind::Array,
        ConfigTargetSupport::SharedAndLocal,
        Some("vulcan plugin set --add-event"),
        Some(TomlValue::Array(Vec::new())),
        &[],
    );
    push(
        "plugins.<name>.sandbox",
        ConfigValueKind::Enum,
        ConfigTargetSupport::SharedAndLocal,
        Some("vulcan plugin set --sandbox"),
        None,
        &["strict", "fs", "net", "none"],
    );
    push(
        "plugins.<name>.permission_profile",
        ConfigValueKind::String,
        ConfigTargetSupport::SharedAndLocal,
        Some("vulcan plugin set --permission-profile"),
        None,
        &[],
    );
    push(
        "plugins.<name>.description",
        ConfigValueKind::String,
        ConfigTargetSupport::SharedAndLocal,
        Some("vulcan plugin set --description"),
        None,
        &[],
    );
    push(
        "permissions.profiles.<name>",
        ConfigValueKind::Object,
        ConfigTargetSupport::SharedAndLocal,
        Some("vulcan config permissions profile create"),
        None,
        &[],
    );
    for key in [
        "permissions.profiles.<name>.read",
        "permissions.profiles.<name>.write",
        "permissions.profiles.<name>.refactor",
        "permissions.profiles.<name>.network",
    ] {
        push(
            key,
            ConfigValueKind::Flexible,
            ConfigTargetSupport::SharedAndLocal,
            Some("vulcan config permissions profile set"),
            None,
            &[],
        );
    }
    for key in [
        "permissions.profiles.<name>.git",
        "permissions.profiles.<name>.index",
        "permissions.profiles.<name>.execute",
        "permissions.profiles.<name>.shell",
    ] {
        push(
            key,
            ConfigValueKind::Enum,
            ConfigTargetSupport::SharedAndLocal,
            Some("vulcan config permissions profile set"),
            None,
            &["allow", "deny"],
        );
    }
    push(
        "permissions.profiles.<name>.config",
        ConfigValueKind::Enum,
        ConfigTargetSupport::SharedAndLocal,
        Some("vulcan config permissions profile set"),
        None,
        &["none", "read", "write"],
    );
    for key in [
        "permissions.profiles.<name>.cpu_limit_ms",
        "permissions.profiles.<name>.memory_limit_mb",
        "permissions.profiles.<name>.stack_limit_kb",
    ] {
        push(
            key,
            ConfigValueKind::Integer,
            ConfigTargetSupport::SharedAndLocal,
            Some("vulcan config permissions profile set"),
            None,
            &["unlimited"],
        );
    }
    push(
        "permissions.profiles.<name>.policy_hook",
        ConfigValueKind::String,
        ConfigTargetSupport::SharedAndLocal,
        Some("vulcan config permissions profile set"),
        None,
        &[],
    );
    push(
        "export.profiles.<name>",
        ConfigValueKind::Object,
        ConfigTargetSupport::SharedOnly,
        Some("vulcan export profile create"),
        None,
        &[],
    );
    push(
        "export.profiles.<name>.format",
        ConfigValueKind::Enum,
        ConfigTargetSupport::SharedOnly,
        Some("vulcan export profile set"),
        None,
        &[
            "markdown",
            "json",
            "csv",
            "graph",
            "epub",
            "zip",
            "sqlite",
            "search-index",
        ],
    );
    for key in [
        "export.profiles.<name>.query",
        "export.profiles.<name>.query_json",
        "export.profiles.<name>.path",
        "export.profiles.<name>.title",
        "export.profiles.<name>.author",
    ] {
        push(
            key,
            ConfigValueKind::String,
            ConfigTargetSupport::SharedOnly,
            Some("vulcan export profile set"),
            None,
            &[],
        );
    }
    push(
        "export.profiles.<name>.toc",
        ConfigValueKind::Enum,
        ConfigTargetSupport::SharedOnly,
        Some("vulcan export profile set"),
        None,
        &["tree", "flat"],
    );
    for key in [
        "export.profiles.<name>.backlinks",
        "export.profiles.<name>.frontmatter",
        "export.profiles.<name>.pretty",
    ] {
        push(
            key,
            ConfigValueKind::Boolean,
            ConfigTargetSupport::SharedOnly,
            Some("vulcan export profile set"),
            None,
            &[],
        );
    }
    push(
        "export.profiles.<name>.graph_format",
        ConfigValueKind::Enum,
        ConfigTargetSupport::SharedOnly,
        Some("vulcan export profile set"),
        None,
        &["json", "dot", "graphml"],
    );
    push(
        "export.profiles.<name>.content_transforms",
        ConfigValueKind::Array,
        ConfigTargetSupport::SharedOnly,
        Some("vulcan export profile rule add"),
        Some(TomlValue::Array(Vec::new())),
        &[],
    );
    push(
        "site.profiles.<name>",
        ConfigValueKind::Object,
        ConfigTargetSupport::SharedAndLocal,
        Some("vulcan config set"),
        None,
        &[],
    );
    for key in [
        "site.profiles.<name>.title",
        "site.profiles.<name>.page_title_template",
        "site.profiles.<name>.base_url",
        "site.profiles.<name>.output_dir",
        "site.profiles.<name>.home",
        "site.profiles.<name>.language",
        "site.profiles.<name>.theme",
        "site.profiles.<name>.favicon",
        "site.profiles.<name>.logo",
        "site.profiles.<name>.include_query",
        "site.profiles.<name>.include_query_json",
    ] {
        push(
            key,
            ConfigValueKind::String,
            ConfigTargetSupport::SharedAndLocal,
            Some("vulcan config set"),
            None,
            &[],
        );
    }
    for key in [
        "site.profiles.<name>.search",
        "site.profiles.<name>.graph",
        "site.profiles.<name>.backlinks",
        "site.profiles.<name>.rss",
    ] {
        push(
            key,
            ConfigValueKind::Boolean,
            ConfigTargetSupport::SharedAndLocal,
            Some("vulcan config set"),
            None,
            &[],
        );
    }
    for key in [
        "site.profiles.<name>.extra_css",
        "site.profiles.<name>.extra_js",
        "site.profiles.<name>.include_paths",
        "site.profiles.<name>.include_folders",
        "site.profiles.<name>.exclude_paths",
        "site.profiles.<name>.exclude_folders",
        "site.profiles.<name>.exclude_tags",
        "site.profiles.<name>.asset_policy.include_folders",
        "site.profiles.<name>.content_transforms",
    ] {
        push(
            key,
            ConfigValueKind::Array,
            ConfigTargetSupport::SharedAndLocal,
            Some("vulcan config set"),
            Some(TomlValue::Array(Vec::new())),
            &[],
        );
    }
    push(
        "site.profiles.<name>.link_policy",
        ConfigValueKind::Enum,
        ConfigTargetSupport::SharedAndLocal,
        Some("vulcan config set"),
        None,
        &["error", "warn", "drop_link", "render_plain_text"],
    );
    push(
        "site.profiles.<name>.asset_policy.mode",
        ConfigValueKind::Enum,
        ConfigTargetSupport::SharedAndLocal,
        Some("vulcan config set"),
        None,
        &["copy_referenced", "error_on_missing"],
    );
    push(
        "site.profiles.<name>.dataview_js",
        ConfigValueKind::Enum,
        ConfigTargetSupport::SharedAndLocal,
        Some("vulcan config set"),
        None,
        &["off", "static"],
    );

    descriptors
}

fn parse_default_config_template_seeds() -> Vec<TemplateDescriptorSeed> {
    let mut entries = Vec::new();
    let mut seen = BTreeSet::<Vec<String>>::new();
    let mut current_section = Vec::<String>::new();
    let mut in_array_table = false;

    for line in default_config_template().lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            continue;
        }
        let content = trimmed.trim_start_matches('#').trim_start();
        if content.is_empty() {
            continue;
        }
        if let Some(section) = content
            .strip_prefix("[[")
            .and_then(|value| value.strip_suffix("]]"))
        {
            current_section = parse_key_segments(section);
            in_array_table = true;
            if seen.insert(current_section.clone()) {
                entries.push(TemplateDescriptorSeed {
                    storage_segments: current_section.clone(),
                    kind: ConfigValueKind::Array,
                    sample_value: Some(TomlValue::Array(Vec::new())),
                    enum_values: Vec::new(),
                });
            }
            continue;
        }
        if let Some(section) = content
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
        {
            current_section = parse_key_segments(section);
            in_array_table = false;
            continue;
        }
        if in_array_table {
            continue;
        }
        let Some((raw_key, raw_value)) = content.split_once('=') else {
            continue;
        };
        let key = raw_key.trim();
        if !is_valid_template_key(key) {
            continue;
        }
        let (value_literal, inline_comment) = split_toml_value_and_comment(raw_value.trim());
        let sample_value = parse_template_sample_value(&value_literal);
        let kind = sample_value
            .as_ref()
            .map_or(ConfigValueKind::String, config_value_kind_from_toml);
        let enum_values = parse_enum_values(inline_comment.as_ref());
        let mut storage_segments = current_section.clone();
        storage_segments.push(key.to_string());
        if seen.insert(storage_segments.clone()) {
            entries.push(TemplateDescriptorSeed {
                storage_segments,
                kind: if enum_values.is_empty() {
                    kind
                } else {
                    ConfigValueKind::Enum
                },
                sample_value,
                enum_values,
            });
        }
    }

    entries
}

fn is_valid_template_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

fn split_toml_value_and_comment(value: &str) -> (String, Option<String>) {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for (index, character) in value.char_indices() {
        if in_double {
            if escaped {
                escaped = false;
                continue;
            }
            match character {
                '\\' => escaped = true,
                '"' => in_double = false,
                _ => {}
            }
            continue;
        }
        if in_single {
            if character == '\'' {
                in_single = false;
            }
            continue;
        }
        match character {
            '"' => in_double = true,
            '\'' => in_single = true,
            '#' => {
                return (
                    value[..index].trim().to_string(),
                    Some(value[index + 1..].trim().to_string()),
                );
            }
            _ => {}
        }
    }

    (value.trim().to_string(), None)
}

fn parse_enum_values(comment: Option<&String>) -> Vec<String> {
    let Some(comment) = comment else {
        return Vec::new();
    };
    if !comment.contains('|') {
        return Vec::new();
    }
    comment
        .split('|')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_template_sample_value(value_literal: &str) -> Option<TomlValue> {
    let wrapped = format!("value = {value_literal}\n");
    wrapped
        .parse::<TomlValue>()
        .ok()
        .and_then(|value| value.get("value").cloned())
}

fn default_config_value_map() -> BTreeMap<String, TomlValue> {
    let mut values = BTreeMap::new();
    let default_toml = TomlValue::try_from(VaultConfig::default())
        .unwrap_or_else(|_| TomlValue::Table(toml::map::Map::new()));
    collect_toml_leaf_values(&default_toml, &mut Vec::new(), &mut values);
    values
        .into_iter()
        .map(|(storage_segments, value)| {
            (
                storage_path_to_display_path(&storage_segments).join("."),
                value,
            )
        })
        .collect()
}

fn collect_toml_leaf_values(
    value: &TomlValue,
    prefix: &mut Vec<String>,
    out: &mut BTreeMap<Vec<String>, TomlValue>,
) {
    match value {
        TomlValue::Table(table) => {
            for (key, child) in table {
                prefix.push(key.clone());
                collect_toml_leaf_values(child, prefix, out);
                prefix.pop();
            }
        }
        _ => {
            if !prefix.is_empty() {
                out.insert(prefix.clone(), value.clone());
            }
        }
    }
}

fn config_value_kind_from_toml(value: &TomlValue) -> ConfigValueKind {
    match value {
        TomlValue::String(_) | TomlValue::Datetime(_) => ConfigValueKind::String,
        TomlValue::Integer(_) => ConfigValueKind::Integer,
        TomlValue::Float(_) => ConfigValueKind::Float,
        TomlValue::Boolean(_) => ConfigValueKind::Boolean,
        TomlValue::Array(_) => ConfigValueKind::Array,
        TomlValue::Table(_) => ConfigValueKind::Object,
    }
}

fn parse_key_segments(path: &str) -> Vec<String> {
    path.split('.').map(ToOwned::to_owned).collect()
}

fn is_placeholder_segment(segment: &str) -> bool {
    segment.starts_with('<') && segment.ends_with('>') && segment.len() > 2
}

fn descriptor_matches_key(
    descriptor: &ConfigDescriptor,
    key: &str,
) -> Option<ConfigDescriptorMatch> {
    let key_segments = parse_key_segments(key);
    let descriptor_segments = parse_key_segments(&descriptor.key);
    if key_segments.len() != descriptor_segments.len() {
        return None;
    }

    let mut placeholders = BTreeMap::<String, String>::new();
    for (expected, actual) in descriptor_segments.iter().zip(&key_segments) {
        if is_placeholder_segment(expected) {
            placeholders.insert(expected.clone(), actual.clone());
        } else if expected != actual {
            return None;
        }
    }

    let storage_segments = parse_key_segments(&descriptor.storage_key)
        .into_iter()
        .map(|segment| placeholders.get(&segment).cloned().unwrap_or(segment))
        .collect::<Vec<_>>();

    Some(ConfigDescriptorMatch {
        descriptor: descriptor.clone(),
        storage_segments,
    })
}

fn resolve_config_descriptor(key: &str) -> Result<ConfigDescriptorMatch, AppError> {
    let catalog = config_descriptor_catalog();

    if let Some(descriptor_match) = catalog
        .iter()
        .filter(|descriptor| !descriptor.key.contains('<'))
        .find_map(|descriptor| descriptor_matches_key(descriptor, key))
    {
        return Ok(descriptor_match);
    }

    catalog
        .iter()
        .filter(|descriptor| descriptor.key.contains('<'))
        .find_map(|descriptor| descriptor_matches_key(descriptor, key))
        .ok_or_else(|| AppError::operation(format!("unknown config key `{key}`")))
}

fn config_target_support_for_key(key: &str) -> ConfigTargetSupport {
    if key == "export" || key.starts_with("export.") {
        ConfigTargetSupport::SharedOnly
    } else {
        ConfigTargetSupport::SharedAndLocal
    }
}

fn is_sample_dynamic_storage_path(segments: &[String]) -> bool {
    matches!(
        segments,
        [first, _, ..] if first == "plugins"
    ) || matches!(
        segments,
        [first, second, _, ..] if first == "permissions" && second == "profiles"
    ) || matches!(
        segments,
        [first, second, _, ..] if first == "export" && second == "profiles"
    ) || matches!(
        segments,
        [first, second, _, ..] if first == "site" && second == "profiles"
    )
}

fn storage_path_to_display_path(storage_segments: &[String]) -> Vec<String> {
    match storage_segments {
        [first, second] if first == "links" && second == "resolution" => {
            vec!["link_resolution".to_string()]
        }
        [first, second] if first == "links" && second == "style" => {
            vec!["link_style".to_string()]
        }
        [first, second] if first == "links" && second == "attachment_folder" => {
            vec!["attachment_folder".to_string()]
        }
        _ => storage_segments.to_vec(),
    }
}

fn display_path_to_storage_path(display_segments: &[String]) -> Vec<String> {
    match display_segments {
        [segment] if segment == "link_resolution" => {
            vec!["links".to_string(), "resolution".to_string()]
        }
        [segment] if segment == "link_style" => {
            vec!["links".to_string(), "style".to_string()]
        }
        [segment] if segment == "attachment_folder" => {
            vec!["links".to_string(), "attachment_folder".to_string()]
        }
        _ => display_segments.to_vec(),
    }
}

fn display_key_to_storage_key(key: &str) -> String {
    let segments = display_path_to_storage_path(&parse_key_segments(key));
    segments.join(".")
}

fn category_descriptor(display_segments: &[String]) -> CategoryDescriptor {
    match display_segments.first().map(String::as_str) {
        Some("link_resolution" | "link_style" | "attachment_folder" | "strict_line_breaks") => {
            CategoryDescriptor {
                key: "links",
                title: "Links",
                description:
                    "Link formatting, resolution rules, attachment paths, and Markdown compatibility.",
            }
        }
        Some("property_types") => CategoryDescriptor {
            key: "properties",
            title: "Properties",
            description: "Typed frontmatter and property parsing overrides.",
        },
        Some("templates") => CategoryDescriptor {
            key: "templates",
            title: "Templates",
            description: "Template folders, triggers, and Templater-compatible defaults.",
        },
        Some("periodic") => CategoryDescriptor {
            key: "periodic",
            title: "Periodic Notes",
            description: "Daily, weekly, monthly, quarterly, and yearly note generation settings.",
        },
        Some("tasks") => CategoryDescriptor {
            key: "tasks",
            title: "Tasks",
            description: "Task query defaults, statuses, and recurrence behavior.",
        },
        Some("tasknotes") => CategoryDescriptor {
            key: "tasknotes",
            title: "TaskNotes",
            description: "TaskNotes folders, statuses, NLP, pomodoro, and saved views.",
        },
        Some("kanban") => CategoryDescriptor {
            key: "kanban",
            title: "Kanban",
            description: "Kanban board formatting, archiving, and display preferences.",
        },
        Some("dataview") => CategoryDescriptor {
            key: "dataview",
            title: "Dataview",
            description: "Dataview compatibility flags, rendering behavior, and JS limits.",
        },
        Some("js_runtime") => CategoryDescriptor {
            key: "js_runtime",
            title: "JS Runtime",
            description: "Sandbox defaults, runtime memory limits, and script locations.",
        },
        Some("web") => CategoryDescriptor {
            key: "web",
            title: "Web",
            description: "Web search backend selection and API endpoint configuration.",
        },
        Some("plugins") => CategoryDescriptor {
            key: "plugins",
            title: "Plugins",
            description: "Registered event-driven plugin settings for the current vault.",
        },
        Some("permissions") => CategoryDescriptor {
            key: "permissions",
            title: "Permissions",
            description: "Static permission profiles used by plugins, MCP, and scripted callers.",
        },
        Some("aliases") => CategoryDescriptor {
            key: "aliases",
            title: "Aliases",
            description: "Custom top-level CLI command aliases expanded before clap parsing.",
        },
        Some("export") => CategoryDescriptor {
            key: "export",
            title: "Export Profiles",
            description: "Named export profiles stored in config and managed by dedicated export commands.",
        },
        Some("site") => CategoryDescriptor {
            key: "site",
            title: "Static Site",
            description: "Static-site publication profiles, filters, route policies, and theme assets.",
        },
        Some(_) | None => CategoryDescriptor {
            key: "general",
            title: "General",
            description: "Top-level vault configuration not covered by a more specific section.",
        },
    }
}

fn config_path_description(path: &str) -> String {
    match path {
        "link_resolution" => "Choose whether new links resolve relative to the current file or the vault root.".to_string(),
        "link_style" => "Select wikilink or Markdown link formatting for generated links.".to_string(),
        "attachment_folder" => "Override the preferred folder for new attachments.".to_string(),
        "strict_line_breaks" => "Mirror Obsidian's strict line break behavior when rendering Markdown.".to_string(),
        _ if path.starts_with("periodic.") => {
            "Periodic note folder, filename format, template, cadence, and schedule heading.".to_string()
        }
        _ if path.starts_with("templates.") => {
            "Template discovery, file triggers, folder mappings, and shell integration.".to_string()
        }
        _ if path.starts_with("tasks.") => {
            "Task query defaults, status sets, created-date behavior, and recurrence settings.".to_string()
        }
        _ if path.starts_with("tasknotes.") => {
            "TaskNotes task storage, metadata mapping, automation defaults, and saved view settings.".to_string()
        }
        _ if path.starts_with("kanban.") => {
            "Kanban board metadata keys, archiving, layout, and card creation settings.".to_string()
        }
        _ if path.starts_with("dataview.") => {
            "Dataview rendering compatibility, inline query prefixes, and JS execution limits.".to_string()
        }
        _ if path.starts_with("js_runtime.") => {
            "Default sandbox, memory, stack, timeout, and script folder settings for `vulcan run`.".to_string()
        }
        _ if path.starts_with("web.search.") => {
            "Configure the preferred web search provider, API key env var, and base URL.".to_string()
        }
        _ if path.starts_with("web.") => {
            "Shared web client settings such as the user agent used by fetch/search helpers.".to_string()
        }
        _ if path.starts_with("permissions.profiles.") => {
            "Static permission profile rule used to restrict reads, writes, network, shell, or runtime limits.".to_string()
        }
        _ if path.starts_with("plugins.") => {
            "Per-plugin registration, hook subscription, sandbox, and permission profile settings.".to_string()
        }
        _ if path.starts_with("aliases.") => {
            "Alias expansion for short custom commands like `today = \"query --format count\"`.".to_string()
        }
        _ if path.starts_with("property_types.") => {
            "Explicit type overrides for frontmatter properties discovered in the vault.".to_string()
        }
        _ if path.starts_with("export.profiles.") => {
            "Named export profile metadata; dedicated `export profile` commands are preferred for edits.".to_string()
        }
        _ if path == "site.profiles.<name>.page_title_template" => {
            "Template for the HTML `<title>` tag on built pages. Supported placeholders: `{page}`, `{site}`, and `{profile}`.".to_string()
        }
        _ if path.starts_with("site.profiles.") => {
            "Static-site publication profile metadata, publish filters, theme assets, and route policy settings.".to_string()
        }
        _ => format!("Edit `{path}` in `.vulcan/config.toml` or `.vulcan/config.local.toml`."),
    }
}

fn preferred_command_for_key(path: &str) -> Option<String> {
    match path {
        _ if path.starts_with("aliases.") => Some("vulcan config alias set".to_string()),
        _ if path.starts_with("permissions.profiles.") => {
            Some("vulcan config permissions profile set".to_string())
        }
        _ if path.starts_with("plugins.") => Some("vulcan plugin set".to_string()),
        _ if path.starts_with("export.profiles.") => Some("vulcan export profile set".to_string()),
        _ if path.starts_with("site.profiles.") => Some("vulcan config set".to_string()),
        _ => None,
    }
}

fn config_path_examples(path: &str) -> Vec<String> {
    match path {
        _ if path == "aliases.<name>" => vec![
            "vulcan config alias set ship \"query --where 'status = shipped'\"".to_string(),
        ],
        _ if path == "permissions.profiles.<name>" => vec![
            "vulcan config permissions profile create agent --clone readonly".to_string(),
        ],
        _ if path.starts_with("permissions.profiles.<name>.") => vec![
            "vulcan config permissions profile set agent network '{ allow = true, domains = [\"example.com\"] }'".to_string(),
        ],
        _ if path.starts_with("plugins.<name>.") || path == "plugins.<name>" => vec![
            "vulcan plugin set lint --path .vulcan/plugins/lint.js --add-event on_pre_commit --sandbox strict".to_string(),
        ],
        _ if path.starts_with("export.profiles.<name>.") || path == "export.profiles.<name>" => vec![
            "vulcan export profile create team-book --format epub 'from notes' -o exports/team.epub".to_string(),
        ],
        _ if path == "site.profiles.<name>" => vec![
            "vulcan config set site.profiles.public '{}'".to_string(),
        ],
        _ if path == "site.profiles.<name>.page_title_template" => vec![
            r#"vulcan config set site.profiles.public.page_title_template '"{site} :: {page}"'"#.to_string(),
        ],
        _ if path.starts_with("site.profiles.<name>.") => vec![
            r#"vulcan config set site.profiles.public.title '"Public Notes"'"#.to_string(),
            r#"vulcan config set site.profiles.public.include_paths '["Home.md", "Garden/Now.md"]'"#.to_string(),
        ],
        _ => vec![format!("vulcan config set {path} <value>")],
    }
}

fn render_toml_summary(value: &TomlValue) -> String {
    match value {
        TomlValue::String(text) => text.clone(),
        TomlValue::Integer(number) => number.to_string(),
        TomlValue::Float(number) => number.to_string(),
        TomlValue::Boolean(value_bool) => value_bool.to_string(),
        TomlValue::Datetime(datetime) => datetime.to_string(),
        TomlValue::Array(values) => format!(
            "[{} item{}]",
            values.len(),
            if values.len() == 1 { "" } else { "s" }
        ),
        TomlValue::Table(values) => format!(
            "{{{} key{}}}",
            values.len(),
            if values.len() == 1 { "" } else { "s" }
        ),
    }
}

#[derive(Debug, Clone)]
struct ConfigMutationPlan {
    config_path: PathBuf,
    created_config: bool,
    updated: bool,
    diagnostics: Vec<ConfigDiagnostic>,
    absolute_config_path: PathBuf,
    rendered_contents: String,
}

pub fn build_config_list_report(
    paths: &VaultPaths,
    section: Option<&str>,
) -> Result<ConfigListReport, AppError> {
    let display = load_display_config_state(paths)?;
    let shared_toml = load_config_file_toml(paths.config_file())?;
    let local_toml = load_config_file_toml(paths.local_config_file())?;
    let mut descriptors = BTreeMap::<String, ConfigDescriptor>::new();
    let catalog = config_descriptor_catalog();

    for descriptor in &catalog {
        descriptors.insert(descriptor.key.clone(), descriptor.clone());
    }

    let known_paths = collect_known_display_paths(&display.toml, &shared_toml, &local_toml);
    for path in &known_paths {
        for descriptor in catalog
            .iter()
            .filter(|descriptor| descriptor.key.contains('<'))
        {
            if let Some(concrete) = instantiate_descriptor(descriptor, path) {
                descriptors.insert(concrete.key.clone(), concrete);
            }
        }
        instantiate_dynamic_family_descriptors_for_parent(&mut descriptors, &catalog, path);
    }

    let mut entries = descriptors
        .into_values()
        .filter(|descriptor| {
            section.is_none_or(|filter| config_key_matches_filter(&descriptor.key, filter))
        })
        .map(|descriptor| {
            build_config_list_entry(&descriptor, &display.toml, &shared_toml, &local_toml)
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| left.key.cmp(&right.key));

    Ok(ConfigListReport {
        section: section.map(ToOwned::to_owned),
        entries,
        diagnostics: display.diagnostics,
    })
}

pub fn build_config_list_report_from_overrides(
    paths: &VaultPaths,
    shared_toml: &TomlValue,
    local_toml: &TomlValue,
    section: Option<&str>,
) -> Result<ConfigListReport, AppError> {
    let display = load_display_config_state_from_overrides(paths, shared_toml, local_toml)?;
    let mut descriptors = BTreeMap::<String, ConfigDescriptor>::new();
    let catalog = config_descriptor_catalog();

    for descriptor in &catalog {
        descriptors.insert(descriptor.key.clone(), descriptor.clone());
    }

    let known_paths = collect_known_display_paths(&display.toml, shared_toml, local_toml);
    for path in &known_paths {
        for descriptor in catalog
            .iter()
            .filter(|descriptor| descriptor.key.contains('<'))
        {
            if let Some(concrete) = instantiate_descriptor(descriptor, path) {
                descriptors.insert(concrete.key.clone(), concrete);
            }
        }
        instantiate_dynamic_family_descriptors_for_parent(&mut descriptors, &catalog, path);
    }

    let mut entries = descriptors
        .into_values()
        .filter(|descriptor| {
            section.is_none_or(|filter| config_key_matches_filter(&descriptor.key, filter))
        })
        .map(|descriptor| {
            build_config_list_entry(&descriptor, &display.toml, shared_toml, local_toml)
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| left.key.cmp(&right.key));

    Ok(ConfigListReport {
        section: section.map(ToOwned::to_owned),
        entries,
        diagnostics: display.diagnostics,
    })
}

fn collect_known_display_paths(
    effective_toml: &TomlValue,
    shared_toml: &TomlValue,
    local_toml: &TomlValue,
) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for value in [effective_toml, shared_toml, local_toml] {
        let mut raw_paths = BTreeSet::<Vec<String>>::new();
        collect_toml_all_paths(value, &mut Vec::new(), &mut raw_paths);
        for path in raw_paths {
            paths.insert(storage_path_to_display_path(&path).join("."));
        }
    }
    paths
}

fn collect_toml_all_paths(
    value: &TomlValue,
    prefix: &mut Vec<String>,
    out: &mut BTreeSet<Vec<String>>,
) {
    if !prefix.is_empty() {
        out.insert(prefix.clone());
    }
    if let TomlValue::Table(table) = value {
        for (key, child) in table {
            prefix.push(key.clone());
            collect_toml_all_paths(child, prefix, out);
            prefix.pop();
        }
    }
}

fn instantiate_descriptor(descriptor: &ConfigDescriptor, key: &str) -> Option<ConfigDescriptor> {
    let descriptor_match = descriptor_matches_key(descriptor, key)?;
    let mut concrete = descriptor.clone();
    concrete.key = key.to_string();
    concrete.storage_key = descriptor_match.storage_segments.join(".");
    Some(concrete)
}

fn descriptor_placeholder_bindings(
    descriptor: &ConfigDescriptor,
    key: &str,
) -> Option<BTreeMap<String, String>> {
    let key_segments = parse_key_segments(key);
    let descriptor_segments = parse_key_segments(&descriptor.key);
    if key_segments.len() != descriptor_segments.len() {
        return None;
    }

    let mut placeholders = BTreeMap::<String, String>::new();
    for (expected, actual) in descriptor_segments.iter().zip(&key_segments) {
        if is_placeholder_segment(expected) {
            placeholders.insert(expected.clone(), actual.clone());
        } else if expected != actual {
            return None;
        }
    }

    Some(placeholders)
}

fn instantiate_dynamic_family_descriptors_for_parent(
    descriptors: &mut BTreeMap<String, ConfigDescriptor>,
    catalog: &[ConfigDescriptor],
    concrete_parent_key: &str,
) {
    for parent in catalog.iter().filter(|descriptor| {
        descriptor.key.contains('<') && descriptor.kind == ConfigValueKind::Object
    }) {
        let Some(bindings) = descriptor_placeholder_bindings(parent, concrete_parent_key) else {
            continue;
        };
        let family_prefix = format!("{}.", parent.key);
        for descriptor in catalog.iter().filter(|descriptor| {
            descriptor.key == parent.key || descriptor.key.starts_with(&family_prefix)
        }) {
            let concrete_key = parse_key_segments(&descriptor.key)
                .into_iter()
                .map(|segment| bindings.get(&segment).cloned().unwrap_or(segment))
                .collect::<Vec<_>>()
                .join(".");
            if let Some(concrete) = instantiate_descriptor(descriptor, &concrete_key) {
                descriptors.insert(concrete.key.clone(), concrete);
            }
        }
    }
}

fn config_key_matches_filter(key: &str, filter: &str) -> bool {
    key == filter
        || key.starts_with(&format!("{filter}."))
        || category_descriptor(&parse_key_segments(key)).key == filter
}

fn build_config_list_entry(
    descriptor: &ConfigDescriptor,
    effective_toml: &TomlValue,
    shared_toml: &TomlValue,
    local_toml: &TomlValue,
) -> Result<ConfigListEntry, AppError> {
    let effective_value = if descriptor.key.contains('<') {
        None
    } else {
        get_toml_value_by_key(effective_toml, &descriptor.key, false)
    };
    let shared_value = if descriptor.key.contains('<') {
        None
    } else {
        get_toml_value_by_key(shared_toml, &descriptor.storage_key, true)
    };
    let local_value = if descriptor.key.contains('<') {
        None
    } else {
        get_toml_value_by_key(local_toml, &descriptor.storage_key, true)
    };

    let effective_json = effective_value
        .as_ref()
        .map(|value| serde_json::to_value(value).map_err(AppError::operation))
        .transpose()?;
    let value_source = if local_value.is_some() {
        ConfigValueSource::LocalOverride
    } else if shared_value.is_some() {
        ConfigValueSource::SharedOverride
    } else if effective_json.is_none() {
        ConfigValueSource::Unset
    } else if descriptor.default_value == effective_json {
        ConfigValueSource::Default
    } else {
        ConfigValueSource::ObsidianImport
    };

    Ok(ConfigListEntry {
        key: descriptor.key.clone(),
        storage_key: descriptor.storage_key.clone(),
        section: descriptor.section.clone(),
        section_title: descriptor.section_title.clone(),
        section_description: descriptor.section_description.clone(),
        description: descriptor.description.clone(),
        kind: descriptor.kind.clone(),
        enum_values: descriptor.enum_values.clone(),
        target_support: descriptor.target_support,
        creatable_when_absent: descriptor.creatable_when_absent,
        preferred_command: descriptor.preferred_command.clone(),
        default_value: descriptor.default_value.clone(),
        default_display: descriptor.default_display.clone(),
        examples: descriptor.examples.clone(),
        effective_value: effective_json,
        value_source,
    })
}

fn get_toml_value_by_key(value: &TomlValue, key: &str, storage: bool) -> Option<TomlValue> {
    let segments = if storage {
        parse_key_segments(key)
    } else {
        display_path_to_storage_path(&parse_key_segments(key))
    };
    get_toml_value(value, &segments).cloned()
}

fn get_toml_value<'a>(value: &'a TomlValue, path: &[String]) -> Option<&'a TomlValue> {
    let mut current = value;
    for segment in path {
        current = current.as_table()?.get(segment)?;
    }
    Some(current)
}

fn plan_config_value_write(
    paths: &VaultPaths,
    key: &str,
    value: TomlValue,
    target: ConfigTarget,
    _dry_run: bool,
) -> Result<ConfigMutationPlan, AppError> {
    let descriptor_match = resolve_config_descriptor(key)?;
    if !descriptor_match.descriptor.target_support.allows(target) {
        return Err(AppError::operation(format!(
            "config key `{key}` is not writable in {}",
            match target {
                ConfigTarget::Shared => ".vulcan/config.toml",
                ConfigTarget::Local => ".vulcan/config.local.toml",
            }
        )));
    }
    validate_config_value(&descriptor_match.descriptor, &value)?;

    let absolute_config_path = target.path(paths);
    let created_config = !absolute_config_path.exists();
    let existing_contents = fs::read_to_string(&absolute_config_path).ok();
    let mut config_value = load_config_file_toml(&absolute_config_path)?;
    let storage_refs = descriptor_match
        .storage_segments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let existed_in_target = config_toml_path_exists(&config_value, &storage_refs);
    if !descriptor_match.descriptor.creatable_when_absent && !existed_in_target {
        return Err(AppError::operation(format!(
            "config key `{key}` cannot be created when absent"
        )));
    }

    set_config_toml_value(&mut config_value, &storage_refs, value)?;
    let rendered_contents = toml::to_string_pretty(&config_value).map_err(AppError::operation)?;
    validate_vulcan_overrides_toml(&rendered_contents).map_err(AppError::operation)?;
    let updated = existing_contents.as_deref() != Some(rendered_contents.as_str());
    let diagnostics = normalize_config_diagnostics(paths, &load_vault_config(paths).diagnostics);

    Ok(ConfigMutationPlan {
        config_path: relativize_config_path(paths, &absolute_config_path),
        created_config,
        updated,
        diagnostics,
        absolute_config_path,
        rendered_contents,
    })
}

fn plan_config_value_unset(
    paths: &VaultPaths,
    key: &str,
    target: ConfigTarget,
    _dry_run: bool,
) -> Result<(ConfigMutationPlan, bool), AppError> {
    let descriptor_match = resolve_config_descriptor(key)?;
    if !descriptor_match.descriptor.target_support.allows(target) {
        return Err(AppError::operation(format!(
            "config key `{key}` is not writable in {}",
            match target {
                ConfigTarget::Shared => ".vulcan/config.toml",
                ConfigTarget::Local => ".vulcan/config.local.toml",
            }
        )));
    }

    let absolute_config_path = target.path(paths);
    let created_config = !absolute_config_path.exists();
    let existing_contents = fs::read_to_string(&absolute_config_path).ok();
    let mut config_value = load_config_file_toml(&absolute_config_path)?;
    let storage_refs = descriptor_match
        .storage_segments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let removed = remove_config_toml_value(&mut config_value, &storage_refs)?;
    let rendered_contents = toml::to_string_pretty(&config_value).map_err(AppError::operation)?;
    validate_vulcan_overrides_toml(&rendered_contents).map_err(AppError::operation)?;
    let updated = removed && existing_contents.as_deref() != Some(rendered_contents.as_str());
    let diagnostics = normalize_config_diagnostics(paths, &load_vault_config(paths).diagnostics);

    Ok((
        ConfigMutationPlan {
            config_path: relativize_config_path(paths, &absolute_config_path),
            created_config,
            updated,
            diagnostics,
            absolute_config_path,
            rendered_contents,
        },
        removed,
    ))
}

fn apply_config_mutation_plan(
    paths: &VaultPaths,
    plan: &ConfigMutationPlan,
) -> Result<Vec<ConfigDiagnostic>, AppError> {
    if plan.updated {
        ensure_vulcan_dir(paths).map_err(AppError::operation)?;
        fs::write(&plan.absolute_config_path, &plan.rendered_contents)
            .map_err(AppError::operation)?;
    }
    Ok(normalize_config_diagnostics(
        paths,
        &load_vault_config(paths).diagnostics,
    ))
}

fn apply_config_operation_to_value(
    config_value: &mut TomlValue,
    operation: &ConfigMutationOperation,
    target: ConfigTarget,
) -> Result<(), AppError> {
    match operation {
        ConfigMutationOperation::Set { key, value } => {
            let descriptor_match = resolve_config_descriptor(key)?;
            if !descriptor_match.descriptor.target_support.allows(target) {
                return Err(AppError::operation(format!(
                    "config key `{key}` is not writable in {}",
                    match target {
                        ConfigTarget::Shared => ".vulcan/config.toml",
                        ConfigTarget::Local => ".vulcan/config.local.toml",
                    }
                )));
            }
            validate_config_value(&descriptor_match.descriptor, value)?;
            let storage_refs = descriptor_match
                .storage_segments
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            let existed_in_target = config_toml_path_exists(config_value, &storage_refs);
            if !descriptor_match.descriptor.creatable_when_absent && !existed_in_target {
                return Err(AppError::operation(format!(
                    "config key `{key}` cannot be created when absent"
                )));
            }
            set_config_toml_value(config_value, &storage_refs, value.clone())?;
        }
        ConfigMutationOperation::Unset { key } => {
            let descriptor_match = resolve_config_descriptor(key)?;
            if !descriptor_match.descriptor.target_support.allows(target) {
                return Err(AppError::operation(format!(
                    "config key `{key}` is not writable in {}",
                    match target {
                        ConfigTarget::Shared => ".vulcan/config.toml",
                        ConfigTarget::Local => ".vulcan/config.local.toml",
                    }
                )));
            }
            let storage_refs = descriptor_match
                .storage_segments
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            remove_config_toml_value(config_value, &storage_refs)?;
        }
    }
    Ok(())
}

fn validate_config_value(descriptor: &ConfigDescriptor, value: &TomlValue) -> Result<(), AppError> {
    match descriptor.kind {
        ConfigValueKind::String => {
            if matches!(value, TomlValue::String(_) | TomlValue::Datetime(_)) {
                Ok(())
            } else {
                Err(AppError::operation(format!(
                    "config key `{}` expects a string value",
                    descriptor.key
                )))
            }
        }
        ConfigValueKind::Integer => {
            if value.as_integer().is_some()
                || (value.as_str().is_some()
                    && descriptor
                        .enum_values
                        .iter()
                        .any(|candidate| candidate == value.as_str().unwrap_or_default()))
            {
                Ok(())
            } else {
                Err(AppError::operation(format!(
                    "config key `{}` expects an integer{}",
                    descriptor.key,
                    if descriptor.enum_values.is_empty() {
                        String::new()
                    } else {
                        format!(" or one of {}", descriptor.enum_values.join(", "))
                    }
                )))
            }
        }
        ConfigValueKind::Float => {
            if value.as_float().is_some() || value.as_integer().is_some() {
                Ok(())
            } else {
                Err(AppError::operation(format!(
                    "config key `{}` expects a numeric value",
                    descriptor.key
                )))
            }
        }
        ConfigValueKind::Boolean => {
            if value.as_bool().is_some() {
                Ok(())
            } else {
                Err(AppError::operation(format!(
                    "config key `{}` expects a boolean value",
                    descriptor.key
                )))
            }
        }
        ConfigValueKind::Array => {
            if value.as_array().is_some() {
                Ok(())
            } else {
                Err(AppError::operation(format!(
                    "config key `{}` expects a TOML array literal",
                    descriptor.key
                )))
            }
        }
        ConfigValueKind::Object => {
            if value.as_table().is_some() {
                Ok(())
            } else {
                Err(AppError::operation(format!(
                    "config key `{}` expects a TOML inline table or table value",
                    descriptor.key
                )))
            }
        }
        ConfigValueKind::Enum => {
            let Some(text) = value.as_str() else {
                return Err(AppError::operation(format!(
                    "config key `{}` expects one of {}",
                    descriptor.key,
                    descriptor.enum_values.join(", ")
                )));
            };
            if descriptor
                .enum_values
                .iter()
                .any(|candidate| candidate == text)
            {
                Ok(())
            } else {
                Err(AppError::operation(format!(
                    "config key `{}` expects one of {}",
                    descriptor.key,
                    descriptor.enum_values.join(", ")
                )))
            }
        }
        ConfigValueKind::Flexible => Ok(()),
    }
}

fn relativize_config_path(paths: &VaultPaths, path: &Path) -> PathBuf {
    let relative_or_original = path
        .strip_prefix(paths.vault_root())
        .map_or_else(|_| path.to_path_buf(), Path::to_path_buf);
    PathBuf::from(relative_or_original.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::{
        apply_config_document_save, apply_config_set_report, build_config_get_report,
        build_config_list_report, build_config_list_report_from_overrides,
        build_config_show_report, build_config_show_report_from_overrides,
        config_descriptor_catalog, config_toml_path_exists, default_config_value_map,
        load_config_file_toml, plan_config_document_save, plan_config_set_report,
        remove_config_toml_value, set_config_toml_value, ConfigValueKind,
    };
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;
    use toml::Value as TomlValue;
    use vulcan_core::{initialize_vulcan_dir, VaultPaths};

    fn test_paths() -> (tempfile::TempDir, VaultPaths) {
        let dir = tempdir().expect("temp dir");
        let vault_root = dir.path().join("vault");
        fs::create_dir_all(&vault_root).expect("vault root should exist");
        let paths = VaultPaths::new(&vault_root);
        initialize_vulcan_dir(&paths).expect("vulcan dir should be initialized");
        (dir, paths)
    }

    #[test]
    fn load_config_file_toml_defaults_missing_files_to_empty_table() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("config.toml");

        let value = load_config_file_toml(&path).expect("missing config should load");
        assert!(value.is_table());
        assert_eq!(value.as_table().expect("table").len(), 0);
    }

    #[test]
    fn set_config_toml_value_creates_nested_tables() {
        let mut value = TomlValue::Table(toml::map::Map::new());

        set_config_toml_value(
            &mut value,
            &["plugins", "lint", "enabled"],
            TomlValue::Boolean(true),
        )
        .expect("config value should be set");

        assert!(config_toml_path_exists(
            &value,
            &["plugins", "lint", "enabled"]
        ));
        assert_eq!(
            value
                .get("plugins")
                .and_then(|plugins| plugins.get("lint"))
                .and_then(|lint| lint.get("enabled"))
                .and_then(TomlValue::as_bool),
            Some(true)
        );
    }

    #[test]
    fn remove_config_toml_value_prunes_empty_tables() {
        let mut value = TomlValue::Table(toml::map::Map::new());
        set_config_toml_value(
            &mut value,
            &["export", "profiles", "team", "title"],
            TomlValue::String("Team".to_string()),
        )
        .expect("config value should be set");

        let removed =
            remove_config_toml_value(&mut value, &["export", "profiles", "team", "title"])
                .expect("config value should be removed");

        assert!(removed);
        assert!(!config_toml_path_exists(
            &value,
            &["export", "profiles", "team", "title"]
        ));
        assert!(!config_toml_path_exists(&value, &["export"]));
    }

    #[test]
    fn load_config_file_toml_parses_existing_files() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("config.toml");
        fs::write(&path, "[plugins.lint]\nenabled = true\n")
            .expect("config file should be written");

        let value = load_config_file_toml(&path).expect("config should parse");
        assert_eq!(
            value
                .get("plugins")
                .and_then(|plugins| plugins.get("lint"))
                .and_then(|lint| lint.get("enabled"))
                .and_then(TomlValue::as_bool),
            Some(true)
        );
    }

    #[test]
    fn build_config_show_report_includes_permission_profile_metadata() {
        let (_dir, paths) = test_paths();
        fs::write(
            paths.config_file(),
            r#"[permissions.profiles.projects_only]
read = { allow = ["folder:Projects/**"] }
"#,
        )
        .expect("config should be written");

        let report = build_config_show_report(&paths, Some("permissions"), Some("projects_only"))
            .expect("permissions section should load");
        let available = report
            .available_permission_profiles
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();

        assert_eq!(report.section.as_deref(), Some("permissions"));
        assert_eq!(
            report.active_permission_profile.as_deref(),
            Some("projects_only")
        );
        assert!(available.contains(&"projects_only"));
        assert!(available.contains(&"readonly"));
        assert!(available.contains(&"unrestricted"));
        assert!(report.config["profiles"]["projects_only"].is_object());
        assert!(report
            .rendered_toml
            .get("profiles")
            .and_then(|profiles| profiles.get("projects_only"))
            .is_some());
    }

    #[test]
    fn build_config_get_report_rejects_section_keys() {
        let (_dir, paths) = test_paths();

        let error =
            build_config_get_report(&paths, "periodic.daily").expect_err("sections should fail");

        assert!(error
            .message()
            .contains("use `vulcan config show periodic.daily` instead"));
    }

    #[test]
    fn plan_and_apply_config_set_report_persists_legacy_aliases() {
        let (_dir, paths) = test_paths();

        let planned = plan_config_set_report(&paths, "link_style", "markdown", false)
            .expect("legacy alias should plan");

        assert_eq!(planned.key, "link_style");
        assert_eq!(planned.value, serde_json::json!("markdown"));
        assert_eq!(planned.config_path, PathBuf::from(".vulcan/config.toml"));
        assert!(planned.created_config);
        assert!(planned.updated);
        assert!(planned.rendered_contents.contains("[links]"));
        assert!(planned.rendered_contents.contains("style = \"markdown\""));

        let applied =
            apply_config_set_report(&paths, planned).expect("planned config should be written");
        let stored = fs::read_to_string(paths.config_file()).expect("config should be readable");
        let loaded =
            build_config_get_report(&paths, "link_style").expect("written config should reload");

        assert_eq!(stored, applied.rendered_contents);
        assert_eq!(loaded.value, serde_json::json!("markdown"));
    }

    #[test]
    fn plan_and_apply_config_document_save_persists_rendered_contents() {
        let (_dir, paths) = test_paths();
        let rendered = "[links]\nstyle = \"markdown\"\n";

        let planned =
            plan_config_document_save(&paths, rendered).expect("document save should plan");
        assert!(planned.created_config);
        assert!(planned.updated);

        let applied =
            apply_config_document_save(&paths, planned).expect("document save should apply");

        assert!(applied.updated);
        assert_eq!(
            fs::read_to_string(paths.config_file()).expect("config should be written"),
            rendered
        );
    }

    #[test]
    fn plan_config_document_save_rejects_invalid_toml() {
        let (_dir, paths) = test_paths();
        let error = plan_config_document_save(&paths, "links = [")
            .expect_err("invalid config should be rejected");
        assert!(error.to_string().contains("parse"));
    }

    #[test]
    fn config_descriptor_catalog_covers_default_keys_and_dynamic_families() {
        let catalog = config_descriptor_catalog();
        let catalog_keys = catalog
            .iter()
            .map(|descriptor| descriptor.key.clone())
            .collect::<BTreeSet<_>>();

        for key in default_config_value_map().keys() {
            assert!(
                catalog_keys.contains(key),
                "descriptor catalog should include default config key `{key}`"
            );
        }

        for key in [
            "aliases.<name>",
            "plugins.<name>",
            "plugins.<name>.sandbox",
            "permissions.profiles.<name>",
            "permissions.profiles.<name>.network",
            "export.profiles.<name>",
            "export.profiles.<name>.format",
            "site.profiles.<name>",
            "site.profiles.<name>.page_title_template",
            "site.profiles.<name>.link_policy",
        ] {
            assert!(
                catalog_keys.contains(key),
                "descriptor catalog should include dynamic config family `{key}`"
            );
        }

        for bogus_key in [
            "web.search.# api_key_env",
            "web.search.# backend",
            "kanban.{ metadata_key",
        ] {
            assert!(
                !catalog_keys.contains(bogus_key),
                "descriptor catalog should not include example-only key `{bogus_key}`"
            );
        }

        let metadata_keys_descriptor = catalog
            .iter()
            .find(|descriptor| descriptor.key == "kanban.metadata_keys")
            .expect("kanban metadata_keys descriptor should exist");
        assert_eq!(metadata_keys_descriptor.kind, ConfigValueKind::Array);
        assert_eq!(
            metadata_keys_descriptor.default_display.as_deref(),
            Some("[0 items]")
        );

        for descriptor in &catalog {
            assert!(
                !descriptor.description.trim().is_empty(),
                "descriptor `{}` should have help text",
                descriptor.key
            );
            assert!(
                !descriptor.examples.is_empty(),
                "descriptor `{}` should include at least one example",
                descriptor.key
            );
        }
    }

    #[test]
    fn build_config_list_report_carries_descriptor_metadata() {
        let (_dir, paths) = test_paths();

        let report = build_config_list_report(&paths, Some("plugins"))
            .expect("config list report should build");
        let placeholder = report
            .entries
            .iter()
            .find(|entry| entry.key == "plugins.<name>.sandbox")
            .expect("plugin sandbox descriptor should exist");

        assert_eq!(placeholder.section, "plugins");
        assert_eq!(placeholder.section_title, "Plugins");
        assert!(!placeholder.section_description.is_empty());
        assert_eq!(placeholder.default_display, None);
        assert!(!placeholder.examples.is_empty());
        assert_eq!(
            placeholder.preferred_command.as_deref(),
            Some("vulcan plugin set --sandbox")
        );
    }

    #[test]
    fn override_reports_reflect_in_memory_site_profile_edits() {
        let (_dir, paths) = test_paths();
        let shared_toml = r#"
[site.profiles.public]
title = "Public Notes"
output_dir = ".vulcan/site/public"
search = true
"#
        .parse::<TomlValue>()
        .expect("shared override should parse");
        let local_toml = r#"
[site.profiles.public]
graph = true
link_policy = "warn"
"#
        .parse::<TomlValue>()
        .expect("local override should parse");

        let list = build_config_list_report_from_overrides(
            &paths,
            &shared_toml,
            &local_toml,
            Some("site"),
        )
        .expect("override list report should build");

        assert!(list
            .entries
            .iter()
            .any(|entry| entry.key == "site.profiles.public"));
        assert!(list
            .entries
            .iter()
            .any(|entry| entry.key == "site.profiles.public.title"));
        assert!(list
            .entries
            .iter()
            .any(|entry| entry.key == "site.profiles.public.graph"));
        assert!(list
            .entries
            .iter()
            .any(|entry| entry.key == "site.profiles.public.link_policy"));
    }

    #[test]
    fn override_reports_reflect_in_memory_permission_profile_edits() {
        let (_dir, paths) = test_paths();
        let shared_toml = r#"
[permissions.profiles.agent]
git = "allow"
"#
        .parse::<TomlValue>()
        .expect("shared override should parse");
        let local_toml = r#"
[permissions.profiles.agent]
write = "all"
"#
        .parse::<TomlValue>()
        .expect("local override should parse");

        let show = build_config_show_report_from_overrides(
            &paths,
            &shared_toml,
            &local_toml,
            Some("permissions"),
            Some("agent"),
        )
        .expect("override show report should build");
        let list = build_config_list_report_from_overrides(
            &paths,
            &shared_toml,
            &local_toml,
            Some("permissions"),
        )
        .expect("override list report should build");

        assert_eq!(show.active_permission_profile.as_deref(), Some("agent"));
        assert!(show
            .rendered_toml
            .as_table()
            .and_then(|table| table.get("profiles"))
            .and_then(TomlValue::as_table)
            .and_then(|profiles| profiles.get("agent"))
            .is_some());
        assert!(list
            .entries
            .iter()
            .any(|entry| entry.key == "permissions.profiles.agent"));
        assert!(list
            .entries
            .iter()
            .any(|entry| entry.key == "permissions.profiles.agent.git"));
        assert!(list
            .entries
            .iter()
            .any(|entry| entry.key == "permissions.profiles.agent.write"));
    }
}
