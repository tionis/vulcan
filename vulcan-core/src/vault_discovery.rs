//! Vault root discovery for vaults that live inside larger directory trees.
//!
//! A vault is often one directory of a larger Git repository, for example the
//! `docs/` directory of an MkDocs site. Discovery lets commands run from any
//! directory inside such a vault (or from an MkDocs project root) without an
//! explicit `--vault` flag while keeping the vault itself the unit that Vulcan
//! indexes and mutates.
//!
//! Discovery is purely filesystem based. It never runs Git, never creates
//! files, and never crosses a Git work-tree boundary or climbs into the user's
//! home directory from below it.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::paths::VULCAN_DIR_NAME;

/// MkDocs configuration files recognized when resolving an MkDocs `docs_dir`.
pub const MKDOCS_CONFIG_FILE_NAMES: [&str; 2] = ["mkdocs.yml", "mkdocs.yaml"];
/// MkDocs' default documentation directory when `docs_dir` is not configured.
pub const MKDOCS_DEFAULT_DOCS_DIR: &str = "docs";

/// How a vault root was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultRootSource {
    /// The caller named the vault root explicitly.
    Explicit,
    /// The nearest enclosing directory that contains `.vulcan/`.
    VulcanDirectory,
    /// The `docs_dir` of an enclosing MkDocs project.
    #[serde(rename = "mkdocs")]
    MkDocs,
    /// No marker was found, so the starting directory is the vault root.
    WorkingDirectory,
}

/// The chosen vault root and why it was chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VaultRootDiscovery {
    pub root: PathBuf,
    pub source: VaultRootSource,
    /// MkDocs configuration file that selected the root, when `source` is `mkdocs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mkdocs_config: Option<PathBuf>,
}

impl VaultRootDiscovery {
    #[must_use]
    pub fn explicit(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            source: VaultRootSource::Explicit,
            mkdocs_config: None,
        }
    }

    fn working_directory(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            source: VaultRootSource::WorkingDirectory,
            mkdocs_config: None,
        }
    }
}

/// An MkDocs project and its resolved documentation directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MkDocsProject {
    pub project_root: PathBuf,
    pub config_file: PathBuf,
    pub docs_dir: PathBuf,
}

/// Discovers the vault root for a command started in `start` (normally the
/// current directory) without an explicit vault.
///
/// Resolution order:
/// 1. the nearest directory at or above `start` containing `.vulcan/`;
/// 2. the `docs_dir` of the nearest enclosing MkDocs project, when `start` is
///    the MkDocs project root or lies inside its `docs_dir`;
/// 3. `start` itself.
///
/// The upward walk stops at the first Git work-tree root (inclusive) and never
/// considers the user's home directory unless `start` is the home directory.
#[must_use]
pub fn discover_vault_root(start: &Path) -> VaultRootDiscovery {
    discover_vault_root_with_home(start, home_dir().as_deref())
}

/// Chooses the root for `vulcan init` started in `start` without an explicit
/// vault. Initialization never adopts an ancestor vault, but an MkDocs project
/// root or a directory inside its `docs_dir` initializes the `docs_dir`.
#[must_use]
pub fn discover_init_root(start: &Path) -> VaultRootDiscovery {
    discover_init_root_with_home(start, home_dir().as_deref())
}

fn discover_vault_root_with_home(start: &Path, home: Option<&Path>) -> VaultRootDiscovery {
    for directory in bounded_ancestors(start, home) {
        if directory.join(VULCAN_DIR_NAME).is_dir() {
            return VaultRootDiscovery {
                root: directory.to_path_buf(),
                source: if directory == start {
                    VaultRootSource::WorkingDirectory
                } else {
                    VaultRootSource::VulcanDirectory
                },
                mkdocs_config: None,
            };
        }
    }
    discover_init_root_with_home(start, home)
}

fn discover_init_root_with_home(start: &Path, home: Option<&Path>) -> VaultRootDiscovery {
    if let Some(project) = enclosing_mkdocs_project_with_home(start, home) {
        let selects_docs = start == project.project_root || start.starts_with(&project.docs_dir);
        if selects_docs && project.docs_dir.is_dir() && project.docs_dir != start {
            return VaultRootDiscovery {
                root: project.docs_dir,
                source: VaultRootSource::MkDocs,
                mkdocs_config: Some(project.config_file),
            };
        }
    }
    VaultRootDiscovery::working_directory(start)
}

/// Finds the nearest MkDocs project at or above `start`, within the same
/// bounds as vault discovery.
#[must_use]
pub fn enclosing_mkdocs_project(start: &Path) -> Option<MkDocsProject> {
    enclosing_mkdocs_project_with_home(start, home_dir().as_deref())
}

fn enclosing_mkdocs_project_with_home(start: &Path, home: Option<&Path>) -> Option<MkDocsProject> {
    bounded_ancestors(start, home).find_map(mkdocs_project_at)
}

/// Reads the MkDocs project whose configuration file lives directly in `directory`.
#[must_use]
pub fn mkdocs_project_at(directory: &Path) -> Option<MkDocsProject> {
    let config_file = MKDOCS_CONFIG_FILE_NAMES
        .iter()
        .map(|name| directory.join(name))
        .find(|path| path.is_file())?;
    let docs_dir = fs::read_to_string(&config_file)
        .ok()
        .and_then(|source| mkdocs_docs_dir(&source))
        .unwrap_or_else(|| MKDOCS_DEFAULT_DOCS_DIR.to_string());
    Some(MkDocsProject {
        project_root: directory.to_path_buf(),
        docs_dir: directory.join(docs_dir),
        config_file,
    })
}

/// Extracts the top-level `docs_dir` value from MkDocs YAML.
///
/// MkDocs configuration commonly contains Python-specific YAML tags (for
/// example `!!python/name:`), so this reads only the top-level key instead of
/// requiring the whole document to deserialize.
#[must_use]
pub fn mkdocs_docs_dir(source: &str) -> Option<String> {
    source.lines().find_map(|line| {
        let value = line.strip_prefix("docs_dir:")?;
        let value = strip_yaml_comment(value).trim();
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .or_else(|| {
                value
                    .strip_prefix('\'')
                    .and_then(|value| value.strip_suffix('\''))
            })
            .unwrap_or(value)
            .trim_end_matches('/');
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn strip_yaml_comment(value: &str) -> &str {
    let mut quote = None;
    let mut previous_is_space = true;
    for (index, character) in value.char_indices() {
        match (quote, character) {
            (None, '"' | '\'') => quote = Some(character),
            (Some(open), _) if character == open => quote = None,
            (None, '#') if previous_is_space => return &value[..index],
            _ => {}
        }
        previous_is_space = character.is_whitespace();
    }
    value
}

/// Returns the nearest directory at or above `path` that contains a `.git`
/// entry (a directory for normal checkouts, a file for linked worktrees and
/// submodules).
#[must_use]
pub fn enclosing_git_work_tree(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|directory| is_git_work_tree_root(directory))
        .map(Path::to_path_buf)
}

/// Whether `directory` holds Git metadata: a `.git` directory with a `HEAD`
/// file, or a `.git` file pointing at a separate Git directory. Empty or
/// unrelated `.git` entries are not treated as repository boundaries.
#[must_use]
pub fn is_git_work_tree_root(directory: &Path) -> bool {
    let marker = directory.join(".git");
    let Ok(metadata) = fs::metadata(&marker) else {
        return false;
    };
    if metadata.is_dir() {
        return marker.join("HEAD").is_file();
    }
    metadata.is_file()
        && fs::read_to_string(&marker).is_ok_and(|content| content.starts_with("gitdir:"))
}

/// Ancestors of `start`, stopping after the first Git work-tree root and
/// before the home directory (unless `start` is the home directory).
fn bounded_ancestors<'a>(
    start: &'a Path,
    home: Option<&'a Path>,
) -> impl Iterator<Item = &'a Path> + 'a {
    let mut passed_work_tree_root = false;
    start.ancestors().take_while(move |directory| {
        if passed_work_tree_root || (*directory != start && Some(*directory) == home) {
            return false;
        }
        passed_work_tree_root = is_git_work_tree_root(directory);
        true
    })
}

fn home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn mkdir(path: &Path) {
        fs::create_dir_all(path).expect("directory should be created");
    }

    fn git_marker(work_tree: &Path) {
        mkdir(&work_tree.join(".git"));
        fs::write(work_tree.join(".git/HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    }

    #[test]
    fn nearest_vulcan_directory_wins_over_start() {
        let temp = TempDir::new().expect("temp dir");
        let vault = temp.path().join("vault");
        mkdir(&vault.join(".vulcan"));
        mkdir(&vault.join("Projects/Deep"));

        let discovery = discover_vault_root_with_home(&vault.join("Projects/Deep"), None);

        assert_eq!(discovery.root, vault);
        assert_eq!(discovery.source, VaultRootSource::VulcanDirectory);
        let at_root = discover_vault_root_with_home(&vault, None);
        assert_eq!(at_root.root, vault);
        assert_eq!(at_root.source, VaultRootSource::WorkingDirectory);
    }

    #[test]
    fn discovery_does_not_cross_git_work_tree_or_home_boundaries() {
        let temp = TempDir::new().expect("temp dir");
        mkdir(&temp.path().join(".vulcan"));
        let repo = temp.path().join("repo");
        git_marker(&repo);
        mkdir(&repo.join("notes"));

        let inside_repo = discover_vault_root_with_home(&repo.join("notes"), None);
        assert_eq!(inside_repo.root, repo.join("notes"));
        assert_eq!(inside_repo.source, VaultRootSource::WorkingDirectory);

        let home = temp.path().join("home");
        mkdir(&home.join(".vulcan"));
        mkdir(&home.join("scratch"));
        let below_home = discover_vault_root_with_home(&home.join("scratch"), Some(&home));
        assert_eq!(below_home.root, home.join("scratch"));
        let at_home = discover_vault_root_with_home(&home, Some(&home));
        assert_eq!(at_home.root, home);
    }

    #[test]
    fn mkdocs_project_root_and_docs_subdirectories_select_docs_dir() {
        let temp = TempDir::new().expect("temp dir");
        let site = temp.path().join("site");
        git_marker(&site);
        mkdir(&site.join("content/guide"));
        mkdir(&site.join("src"));
        fs::write(
            site.join("mkdocs.yml"),
            "site_name: Demo\ndocs_dir: 'content' # custom\nmarkdown_extensions:\n  - pymdownx.emoji:\n      emoji_index: !!python/name:material.extensions.emoji.twemoji\n",
        )
        .expect("mkdocs config");

        for start in [site.clone(), site.join("content/guide")] {
            let discovery = discover_vault_root_with_home(&start, None);
            assert_eq!(discovery.root, site.join("content"), "start {start:?}");
            assert_eq!(discovery.source, VaultRootSource::MkDocs);
            assert_eq!(discovery.mkdocs_config, Some(site.join("mkdocs.yml")));
        }
        // Unrelated project directories keep their own root.
        let source_dir = discover_vault_root_with_home(&site.join("src"), None);
        assert_eq!(source_dir.root, site.join("src"));
        // Starting at the docs_dir itself is an ordinary working-directory root.
        let docs = discover_vault_root_with_home(&site.join("content"), None);
        assert_eq!(docs.source, VaultRootSource::WorkingDirectory);
    }

    #[test]
    fn initialized_docs_vault_is_found_from_the_project_root() {
        let temp = TempDir::new().expect("temp dir");
        let site = temp.path().join("site");
        git_marker(&site);
        mkdir(&site.join("docs/.vulcan"));
        fs::write(site.join("mkdocs.yml"), "site_name: Demo\n").expect("mkdocs config");

        let discovery = discover_vault_root_with_home(&site, None);
        assert_eq!(discovery.root, site.join("docs"));
        assert_eq!(discovery.source, VaultRootSource::MkDocs);

        // An explicitly initialized repository-root vault takes precedence.
        mkdir(&site.join(".vulcan"));
        let root_vault = discover_vault_root_with_home(&site, None);
        assert_eq!(root_vault.root, site);
    }

    #[test]
    fn init_root_ignores_ancestor_vaults_but_follows_mkdocs() {
        let temp = TempDir::new().expect("temp dir");
        mkdir(&temp.path().join("outer/.vulcan"));
        mkdir(&temp.path().join("outer/inner"));
        let discovery = discover_init_root_with_home(&temp.path().join("outer/inner"), None);
        assert_eq!(discovery.root, temp.path().join("outer/inner"));

        let site = temp.path().join("site");
        mkdir(&site.join("docs"));
        fs::write(site.join("mkdocs.yaml"), "site_name: Demo\n").expect("mkdocs config");
        let mkdocs = discover_init_root_with_home(&site, None);
        assert_eq!(mkdocs.root, site.join("docs"));
        assert_eq!(mkdocs.mkdocs_config, Some(site.join("mkdocs.yaml")));
    }

    #[test]
    fn mkdocs_docs_dir_parser_reads_only_top_level_values() {
        assert_eq!(
            mkdocs_docs_dir("docs_dir: site-docs\n"),
            Some("site-docs".into())
        );
        assert_eq!(mkdocs_docs_dir("docs_dir: \"a b/\"\n"), Some("a b".into()));
        assert_eq!(
            mkdocs_docs_dir("docs_dir: 'x#y' # note\n"),
            Some("x#y".into())
        );
        assert_eq!(mkdocs_docs_dir("plugins:\n  docs_dir: nested\n"), None);
        assert_eq!(mkdocs_docs_dir("docs_dir:\n"), None);
    }

    #[test]
    fn enclosing_git_work_tree_accepts_git_files() {
        let temp = TempDir::new().expect("temp dir");
        let worktree = temp.path().join("linked");
        mkdir(&worktree.join("docs"));
        fs::write(worktree.join(".git"), "gitdir: /elsewhere\n").expect("git file");
        assert_eq!(
            enclosing_git_work_tree(&worktree.join("docs")),
            Some(worktree.clone())
        );
        // An empty `.git` directory is not a repository boundary.
        let bogus = temp.path().join("bogus");
        mkdir(&bogus.join(".git"));
        mkdir(&bogus.join(".vulcan"));
        mkdir(&bogus.join("child/sub"));
        assert!(!is_git_work_tree_root(&bogus));
        let discovery = discover_vault_root_with_home(&bogus.join("child/sub"), None);
        assert_eq!(discovery.root, bogus);
    }
}
