//! Vault trust management — persists the set of trusted vault paths in
//! `~/.config/vulcan/trusted_vaults.json`.
//!
//! Only trusted vaults may run startup scripts (`.vulcan/scripts/startup.js`)
//! and plugins. This prevents arbitrary code execution when opening an
//! untrusted vault from a shared or downloaded source.

use crate::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The file where trusted vault paths are stored.
fn trusted_vaults_file() -> Result<PathBuf, AppError> {
    // Prefer $XDG_CONFIG_HOME, fall back to $HOME/.config (Linux/macOS convention).
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or_else(|| AppError::operation("could not determine user config directory"))?;
    Ok(config_dir.join("vulcan").join("trusted_vaults.json"))
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustedVaults {
    vaults: BTreeSet<PathBuf>,
}

fn load() -> Result<TrustedVaults, AppError> {
    let path = trusted_vaults_file()?;
    if !path.exists() {
        return Ok(TrustedVaults::default());
    }
    let content = std::fs::read_to_string(&path).map_err(AppError::operation)?;
    serde_json::from_str(&content).map_err(AppError::operation)
}

fn save(data: &TrustedVaults) -> Result<(), AppError> {
    let path = trusted_vaults_file()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(AppError::operation)?;
    }
    let content = serde_json::to_string_pretty(data).map_err(AppError::operation)?;
    std::fs::write(&path, content).map_err(AppError::operation)
}

/// Returns `true` if `vault_root` is in the trusted vaults list.
#[must_use]
pub fn is_trusted(vault_root: &Path) -> bool {
    let Ok(canonical) = vault_root.canonicalize() else {
        return false;
    };
    load().is_ok_and(|data| data.vaults.contains(&canonical))
}

/// Mark `vault_root` as trusted. Returns `true` if it was newly added.
pub fn add_trust(vault_root: &Path) -> Result<bool, AppError> {
    let canonical = vault_root
        .canonicalize()
        .map_err(|e| AppError::operation(format!("cannot canonicalize vault path: {e}")))?;
    let mut data = load()?;
    let added = data.vaults.insert(canonical);
    save(&data)?;
    Ok(added)
}

/// Remove trust from `vault_root`. Returns `true` if it was present.
pub fn revoke_trust(vault_root: &Path) -> Result<bool, AppError> {
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
pub fn list_trusted() -> Result<Vec<PathBuf>, AppError> {
    Ok(load()?.vaults.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::{add_trust, is_trusted, list_trusted, revoke_trust};
    use tempfile::tempdir;

    #[test]
    fn trust_roundtrip_marks_and_unmarks_vaults() {
        let config_home = tempdir().expect("config home");
        let vault = tempdir().expect("vault");
        std::env::set_var("XDG_CONFIG_HOME", config_home.path());

        assert!(!is_trusted(vault.path()));
        assert!(add_trust(vault.path()).expect("trust should be added"));
        assert!(is_trusted(vault.path()));
        assert_eq!(list_trusted().expect("trusts should load").len(), 1);
        assert!(revoke_trust(vault.path()).expect("trust should be removed"));
        assert!(!is_trusted(vault.path()));
    }
}
