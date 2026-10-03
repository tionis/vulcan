//! Vault trust management — persists the set of trusted vault paths in
//! the per-user Vulcan config directory.
//!
//! Only trusted vaults may run startup scripts (`.vulcan/scripts/startup.js`)
//! and plugins. This prevents arbitrary code execution when opening an
//! untrusted vault from a shared or downloaded source.

use crate::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Mutex, OnceLock};
use vulcan_core::durable::{self, Durability};

/// The file where trusted vault paths are stored.
fn trusted_vaults_file() -> Result<PathBuf, AppError> {
    vulcan_core::trusted_vaults_file()
        .ok_or_else(|| AppError::operation("could not determine user config directory"))
}

#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustedVaults {
    vaults: BTreeSet<PathBuf>,
}

fn load() -> Result<TrustedVaults, AppError> {
    let path = trusted_vaults_file()?;
    Ok(
        durable::read_json(&path, vulcan_core::MAX_TRUSTED_VAULTS_FILE_BYTES)
            .map_err(AppError::operation)?
            .unwrap_or_default(),
    )
}

/// Replaces the list atomically and durably: a crash mid-write must never
/// leave a truncated file, which would silently untrust every vault.
fn save(data: &TrustedVaults) -> Result<(), AppError> {
    let path = trusted_vaults_file()?;
    durable::replace_json(&path, data, Durability::Full).map_err(AppError::operation)
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
    use super::{add_trust, is_trusted, list_trusted, revoke_trust, test_env_lock};
    use tempfile::tempdir;

    #[test]
    fn trust_roundtrip_marks_and_unmarks_vaults() {
        let _lock = test_env_lock().lock().expect("test env lock");
        let config_home = tempdir().expect("config home");
        let vault = tempdir().expect("vault");
        let previous_xdg = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", config_home.path());

        assert!(!is_trusted(vault.path()));
        assert!(add_trust(vault.path()).expect("trust should be added"));
        assert!(is_trusted(vault.path()));
        assert_eq!(list_trusted().expect("trusts should load").len(), 1);
        let file = vulcan_core::trusted_vaults_file().expect("trust file");
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).expect("saved list"))
                .expect("a saved list is complete JSON");
        assert_eq!(saved["vaults"].as_array().map(Vec::len), Some(1));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the trust list is owner-only");
        }
        assert!(revoke_trust(vault.path()).expect("trust should be removed"));
        assert!(!is_trusted(vault.path()));

        std::fs::write(&file, b"{\"vaults\": [").expect("truncated list");
        assert!(
            add_trust(vault.path()).is_err(),
            "a damaged list must be reported, not silently replaced"
        );
        match previous_xdg {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
}
