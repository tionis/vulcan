use crate::AppError;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value as TomlValue;
use vulcan_core::{
    ensure_vulcan_dir, load_permission_profiles, load_vault_config, validate_vulcan_overrides_toml,
    ConfigDiagnostic, PermissionProfile, VaultConfig, VaultPaths,
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
    let loaded = load_vault_config(paths);
    let json_config = serde_json::to_value(&loaded.config).map_err(AppError::operation)?;
    select_config_json_value(&json_config, key)?;

    let key_path = parse_config_path(key, "key")?;
    let storage_path = writable_config_storage_path(&key_path, key)?;
    let value = parse_config_set_value(raw_value);
    let value_json = serde_json::to_value(&value).map_err(AppError::operation)?;

    let absolute_config_path = paths.config_file().to_path_buf();
    let created_config = !absolute_config_path.exists();
    let existing_contents = fs::read_to_string(&absolute_config_path).ok();
    let mut config_value = load_config_file_toml(&absolute_config_path)?;
    set_config_toml_value(&mut config_value, &storage_path, value.clone())?;
    let rendered_contents = toml::to_string_pretty(&config_value).map_err(AppError::operation)?;
    validate_vulcan_overrides_toml(&rendered_contents).map_err(AppError::operation)?;
    let updated = existing_contents.as_deref() != Some(rendered_contents.as_str());

    let diagnostics = if dry_run || !updated {
        normalize_config_diagnostics(paths, &loaded.diagnostics)
    } else {
        normalize_config_diagnostics(paths, &load_vault_config(paths).diagnostics)
    };

    Ok(ConfigSetReport {
        key: key.to_string(),
        value: value_json,
        config_path: relativize_config_path(paths, &absolute_config_path),
        created_config,
        updated,
        dry_run,
        diagnostics,
        absolute_config_path,
        rendered_contents,
    })
}

pub fn apply_config_set_report(
    paths: &VaultPaths,
    mut report: ConfigSetReport,
) -> Result<ConfigSetReport, AppError> {
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

fn writable_config_storage_path<'a>(
    key_path: &[&'a str],
    key: &str,
) -> Result<Vec<&'a str>, AppError> {
    match key_path {
        ["link_resolution"] => Ok(vec!["links", "resolution"]),
        ["link_style"] => Ok(vec!["links", "style"]),
        ["attachment_folder"] => Ok(vec!["links", "attachment_folder"]),
        ["strict_line_breaks"] | ["property_types", ..] => Err(AppError::operation(format!(
            "config key `{key}` is not writable via `config set`"
        ))),
        _ => Ok(key_path.to_vec()),
    }
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
    raw_value
        .parse::<TomlValue>()
        .unwrap_or_else(|_| TomlValue::String(raw_value.to_string()))
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
        apply_config_set_report, build_config_get_report, build_config_show_report,
        config_toml_path_exists, load_config_file_toml, plan_config_set_report,
        remove_config_toml_value, set_config_toml_value,
    };
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
}
