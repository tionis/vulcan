use std::fs;
use std::path::{Component, Path, PathBuf};

pub const VULCAN_DIR_NAME: &str = ".vulcan";
pub const CACHE_DB_NAME: &str = "cache.db";
pub const CONFIG_FILE_NAME: &str = "config.toml";
pub const LOCAL_CONFIG_FILE_NAME: &str = "config.local.toml";
pub const GITIGNORE_FILE_NAME: &str = ".gitignore";
pub const REPORTS_DIR_NAME: &str = "reports";
pub const DEFAULT_ATTACHMENT_FOLDER: &str = ".";
const DEFAULT_VULCAN_GITIGNORE: &str =
    "*\n!.gitignore\n!config.toml\nconfig.local.toml\n!reports/\nreports/*\n!reports/*.toml\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelativePathOptions {
    pub expected_extension: Option<&'static str>,
    pub append_extension_if_missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelativePathError {
    path: String,
    expected_extension: Option<&'static str>,
}

impl RelativePathError {
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl std::fmt::Display for RelativePathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.expected_extension {
            Some(extension) => write!(
                formatter,
                "expected a relative .{extension} path without control characters or traversal: {}",
                self.path
            ),
            None => write!(
                formatter,
                "expected a relative path without control characters or traversal: {}",
                self.path
            ),
        }
    }
}

impl std::error::Error for RelativePathError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultPaths {
    vault_root: PathBuf,
    vulcan_dir: PathBuf,
    cache_db: PathBuf,
    config_file: PathBuf,
    local_config_file: PathBuf,
    reports_dir: PathBuf,
}

impl VaultPaths {
    #[must_use]
    pub fn new(vault_root: impl Into<PathBuf>) -> Self {
        let vault_root = vault_root.into();
        let vulcan_dir = vault_root.join(VULCAN_DIR_NAME);

        Self {
            cache_db: vulcan_dir.join(CACHE_DB_NAME),
            config_file: vulcan_dir.join(CONFIG_FILE_NAME),
            local_config_file: vulcan_dir.join(LOCAL_CONFIG_FILE_NAME),
            reports_dir: vulcan_dir.join(REPORTS_DIR_NAME),
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
    pub fn local_config_file(&self) -> &Path {
        &self.local_config_file
    }

    #[must_use]
    pub fn reports_dir(&self) -> &Path {
        &self.reports_dir
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

pub fn ensure_vulcan_dir(paths: &VaultPaths) -> Result<(), std::io::Error> {
    fs::create_dir_all(paths.vulcan_dir())?;
    fs::create_dir_all(paths.reports_dir())?;

    let gitignore = paths.gitignore_file();
    if !gitignore.exists() {
        fs::write(gitignore, DEFAULT_VULCAN_GITIGNORE)?;
    }

    Ok(())
}

pub fn normalize_relative_input_path(
    path: &str,
    options: RelativePathOptions,
) -> Result<String, RelativePathError> {
    if path.is_empty() || path.chars().any(char::is_control) {
        return Err(RelativePathError {
            path: path.to_string(),
            expected_extension: options.expected_extension,
        });
    }

    let mut parts = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err(RelativePathError {
                    path: path.to_string(),
                    expected_extension: options.expected_extension,
                });
            }
        }
    }

    if parts.is_empty() {
        return Err(RelativePathError {
            path: path.to_string(),
            expected_extension: options.expected_extension,
        });
    }

    let mut normalized = parts.join("/");
    if let Some(expected_extension) = options.expected_extension {
        if options.append_extension_if_missing && Path::new(&normalized).extension().is_none() {
            normalized.push('.');
            normalized.push_str(expected_extension);
        }

        if !Path::new(&normalized)
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case(expected_extension))
        {
            return Err(RelativePathError {
                path: path.to_string(),
                expected_extension: options.expected_extension,
            });
        }
    }

    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use tempfile::TempDir;

    fn path_segment_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[A-Za-z0-9_-]{1,8}")
            .expect("path segment regex should be valid")
    }

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
            paths.local_config_file(),
            Path::new("/tmp/example-vault/.vulcan/config.local.toml")
        );
        assert_eq!(
            paths.reports_dir(),
            Path::new("/tmp/example-vault/.vulcan/reports")
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
        assert!(paths.reports_dir().exists());
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

    #[test]
    fn normalize_relative_input_path_rejects_traversal_and_controls() {
        for invalid in ["../outside.md", "/absolute.md", "bad\nname.md", ""] {
            assert!(normalize_relative_input_path(
                invalid,
                RelativePathOptions {
                    expected_extension: Some("md"),
                    append_extension_if_missing: true,
                }
            )
            .is_err());
        }
    }

    #[test]
    fn normalize_relative_input_path_normalizes_and_appends_extensions() {
        assert_eq!(
            normalize_relative_input_path(
                "./notes/alpha",
                RelativePathOptions {
                    expected_extension: Some("md"),
                    append_extension_if_missing: true,
                }
            )
            .expect("path should normalize"),
            "notes/alpha.md"
        );
        assert_eq!(
            normalize_relative_input_path(
                "./views/release.base",
                RelativePathOptions {
                    expected_extension: Some("base"),
                    append_extension_if_missing: false,
                }
            )
            .expect("path should normalize"),
            "views/release.base"
        );
    }

    #[test]
    fn normalize_relative_input_path_rejects_wrong_extension() {
        assert!(normalize_relative_input_path(
            "release.md",
            RelativePathOptions {
                expected_extension: Some("base"),
                append_extension_if_missing: false,
            }
        )
        .is_err());
    }

    proptest! {
        #[test]
        fn normalize_relative_input_path_is_idempotent_for_valid_markdown_paths(
            mut segments in prop::collection::vec(path_segment_strategy(), 1..4),
            include_current_dir in any::<bool>(),
            include_extension in any::<bool>(),
        ) {
            let last = segments
                .last_mut()
                .expect("segment list should always include at least one segment");
            if include_extension {
                last.push_str(".md");
            }

            let raw = if include_current_dir {
                format!("./{}", segments.join("/"))
            } else {
                segments.join("/")
            };
            let options = RelativePathOptions {
                expected_extension: Some("md"),
                append_extension_if_missing: true,
            };

            let normalized = normalize_relative_input_path(&raw, options)
                .expect("generated markdown path should normalize");

            prop_assert!(Path::new(&normalized)
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md")));
            prop_assert!(!normalized.contains("/./"));
            prop_assert_eq!(
                normalize_relative_input_path(&normalized, options)
                    .expect("normalized path should remain valid"),
                normalized
            );
        }
    }
}
