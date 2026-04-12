use crate::AppError;
use std::fs;
use std::path::Path;
use toml::Value as TomlValue;

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

#[cfg(test)]
mod tests {
    use super::{
        config_toml_path_exists, load_config_file_toml, remove_config_toml_value,
        set_config_toml_value,
    };
    use std::fs;
    use tempfile::tempdir;
    use toml::Value as TomlValue;

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
}
