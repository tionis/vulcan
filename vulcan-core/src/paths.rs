use std::path::{Path, PathBuf};

pub const VULCAN_DIR_NAME: &str = ".vulcan";
pub const CACHE_DB_NAME: &str = "cache.db";
pub const CONFIG_FILE_NAME: &str = "config.toml";
pub const DEFAULT_ATTACHMENT_FOLDER: &str = ".";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultPaths {
    vault_root: PathBuf,
    vulcan_dir: PathBuf,
    cache_db: PathBuf,
    config_file: PathBuf,
}

impl VaultPaths {
    #[must_use]
    pub fn new(vault_root: impl Into<PathBuf>) -> Self {
        let vault_root = vault_root.into();
        let vulcan_dir = vault_root.join(VULCAN_DIR_NAME);

        Self {
            cache_db: vulcan_dir.join(CACHE_DB_NAME),
            config_file: vulcan_dir.join(CONFIG_FILE_NAME),
            vulcan_dir,
            vault_root,
        }
    }

    #[must_use]
    pub fn vault_root(&self) -> &Path {
        &self.vault_root
    }

    #[must_use]
    pub fn vulcan_dir(&self) -> &Path {
        &self.vulcan_dir
    }

    #[must_use]
    pub fn cache_db(&self) -> &Path {
        &self.cache_db
    }

    #[must_use]
    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    #[must_use]
    pub fn relative_to_vault(&self, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(&self.vault_root)
            .ok()
            .map(Path::to_path_buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_standard_vulcan_paths() {
        let paths = VaultPaths::new("/tmp/example-vault");

        assert_eq!(paths.vault_root(), Path::new("/tmp/example-vault"));
        assert_eq!(paths.vulcan_dir(), Path::new("/tmp/example-vault/.vulcan"));
        assert_eq!(
            paths.cache_db(),
            Path::new("/tmp/example-vault/.vulcan/cache.db")
        );
        assert_eq!(
            paths.config_file(),
            Path::new("/tmp/example-vault/.vulcan/config.toml")
        );
    }

    #[test]
    fn computes_relative_paths_from_vault_root() {
        let paths = VaultPaths::new("/tmp/example-vault");
        let file = Path::new("/tmp/example-vault/notes/alpha.md");

        assert_eq!(
            paths.relative_to_vault(file),
            Some(PathBuf::from("notes/alpha.md"))
        );
    }

    #[test]
    fn returns_none_for_paths_outside_the_vault() {
        let paths = VaultPaths::new("/tmp/example-vault");
        let file = Path::new("/tmp/other-vault/notes/alpha.md");

        assert_eq!(paths.relative_to_vault(file), None);
    }
}
