//! Device-global configuration declared in the daemon's `daemon.toml`.
//!
//! `daemon.toml` is device-local by construction (it never lives in a
//! vault), so it may define permission profiles that are valid as device
//! ceilings for every vault, and each `[[vault]]` entry may carry a `config`
//! table with the same schema as that vault's `.vulcan/config.local.toml`.
//! Like `trusted_vaults.json`, the file is read from the user configuration
//! directory directly, so every code path that loads vault configuration sees
//! the same layers, with or without the daemon
//! (`docs/specs/declarative-device-config.md`).

#![allow(clippy::wildcard_imports)]

use super::partial::{PartialPermissionsConfig, PartialVulcanConfig};
use super::*;

pub const DAEMON_CONFIG_FILE_NAME: &str = "daemon.toml";

/// The `daemon.toml` layers that apply to one vault.
#[derive(Debug, Default)]
pub(super) struct DeviceLayer {
    pub(super) path: PathBuf,
    /// Device-global `[permissions]`, as written.
    pub(super) permissions_raw: Option<TomlValue>,
    /// The vault entry's `config` table, as written.
    pub(super) vault_raw: Option<TomlValue>,
}

impl DeviceLayer {
    /// Device-global permissions, parsed.
    pub(super) fn permissions(
        &self,
        diagnostics: &mut Vec<ConfigDiagnostic>,
    ) -> Option<PartialPermissionsConfig> {
        let wrapped = self.permissions_raw.as_ref().map(|permissions| {
            let mut table = toml::map::Map::new();
            table.insert("permissions".to_string(), permissions.clone());
            TomlValue::Table(table)
        })?;
        parse_validated(&wrapped, "device permissions", &self.path, diagnostics)
            .and_then(|config| config.permissions)
    }

    /// The vault entry's `config` table, parsed.
    pub(super) fn vault_config(
        &self,
        diagnostics: &mut Vec<ConfigDiagnostic>,
    ) -> Option<PartialVulcanConfig> {
        parse_validated(
            self.vault_raw.as_ref()?,
            "device vault config",
            &self.path,
            diagnostics,
        )
    }

    /// Both device layers as one table, the way they combine.
    fn combined_raw(&self) -> toml::map::Map<String, TomlValue> {
        let mut combined = toml::map::Map::new();
        if let Some(permissions) = &self.permissions_raw {
            combined.insert("permissions".to_string(), permissions.clone());
        }
        if let Some(TomlValue::Table(vault)) = &self.vault_raw {
            merge_raw(&mut combined, vault);
        }
        combined
    }
}

/// `daemon.toml` in the user configuration directory.
#[must_use]
pub fn daemon_config_file() -> Option<PathBuf> {
    #[cfg(test)]
    {
        test_override::get()
    }
    #[cfg(not(test))]
    {
        crate::paths::vulcan_user_config_dir()
            .map(|directory| directory.join(DAEMON_CONFIG_FILE_NAME))
    }
}

pub(super) fn load_device_layer(
    vault_root: &Path,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<DeviceLayer> {
    load_device_layer_from(&daemon_config_file()?, vault_root, diagnostics)
}

pub(super) fn load_device_layer_from(
    path: &Path,
    vault_root: &Path,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<DeviceLayer> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            diagnostics.push(ConfigDiagnostic::message(
                path,
                format!("failed to read device config: {error}"),
            ));
            return None;
        }
    };
    let document = match toml::from_str::<TomlValue>(&source) {
        Ok(TomlValue::Table(document)) => document,
        Ok(_) => return None,
        Err(error) => {
            diagnostics.push(ConfigDiagnostic::parse_failure(
                path,
                format!("failed to parse device config: {error}"),
            ));
            return None;
        }
    };
    let permissions_raw = document.get("permissions").cloned();
    let canonical_root = fs::canonicalize(vault_root).ok();
    let vault_raw = canonical_root.and_then(|root| {
        document
            .get("vault")
            .and_then(TomlValue::as_array)?
            .iter()
            .filter_map(TomlValue::as_table)
            .find(|entry| {
                entry
                    .get("path")
                    .and_then(TomlValue::as_str)
                    .and_then(|declared| fs::canonicalize(expand_home(declared)).ok())
                    .is_some_and(|declared| declared == root)
            })
            .and_then(|entry| entry.get("config").cloned())
    });
    if permissions_raw.is_none() && vault_raw.is_none() {
        return None;
    }
    Some(DeviceLayer {
        path: path.to_path_buf(),
        permissions_raw,
        vault_raw,
    })
}

/// Reports keys that both the device layers and the vault's
/// `config.local.toml` set to different values. `config.local.toml` wins,
/// but never silently.
/// `only` limits the check to one top-level key, such as `permissions`.
pub(super) fn report_local_conflicts(
    device: &DeviceLayer,
    local_path: &Path,
    only: Option<&str>,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) {
    let Ok(source) = fs::read_to_string(local_path) else {
        return;
    };
    let Ok(TomlValue::Table(local)) = toml::from_str::<TomlValue>(&source) else {
        return;
    };
    let mut combined = device.combined_raw();
    if let Some(only) = only {
        combined.retain(|key, _| key == only);
    }
    let mut conflicts = Vec::new();
    collect_conflicts(&combined, &local, "", &mut conflicts);
    for key in conflicts {
        diagnostics.push(ConfigDiagnostic::message(
            local_path,
            format!(
                "`{key}` is set differently in {} and in {}; the vault's local config wins. Keep it in one place",
                device.path.display(),
                local_path.display()
            ),
        ));
    }
}

fn collect_conflicts(
    device: &toml::map::Map<String, TomlValue>,
    local: &toml::map::Map<String, TomlValue>,
    prefix: &str,
    conflicts: &mut Vec<String>,
) {
    for (key, device_value) in device {
        let Some(local_value) = local.get(key) else {
            continue;
        };
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match (device_value, local_value) {
            (TomlValue::Table(device), TomlValue::Table(local)) => {
                collect_conflicts(device, local, &path, conflicts);
            }
            (device, local) if device != local => conflicts.push(path),
            _ => {}
        }
    }
}

fn merge_raw(
    target: &mut toml::map::Map<String, TomlValue>,
    source: &toml::map::Map<String, TomlValue>,
) {
    for (key, value) in source {
        match (target.get_mut(key), value) {
            (Some(TomlValue::Table(target)), TomlValue::Table(source)) => merge_raw(target, source),
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

fn parse_validated(
    value: &TomlValue,
    description: &str,
    path: &Path,
    diagnostics: &mut Vec<ConfigDiagnostic>,
) -> Option<PartialVulcanConfig> {
    let rendered = match toml::to_string(value) {
        Ok(rendered) => rendered,
        Err(error) => {
            diagnostics.push(ConfigDiagnostic::message(
                path,
                format!("failed to serialize {description}: {error}"),
            ));
            return None;
        }
    };
    let parsed = toml::from_str::<PartialVulcanConfig>(&rendered)
        .map_err(|error| error.to_string())
        .and_then(|config| {
            validate_partial_vulcan_config(&config)
                .map(|()| config)
                .map_err(|error| error.to_string())
        });
    match parsed {
        Ok(config) => Some(config),
        Err(error) => {
            diagnostics.push(ConfigDiagnostic::parse_failure(
                path,
                format!("failed to parse {description}: {error}"),
            ));
            None
        }
    }
}

/// Expands a leading `~/` (or `~` alone) to the home directory, so one
/// `daemon.toml` fits devices with different home directories.
#[must_use]
pub fn expand_home(path: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    if path == "~" {
        if let Some(home) = home() {
            return home;
        }
    } else if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = home() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
pub(crate) mod test_override {
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        static DEVICE_FILE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    /// Unit tests never read the developer's real `daemon.toml`; a test
    /// that wants a device file sets one for its own thread.
    pub(crate) fn get() -> Option<PathBuf> {
        DEVICE_FILE.with(|file| file.borrow().clone())
    }

    pub(crate) struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            DEVICE_FILE.with(|file| file.borrow_mut().take());
        }
    }

    pub(crate) fn set(path: PathBuf) -> Guard {
        DEVICE_FILE.with(|file| *file.borrow_mut() = Some(path));
        Guard
    }
}
