use std::fs;
use std::path::{Path, PathBuf};

pub const VULCAN_DIR_NAME: &str = ".vulcan";
pub const CACHE_DB_NAME: &str = "cache.db";
pub const CONFIG_FILE_NAME: &str = "config.toml";
pub const GITIGNORE_FILE_NAME: &str = ".gitignore";
pub const DEFAULT_ATTACHMENT_FOLDER: &str = ".";
const DEFAULT_VULCAN_GITIGNORE: &str = "*\n!.gitignore\n!config.toml\n";

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
    pub fn gitignore_file(&self) -> PathBuf {
        self.vulcan_dir.join(GITIGNORE_FILE_NAME)
    }

    #[must_use]
    pub fn relative_to_vault(&self, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(&self.vault_root)
            .ok()
            .map(Path::to_path_buf)
    }
}

pub(crate) fn ensure_vulcan_dir(paths: &VaultPaths) -> Result<(), std::io::Error> {
    fs::create_dir_all(paths.vulcan_dir())?;

    let gitignore = paths.gitignore_file();
    if !gitignore.exists() {
        fs::write(gitignore, DEFAULT_VULCAN_GITIGNORE)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
        assert_eq!(
            paths.gitignore_file(),
            PathBuf::from("/tmp/example-vault/.vulcan/.gitignore")
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

    #[test]
    fn ensure_vulcan_dir_creates_default_gitignore_without_overwriting() {
        let temp_dir = TempDir::new().expect("temp dir should be created");
        let paths = VaultPaths::new(temp_dir.path());

        ensure_vulcan_dir(&paths).expect("vulcan dir should be created");
        assert_eq!(
            fs::read_to_string(paths.gitignore_file()).expect("gitignore should exist"),
            DEFAULT_VULCAN_GITIGNORE
        );

        fs::write(paths.gitignore_file(), "custom\n").expect("custom gitignore should be written");
        ensure_vulcan_dir(&paths).expect("vulcan dir should remain available");
        assert_eq!(
            fs::read_to_string(paths.gitignore_file()).expect("gitignore should remain readable"),
            "custom\n"
        );
    }
}
