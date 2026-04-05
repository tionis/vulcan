//! Vault trust management — persists the set of trusted vault paths in
//! `~/.config/vulcan/trusted_vaults.json`.
//!
//! Only trusted vaults may run startup scripts (`.vulcan/scripts/startup.js`)
//! and plugins. This prevents arbitrary code execution when opening an
//! untrusted vault from a shared or downloaded source.

use crate::CliError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The file where trusted vault paths are stored.
fn trusted_vaults_file() -> Result<PathBuf, CliError> {
    // Prefer $XDG_CONFIG_HOME, fall back to $HOME/.config (Linux/macOS convention).
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or_else(|| CliError::operation("could not determine user config directory"))?;
    Ok(config_dir.join("vulcan").join("trusted_vaults.json"))
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustedVaults {
    vaults: BTreeSet<PathBuf>,
}

fn load() -> Result<TrustedVaults, CliError> {
    let path = trusted_vaults_file()?;
    if !path.exists() {
        return Ok(TrustedVaults::default());
    }
    let content = std::fs::read_to_string(&path).map_err(CliError::operation)?;
    serde_json::from_str(&content).map_err(CliError::operation)
}

fn save(data: &TrustedVaults) -> Result<(), CliError> {
    let path = trusted_vaults_file()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    let content = serde_json::to_string_pretty(data).map_err(CliError::operation)?;
    std::fs::write(&path, content).map_err(CliError::operation)
}

/// Returns `true` if `vault_root` is in the trusted vaults list.
pub fn is_trusted(vault_root: &Path) -> bool {
    let Ok(canonical) = vault_root.canonicalize() else {
        return false;
    };
    load()
        .map(|data| data.vaults.contains(&canonical))
        .unwrap_or(false)
}

/// Mark `vault_root` as trusted. Returns `true` if it was newly added.
pub fn add_trust(vault_root: &Path) -> Result<bool, CliError> {
    let canonical = vault_root
        .canonicalize()
        .map_err(|e| CliError::operation(format!("cannot canonicalize vault path: {e}")))?;
    let mut data = load()?;
    let added = data.vaults.insert(canonical);
    save(&data)?;
    Ok(added)
}

/// Remove trust from `vault_root`. Returns `true` if it was present.
pub fn revoke_trust(vault_root: &Path) -> Result<bool, CliError> {
    let canonical = match vault_root.canonicalize() {
        Ok(p) => p,
        Err(_) => vault_root.to_path_buf(),
    };
    let mut data = load()?;
    let removed = data.vaults.remove(&canonical);
    save(&data)?;
    Ok(removed)
}

/// Return the list of all trusted vault paths.
pub fn list_trusted() -> Result<Vec<PathBuf>, CliError> {
    Ok(load()?.vaults.into_iter().collect())
}
